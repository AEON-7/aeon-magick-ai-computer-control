//! OS package updates — keep the underlying Raspberry Pi OS patched.
//!
//! Two capabilities, both driven through the `/usr/local/bin/aeon-update` infra
//! script (the supervisor runs as root, so it invokes apt directly):
//!   1. On-demand `apt upgrade` — backgrounded, with pollable PHASE progress,
//!      mirroring `hailo.rs`'s in-process `TaskState`/`TASKS` + PHASE-log pattern.
//!   2. An automatic-security-updates toggle (unattended-upgrades).
//!
//! This is the OS layer only. The image/app version notifier lives in
//! `image_update.rs`; the Aeon-Bench-Pod updater lives in `bench.rs`.

use crate::api::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

const SCRIPT: &str = "/usr/local/bin/aeon-update";
const APT_LOG: &str = "/run/aeon/apt-update.log";

#[derive(Debug, Clone, Default, Serialize)]
struct TaskState {
    phase: String,
    percent: f64,
    done: bool,
    ok: bool,
    log: String,
    reboot_required: bool,
}

static TASKS: OnceLock<Mutex<HashMap<String, TaskState>>> = OnceLock::new();
fn tasks() -> &'static Mutex<HashMap<String, TaskState>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn set_task(k: &str, s: TaskState) {
    if let Ok(mut m) = tasks().lock() {
        m.insert(k.to_string(), s);
    }
}
fn get_task(k: &str) -> Option<TaskState> {
    tasks().lock().ok().and_then(|m| m.get(k).cloned())
}
/// Atomically claim the task slot: returns true iff it was free (and marks it
/// `starting` in the same lock hold). Closes the check-then-set TOCTOU where two
/// concurrent apply() calls (web + MCP, or two tabs) both pass a separate
/// `task_running` check and launch racing apt runs.
fn try_begin(key: &str) -> bool {
    match tasks().lock() {
        Ok(mut m) => {
            if m.get(key).map(|t| !t.done).unwrap_or(false) {
                return false;
            }
            m.insert(key.to_string(), TaskState { phase: "starting".into(), ..Default::default() });
            true
        }
        Err(_) => false,
    }
}

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

/// Parse JSON stdout from the script, or wrap a plain string as an error.
fn script_json(args: &[&str]) -> Value {
    match run_script(args) {
        Ok(s) => serde_json::from_str::<Value>(&s)
            .unwrap_or_else(|_| json!({"ok": false, "err": "unparseable script output", "raw": s})),
        Err(e) => json!({"ok": false, "err": e}),
    }
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

/// Most recent `PHASE <name> <pct>` line → (phase, percent). Same convention as
/// the aeon-hailo/aeon-update infra scripts.
fn parse_progress(path: &str) -> (String, f64) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut phase = "starting".to_string();
    let mut percent = 0.0_f64;
    for line in text.lines() {
        let rest = line.trim().strip_prefix("PHASE ").or_else(|| line.trim().strip_prefix("PHASE\t"));
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

// ── handlers ────────────────────────────────────────────────────────────────

/// GET /api/system/update/check — refresh apt and count upgradable packages.
/// Returns {ok, count, security, packages}. Read-scope over MCP.
pub async fn check(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| script_json(&["check"]))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "check task failed"}));
    Json(v)
}

/// POST /api/system/update — apply all pending apt upgrades. Backgrounded and
/// guarded (single-flight); returns {ok, started} immediately, poll /update/status.
pub async fn apply(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    // Atomic single-flight claim (see try_begin) — no check-then-set gap.
    if !try_begin("apt") {
        return Json(json!({"ok": false, "started": false, "err": "an update is already in progress"}));
    }
    crate::audit::log(&actor, "os_update", "apply", "ok", None);
    // Clear any prior run's log BEFORE spawning, so if the script fails to spawn
    // (missing/non-exec) the status can't read a stale "PHASE done 100" and report
    // a phantom success. The script re-truncates it once it actually runs.
    let _ = std::fs::create_dir_all("/run/aeon");
    let _ = std::fs::write(APT_LOG, "");
    tokio::task::spawn_blocking(|| {
        let res = run_script(&["upgrade"]);
        let (phase, percent) = parse_progress(APT_LOG);
        let log = log_tail(APT_LOG, 24);
        let reboot = log.contains("REBOOT_RECOMMENDED");
        match res {
            Ok(_) => set_task("apt", TaskState {
                phase: "done".into(), percent: 100.0, done: true, ok: true, log, reboot_required: reboot,
            }),
            Err(e) => set_task("apt", TaskState {
                phase: if phase == "starting" { "failed".into() } else { phase },
                percent, done: true, ok: false,
                log: if log.is_empty() { e } else { format!("{log}\n{e}") },
                reboot_required: reboot,
            }),
        }
    });
    Json(json!({"ok": true, "started": true}))
}

/// GET /api/system/update/status — poll the in-flight (or last) apt upgrade.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| match get_task("apt") {
        Some(mut t) => {
            if !t.done {
                let (phase, percent) = parse_progress(APT_LOG);
                t.phase = phase;
                t.percent = percent;
                t.log = log_tail(APT_LOG, 16);
            }
            json!({
                "phase": t.phase, "percent": t.percent, "done": t.done,
                "ok": t.ok, "log": t.log, "reboot_required": t.reboot_required,
            })
        }
        None => json!({"phase": "idle", "percent": 0, "done": true, "ok": false, "log": "", "reboot_required": false}),
    })
    .await
    .unwrap_or_else(|_| json!({"phase": "failed", "percent": 0, "done": true, "ok": false, "log": "status task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct AutoReq {
    #[serde(default)]
    pub enable: bool,
}

/// GET /api/system/auto-updates — is automatic security patching on?
pub async fn auto_status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| script_json(&["auto-status"]))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "auto-status task failed", "enabled": false}));
    Json(v)
}

/// POST /api/system/auto-updates {enable} — turn automatic security patching on/off
/// (installs unattended-upgrades on first enable; security-only, no auto-reboot).
pub async fn auto_set(State(state): State<AppState>, headers: HeaderMap, Json(req): Json<AutoReq>) -> Json<Value> {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    let enable = req.enable;
    let arg = if enable { "auto-enable" } else { "auto-disable" };
    crate::audit::log(&actor, "os_auto_updates", arg, "ok", None);
    let v = tokio::task::spawn_blocking(move || script_json(&[arg]))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "auto-updates task failed"}));
    Json(v)
}
