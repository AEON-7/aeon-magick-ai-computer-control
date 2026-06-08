//! Hidden Services — host as many Tor v3 `.onion` services as you like, one per
//! local app/site, each with a friendly nickname + the onion shown for sharing.
//! Drives the `aeon-onions` infra script (a dedicated Tor instance, separate
//! from the privacy-stack and OrbNet Tor). OFF by default; admin-gated over REST.
//! Also exposed to agents via MCP tools (hidden_service_*) so an agent can stand
//! up an onion for a site or app it hosts.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/onions.toml";
const SCRIPT: &str = "/usr/local/bin/aeon-onions";

fn default_virt() -> u16 {
    80
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OnionService {
    pub id: String,
    #[serde(default)]
    pub nickname: String,
    pub local_port: u16,
    #[serde(default = "default_virt")]
    pub virt_port: u16,
    #[serde(default)]
    pub onion: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OnionsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub services: Vec<OnionService>,
}

fn read_config() -> OnionsConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &OnionsConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

/// Run the `aeon-onions` infra script, returning trimmed stdout.
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
        .unwrap_or_else(|| json!({"tor": "inactive", "count": 0}))
}

/// Live `id -> onion` map from `aeon-onions list` (the on-disk hostname files).
fn list_live() -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(out) = run_script(&["list"]) {
        for line in out.lines() {
            let mut it = line.split('\t');
            if let (Some(id), Some(onion)) = (it.next(), it.next()) {
                if !id.is_empty() {
                    m.insert(id.to_string(), onion.to_string());
                }
            }
        }
    }
    m
}

fn rand_hex(bytes: usize) -> String {
    std::fs::read("/dev/urandom")
        .ok()
        .map(|b| b.into_iter().take(bytes).map(|x| format!("{x:02x}")).collect())
        .unwrap_or_else(|| "0".repeat(bytes * 2))
}

fn sanitize_nick(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect::<String>().trim().to_string()
}

fn svc_json(s: &OnionService, live: &HashMap<String, String>) -> Value {
    let onion = live
        .get(&s.id)
        .cloned()
        .filter(|o| !o.is_empty() && o.as_str() != "pending")
        .unwrap_or_else(|| s.onion.clone());
    json!({
        "id": s.id,
        "nickname": s.nickname,
        "local_port": s.local_port,
        "virt_port": s.virt_port,
        "onion": onion,
    })
}

/// GET /api/onions/status — feature state + the hosted services. Admin-gated.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let st = script_status();
        let live = list_live();
        let services: Vec<Value> = cfg.services.iter().map(|s| svc_json(s, &live)).collect();
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "tor": st.get("tor").cloned().unwrap_or(json!("inactive")),
            "count": services.len(),
            "services": services,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/onions/enable — start the hidden-services Tor. Admin-gated.
pub async fn enable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let mut cfg = read_config();
        cfg.enabled = true;
        let _ = write_config(&cfg);
        match run_script(&["up"]) {
            Ok(_) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "enable task failed"}));
    Json(v)
}

/// POST /api/onions/disable — stop the hidden-services Tor. Keeps the configured
/// services so re-enabling restores them all. Admin-gated.
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

#[derive(Deserialize)]
pub struct CreateReq {
    #[serde(default)]
    pub nickname: String,
    pub local_port: u16,
    #[serde(default)]
    pub virt_port: Option<u16>,
}

/// POST /api/onions/create — mint a new `.onion` that forwards `virt_port`
/// (default 80) to `127.0.0.1:<local_port>`, returning the address. Auto-enables
/// the feature. Admin-gated over REST; also the `hidden_service_create` MCP tool.
pub async fn create(State(_s): State<AppState>, Json(req): Json<CreateReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        if req.local_port == 0 {
            return json!({"ok": false, "err": "local_port required"});
        }
        let virt = req.virt_port.unwrap_or(80);
        let id = format!("hs-{}", rand_hex(4));
        let lp = req.local_port.to_string();
        let vp = virt.to_string();
        let onion = match run_script(&["add", &id, &lp, &vp]) {
            Ok(o) if !o.is_empty() => o,
            Ok(_) => return json!({"ok": false, "err": "onion not provisioned (is the onion Tor up?)"}),
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let svc = OnionService {
            id,
            nickname: sanitize_nick(&req.nickname),
            local_port: req.local_port,
            virt_port: virt,
            onion,
        };
        let service = svc_json(&svc, &HashMap::new());
        let mut cfg = read_config();
        cfg.enabled = true;
        cfg.services.push(svc);
        match write_config(&cfg) {
            Ok(_) => json!({"ok": true, "service": service}),
            Err(e) => json!({"ok": false, "err": format!("onion created but config save failed: {e}")}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "create task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct RemoveReq {
    pub id: String,
}

/// POST /api/onions/remove — retire a hidden service (drops its onion key + dir).
/// Admin-gated over REST; also the `hidden_service_remove` MCP tool.
pub async fn remove(State(_s): State<AppState>, Json(req): Json<RemoveReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let _ = run_script(&["remove", &req.id]);
        let mut cfg = read_config();
        cfg.services.retain(|s| s.id != req.id);
        let _ = write_config(&cfg);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "remove task failed"}));
    Json(v)
}
