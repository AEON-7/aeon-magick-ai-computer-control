//! Mysterium — run a Mysterium Network node: share idle bandwidth to help
//! decentralize internet access and earn MYST. The `aeon-mysterium` script
//! installs + runs the official myst node; this module reads live stats from the
//! node's local TequilAPI (127.0.0.1:4050) for the dashboard. Account + wallet/
//! payout are set up non-custodially at mystnodes.co. Admin-gated; OFF by default.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/mysterium.toml";
const SCRIPT: &str = "/usr/local/bin/aeon-mysterium";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MysteriumConfig {
    #[serde(default)]
    pub enabled: bool,
}

fn read_config() -> MysteriumConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &MysteriumConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
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

fn script_status() -> Value {
    run_script(&["status"])
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"installed": false, "daemon": "inactive"}))
}

/// Query the node's local TequilAPI (default Basic auth myst:mystberry).
fn tq(path: &str) -> Option<Value> {
    let url = format!("http://127.0.0.1:4050{path}");
    let out = Command::new("curl")
        .args(["-fsS", "-u", "myst:mystberry", "--max-time", "6", "--", &url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn ptr_str(o: &Option<Value>, p: &str) -> String {
    o.as_ref()
        .and_then(|v| v.pointer(p))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn ptr_u64(o: &Option<Value>, p: &str) -> u64 {
    o.as_ref()
        .and_then(|v| v.pointer(p))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// GET /api/mysterium/status — node lifecycle + registration + earnings +
/// sessions + bandwidth + region, aggregated from the TequilAPI. Admin-gated.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let st = script_status();
        let installed = st.get("installed").and_then(|v| v.as_bool()).unwrap_or(false);
        let daemon = st.get("daemon").and_then(|v| v.as_str()).unwrap_or("inactive").to_string();
        if daemon != "active" {
            return json!({"ok": true, "enabled": cfg.enabled, "installed": installed, "daemon": daemon});
        }
        let hc = tq("/healthcheck");
        let id = tq("/identities")
            .and_then(|v| v.pointer("/identities/0/id").and_then(|x| x.as_str()).map(String::from));
        let info = id.as_ref().and_then(|i| tq(&format!("/identities/{i}")));
        let bene = id.as_ref().and_then(|i| tq(&format!("/identities/{i}/beneficiary")));
        let loc = tq("/location");
        let data = tq("/node/provider/transferred-data?range=30d");
        let sess = tq("/node/provider/sessions-count?range=30d");
        let cons = tq("/node/provider/consumers-count?range=30d");
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "installed": true,
            "daemon": daemon,
            "version": ptr_str(&hc, "/version"),
            "uptime": ptr_str(&hc, "/uptime"),
            "identity": id.clone().unwrap_or_default(),
            "registration": ptr_str(&info, "/registration_status"),
            "earnings_myst": ptr_str(&info, "/earnings_tokens/human"),
            "earnings_total_myst": ptr_str(&info, "/earnings_total_tokens/human"),
            "balance_myst": ptr_str(&info, "/balance_tokens/human"),
            "beneficiary": ptr_str(&bene, "/beneficiary"),
            "country": ptr_str(&loc, "/country"),
            "region": ptr_str(&loc, "/region"),
            "city": ptr_str(&loc, "/city"),
            "ip": ptr_str(&loc, "/ip"),
            "data_bytes_30d": ptr_u64(&data, "/transferred_data_bytes"),
            "sessions_30d": ptr_u64(&sess, "/count"),
            "consumers_30d": ptr_u64(&cons, "/count"),
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/mysterium/enable — install (first run downloads ~19 MB) + start the
/// node. Non-blocking: persists config + brings it up in the background; the
/// dashboard polls /status. Admin-gated.
pub async fn enable(State(_s): State<AppState>) -> Json<Value> {
    let saved = tokio::task::spawn_blocking(|| -> bool {
        let mut cfg = read_config();
        cfg.enabled = true;
        write_config(&cfg).is_ok()
    })
    .await
    .unwrap_or(false);
    if saved {
        tokio::task::spawn_blocking(|| {
            let _ = run_script(&["up"]);
        });
        Json(json!({"ok": true, "starting": true}))
    } else {
        Json(json!({"ok": false, "err": "write config failed"}))
    }
}

/// POST /api/mysterium/disable — stop the node (keeps it installed + your claim).
/// Admin-gated.
pub async fn disable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let mut cfg = read_config();
        cfg.enabled = false;
        let _ = write_config(&cfg);
        let _ = run_script(&["down"]);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "disable task failed"}));
    Json(v)
}
