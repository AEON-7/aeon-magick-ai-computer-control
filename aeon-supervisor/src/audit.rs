//! Lightweight append-only audit log.
//!
//! Stores one JSON object per line at /var/lib/aeon/audit.jsonl. Cap
//! the file at ~5 MB (≈40k entries); on hit we rotate to audit.jsonl.1
//! (keeping one generation). Reads tail the last N lines.
//!
//! What we log:
//!   • login_ok / login_fail / logout / password_set / password_change
//!   • token_create / token_revoke
//!   • Any non-GET request to /api/network/*, /api/storage/*,
//!     /api/firewall/*, /api/ssh/*, /api/dns/*, /api/hid/persona
//!   • Streamer/HID action proxies are deliberately NOT audited — too
//!     chatty + low value.
//!
//! What we DON'T log:
//!   • Snapshot fetches, frame streams (huge, useless).
//!   • Per-keystroke HID events (we audit persona change, not typing).
//!   • State reads (GET handlers).

use crate::api::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const AUDIT_LOG: &str = "/var/lib/aeon/audit.jsonl";
const MAX_BYTES: u64 = 5_242_880; // 5 MB — generous for major actions only

// Single writer-side mutex so concurrent appends don't interleave lines.
static APPEND_LOCK: Mutex<()> = Mutex::new(());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Public entry point. Pass actor like "admin (session)" / "agent (token, full)" /
/// "anonymous", action like "login_ok", detail like "from 192.168.1.42", and
/// result either "ok" or "fail" (with optional err string).
pub fn log(actor: &str, action: &str, detail: &str, result: &str, err: Option<&str>) {
    let entry = json!({
        "ts_ms": now_ms(),
        "actor": actor,
        "action": action,
        "detail": detail,
        "result": result,
        "err": err.unwrap_or(""),
    });
    let line = entry.to_string();
    if let Err(e) = append_line(&line) {
        tracing::warn!(error = %e, "audit log write failed");
    }
}

fn append_line(line: &str) -> std::io::Result<()> {
    let _g = APPEND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Rotate if needed BEFORE we write — keeps the rotation invariant
    // simple (post-rotate the file is empty and we can write fresh).
    if let Ok(meta) = std::fs::metadata(AUDIT_LOG) {
        if meta.len() > MAX_BYTES {
            let rotated: PathBuf = format!("{AUDIT_LOG}.1").into();
            // best-effort rotate; on failure we still try to write.
            let _ = std::fs::rename(AUDIT_LOG, &rotated);
        }
    }
    // Ensure parent dir exists (the systemd-tmpfiles entry creates
    // /var/lib/aeon but the first boot might not have run it yet).
    if let Some(parent) = PathBuf::from(AUDIT_LOG).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(AUDIT_LOG)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

#[derive(Deserialize)]
pub struct AuditQuery {
    /// Last N entries to return (default 200, max 2000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Filter by exact actor string (e.g. "admin").
    #[serde(default)]
    pub actor: Option<String>,
    /// Filter by action prefix (e.g. "login" matches login_ok + login_fail).
    #[serde(default)]
    pub action: Option<String>,
}

/// GET /api/audit?limit=&actor=&action=
pub async fn list(
    State(_state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(200).min(2000);
    let actor_filter = q.actor.as_deref();
    let action_filter = q.action.as_deref();

    // Read both files (rotated + active), most-recent-first.
    let mut all_lines: Vec<String> = Vec::new();
    for p in [format!("{AUDIT_LOG}.1"), AUDIT_LOG.to_string()] {
        if let Ok(f) = std::fs::File::open(&p) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                all_lines.push(line);
            }
        }
    }
    // Take the last `limit` matching entries; iterate from newest to
    // oldest so the response is sorted DESC by ts.
    let mut entries: Vec<Value> = Vec::with_capacity(limit);
    for raw in all_lines.iter().rev() {
        let Ok(parsed) = serde_json::from_str::<Value>(raw) else { continue };
        if let Some(a) = actor_filter {
            if parsed.get("actor").and_then(|v| v.as_str()).unwrap_or("") != a {
                continue;
            }
        }
        if let Some(prefix) = action_filter {
            let act = parsed.get("action").and_then(|v| v.as_str()).unwrap_or("");
            if !act.starts_with(prefix) {
                continue;
            }
        }
        entries.push(parsed);
        if entries.len() >= limit {
            break;
        }
    }
    // Total counts (cheap) for the summary panel.
    let total = all_lines.len();
    Json(json!({
        "ok": true,
        "entries": entries,
        "total_lines": total,
        "max_bytes": MAX_BYTES,
        "current_bytes": std::fs::metadata(AUDIT_LOG).map(|m| m.len()).unwrap_or(0),
    })).into_response()
}

/// DELETE /api/audit — clear the log. Admin-only at the route level.
pub async fn clear(State(_state): State<AppState>) -> impl IntoResponse {
    let _g = APPEND_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _ = std::fs::remove_file(format!("{AUDIT_LOG}.1"));
    let _ = std::fs::remove_file(AUDIT_LOG);
    log("system", "audit_clear", "", "ok", None);
    Json(json!({"ok": true})).into_response()
}

/// Convenience: format an Identity into the canonical audit `actor` string.
pub fn actor_for(identity: &crate::auth::Identity) -> String {
    use crate::auth::TokenScope;
    let kind = if identity.user.starts_with("token") {
        format!("token, {}", identity.scope.as_str())
    } else {
        match identity.scope {
            TokenScope::Admin => "session".into(),
            other => format!("{}", other.as_str()),
        }
    };
    format!("{} ({})", identity.user, kind)
}
