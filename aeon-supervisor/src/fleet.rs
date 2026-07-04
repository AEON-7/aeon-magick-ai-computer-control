//! Orb fleet — Phase 1: per-Orb identity, peer discovery, and a roster.
//!
//! Each Aeon Magick Orb is a peer in a decentralized fleet (no central
//! controller). This module gives every Orb:
//!   * a **status heartbeat** (`GET /api/fleet/status`) describing itself —
//!     model, what it controls, UPS %, capture sources, current view source,
//!     webcam state, health, version. This is the PEER-facing endpoint: it's
//!     allow-listed past the user-auth middleware but requires the shared
//!     `X-Fleet-Token` (from `/etc/aeon/fleet.toml`, same value on every fleet
//!     member) so only fellow Orbs can read it.
//!   * a **roster** (`GET /api/fleet/roster`) — behind normal user auth — that
//!     the local web console calls. The supervisor fans out (concurrently, over
//!     the tailnet/LAN, via `curl`) to each peer's `/api/fleet/status`, so the
//!     browser only ever talks to its own Orb.
//!   * `GET`/`PUT /api/fleet/config` to read + edit this Orb's `label` and peer
//!     `seeds` (the token is managed out-of-band / by the later sealed key-sync,
//!     never exposed or set through the API).
//!
//! All facts are assembled from helpers that already exist (streamer_config,
//! system, ups, webcam, agent_connect) so this is mostly wiring. No new system
//! service, no new dependency — peer fetch reuses the codebase's `curl` idiom.
//! Until an Orb has a `fleet.toml` with a non-empty `token`, the fleet is
//! considered unconfigured and the roster shows only this Orb.

use crate::api::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

const FLEET_TOML: &str = "/etc/aeon/fleet.toml";

/// On-disk fleet config. An empty `token` (or a missing file) means "fleet not
/// configured" — the heartbeat refuses and the roster returns only self.
#[derive(Default)]
struct FleetCfg {
    id: String,
    label: String,
    token: String,
    tailscale: bool,
    seeds: Vec<String>,
}

fn load_cfg() -> FleetCfg {
    let v: toml::Value = std::fs::read_to_string(FLEET_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    let fleet = v.get("fleet");
    let id = fleet
        .and_then(|f| f.get("id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(machine_id);
    let label = fleet
        .and_then(|f| f.get("label"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let token = fleet
        .and_then(|f| f.get("token"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let disc = v.get("discovery");
    let tailscale = disc
        .and_then(|d| d.get("tailscale"))
        .and_then(|x| x.as_bool())
        .unwrap_or(true);
    let seeds = disc
        .and_then(|d| d.get("seeds"))
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    FleetCfg { id, label, token, tailscale, seeds }
}

/// Stable per-Orb id: the (install-unique) machine-id, else the hostname.
fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|s| s.trim().chars().take(12).collect::<String>())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(hostname)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn pi_model() -> &'static str {
    match std::fs::read_to_string("/proc/device-tree/model") {
        Ok(m) if m.contains("Raspberry Pi 5") => "pi5",
        Ok(m) if m.contains("Raspberry Pi 4") => "pi4",
        _ => "other",
    }
}

/// All local interface IPs (`hostname -I`) — used both as this Orb's addresses
/// and to drop our own address out of the discovered peer set.
fn local_ips() -> Vec<String> {
    Command::new("hostname")
        .arg("-I")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Concrete capture sources present on this Orb (drops the synthetic "auto").
fn sources() -> Vec<String> {
    crate::streamer_config::detect_available_sources()
        .into_iter()
        .filter(|s| *s != "auto")
        .map(str::to_string)
        .collect()
}

/// Current console view source from streamer.toml `[capture] source`.
fn view_source() -> String {
    std::fs::read_to_string("/etc/aeon/streamer.toml")
        .ok()
        .and_then(|t| toml::from_str::<toml::Value>(&t).ok())
        .and_then(|v| {
            v.get("capture")
                .and_then(|c| c.get("source"))
                .and_then(|s| s.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "auto".to_string())
}

/// Webcam enable/source from uvc.toml (mirrors webcam.rs's read).
fn webcam_brief() -> Value {
    let v: toml::Value = std::fs::read_to_string("/etc/aeon/uvc.toml")
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    let enabled = v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false);
    let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("").to_string();
    json!({ "enabled": enabled, "source": if enabled { source } else { "off".to_string() } })
}

/// UPS state from the aeon-ups daemon's json drop (mirrors ups.rs).
fn ups() -> Value {
    std::fs::read_to_string("/run/aeon/ups.json")
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "present": false }))
}

/// This Orb's facts. Used by the peer heartbeat AND as the `self` roster entry.
fn self_status(cfg: &FleetCfg) -> Value {
    let ips = local_ips();
    let lan_ip = ips.first().cloned().unwrap_or_default();
    json!({
        "id": cfg.id,
        "hostname": hostname(),
        "label": cfg.label,
        "model": pi_model(),
        "lan_ip": lan_ip,
        "addrs": ips,
        "sources": sources(),
        "view_source": view_source(),
        "webcam": webcam_brief(),
        "ups": ups(),
        "health": crate::system::snapshot(),
        "version": env!("CARGO_PKG_VERSION"),
        "seeds": cfg.seeds,
        // Shared AI-model catalog + kubo PeerID (ipfs.rs). Rides the heartbeat
        // so every fleet peer sees a federated model index with zero extra
        // protocol — fetch_peer() forwards unknown fields wholesale.
        "ipfs_models": crate::ipfs::shared_models(),
    })
}

/// This Orb's fleet identity (id, label) — provenance stamp for content it
/// publishes (e.g. shared IPFS models).
pub fn identity() -> (String, String) {
    let cfg = load_cfg();
    (cfg.id, cfg.label)
}

/// Constant-time string compare for the shared fleet token.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn err500(msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "ok": false, "err": msg })),
    )
        .into_response()
}

/// GET /api/fleet/status — peer-facing heartbeat. Allow-listed past user auth
/// (api.rs), but requires the shared `X-Fleet-Token`.
pub async fn get_status(State(_s): State<AppState>, headers: HeaderMap) -> Response {
    let cfg = load_cfg();
    if cfg.token.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false, "err": "fleet not configured on this Orb" })),
        )
            .into_response();
    }
    let provided = headers
        .get("x-fleet-token")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !ct_eq(provided, &cfg.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "err": "bad or missing X-Fleet-Token" })),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "status": self_status(&cfg) })).into_response()
}

/// Fetch one peer's `/api/fleet/status` over HTTPS with the fleet token. Never
/// errors — a down/slow peer resolves to an `online:false` stub.
async fn fetch_peer(addr: &str, token: &str) -> Value {
    let url = if addr.contains("://") {
        format!("{}/api/fleet/status", addr.trim_end_matches('/'))
    } else {
        format!("https://{addr}/api/fleet/status")
    };
    let header = format!("X-Fleet-Token: {token}");
    let addr_owned = addr.to_string();
    let out = tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args(["-sk", "--max-time", "3", "-H", &header, &url])
            .output()
    })
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            if let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) {
                if let Some(mut status) = v.get("status").cloned() {
                    if let Some(obj) = status.as_object_mut() {
                        obj.insert("online".to_string(), json!(true));
                        obj.insert("addr".to_string(), json!(addr_owned));
                    }
                    return status;
                }
            }
            json!({ "online": false, "addr": addr_owned, "err": "unexpected response" })
        }
        _ => json!({ "online": false, "addr": addr_owned }),
    }
}

/// GET /api/fleet/roster — local-web. Aggregates self + every reachable peer.
pub async fn get_roster(State(_s): State<AppState>) -> Response {
    let cfg = load_cfg();
    let mut me = self_status(&cfg);
    if let Some(o) = me.as_object_mut() {
        o.insert("online".to_string(), json!(true));
        o.insert("is_self".to_string(), json!(true));
    }

    if cfg.token.is_empty() {
        return Json(json!({ "ok": true, "configured": false, "self": me, "peers": [] }))
            .into_response();
    }

    // Peer set = static seeds ∪ tailnet-discovered peers, minus our own IPs.
    let mut addrs: Vec<String> = cfg.seeds.clone();
    if cfg.tailscale {
        addrs.extend(crate::agent_connect::tailscale_peer_ipv4s());
    }
    let mine = local_ips();
    addrs.sort();
    addrs.dedup();
    addrs.retain(|a| !mine.contains(a));

    // Fan out concurrently so one slow/down peer never stalls the roster.
    let token = cfg.token.clone();
    let mut handles = Vec::new();
    for a in addrs {
        let t = token.clone();
        handles.push(tokio::spawn(async move { fetch_peer(&a, &t).await }));
    }
    let mut peers = Vec::new();
    for h in handles {
        if let Ok(p) = h.await {
            peers.push(p);
        }
    }

    Json(json!({ "ok": true, "configured": true, "self": me, "peers": peers })).into_response()
}

/// GET /api/fleet/config — this Orb's editable settings (never the token).
pub async fn get_config(State(_s): State<AppState>) -> Json<Value> {
    let cfg = load_cfg();
    Json(json!({
        "ok": true,
        "configured": !cfg.token.is_empty(),
        "id": cfg.id,
        "label": cfg.label,
        "tailscale": cfg.tailscale,
        "seeds": cfg.seeds,
    }))
}

#[derive(Deserialize)]
pub struct ConfigPatch {
    pub label: Option<String>,
    pub seeds: Option<Vec<String>>,
    pub tailscale: Option<bool>,
}

/// PUT /api/fleet/config — update label / seeds / tailscale-discovery. Preserves
/// `token` + `id`; atomic write (tmp + rename), same idiom as webcam.rs.
pub async fn put_config(State(_s): State<AppState>, Json(patch): Json<ConfigPatch>) -> Response {
    let text = std::fs::read_to_string(FLEET_TOML).unwrap_or_default();
    let mut parsed: toml::Value =
        toml::from_str(&text).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let table = match parsed.as_table_mut() {
        Some(t) => t,
        None => return err500("fleet.toml is not a table"),
    };

    let fleet = table
        .entry("fleet".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if let (Some(ft), Some(label)) = (fleet.as_table_mut(), patch.label.as_ref()) {
        ft.insert("label".into(), toml::Value::String(label.clone()));
    }

    let disc = table
        .entry("discovery".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if let Some(dt) = disc.as_table_mut() {
        if let Some(seeds) = patch.seeds.as_ref() {
            dt.insert(
                "seeds".into(),
                toml::Value::Array(seeds.iter().map(|s| toml::Value::String(s.clone())).collect()),
            );
        }
        if let Some(ts) = patch.tailscale {
            dt.insert("tailscale".into(), toml::Value::Boolean(ts));
        }
    }

    let serialized = match toml::to_string_pretty(&parsed) {
        Ok(s) => s,
        Err(e) => return err500(&format!("serialize fleet.toml: {e}")),
    };
    let tmp = format!("{FLEET_TOML}.tmp");
    if let Err(e) = std::fs::write(&tmp, &serialized).and_then(|_| std::fs::rename(&tmp, FLEET_TOML)) {
        return err500(&format!("write fleet.toml: {e}"));
    }
    Json(json!({ "ok": true })).into_response()
}
