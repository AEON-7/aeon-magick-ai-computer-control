//! /api/network/i2p/status — runtime introspection of the I2P daemon.
//!
//! Surfaces:
//!   - is the i2pd binary installed?
//!   - is the systemd unit running?
//!   - what IPs is each user-facing service bound to right now? (HTTP
//!     proxy 4444, SOCKS proxy 4447, web console 7070)
//!   - what outproxy is configured (if any)?
//!   - a usb_addr the UI shows to users for "point your browser at..."
//!
//! No mutation here — the user changes outproxy via the existing
//! PUT /api/network/vpn handler (vpn.i2p.outproxy). This is purely
//! a read-side endpoint that the /network/i2p UI polls every few
//! seconds.

use crate::api::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use std::path::Path;
use tokio::process::Command;

const I2PD_CONF: &str = "/etc/i2pd/i2pd.conf";

/// GET /api/network/i2p/status
pub async fn get_status(State(_state): State<AppState>) -> Json<Value> {
    let installed = Path::new("/usr/sbin/i2pd").exists()
        || Path::new("/usr/bin/i2pd").exists();
    let service_active = systemctl_is_active("i2pd.service").await;

    // Parse out the bind addresses + ports + outproxy from i2pd.conf.
    let cfg = read_i2pd_config();

    Json(json!({
        "ok": true,
        "installed": installed,
        "service_active": service_active,
        "http_proxy": {
            "addr": cfg.httpproxy_addr.clone().unwrap_or_default(),
            "port": cfg.httpproxy_port.unwrap_or(4444),
        },
        "socks_proxy": {
            "addr": cfg.socksproxy_addr.clone().unwrap_or_default(),
            "port": cfg.socksproxy_port.unwrap_or(4447),
        },
        "web_console": {
            "addr": cfg.http_addr.clone().unwrap_or_default(),
            "port": cfg.http_port.unwrap_or(7070),
        },
        "outproxy": cfg.outproxy.clone().unwrap_or_default(),
        // Convenience URLs the UI displays as "point your browser at this".
        // Falls back to localhost when nothing useful is parseable.
        "browser_hint": {
            "http_proxy_url": format!(
                "http://{}:{}",
                cfg.httpproxy_addr.clone().unwrap_or_else(|| "127.0.0.1".into()),
                cfg.httpproxy_port.unwrap_or(4444),
            ),
            "console_url": format!(
                "http://{}:{}/",
                cfg.http_addr.clone().unwrap_or_else(|| "127.0.0.1".into()),
                cfg.http_port.unwrap_or(7070),
            ),
        },
    }))
}

#[derive(Default, Debug)]
struct I2pdConfig {
    httpproxy_addr: Option<String>,
    httpproxy_port: Option<u16>,
    socksproxy_addr: Option<String>,
    socksproxy_port: Option<u16>,
    http_addr: Option<String>,
    http_port: Option<u16>,
    outproxy: Option<String>,
}

/// Read /etc/i2pd/i2pd.conf and pull the bind address + port out of
/// the three sections we care about. Tolerant of inline comments,
/// blank lines, and missing sections — anything we can't parse
/// becomes None and the API surfaces the i2pd defaults instead.
fn read_i2pd_config() -> I2pdConfig {
    let mut out = I2pdConfig::default();
    let text = match std::fs::read_to_string(I2PD_CONF) {
        Ok(t) => t,
        Err(_) => return out,
    };
    let mut current_section: Option<String> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current_section = Some(rest.trim().to_lowercase());
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_lowercase();
            let val = v.trim().to_string();
            match (current_section.as_deref(), key.as_str()) {
                (Some("httpproxy"), "address") => out.httpproxy_addr = Some(val),
                (Some("httpproxy"), "port") => out.httpproxy_port = val.parse().ok(),
                (Some("httpproxy"), "outproxy") => {
                    if !val.is_empty() {
                        out.outproxy = Some(val);
                    }
                }
                (Some("socksproxy"), "address") => out.socksproxy_addr = Some(val),
                (Some("socksproxy"), "port") => out.socksproxy_port = val.parse().ok(),
                (Some("http"), "address") => out.http_addr = Some(val),
                (Some("http"), "port") => out.http_port = val.parse().ok(),
                _ => {}
            }
        }
    }
    out
}

async fn systemctl_is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
