//! HTTP handlers for /api/network/vpn/providers/* (v59+).
//!
//! Unified surface across all three providers (Mullvad / IVPN /
//! AzireVPN) — the URL's :provider path param picks which module
//! services the request. Keeping them on one shape means the UI
//! has one client to build instead of three.

use crate::api::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{provider_meta, PROVIDERS, mullvad, ivpn, azirevpn, airvpn, EyesTier};

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
    /// API token, depending on provider. Unused for AirVPN (which
    /// authenticates via `api_key` + a pasted config instead).
    #[serde(default)]
    pub credential: String,
    /// Optional device label shown to the user. Defaults to "aeon-magick".
    #[serde(default = "default_device_name")]
    pub device_name: String,

    // ── AirVPN-only fields ──────────────────────────────────────────
    // AirVPN's API does NOT mint client credentials. The user pastes a
    // config from AirVPN's Config Generator (once) and supplies an API
    // key (which powers the server list / No-Eyes / pick-fastest).
    /// AirVPN API key (member area → Client Area → API).
    #[serde(default)]
    pub api_key: Option<String>,
    /// AirVPN connection mode: "wireguard" | "openvpn" | "openvpn_ssl"
    /// | "openvpn_ssh". The last two are the stealth/obfuscation modes.
    #[serde(default)]
    pub mode: Option<String>,
    /// Pasted AirVPN WireGuard config (Config Generator → WireGuard).
    #[serde(default)]
    pub wg_config: Option<String>,
    /// Pasted AirVPN OpenVPN bundle (.ovpn; certs inline). Used by the
    /// openvpn / openvpn_ssl / openvpn_ssh modes.
    #[serde(default)]
    pub openvpn_config: Option<String>,
}
fn default_device_name() -> String { "aeon-magick".into() }

pub async fn setup(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<SetupReq>,
) -> impl IntoResponse {
    let cred = req.credential.trim();
    // AirVPN authenticates via api_key + a pasted config, not a single
    // credential — so don't reject an empty `credential` for it.
    if cred.is_empty() && provider != "airvpn" {
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
    // AirVPN setup inputs (moved into the closure below).
    let av_api_key = req.api_key.clone();
    let av_mode = req.mode.clone();
    let av_wg_config = req.wg_config.clone();
    let av_ovpn_config = req.openvpn_config.clone();
    let blocking = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        // Generate a fresh WG keypair for this device.
        let (priv_key, pub_key) = super::generate_wg_keypair()?;
        match provider_id.as_str() {
            "mullvad" => {
                // v67.6: fetch relays FIRST (public, no device impact) so a
                // relay-list problem can't burn a Mullvad device slot — the
                // old order registered a device before the relay parse, and
                // every failed retry left an orphaned device behind (Mullvad
                // caps accounts at 5). Then REUSE an existing device for the
                // same account instead of registering a new one each run.
                let servers = mullvad::fetch_relays(trust)?;
                let existing = mullvad::read_state();
                let reuse = existing.account_number == cred
                    && !existing.device_id.is_empty()
                    && !existing.wg_private_key.is_empty();
                let keep_selected = if reuse
                    && servers.iter().any(|sv| sv.id == existing.selected_server)
                {
                    existing.selected_server.clone()
                } else {
                    String::new()
                };
                let s = if reuse {
                    mullvad::MullvadState {
                        account_number: cred,
                        device_id: existing.device_id,
                        device_name: existing.device_name,
                        peer_ipv4: existing.peer_ipv4,
                        peer_ipv6: existing.peer_ipv6,
                        wg_private_key: existing.wg_private_key,
                        wg_public_key: existing.wg_public_key,
                        selected_server: keep_selected,
                        selection_mode: "manual".into(),
                        servers_updated_ms: now_ms(),
                        servers: servers.clone(),
                    }
                } else {
                    let dev = mullvad::register_device(&cred, &pub_key, &device_name)?;
                    mullvad::MullvadState {
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
                    }
                };
                mullvad::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({
                    "ok": true,
                    "server_count": servers.len(),
                    "peer_ipv4": s.peer_ipv4,
                    "reused_device": reuse,
                }))
            }
            "ivpn" => {
                // v67.5: REUSE an existing valid session instead of always
                // calling /v4/session/new. Every new_session registers a
                // fresh WireGuard device with IVPN and consumes a device
                // slot — re-running setup (e.g. to refresh the server list)
                // would burn through the account's per-plan device limit
                // and eventually fail. The server list itself comes from a
                // PUBLIC endpoint (servers.json, no auth, no device impact),
                // so when we already hold a non-expired session for THIS
                // account we keep it and just refresh servers.
                let existing = ivpn::read_state();
                let reuse = existing.account_id == cred
                    && !existing.session_token.is_empty()
                    && !existing.wg_private_key.is_empty()
                    // Don't reuse a session whose WG IP never got captured — that
                    // would perpetuate the broken empty "Address = /32" config.
                    // Forcing a fresh /session/new re-allocates + re-parses it.
                    && !existing.peer_ipv4.is_empty()
                    && existing.session_expires_ms > now_ms();

                let servers = ivpn::fetch_servers(trust)?;
                // keep a previously-selected server only if it still exists.
                let keep_selected = if reuse
                    && servers.iter().any(|sv| sv.id == existing.selected_server)
                {
                    existing.selected_server.clone()
                } else {
                    String::new()
                };
                let s = if reuse {
                    ivpn::IvpnState {
                        account_id: cred,
                        session_token: existing.session_token,
                        session_expires_ms: existing.session_expires_ms,
                        peer_ipv4: existing.peer_ipv4,
                        wg_private_key: existing.wg_private_key,
                        wg_public_key: existing.wg_public_key,
                        selected_server: keep_selected,
                        selection_mode: "manual".into(),
                        servers_updated_ms: now_ms(),
                        servers: servers.clone(),
                    }
                } else {
                    let session = ivpn::new_session(&cred, &pub_key)?;
                    ivpn::IvpnState {
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
                    }
                };
                ivpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({
                    "ok": true,
                    "server_count": servers.len(),
                    "peer_ipv4": s.peer_ipv4,
                    "reused_session": reuse,
                }))
            }
            "azirevpn" => {
                // v63.1: AzireVPN's "register key" actually goes through
                // POST /v3/ips — it returns the assigned IPv4/IPv6, the
                // DNS servers AzireVPN wants us to use, and an auto-
                // assigned device name. All three get persisted into
                // the state file so the wg.conf renderer doesn't need
                // hardcoded values.
                let alloc = azirevpn::register_key(&cred, &pub_key)?;
                let servers = azirevpn::fetch_servers(&cred, trust)?;
                let s = azirevpn::AzireState {
                    api_token: cred,
                    peer_ipv4: alloc.ipv4.clone(),
                    peer_ipv6: alloc.ipv6,
                    dns_servers: alloc.dns,
                    device_name: alloc.device_name,
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
            "airvpn" => {
                // v77: AirVPN setup just stores the API key + fetches the
                // server list (which also validates the key). Per-mode configs
                // are pulled on demand from AirVPN's generator via the separate
                // /generate endpoint (no manual paste) — see airvpn::generate_package.
                let _ = (&av_wg_config, &av_ovpn_config); // legacy paste fields, unused now
                let mut s = airvpn::read_state();
                if let Some(k) = av_api_key.as_deref() {
                    let k = k.trim();
                    if !k.is_empty() { s.api_key = k.to_string(); }
                }
                if s.api_key.is_empty() {
                    return Err("AirVPN needs an API key (member area → Client Area → API)".into());
                }
                if let Some(m) = av_mode.as_deref() {
                    let m = m.trim();
                    if !m.is_empty() { s.mode = m.to_string(); }
                }
                let servers = airvpn::fetch_servers(&s.api_key, &s.wg_public_key, s.wg_port, trust)?;
                if !servers.iter().any(|sv| sv.id == s.selected_server) {
                    s.selected_server = String::new();
                }
                s.servers = servers.clone();
                s.servers_updated_ms = now_ms();
                if s.selection_mode.is_empty() { s.selection_mode = "manual".into(); }
                airvpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(json!({
                    "ok": true,
                    "server_count": servers.len(),
                    "mode": s.mode,
                }))
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
        "airvpn" => {
            let s = airvpn::read_state();
            // v77: "configured" = key present AND the ACTIVE mode has a
            // generated package. `generated` lists every mode that's been
            // auto-pulled (so the UI can show per-mode readiness). Secrets are
            // never echoed — only presence + the non-secret generated metadata.
            let configured = !s.api_key.is_empty() && s.generated.contains_key(&s.mode);
            json!({
                "ok": true,
                "configured": configured,
                "mode": s.mode,
                "has_api_key": !s.api_key.is_empty(),
                "generated": s.generated,
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
        "airvpn" => {
            let mut s = airvpn::read_state();
            if let Some(id) = req.server_id {
                if !s.servers.iter().any(|sv| sv.id == id) {
                    return (StatusCode::BAD_REQUEST,
                        Json(json!({"ok": false, "err": format!("server '{id}' not in cache")}))
                    ).into_response();
                }
                s.selected_server = id;
            }
            if let Some(m) = req.mode { s.selection_mode = m; }
            if let Err(e) = airvpn::write_state(&s) {
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

/// Query params for pick-fastest. `eyes=none` restricts the probe pool to
/// servers OUTSIDE the 5/9/14-Eyes alliances ("No Eyes" / privacy-max).
#[derive(Deserialize, Default)]
pub struct PickFastestParams {
    pub eyes: Option<String>,
}

pub async fn pick_fastest(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<PickFastestParams>,
) -> impl IntoResponse {
    if provider_meta(&provider).is_none() {
        return unknown_provider(&provider);
    }
    let mut servers: Vec<crate::vpn_providers::Server> = match provider.as_str() {
        "mullvad" => mullvad::read_state().servers,
        "ivpn"    => ivpn::read_state().servers,
        "azirevpn" => azirevpn::read_state().servers,
        "airvpn"  => airvpn::read_state().servers,
        _ => return unknown_provider(&provider),
    };
    if servers.is_empty() {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "no servers in cache — finish setup first"}))).into_response();
    }
    // v76: "No Eyes" filter — restrict the latency probe to servers in
    // countries OUTSIDE the 5/9/14-Eyes intelligence-sharing alliances.
    let no_eyes = params.eyes.as_deref() == Some("none");
    if no_eyes {
        servers.retain(|s| s.eyes == EyesTier::None);
        if servers.is_empty() {
            return Json(json!({
                "ok": true, "no_eyes": true, "ranking": [],
                "note": "this provider has no servers outside the 14-Eyes alliances"
            })).into_response();
        }
    }
    // ICMP-ping each endpoint, concurrently. WireGuard endpoints are UDP-only,
    // so the previous TCP connect to endpoint_port (2049) ALWAYS failed and
    // EVERY server came back "unreachable". ICMP to the host IP is the right
    // latency proxy — same network path, and VPN servers answer it. Parallel
    // (JoinSet) so the whole 80–200 server list finishes in ~1s.
    let mut set = tokio::task::JoinSet::new();
    for s in servers {
        set.spawn(async move {
            let rtt_ms = ping_rtt_ms(&s.endpoint_ip).await;
            json!({
                "id": s.id,
                "label": s.label,
                "country": s.country,
                "city": s.city,
                "rtt_ms": rtt_ms,
                "server_score": s.server_score,
                "eyes": s.eyes.as_str(),
            })
        });
    }
    let mut v = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(entry) = res {
            v.push(entry);
        }
    }
    // Sort: reachable first (lowest rtt_ms), unreachable (None → MAX) last.
    v.sort_by_key(|e| e.get("rtt_ms").and_then(|x| x.as_u64()).unwrap_or(u64::MAX));
    Json(json!({"ok": true, "no_eyes": no_eyes, "ranking": v})).into_response()
}

/// ICMP round-trip to `ip` (1 packet, 1s deadline) in ms, or None if the host
/// didn't answer. Uses the `ping` binary (setcap'd on Pi OS; the supervisor
/// runs as root regardless) to avoid pulling in a raw-socket dependency.
async fn ping_rtt_ms(ip: &str) -> Option<u32> {
    let out = tokio::process::Command::new("ping")
        .args(["-n", "-c", "1", "-W", "1", ip])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Parse the "time=12.3 ms" token from the reply line.
    let s = String::from_utf8_lossy(&out.stdout);
    let after = s.split("time=").nth(1)?;
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse::<f32>().ok().map(|f| f.round() as u32)
}

// ── POST /api/network/vpn/providers/:id/refresh ─────────────────────
//
// Re-fetch the provider's server list and update the cache WITHOUT
// re-registering a device/session. The server lists come from cheap
// endpoints — public for IVPN (servers.json) and Mullvad (relays),
// token-authed (but device-free) for AzireVPN (/v3/locations). Requires
// the provider to already be configured (we reuse the stored creds).
// This is what the wizard's "refresh server list" button calls, so the
// user doesn't have to re-run the whole setup just to pick up the
// provider's latest servers.
pub async fn refresh_servers(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
) -> impl IntoResponse {
    let meta = match provider_meta(&provider) {
        Some(m) => m,
        None => return unknown_provider(&provider),
    };
    let trust = meta.trust_score;
    let pid = provider.clone();
    let blocking = tokio::task::spawn_blocking(move || -> Result<usize, String> {
        match pid.as_str() {
            "mullvad" => {
                let mut s = mullvad::read_state();
                if s.account_number.is_empty() || s.device_id.is_empty() {
                    return Err("not configured — run setup first".into());
                }
                let servers = mullvad::fetch_relays(trust)?;
                if !servers.iter().any(|sv| sv.id == s.selected_server) {
                    s.selected_server = String::new();
                }
                let n = servers.len();
                s.servers = servers;
                s.servers_updated_ms = now_ms();
                mullvad::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(n)
            }
            "ivpn" => {
                let mut s = ivpn::read_state();
                if s.account_id.is_empty() || s.session_token.is_empty() {
                    return Err("not configured — run setup first".into());
                }
                let servers = ivpn::fetch_servers(trust)?;
                if !servers.iter().any(|sv| sv.id == s.selected_server) {
                    s.selected_server = String::new();
                }
                let n = servers.len();
                s.servers = servers;
                s.servers_updated_ms = now_ms();
                ivpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(n)
            }
            "azirevpn" => {
                let mut s = azirevpn::read_state();
                if s.api_token.is_empty() {
                    return Err("not configured — run setup first".into());
                }
                let servers = azirevpn::fetch_servers(&s.api_token, trust)?;
                if !servers.iter().any(|sv| sv.id == s.selected_server) {
                    s.selected_server = String::new();
                }
                let n = servers.len();
                s.servers = servers;
                s.servers_updated_ms = now_ms();
                azirevpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(n)
            }
            "airvpn" => {
                let mut s = airvpn::read_state();
                if s.api_key.is_empty() {
                    return Err("not configured — run setup first".into());
                }
                let servers = airvpn::fetch_servers(&s.api_key, &s.wg_public_key, s.wg_port, trust)?;
                if !servers.iter().any(|sv| sv.id == s.selected_server) {
                    s.selected_server = String::new();
                }
                let n = servers.len();
                s.servers = servers;
                s.servers_updated_ms = now_ms();
                airvpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
                Ok(n)
            }
            _ => Err(format!("unknown provider '{pid}'")),
        }
    })
    .await;
    match blocking {
        Ok(Ok(n)) => {
            crate::audit::log("admin (session)", "vpn_provider_refresh",
                &format!("provider={provider}"), "ok", None);
            Json(json!({"ok": true, "server_count": n})).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": e}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("join: {e}")}))).into_response(),
    }
}

// ── POST /api/network/vpn/providers/airvpn/generate ─────────────────
//
// v77: AirVPN-only. Auto-pull the ready-to-run config package for a given
// (server, mode) straight from AirVPN's generator (download=zip) and store it
// under /etc/aeon/vpn-secrets/airvpn/<mode>/. Each mode is generated + stored
// independently, so the user can have WireGuard + OpenVPN + SSL + SSH all set
// up and switch freely. No manual paste; SSL/SSH ship with AirVPN's genuine
// stunnel/ssh configs + keys, so net-services runs them verbatim.

#[derive(Deserialize)]
pub struct GenerateReq {
    /// "wireguard" | "openvpn" | "openvpn_ssl" | "openvpn_ssh".
    pub mode: String,
    /// Server public_name. Defaults to the currently-selected server.
    #[serde(default)]
    pub server_id: Option<String>,
}

pub async fn generate(
    State(_state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<GenerateReq>,
) -> impl IntoResponse {
    if provider != "airvpn" {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "config generation is AirVPN-only"}))).into_response();
    }
    let mode = req.mode.trim().to_string();
    let mode_for_log = mode.clone();
    let server_req = req.server_id.clone();
    let blocking = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut s = airvpn::read_state();
        if s.api_key.is_empty() {
            return Err("not configured — add your AirVPN API key first".into());
        }
        // Use the requested server, else the currently-selected one.
        let server_id = server_req
            .as_deref()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .or_else(|| (!s.selected_server.is_empty()).then(|| s.selected_server.clone()))
            .ok_or_else(|| "no server selected — pick one first".to_string())?;
        let label = s.servers.iter()
            .find(|sv| sv.id == server_id)
            .map(|sv| sv.label.clone())
            .ok_or_else(|| format!("server '{server_id}' not in cache — refresh the server list"))?;

        let files = airvpn::generate_package(&s.api_key, &server_id, &mode)?;
        let names = airvpn::store_package(&mode, &files).map_err(|e| format!("store package: {e}"))?;

        s.generated.insert(mode.clone(), airvpn::GeneratedInfo {
            server: server_id.clone(),
            server_label: label,
            generated_ms: now_ms(),
            files: names.clone(),
        });
        // Make the just-generated mode + server active.
        s.mode = mode.clone();
        s.selected_server = server_id.clone();
        airvpn::write_state(&s).map_err(|e| format!("persist: {e}"))?;
        Ok(json!({"ok": true, "mode": mode, "server": server_id, "files": names}))
    }).await;

    match blocking {
        Ok(Ok(v)) => {
            crate::audit::log("admin (session)", "vpn_airvpn_generate",
                &format!("mode={mode_for_log}"), "ok", None);
            Json(v).into_response()
        }
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": e}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("join: {e}")}))).into_response(),
    }
}
