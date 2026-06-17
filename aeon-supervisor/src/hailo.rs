//! Hailo — on-device AI accelerator (Hailo-10H AI HAT+) control plane.
//!
//! The Pi's Hailo-10H runs LLMs / VLMs / STT / vision models on its NNC instead
//! of the CPU. All the device-specific unknowns (driver install, model zoo,
//! hailortcli / hailo-ollama plumbing) live in ONE infra script,
//! `/usr/local/bin/aeon-hailo`; this module is the Rust control plane that owns
//! `/etc/aeon/hailo.toml`, drives that script, tracks non-blocking install /
//! deploy progress in a process-global TASKS map, and merges the script's live
//! state with a curated model library for the dashboard.
//!
//! Mirrors `ipfs.rs` (config TOML + run_script + install-on-demand) and
//! `orbnet.rs` (non-blocking spawn_blocking bring-up + boot reconcile +
//! process-global state). OFF by default (`enabled = false`). Admin-gated over
//! REST exactly like `/api/orbnet/*` (see auth.rs) — no allow-list bypass.

use axum::extract::{Path as AxPath, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/hailo.toml";
const SCRIPT: &str = "/usr/local/bin/aeon-hailo";
const LIBRARY_JSON: &str = "/usr/share/aeon/hailo/library.json";
const INSTALL_LOG: &str = "/run/aeon/hailo-install.log";
/// Per-model deploy progress log; `<id>` substituted at runtime.
const DEPLOY_LOG_FMT: &str = "/run/aeon/hailo-deploy-{id}.log";
/// Usable on-device NNC budget (the ledger's mem_total is informational; this is
/// the practical ceiling for `fits`).
const USABLE_BUDGET_MB: i64 = 5500;

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HailoConfig {
    /// Off by default. When on, the device is installed/usable and an optional
    /// model is auto-loaded on boot.
    #[serde(default)]
    pub enabled: bool,
    /// Model id to auto-deploy on boot (reconcile_on_boot), if installed.
    #[serde(default)]
    pub autoload: Option<String>,
}

impl Default for HailoConfig {
    fn default() -> Self {
        Self { enabled: false, autoload: None }
    }
}

fn read_config() -> HailoConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &HailoConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

// ── process-global non-blocking task state ────────────────────────────────────

/// Live progress for a long-running install or per-model deploy. Mirrors the
/// `DeployStatus` shape the web client already polls (phase/percent/done/ok/log).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TaskState {
    pub phase: String,
    pub percent: f64,
    pub done: bool,
    pub ok: bool,
    /// Short tail of the progress log.
    pub log: String,
}

/// Keyed by "install" or "model:<id>". A process-global, like orbnet's owner
/// state — survives across requests so the dashboard can poll progress.
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

/// True if a task is currently running (exists and not done).
fn task_running(key: &str) -> bool {
    get_task(key).map(|t| !t.done).unwrap_or(false)
}

// ── infra script ──────────────────────────────────────────────────────────────

/// Run the `aeon-hailo` infra script and return trimmed stdout.
fn run_script(args: &[&str]) -> Result<String, String> {
    let out = Command::new(SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{SCRIPT} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `aeon-hailo <cmd>` → parsed JSON, or a canned honest fallback so the
/// device-free web scaffold renders cleanly when the script/device is absent.
fn script_json(args: &[&str], fallback: Value) -> Value {
    run_script(args)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(fallback)
}

/// The curated model library (`/usr/share/aeon/hailo/library.json`) — the set of
/// models we offer for deploy, independent of what's installed. Empty if missing.
fn read_library() -> Vec<Value> {
    std::fs::read_to_string(LIBRARY_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            // Accept either a bare array or {"models":[...]}.
            v.as_array().cloned().or_else(|| {
                v.get("models").and_then(|m| m.as_array()).cloned()
            })
        })
        .unwrap_or_default()
}

/// Tail the last N lines of a progress log (PHASE <name> <pct> lines).
fn log_tail(path: &str, lines: usize) -> String {
    std::fs::read_to_string(path)
        .map(|s| {
            let all: Vec<&str> = s.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        })
        .unwrap_or_default()
}

/// Parse the most recent `PHASE <name> <pct>` line from a progress log into
/// (phase, percent). Defaults to ("starting", 0.0) when nothing parses yet.
fn parse_progress(path: &str) -> (String, f64) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut phase = "starting".to_string();
    let mut percent = 0.0_f64;
    for line in text.lines() {
        let l = line.trim();
        // Format: "PHASE <name> <pct>" (pct optional).
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

// ── handlers ──────────────────────────────────────────────────────────────────

/// GET /api/hailo/status — feature flag + device state (merged from the infra
/// script's `status`, the curated library, and any running install task).
/// Admin-gated. Returns valid canned JSON (device_present:false, stats:null,
/// empty loaded) when the device/script is absent.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let st = script_json(
            &["status"],
            json!({
                "device_present": false,
                "hat": "none",
                "installed": false,
                "mem_total_mb": 0,
                "mem_used_mb": 0,
                "mem_free_mb": 0,
                "stats": Value::Null,
                "loaded": [],
            }),
        );
        let get = |k: &str, d: Value| st.get(k).cloned().unwrap_or(d);
        // An install in flight overrides the script's `installed` for display.
        let installing = task_running("install");
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "device_present": get("device_present", json!(false)),
            "hat": get("hat", json!("none")),
            "installed": get("installed", json!(false)),
            "installing": installing,
            "mem_total_mb": get("mem_total_mb", json!(0)),
            "mem_used_mb": get("mem_used_mb", json!(0)),
            "mem_free_mb": get("mem_free_mb", json!(0)),
            "stats": get("stats", Value::Null),
            "loaded": get("loaded", json!([])),
            "consumers": get("consumers", json!([])),
            "autoload": cfg.autoload,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/hailo/install — install the Hailo-10H driver stack + model zoo.
/// Non-blocking: guarded (device present, the 10H HAT, not already installed),
/// then runs `aeon-hailo install` on the blocking pool while streaming progress
/// from /run/aeon/hailo-install.log into TASKS["install"]. Returns {ok,started}
/// immediately; the dashboard polls /install/status. A reboot is required after.
/// Admin-gated.
pub async fn post_install(State(_s): State<AppState>) -> Json<Value> {
    // Guard on current device state before kicking off.
    let guard = tokio::task::spawn_blocking(|| -> Result<(), String> {
        if task_running("install") {
            return Err("install already in progress".into());
        }
        let st = script_json(&["status"], json!({}));
        let device_present = st.get("device_present").and_then(|v| v.as_bool()).unwrap_or(false);
        let hat = st.get("hat").and_then(|v| v.as_str()).unwrap_or("none");
        let installed = st.get("installed").and_then(|v| v.as_bool()).unwrap_or(false);
        if !device_present {
            return Err("no Hailo device present".into());
        }
        if hat != "hailo-10h" {
            return Err(format!("unsupported HAT: {hat} (need hailo-10h)"));
        }
        if installed {
            return Err("already installed".into());
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err("install guard task failed".into()));

    if let Err(e) = guard {
        return Json(json!({"ok": false, "started": false, "err": e}));
    }

    // Mark running, then fire-and-forget the slow install.
    set_task("install", TaskState { phase: "starting".into(), percent: 0.0, done: false, ok: false, log: String::new() });
    tokio::task::spawn_blocking(|| {
        // The script writes 'PHASE <name> <pct>' to INSTALL_LOG as it runs.
        let res = run_script(&["install"]);
        let (phase, percent) = parse_progress(INSTALL_LOG);
        let log = log_tail(INSTALL_LOG, 20);
        match res {
            Ok(_) => {
                // Persist enabled=true so the feature is on after the required
                // reboot and reconcile_on_boot can auto-load `autoload`. Mirrors
                // how ipfs/orbnet `enable` flips the config on success.
                let mut cfg = read_config();
                cfg.enabled = true;
                let _ = write_config(&cfg);
                set_task("install", TaskState {
                    phase: "done".into(), percent: 100.0, done: true, ok: true,
                    log: if log.is_empty() { "install complete — reboot required".into() } else { log },
                });
            }
            Err(e) => set_task("install", TaskState {
                phase: if phase == "starting" { "failed".into() } else { phase },
                percent, done: true, ok: false,
                log: if log.is_empty() { e } else { format!("{log}\n{e}") },
            }),
        }
    });

    Json(json!({"ok": true, "started": true}))
}

/// GET /api/hailo/install/status — DeployStatus for the in-flight (or last)
/// install. Reads TASKS["install"], refreshing phase/percent from the live log
/// while the task is still running. Admin-gated.
pub async fn get_install_status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        match get_task("install") {
            Some(mut t) => {
                if !t.done {
                    // Refresh live progress from the log.
                    let (phase, percent) = parse_progress(INSTALL_LOG);
                    t.phase = phase;
                    t.percent = percent;
                    t.log = log_tail(INSTALL_LOG, 12);
                }
                json!({
                    "phase": t.phase,
                    "percent": t.percent,
                    "done": t.done,
                    "ok": t.ok,
                    "log": t.log,
                })
            }
            None => json!({"phase": "idle", "percent": 0, "done": true, "ok": false, "log": ""}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"phase": "failed", "percent": 0, "done": true, "ok": false, "log": "install status task failed"}));
    Json(v)
}

/// GET /api/hailo/models — the deployable model list: the curated library merged
/// with the script's live `models` (deployed/loaded state), with `fits` computed
/// from the current free-memory ledger. Admin-gated. Degrades to library-only
/// (all `available`) when the device/script is absent.
pub async fn get_models(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        // Free budget from the live ledger; fall back to the usable budget.
        let st = script_json(&["status"], json!({}));
        let mem_free = st.get("mem_free_mb").and_then(|v| v.as_i64()).unwrap_or(USABLE_BUDGET_MB);

        // Live per-model state keyed by id.
        let live = script_json(&["models"], json!({"models": []}));
        let live_arr = live.get("models").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let live_state = |id: &str| -> Option<String> {
            live_arr.iter().find_map(|m| {
                if m.get("id").and_then(|v| v.as_str()) == Some(id) {
                    m.get("state").and_then(|v| v.as_str()).map(String::from)
                } else {
                    None
                }
            })
        };

        // Merge: library is the source of truth for catalog metadata; live state
        // overrides `state`; `fits` from the current free budget.
        let mut out: Vec<Value> = vec![];
        for mut m in read_library() {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let size = m.get("size_mb").and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(state) = live_state(&id) {
                m["state"] = json!(state);
            } else if m.get("state").is_none() {
                m["state"] = json!("available");
            }
            m["fits"] = json!(size <= mem_free);
            // A deploy in flight surfaces as "downloading".
            if task_running(&format!("model:{id}")) {
                m["state"] = json!("downloading");
            }
            out.push(m);
        }
        json!({"ok": true, "models": out})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "models task failed", "models": []}));
    Json(v)
}

/// POST /api/hailo/models/:id/deploy — pull/load a model onto the device.
/// Non-blocking: runs `aeon-hailo deploy <id>` on the blocking pool, tracking
/// progress in TASKS["model:<id>"] from /run/aeon/hailo-deploy-<id>.log. Returns
/// {ok,started}; the dashboard polls /models (or computes from TASKS). Admin-gated.
pub async fn post_deploy(State(_s): State<AppState>, AxPath(id): AxPath<String>) -> Json<Value> {
    let key = format!("model:{id}");
    if task_running(&key) {
        return Json(json!({"ok": false, "started": false, "err": "deploy already in progress"}));
    }
    set_task(&key, TaskState { phase: "starting".into(), percent: 0.0, done: false, ok: false, log: String::new() });
    let id_for_task = id.clone();
    tokio::task::spawn_blocking(move || {
        let log_path = DEPLOY_LOG_FMT.replace("{id}", &id_for_task);
        let key = format!("model:{id_for_task}");
        let res = run_script(&["deploy", &id_for_task]);
        let (phase, percent) = parse_progress(&log_path);
        let log = log_tail(&log_path, 20);
        match res {
            Ok(_) => set_task(&key, TaskState {
                phase: "loaded".into(), percent: 100.0, done: true, ok: true,
                log: if log.is_empty() { "deployed".into() } else { log },
            }),
            Err(e) => set_task(&key, TaskState {
                phase: if phase == "starting" { "failed".into() } else { phase },
                percent, done: true, ok: false,
                log: if log.is_empty() { e } else { format!("{log}\n{e}") },
            }),
        }
    });
    Json(json!({"ok": true, "started": true}))
}

/// POST /api/hailo/models/:id/unload — evict a loaded model to free NNC RAM.
/// Synchronous (eviction is fast). Admin-gated.
pub async fn post_unload(State(_s): State<AppState>, AxPath(id): AxPath<String>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["unload", &id]) {
            Ok(_) => {
                // Clear any lingering deploy task for this model.
                if let Ok(mut m) = tasks().lock() {
                    m.remove(&format!("model:{id}"));
                }
                json!({"ok": true})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unload task failed"}));
    Json(v)
}

/// On boot, if Hailo is enabled, installed, and an autoload model is configured,
/// deploy it after a short delay (mirrors orbnet::reconcile_on_boot). Idempotent
/// — a no-op when disabled, not installed, or no autoload set. Best-effort.
pub async fn reconcile_on_boot() {
    tokio::time::sleep(std::time::Duration::from_secs(25)).await;
    let cfg = read_config();
    let Some(model) = cfg.autoload.clone() else { return };
    if !cfg.enabled || model.is_empty() {
        return;
    }
    let installed = tokio::task::spawn_blocking(|| {
        script_json(&["status"], json!({}))
            .get("installed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !installed {
        eprintln!("hailo: enabled with autoload={model} but device not installed — skipping boot deploy");
        return;
    }
    eprintln!("hailo: boot reconcile — auto-deploying {model}");
    let key = format!("model:{model}");
    set_task(&key, TaskState { phase: "starting".into(), percent: 0.0, done: false, ok: false, log: String::new() });
    let _ = tokio::task::spawn_blocking(move || {
        let log_path = DEPLOY_LOG_FMT.replace("{id}", &model);
        let key = format!("model:{model}");
        let res = run_script(&["deploy", &model]);
        let log = log_tail(&log_path, 20);
        match res {
            Ok(_) => set_task(&key, TaskState { phase: "loaded".into(), percent: 100.0, done: true, ok: true, log }),
            Err(e) => set_task(&key, TaskState { phase: "failed".into(), percent: 0.0, done: true, ok: false, log: if log.is_empty() { e } else { log } }),
        }
    })
    .await;
}
