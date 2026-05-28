//! System-level controls — reboot + power-off.
//!
//! Both endpoints fire a systemd shutdown via the system D-Bus (the
//! supervisor already runs as root for port 80 / 443 + configfs USB
//! gadget access, so we can just shell out to `systemctl`). The audit
//! log captures the actor; the response returns immediately while
//! systemd handles the rest.

use crate::api::AppState;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
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

/// GET /api/system/info — uptime, load, temp, free memory. Useful for
/// the UI to show "ok to reboot" indicators (e.g. don't reboot during
/// heavy load) AND as a one-stop health endpoint for agents.
pub async fn info(State(_state): State<AppState>) -> Json<Value> {
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

    Json(json!({
        "ok": true,
        "uptime_seconds": uptime_s,
        "loadavg": { "1m": loadavg.0, "5m": loadavg.1, "15m": loadavg.2 },
        "cpu_temp_c": temp_c,
        "cpu_count": cpu_count,
        "mem_total_kb": mem_total_kb,
        "mem_available_kb": mem_avail_kb,
    }))
}
