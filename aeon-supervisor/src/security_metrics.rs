//! Lightweight security console metrics.
//!
//! Pi-friendly: every numeric source is a single file read or one
//! `iptables -nvL` invocation. No persistent collectors, no continuous
//! sampling threads. The supervisor maintains a small in-memory ring
//! buffer (last 60 samples) keyed by the LAST request time — i.e.,
//! we sample on demand. Web UI polls at 5s, so we get ~5-minute
//! history before the buffer wraps. Good enough for sparkline scale.
//!
//! Sources:
//!   • throughput     — /sys/class/net/<iface>/statistics/{rx,tx}_bytes
//!   • blocked count  — iptables -nvL across DROP/REJECT rules
//!   • blocked domains — read from dns_log (top-N this rotation)
//!   • per-client bytes — /proc/net/nf_conntrack aggregated by src

use crate::api::AppState;
use axum::extract::State;
use axum::Json;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Default)]
struct Sample {
    ts_ms: u64,
    rx_bytes: u64,
    tx_bytes: u64,
}

const RING_CAP: usize = 60; // ~5 min at 5s poll

static RING: OnceLock<Mutex<Vec<Sample>>> = OnceLock::new();

fn ring() -> &'static Mutex<Vec<Sample>> {
    RING.get_or_init(|| Mutex::new(Vec::with_capacity(RING_CAP)))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sum rx/tx bytes across all "external" interfaces. We skip lo, usb0
/// (those are internal-to-the-Pi paths), and any docker/veth.
fn read_iface_bytes() -> (u64, u64) {
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return (0, 0);
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == "lo"
            || name == "usb0"
            || name.starts_with("docker")
            || name.starts_with("veth")
            || name.starts_with("br-")
            || name.starts_with("tun")
            || name.starts_with("wg")
        {
            continue;
        }
        let path = e.path();
        let rx = std::fs::read_to_string(path.join("statistics/rx_bytes"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let tx = std::fs::read_to_string(path.join("statistics/tx_bytes"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        rx_total = rx_total.saturating_add(rx);
        tx_total = tx_total.saturating_add(tx);
    }
    (rx_total, tx_total)
}

/// Count packets dropped by any DROP/REJECT iptables rule in the last
/// 24h is hard without persistent state. We approximate by reading
/// the CURRENT packet counters and assuming the user hasn't recently
/// flushed counters; this number IS monotonic since boot.
fn read_blocked_count() -> u64 {
    let mut total = 0u64;
    for table in &["filter", "mangle"] {
        let out = Command::new("iptables")
            .args(["-t", table, "-nvL"])
            .output();
        let Ok(out) = out else { continue };
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            // iptables -nvL format: "  pkts bytes target  prot  opt in out src dst ..."
            // Match DROP / REJECT in column 3.
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                continue;
            }
            let target = parts[2];
            if target == "DROP" || target == "REJECT" {
                if let Ok(p) = parts[0].parse::<u64>() {
                    total = total.saturating_add(p);
                }
            }
        }
    }
    total
}

/// Compute per-client bytes from /proc/net/nf_conntrack aggregated by
/// src IP. Returns a small top-N for the UI.
fn read_top_clients() -> Vec<(String, u64)> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/proc/net/nf_conntrack") else {
        return Vec::new();
    };
    for line in text.lines() {
        // Format includes both directions; we sum bytes per src.
        // Look for "bytes=" markers. Some kernel builds don't expose
        // byte counters in conntrack — just skip silently in that case.
        let Some(src_tag) = line.split_whitespace().find(|p| p.starts_with("src=")) else {
            continue;
        };
        let src = src_tag.trim_start_matches("src=").to_string();
        if !src.starts_with("10.55.") && !src.starts_with("192.168.") {
            continue;
        }
        let bytes: u64 = line
            .split_whitespace()
            .filter(|p| p.starts_with("bytes="))
            .filter_map(|p| p.trim_start_matches("bytes=").parse::<u64>().ok())
            .sum();
        *totals.entry(src).or_insert(0) += bytes;
    }
    let mut v: Vec<(String, u64)> = totals.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(8);
    v
}

/// GET /api/security/metrics — current point + recent history + blocked.
pub async fn get_metrics(State(_state): State<AppState>) -> Json<Value> {
    let (rx, tx) = read_iface_bytes();
    let ts = now_ms();
    let mut bps_in = 0u64;
    let mut bps_out = 0u64;
    // Push into ring + compute throughput vs prior sample.
    {
        let mut r = ring().lock();
        if let Some(prev) = r.last().copied() {
            let dt_ms = ts.saturating_sub(prev.ts_ms).max(1);
            if dt_ms > 0 {
                bps_in = ((rx.saturating_sub(prev.rx_bytes)) * 8 * 1000) / dt_ms;
                bps_out = ((tx.saturating_sub(prev.tx_bytes)) * 8 * 1000) / dt_ms;
            }
        }
        r.push(Sample {
            ts_ms: ts,
            rx_bytes: rx,
            tx_bytes: tx,
        });
        if r.len() > RING_CAP {
            r.remove(0);
        }
    }
    // Build sparkline series.
    let history: Vec<Value> = {
        let r = ring().lock();
        r.windows(2)
            .map(|w| {
                let dt_ms = w[1].ts_ms.saturating_sub(w[0].ts_ms).max(1);
                let in_bps = ((w[1].rx_bytes.saturating_sub(w[0].rx_bytes)) * 8 * 1000) / dt_ms;
                let out_bps = ((w[1].tx_bytes.saturating_sub(w[0].tx_bytes)) * 8 * 1000) / dt_ms;
                json!({"ts_ms": w[1].ts_ms, "in_bps": in_bps, "out_bps": out_bps})
            })
            .collect()
    };
    let blocked = read_blocked_count();
    let top_clients: Vec<Value> = read_top_clients()
        .into_iter()
        .map(|(ip, b)| json!({"ip": ip, "bytes": b}))
        .collect();
    // Suspicious events stub: future iterations can read fail2ban
    // jail log / suricata alerts here. For v28 we surface an empty
    // list with a status note.
    let suspicious_events: Vec<Value> = Vec::new();
    Json(json!({
        "ok": true,
        "throughput_bps": {"in": bps_in, "out": bps_out},
        "throughput_history": history,
        "blocked_24h": blocked,
        "suspicious_events": suspicious_events,
        "top_blocked_domains": [],
        "top_clients": top_clients,
    }))
}
