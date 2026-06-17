//! BrainCraft HAT — on-device AI interface + camera viewfinder + voice assistant.
//!
//! The Adafruit BrainCraft HAT gives an Orb a 240x240 ST7789 TFT, buttons, and
//! (optionally) a speaker/mic. The `aeon-braincraft` daemon owns all the device
//! work — display, buttons, the 3 modes (ai / viewfinder / voice), and the
//! local|hosted voice backend — and publishes state to /run/aeon/braincraft.json.
//! The heavy voice/display stack (Piper TTS, Vosk ASR, display libs) is NOT baked;
//! it's installed on demand by `/usr/local/bin/aeon-voice`, button-driven from the
//! web console exactly like the Hailo runtime.
//!
//! This module is the Rust control plane: it relays the daemon's status, owns
//! `/etc/aeon/braincraft.toml` (the daemon re-reads it every loop, so a PUT takes
//! effect without a restart), pushes transient actions (photo/record/say) to the
//! daemon via an atomically-claimed command file, and drives the non-blocking
//! voice install with a progress-polled TASKS map.
//!
//! Mirrors `vision.rs` (relay a daemon's /run/aeon JSON) for status and `hailo.rs`
//! (config TOML + run_script + spawn_blocking install + TASKS progress) for the
//! voice install. Off by default (`enabled = false`); the daemon self-idles when
//! disabled or no HAT is present, so the feature is harmless on a bare Pi.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write as _;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/braincraft.toml";
const STATUS_JSON: &str = "/run/aeon/braincraft.json";
const CMD_FILE: &str = "/run/aeon/braincraft.cmd";
const VOICE_SCRIPT: &str = "/usr/local/bin/aeon-voice";
const VOICE_INSTALL_LOG: &str = "/run/aeon/voice-install.log";

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BraincraftConfig {
    /// Off by default. The daemon idles (writes {"present":false}) until on.
    #[serde(default)]
    pub enabled: bool,
    /// Active mode: "ai" (interface), "viewfinder" (camera), or "voice".
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Draw the live vision.json detection boxes over the viewfinder.
    #[serde(default = "default_true")]
    pub overlay: bool,
    #[serde(default)]
    pub audio: AudioCfg,
    #[serde(default)]
    pub backend: BackendCfg,
    #[serde(default)]
    pub hosted: HostedCfg,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioCfg {
    /// "auto" | "usb" | "wm8960" | <alsa name>. USB is the supported default;
    /// the WM8960 I2S codec is unverified on Pi 5 (RP1), so it's opt-in.
    #[serde(default = "default_auto")]
    pub device: String,
    #[serde(default = "default_kokoro")]
    pub tts: String,
    #[serde(default = "default_whisper")]
    pub asr: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackendCfg {
    /// "local" (sherpa-onnx Kokoro/Whisper + on-device Hailo LLM) or "hosted"
    /// (DGX Spark + persona).
    #[serde(default = "default_local")]
    pub mode: String,
    /// Hosted persona name (qwen3-tts voice / gateway persona). Empty = none.
    #[serde(default)]
    pub persona: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HostedCfg {
    #[serde(default = "default_llm_url")]
    pub llm_url: String,
    #[serde(default = "default_tts_url")]
    pub tts_url: String,
    #[serde(default = "default_asr_url")]
    pub asr_url: String,
}

fn default_mode() -> String { "ai".into() }
fn default_true() -> bool { true }
fn default_auto() -> String { "auto".into() }
fn default_kokoro() -> String { "kokoro".into() }
fn default_whisper() -> String { "whisper".into() }
fn default_local() -> String { "local".into() }
fn default_llm_url() -> String { "http://192.168.1.116:8000/v1".into() }
fn default_tts_url() -> String { "http://192.168.1.116:8002".into() }
fn default_asr_url() -> String { "http://192.168.1.116:8001".into() }

impl Default for AudioCfg {
    fn default() -> Self {
        Self { device: default_auto(), tts: default_kokoro(), asr: default_whisper() }
    }
}
impl Default for BackendCfg {
    fn default() -> Self {
        Self { mode: default_local(), persona: String::new() }
    }
}
impl Default for HostedCfg {
    fn default() -> Self {
        Self { llm_url: default_llm_url(), tts_url: default_tts_url(), asr_url: default_asr_url() }
    }
}
impl Default for BraincraftConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_mode(),
            overlay: true,
            audio: AudioCfg::default(),
            backend: BackendCfg::default(),
            hosted: HostedCfg::default(),
        }
    }
}

fn read_config() -> BraincraftConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &BraincraftConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

// ── non-blocking voice-install task state (mirrors hailo.rs) ───────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct TaskState {
    pub phase: String,
    pub percent: f64,
    pub done: bool,
    pub ok: bool,
    pub log: String,
}

static TASKS: OnceLock<Mutex<HashMap<String, TaskState>>> = OnceLock::new();

fn tasks() -> &'static Mutex<HashMap<String, TaskState>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn set_task(key: &str, st: TaskState) {
    if let Ok(mut m) = tasks().lock() {
        m.insert(key.to_string(), st);
    }
}
fn get_task(key: &str) -> Option<TaskState> {
    tasks().lock().ok().and_then(|m| m.get(key).cloned())
}
fn task_running(key: &str) -> bool {
    get_task(key).map(|t| !t.done).unwrap_or(false)
}

// ── infra script (aeon-voice) ──────────────────────────────────────────────────

fn run_script(args: &[&str]) -> Result<String, String> {
    let out = Command::new(VOICE_SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {VOICE_SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{VOICE_SCRIPT} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `aeon-voice <cmd>` → parsed JSON, or a canned fallback so the device-free web
/// scaffold renders when the script/deps are absent.
fn script_json(args: &[&str], fallback: Value) -> Value {
    run_script(args)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(fallback)
}

fn log_tail(path: &str, lines: usize) -> String {
    std::fs::read_to_string(path)
        .map(|s| {
            let all: Vec<&str> = s.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        })
        .unwrap_or_default()
}

/// Parse the most recent `PHASE <name> <pct>` line into (phase, percent).
fn parse_progress(path: &str) -> (String, f64) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut phase = "starting".to_string();
    let mut percent = 0.0_f64;
    for line in text.lines() {
        let l = line.trim();
        let rest = l.strip_prefix("PHASE ").or_else(|| l.strip_prefix("PHASE\t"));
        if let Some(rest) = rest {
            let mut parts = rest.split_whitespace();
            if let Some(name) = parts.next() {
                phase = name.to_string();
            }
            if let Some(pct) = parts.next() {
                if let Ok(p) = pct.trim_end_matches('%').parse::<f64>() {
                    percent = p;
                }
            }
        }
    }
    (phase, percent)
}

/// Append one line to the daemon's command file. The daemon atomically claims the
/// file (rename) before reading, so a concurrent append lands in a fresh file and
/// is never lost. Best-effort: a missing /run/aeon just means the daemon is down.
fn push_cmd(line: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(CMD_FILE)?;
    writeln!(f, "{line}")
}

// ── handlers ──────────────────────────────────────────────────────────────────

/// GET /api/braincraft/status — the daemon's live state (relayed from
/// /run/aeon/braincraft.json) merged with the config flag + the voice-install
/// status. `{"present":false}` when the daemon is idle / no HAT. Mirrors vision.rs.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let daemon = std::fs::read_to_string(STATUS_JSON)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .unwrap_or_else(|| json!({ "present": false }));
        let voice = script_json(
            &["status"],
            json!({
                "installed": false, "sherpa": false, "kokoro": false, "whisper": false,
                "display_lib": false, "audio": false,
                "voice_model": Value::Null, "asr_model": Value::Null,
            }),
        );
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "config": cfg,
            "daemon": daemon,
            "voice": voice,
            "voice_installing": task_running("voice-install"),
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// GET /api/braincraft/config — current config.
pub async fn get_config(State(_s): State<AppState>) -> Json<Value> {
    let cfg = tokio::task::spawn_blocking(read_config).await.unwrap_or_default();
    Json(json!({ "ok": true, "config": cfg }))
}

/// A partial update — every field optional; only the present ones are merged.
#[derive(Debug, Deserialize)]
pub struct ConfigPatch {
    pub enabled: Option<bool>,
    pub mode: Option<String>,
    pub overlay: Option<bool>,
    pub audio_device: Option<String>,
    pub tts: Option<String>,
    pub asr: Option<String>,
    pub backend_mode: Option<String>,
    pub persona: Option<String>,
    pub llm_url: Option<String>,
    pub tts_url: Option<String>,
    pub asr_url: Option<String>,
}

/// PUT /api/braincraft/config — merge a partial update into braincraft.toml. The
/// daemon re-reads the file every loop, so the change is live within a tick (no
/// restart). Validates the enum-ish fields. Same atomic write as webcam.rs.
pub async fn put_config(State(_s): State<AppState>, Json(p): Json<ConfigPatch>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        // Validate the constrained fields up front.
        if let Some(m) = &p.mode {
            if !["ai", "viewfinder", "voice"].contains(&m.as_str()) {
                return json!({"ok": false, "err": format!("invalid mode: {m}")});
            }
        }
        if let Some(m) = &p.backend_mode {
            if !["local", "hosted"].contains(&m.as_str()) {
                return json!({"ok": false, "err": format!("invalid backend mode: {m}")});
            }
        }
        let mut cfg = read_config();
        if let Some(v) = p.enabled { cfg.enabled = v; }
        if let Some(v) = p.mode { cfg.mode = v; }
        if let Some(v) = p.overlay { cfg.overlay = v; }
        if let Some(v) = p.audio_device { cfg.audio.device = v; }
        if let Some(v) = p.tts { cfg.audio.tts = v; }
        if let Some(v) = p.asr { cfg.audio.asr = v; }
        if let Some(v) = p.backend_mode { cfg.backend.mode = v; }
        if let Some(v) = p.persona { cfg.backend.persona = v; }
        if let Some(v) = p.llm_url { cfg.hosted.llm_url = v; }
        if let Some(v) = p.tts_url { cfg.hosted.tts_url = v; }
        if let Some(v) = p.asr_url { cfg.hosted.asr_url = v; }
        match write_config(&cfg) {
            Ok(()) => json!({"ok": true, "config": cfg}),
            Err(e) => json!({"ok": false, "err": format!("write {CONFIG_TOML}: {e}")}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "config task failed"}));
    Json(v)
}

/// POST /api/braincraft/capture — take a still in viewfinder mode (pushes `photo`
/// to the daemon command channel). The daemon saves the full-res frame.
pub async fn post_capture(State(_s): State<AppState>) -> Json<Value> {
    match push_cmd("photo") {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "err": format!("{e}")})),
    }
}

#[derive(Debug, Deserialize)]
pub struct RecordBody {
    /// "start" | "stop".
    pub action: String,
}

/// POST /api/braincraft/record — start/stop a viewfinder video recording.
pub async fn post_record(State(_s): State<AppState>, Json(b): Json<RecordBody>) -> Json<Value> {
    let line = match b.action.as_str() {
        "start" => "record:start",
        "stop" => "record:stop",
        other => return Json(json!({"ok": false, "err": format!("invalid action: {other}")})),
    };
    match push_cmd(line) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "err": format!("{e}")})),
    }
}

#[derive(Debug, Deserialize)]
pub struct SayBody {
    pub text: String,
}

/// POST /api/braincraft/say — speak text through the active TTS path (local Piper
/// or the hosted persona voice). Pushes `say:<text>` to the daemon.
pub async fn post_say(State(_s): State<AppState>, Json(b): Json<SayBody>) -> Json<Value> {
    let text = b.text.replace('\n', " ");
    if text.trim().is_empty() {
        return Json(json!({"ok": false, "err": "empty text"}));
    }
    match push_cmd(&format!("say:{text}")) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "err": format!("{e}")})),
    }
}

/// POST /api/braincraft/voice/install — install the Piper/Vosk + display/audio
/// stack on demand. Non-blocking: runs `aeon-voice install` on the blocking pool
/// while streaming `PHASE` progress from the install log into TASKS. Returns
/// {ok,started}; the dashboard polls /voice/install/status. Mirrors hailo install.
pub async fn voice_install(State(_s): State<AppState>) -> Json<Value> {
    if task_running("voice-install") {
        return Json(json!({"ok": false, "started": false, "err": "install already in progress"}));
    }
    // Already fully installed? Don't redo it.
    let installed = tokio::task::spawn_blocking(|| {
        script_json(&["status"], json!({}))
            .get("installed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if installed {
        return Json(json!({"ok": false, "started": false, "err": "voice stack already installed"}));
    }

    set_task("voice-install", TaskState { phase: "starting".into(), percent: 0.0, done: false, ok: false, log: String::new() });
    tokio::task::spawn_blocking(|| {
        let res = run_script(&["install"]);
        let (phase, percent) = parse_progress(VOICE_INSTALL_LOG);
        let log = log_tail(VOICE_INSTALL_LOG, 20);
        match res {
            Ok(_) => set_task("voice-install", TaskState {
                phase: "done".into(), percent: 100.0, done: true, ok: true,
                log: if log.is_empty() { "voice stack installed".into() } else { log },
            }),
            Err(e) => set_task("voice-install", TaskState {
                phase: if phase == "starting" { "failed".into() } else { phase },
                percent, done: true, ok: false,
                log: if log.is_empty() { e } else { format!("{log}\n{e}") },
            }),
        }
    });
    Json(json!({"ok": true, "started": true}))
}

/// GET /api/braincraft/voice/install/status — progress for the in-flight (or last)
/// voice install. Mirrors hailo's get_install_status.
pub async fn voice_install_status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        match get_task("voice-install") {
            Some(mut t) => {
                if !t.done {
                    let (phase, percent) = parse_progress(VOICE_INSTALL_LOG);
                    t.phase = phase;
                    t.percent = percent;
                    t.log = log_tail(VOICE_INSTALL_LOG, 12);
                }
                json!({"phase": t.phase, "percent": t.percent, "done": t.done, "ok": t.ok, "log": t.log})
            }
            None => json!({"phase": "idle", "percent": 0, "done": true, "ok": false, "log": ""}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"phase": "failed", "percent": 0, "done": true, "ok": false, "log": "voice install status task failed"}));
    Json(v)
}
