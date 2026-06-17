//! System-level controls — reboot + power-off.
//!
//! Both endpoints fire a systemd shutdown via the system D-Bus (the
//! supervisor already runs as root for port 80 / 443 + configfs USB
//! gadget access, so we can just shell out to `systemctl`). The audit
//! log captures the actor; the response returns immediately while
//! systemd handles the rest.

use crate::api::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

/// POST /api/system/reboot — reboot the Pi.
pub async fn reboot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<Value> {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    crate::audit::log(&actor, "system_reboot", "scheduled", "ok", None);

    // Fire-and-forget. systemd-shutdown takes ~10 s to bring services
    // down + reboot; we want the HTTP response to return immediately so
    // the browser sees a clean "ok" before the connection drops.
    //
    // `+1` minutes is the fastest way to schedule a reboot via the
    // userspace wrapper that doesn't immediately kill the supervisor
    // mid-response. Actually no, `systemctl reboot` returns immediately
    // and the shutdown happens asynchronously. Use that directly.
    tokio::task::spawn_blocking(|| {
        // 3-second grace so the response definitely lands.
        std::thread::sleep(std::time::Duration::from_secs(3));
        let _ = Command::new("systemctl").arg("reboot").status();
    });

    Json(json!({
        "ok": true,
        "action": "reboot",
        "in_seconds": 3,
        "message": "Pi will reboot in 3 seconds. You'll need to wait ~30-60 s before reconnecting.",
    }))
}

/// POST /api/system/poweroff — shut down the Pi.
pub async fn poweroff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Json<Value> {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    crate::audit::log(&actor, "system_poweroff", "scheduled", "ok", None);

    tokio::task::spawn_blocking(|| {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let _ = Command::new("systemctl").arg("poweroff").status();
    });

    Json(json!({
        "ok": true,
        "action": "poweroff",
        "in_seconds": 3,
        "message": "Pi will power off in 3 seconds. Wake by power-cycling.",
    }))
}

/// Point-in-time system health — uptime, load, temp, free memory, cpu count.
/// Shared by GET /api/system/info and the fleet heartbeat (`fleet.rs`), so the
/// two never drift. Returns a JSON object WITHOUT an `ok` field (callers add it).
pub fn snapshot() -> Value {
    let uptime_s: u64 = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0);

    // /proc/loadavg → "0.32 0.41 0.39 1/220 12345"
    let loadavg: (f32, f32, f32) = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| {
            let mut it = s.split_whitespace();
            Some((
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
                it.next()?.parse().ok()?,
            ))
        })
        .unwrap_or((0.0, 0.0, 0.0));

    // /sys/class/thermal/thermal_zone0/temp → millidegrees Celsius
    let temp_c: f32 = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|m| m as f32 / 1000.0)
        .unwrap_or(0.0);

    // /proc/meminfo → "MemTotal: 4032512 kB" etc.
    let mut mem_total_kb: u64 = 0;
    let mut mem_avail_kb: u64 = 0;
    if let Ok(s) = std::fs::read_to_string("/proc/meminfo") {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("MemTotal:") {
                mem_total_kb = v.split_whitespace().next()
                    .and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                mem_avail_kb = v.split_whitespace().next()
                    .and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
    }

    // CPU model + count (best-effort).
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);

    json!({
        "uptime_seconds": uptime_s,
        "loadavg": { "1m": loadavg.0, "5m": loadavg.1, "15m": loadavg.2 },
        "cpu_temp_c": temp_c,
        "cpu_count": cpu_count,
        "mem_total_kb": mem_total_kb,
        "mem_available_kb": mem_avail_kb,
    })
}

/// GET /api/system/info — uptime, load, temp, free memory. Useful for
/// the UI to show "ok to reboot" indicators (e.g. don't reboot during
/// heavy load) AND as a one-stop health endpoint for agents.
pub async fn info(State(_state): State<AppState>) -> Json<Value> {
    let mut v = snapshot();
    if let Some(o) = v.as_object_mut() {
        o.insert("ok".to_string(), json!(true));
    }
    Json(v)
}

// ── Configuration backup / restore ─────────────────────────────────────
//
// A password-encrypted snapshot of everything you'd want back after a
// reflash: all of /etc/aeon (configs, network/VPN, auth + API tokens, TLS
// cert, macros, prompts, vpn-secrets), the NetworkManager saved-WiFi profiles
// (system-connections), the admin user's SSH authorized_keys (~/.ssh), the
// agent-connect SSH keypair + connected-systems registry, agent tokens, DNS
// subscriptions, the Tailscale node identity, AND the OrbNet service IDENTITIES
// — the registered Mysterium node keystore + MMN key, the OrbNet .onion secret +
// Matrix (Conduit) homeserver DB + owner/persona creds, user hidden-service
// keys, and the IPFS PeerID config. WITHOUT those last items a reflash mints
// brand-new identities (the bake scrubs + re-keys on first boot), PERMANENTLY
// losing the registered node, every .onion address, and the PeerID — so they
// are included here. Large, re-fetchable blobs (the IPFS blockstore, Tor
// consensus caches, the Mysterium chain cache, ISOs, staged files, the audit
// log) are excluded. Encryption is openssl AES-256-CBC with a PBKDF2-derived
// key; the password is passed via env (never argv/ps), and the same password
// decrypts on import. Admin-scope only. (See `aeon-orbnet-backup` for an
// OrbNet-scoped CLI with a perms-correct, service-bouncing restore.)

/// Paths (relative to `/`) included in a config backup. Each is skipped if
/// absent so a fresh device still produces a valid (smaller) archive.
const BACKUP_LIST: &str = "etc/aeon etc/NetworkManager/system-connections home/admin/.ssh var/lib/aeon/agent-connect var/lib/aeon/agent-tokens var/lib/aeon/dns-sources var/lib/tailscale/tailscaled.state var/lib/mysterium-node/keystore var/lib/mysterium-node/nodeui-pass etc/mysterium-node var/lib/aeon/orbnet/tor/hs var/lib/aeon/orbnet/tls var/lib/aeon/orbnet/db var/lib/aeon/orbnet/owner.json var/lib/aeon/orbnet/personas.json var/lib/aeon/orbnet/persona-since var/lib/aeon/orbnet/reg-token var/lib/aeon/orbnet/conduit.toml var/lib/aeon/onions/hs var/lib/aeon/onions/services.d var/lib/aeon/ipfs/config var/lib/aeon/ipfs/keystore var/lib/aeon/ipfs/datastore_spec";

#[derive(Deserialize)]
pub struct ExportReq {
    #[serde(default)]
    pub password: String,
}

/// POST /api/system/config/export — returns a password-encrypted backup as a
/// binary download. Body: `{ "password": "…" }`.
pub async fn config_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ExportReq>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    if req.password.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, HeaderMap::new(), b"password required".to_vec());
    }
    let pw = req.password;
    let script = format!(
        "set -eo pipefail; cd /; P=\"\"; for p in {BACKUP_LIST}; do [ -e \"/$p\" ] && P=\"$P $p\"; done; \
         [ -n \"$P\" ] || {{ echo 'nothing to back up' >&2; exit 5; }}; \
         tar czf - $P | openssl enc -aes-256-cbc -salt -pbkdf2 -iter 200000 -pass env:AEON_BK_PW"
    );
    let out = tokio::task::spawn_blocking(move || {
        Command::new("bash").arg("-c").arg(script).env("AEON_BK_PW", pw).output()
    })
    .await;
    let out = match out {
        Ok(Ok(o)) => o,
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), b"export process failed".to_vec()),
    };
    if !out.status.success() {
        crate::audit::log(&actor, "config_export", "encrypt-failed", "fail", None);
        let msg = String::from_utf8_lossy(&out.stderr);
        return (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), format!("export failed: {msg}").into_bytes());
    }
    crate::audit::log(&actor, "config_export", &format!("{} bytes", out.stdout.len()), "ok", None);
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"aeon-config-backup.aeonbackup\""),
    );
    (StatusCode::OK, h, out.stdout)
}

#[derive(Deserialize)]
pub struct ImportReq {
    #[serde(default)]
    pub password: String,
    /// base64 of the encrypted .aeonbackup file.
    #[serde(default)]
    pub data_b64: String,
}

/// POST /api/system/config/import — decrypt a backup with the password and
/// restore it in place. Returns ok + a reboot recommendation.
pub async fn config_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ImportReq>,
) -> Json<Value> {
    let actor = crate::auth::identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    if req.password.trim().is_empty() || req.data_b64.trim().is_empty() {
        return Json(json!({"ok": false, "err": "password and backup file required"}));
    }
    let enc = match base64::engine::general_purpose::STANDARD.decode(req.data_b64.trim()) {
        Ok(b) => b,
        Err(_) => return Json(json!({"ok": false, "err": "invalid backup encoding"})),
    };
    let pw = req.password;
    let script = format!(
        "set -eo pipefail; TMP=$(mktemp -d); trap 'rm -rf \"$TMP\"' EXIT; \
         openssl enc -d -aes-256-cbc -pbkdf2 -iter 200000 -pass env:AEON_BK_PW -in \"$AEON_BK_IN\" | tar xzf - -C \"$TMP\"; \
         [ -d \"$TMP/etc/aeon\" ] || {{ echo NOT_A_BACKUP >&2; exit 4; }}; \
         for sub in {BACKUP_LIST}; do if [ -e \"$TMP/$sub\" ]; then mkdir -p \"/$(dirname \"$sub\")\"; cp -aT \"$TMP/$sub\" \"/$sub\"; fi; done; \
         echo RESTORED"
    );
    let out = tokio::task::spawn_blocking(move || -> std::io::Result<std::process::Output> {
        let tmp_in = format!("/var/lib/aeon/.import-{}.bin", std::process::id());
        std::fs::write(&tmp_in, &enc)?;
        let res = Command::new("bash")
            .arg("-c")
            .arg(&script)
            .env("AEON_BK_PW", pw)
            .env("AEON_BK_IN", &tmp_in)
            .output();
        let _ = std::fs::remove_file(&tmp_in);
        res
    })
    .await;
    let out = match out {
        Ok(Ok(o)) => o,
        _ => return Json(json!({"ok": false, "err": "restore process failed"})),
    };
    if out.status.success() {
        crate::audit::log(&actor, "config_import", "restored", "ok", None);
        Json(json!({
            "ok": true,
            "restored": true,
            "message": "Configuration restored. Reboot the Pi to apply — services and the network stack reload their config on boot.",
        }))
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let code = out.status.code().unwrap_or(-1);
        crate::audit::log(&actor, "config_import", "failed", "fail", None);
        let err = if code == 4 {
            "That file isn't a valid AEON backup."
        } else if stderr.contains("bad decrypt") || stderr.contains("bad magic") || stderr.to_lowercase().contains("error") {
            "Wrong password, or the backup is corrupt."
        } else {
            "Restore failed."
        };
        Json(json!({"ok": false, "err": err, "detail": stderr.chars().take(200).collect::<String>()}))
    }
}
