//! HTTP handlers for /api/network/vpn/providers/* (v59+).
//!
//! Unified surface across all three providers (Mullvad / IVPN /
//! AzireVPN) — the URL's :provider path param picks which module
//! services the request. Keeping them on one shape means the UI
//! has one client to build instead of three.

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{provider_meta, PROVIDERS, mullvad, ivpn, azirevpn};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn unknown_provider(id: &str) -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "err": format!("unknown provider '{id}'")})),
    )
        .into_response()
}

// ── GET /api/network/vpn/providers/catalog ──────────────────────────

pub async fn get_catalog(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "providers": PROVIDERS,
    }))
}

// ── POST /api/network/vpn/providers/:id/setup ───────────────────────

#[derive(Deserialize)]
pub struct SetupReq {
    /// 16-digit Mullvad account number / IVPN account ID / AzireVPN
    /// API token, depending on provider.
    pub credential: String,
    /// Optional device label shown to the user. Defaults to "aeon-magick".
    #[serde(default = "default_device_name")]
    pub device_name: String,
}
fn default_device_name() -> String { "aeon-magick".into() }

pub async fn setup(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<SetupReq>,
) -> impl IntoResponse {
    let cred = req.credential.trim();
    if cred.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "credential is empty"})),
        )
            .into_response();
    }
    let meta = match provider_meta(&provider) {
        Some(m) => m,
        None => return unknown_provider(&provider),
    };

    // Run the blocking provider call in a worker thread. Each
    // round-trip is a couple of HTTPS POSTs; 10-30s typical.
    let provider_id = provider.clone();
    let cred = cred.to_string();
    let device_name = req.device_name.clone();
    let trust = meta.trust_score;
    let blocking = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        // Generate a fresh WG keypair for this device.
        let (priv_key, pub_key) = super::generate_wg_keypair()?;
        match provider_id.as_str() {
            "mullvad" => {
                let dev = mullvad::register_device(&cred, &pub_key, &device_name)?;
                let servers = mullvad::fetch_relays(trust)?;
                let s = mullvad::MullvadState {
                    account_number: cred,
                    device_id: dev.id,
                    device_name: dev.name,
                    peer_ipv4: dev.ipv4,
                    peer_ipv6: dev.ipv6,
                    wg_private_key: priv_key,
                    wg_public_key: pub_key,
                    selected_server: String::new(),
                    selection_mode: "manual".into(),
                    servers_updated_ms: now_ms(),
                    servers: servers.clone(),
                };
                mullvad::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({"ok": true, "server_count": servers.len(), "peer_ipv4": s.peer_ipv4}))
            }
            "ivpn" => {
                let session = ivpn::new_session(&cred, &pub_key)?;
                let servers = ivpn::fetch_servers(trust)?;
                let s = ivpn::IvpnState {
                    account_id: cred,
                    session_token: session.token,
                    session_expires_ms: now_ms() + 12 * 3600 * 1000,
                    peer_ipv4: session.ipv4,
                    wg_private_key: priv_key,
                    wg_public_key: pub_key,
                    selected_server: String::new(),
                    selection_mode: "manual".into(),
                    servers_updated_ms: now_ms(),
                    servers: servers.clone(),
                };
                ivpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({"ok": true, "server_count": servers.len(), "peer_ipv4": s.peer_ipv4}))
            }
            "azirevpn" => {
                let kr = azirevpn::register_key(&cred, &pub_key)?;
                let servers = azirevpn::fetch_servers(&cred, trust)?;
                let s = azirevpn::AzireState {
                    api_token: cred,
                    peer_ipv4: kr.ipv4,
                    wg_private_key: priv_key,
                    wg_public_key: pub_key,
                    selected_server: String::new(),
                    selection_mode: "manual".into(),
                    servers_updated_ms: now_ms(),
                    servers: servers.clone(),
                };
                azirevpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({"ok": true, "server_count": servers.len(), "peer_ipv4": s.peer_ipv4}))
            }
            _ => Err(format!("unknown provider '{provider_id}'")),
        }
    }).await;

    match blocking {
        Ok(Ok(v)) => {
            crate::audit::log("admin (session)", "vpn_provider_setup",
                &format!("provider={provider}"), "ok", None);
            Json(v).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": e})),
        ).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("join: {e}")})),
        ).into_response(),
    }
}

// ── GET /api/network/vpn/providers/:id/state ────────────────────────
//
// Returns the supervisor's view of the provider — server list, the
// chosen server, selection mode, peer IP. Credentials are NOT echoed.

pub async fn get_state(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
) -> impl IntoResponse {
    let meta = match provider_meta(&provider) {
        Some(m) => m,
        None => return unknown_provider(&provider),
    };
    let body = match provider.as_str() {
        "mullvad" => {
            let s = mullvad::read_state();
            json!({
                "ok": true,
                "configured": !s.account_number.is_empty(),
                "device_name": s.device_name,
                "peer_ipv4": s.peer_ipv4,
                "peer_ipv6": s.peer_ipv6,
                "selected_server": s.selected_server,
                "selection_mode": s.selection_mode,
                "servers": s.servers,
                "servers_updated_ms": s.servers_updated_ms,
                "meta": meta,
            })
        }
        "ivpn" => {
            let s = ivpn::read_state();
            json!({
                "ok": true,
                "configured": !s.account_id.is_empty(),
                "peer_ipv4": s.peer_ipv4,
                "selected_server": s.selected_server,
                "selection_mode": s.selection_mode,
                "servers": s.servers,
                "servers_updated_ms": s.servers_updated_ms,
                "session_expires_ms": s.session_expires_ms,
                "meta": meta,
            })
        }
        "azirevpn" => {
            let s = azirevpn::read_state();
            json!({
                "ok": true,
                "configured": !s.api_token.is_empty(),
                "peer_ipv4": s.peer_ipv4,
                "selected_server": s.selected_server,
                "selection_mode": s.selection_mode,
                "servers": s.servers,
                "servers_updated_ms": s.servers_updated_ms,
                "meta": meta,
            })
        }
        _ => return unknown_provider(&provider),
    };
    Json(body).into_response()
}

// ── POST /api/network/vpn/providers/:id/select ──────────────────────

#[derive(Deserialize)]
pub struct SelectReq {
    pub server_id: Option<String>,
    pub mode: Option<String>,  // "manual" or "auto"
}

pub async fn select(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<SelectReq>,
) -> impl IntoResponse {
    if provider_meta(&provider).is_none() {
        return unknown_provider(&provider);
    }
    match provider.as_str() {
        "mullvad" => {
            let mut s = mullvad::read_state();
            if let Some(id) = req.server_id {
                if !s.servers.iter().any(|sv| sv.id == id) {
                    return (StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "err": format!("server '{id}' not in cache")}))
                    ).into_response();
                }
                s.selected_server = id;
            }
            if let Some(m) = req.mode { s.selection_mode = m; }
            if let Err(e) = mullvad::write_state(&s) {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "err": format!("persist: {e}")}))).into_response();
            }
            Json(json!({"ok": true, "selected_server": s.selected_server})).into_response()
        }
        "ivpn" => {
            let mut s = ivpn::read_state();
            if let Some(id) = req.server_id {
                if !s.servers.iter().any(|sv| sv.id == id) {
                    return (StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "err": format!("server '{id}' not in cache")}))
                    ).into_response();
                }
                s.selected_server = id;
            }
            if let Some(m) = req.mode { s.selection_mode = m; }
            if let Err(e) = ivpn::write_state(&s) {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "err": format!("persist: {e}")}))).into_response();
            }
            Json(json!({"ok": true, "selected_server": s.selected_server})).into_response()
        }
        "azirevpn" => {
            let mut s = azirevpn::read_state();
            if let Some(id) = req.server_id {
                if !s.servers.iter().any(|sv| sv.id == id) {
                    return (StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "err": format!("server '{id}' not in cache")}))
                    ).into_response();
                }
                s.selected_server = id;
            }
            if let Some(m) = req.mode { s.selection_mode = m; }
            if let Err(e) = azirevpn::write_state(&s) {
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "err": format!("persist: {e}")}))).into_response();
            }
            Json(json!({"ok": true, "selected_server": s.selected_server})).into_response()
        }
        _ => unknown_provider(&provider),
    }
}

// ── POST /api/network/vpn/providers/:id/pick-fastest ────────────────
//
// Probe TCP latency to each cached server's endpoint:port and return
// the list sorted by RTT. The supervisor doesn't auto-apply the
// result — the UI shows the ranking and lets the user pick.

pub async fn pick_fastest(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
) -> impl IntoResponse {
    if provider_meta(&provider).is_none() {
        return unknown_provider(&provider);
    }
    let servers: Vec<crate::vpn_providers::Server> = match provider.as_str() {
        "mullvad" => mullvad::read_state().servers,
        "ivpn"    => ivpn::read_state().servers,
        "azirevpn" => azirevpn::read_state().servers,
        _ => return unknown_provider(&provider),
    };
    if servers.is_empty() {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "no servers in cache — finish setup first"}))).into_response();
    }
    let probed = tokio::task::spawn_blocking(move || {
        servers.into_iter().map(|s| {
            let addr = format!("{}:{}", s.endpoint_ip, s.endpoint_port);
            let t0 = std::time::Instant::now();
            let rtt_ms = match std::net::TcpStream::connect_timeout(
                &addr.parse().unwrap_or_else(|_| "127.0.0.1:1".parse().unwrap()),
                std::time::Duration::from_millis(1500),
            ) {
                Ok(_) => Some(t0.elapsed().as_millis() as u32),
                Err(_) => None,
            };
            json!({
                "id": s.id,
                "label": s.label,
                "country": s.country,
                "city": s.city,
                "rtt_ms": rtt_ms,
                "server_score": s.server_score,
            })
        }).collect::<Vec<_>>()
    }).await;
    match probed {
        Ok(mut v) => {
            // Sort: reachable first (lowest rtt_ms), then unreachable.
            v.sort_by_key(|e| e.get("rtt_ms").and_then(|x| x.as_u64()).unwrap_or(u64::MAX));
            Json(json!({"ok": true, "ranking": v})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("probe: {e}")}))).into_response(),
    }
}
