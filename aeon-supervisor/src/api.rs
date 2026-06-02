//! Top-level router for the supervisor's HTTPS endpoint. Combines:
//!   - Static SvelteKit asset serving (under /)
//!   - JSON REST API (under /api/...)
//!   - Streamer proxy (snapshot/stream/state)
//!   - HID proxy (logical input ops)

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub listen: String,
    pub web_root: PathBuf,
    pub streamer_sock: PathBuf,
    pub hid_sock: PathBuf,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub auth_path: PathBuf,
    pub tokens_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:443".into(),
            web_root: PathBuf::from("/usr/share/aeon/web"),
            streamer_sock: PathBuf::from("/run/aeon/streamer.sock"),
            hid_sock: PathBuf::from("/run/aeon/hid.sock"),
            cert_path: PathBuf::from("/etc/aeon/cert.pem"),
            key_path: PathBuf::from("/etc/aeon/key.pem"),
            auth_path: PathBuf::from("/etc/aeon/auth.toml"),
            tokens_path: PathBuf::from("/etc/aeon/tokens.toml"),
        }
    }
}

impl Config {
    pub fn load(path: PathBuf) -> Result<Self> {
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Ok(toml::from_str(&text)?)
        } else {
            tracing::warn!(path = %path.display(), "config missing, using defaults");
            Ok(Self::default())
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub auth: Arc<crate::auth::AuthStore>,
    pub stores: Arc<crate::macros::Stores>,
    /// Long-lived hyper-util Client for proxying to the streamer/hid
    /// unix sockets. Has to live as long as any streaming response
    /// body — when the Client is dropped, its connection pool drops
    /// with it, killing in-flight long-lived streams like /api/streamer/stream.
    /// Sharing one Client across all requests is also the right design
    /// per axum/hyper conventions.
    pub uds_client_empty: Arc<
        hyper_util::client::legacy::Client<
            hyperlocal::UnixConnector,
            http_body_util::Empty<bytes::Bytes>,
        >,
    >,
    pub uds_client_full: Arc<
        hyper_util::client::legacy::Client<
            hyperlocal::UnixConnector,
            http_body_util::Full<bytes::Bytes>,
        >,
    >,
}

pub fn build_router(cfg: Config) -> Router {
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let auth = crate::auth::AuthStore::load(&cfg.auth_path, &cfg.tokens_path);
    let stores = crate::macros::Stores::default();
    stores.ensure_dirs();
    let uds_client_empty: Client<_, http_body_util::Empty<bytes::Bytes>> =
        Client::builder(TokioExecutor::new()).build(hyperlocal::UnixConnector);
    let uds_client_full: Client<_, http_body_util::Full<bytes::Bytes>> =
        Client::builder(TokioExecutor::new()).build(hyperlocal::UnixConnector);
    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        auth: Arc::new(auth),
        stores: Arc::new(stores),
        uds_client_empty: Arc::new(uds_client_empty),
        uds_client_full: Arc::new(uds_client_full),
    };

    let api = Router::new()
        // ── Always-open (no auth required) ──
        // /api/auth/me lets the SPA discover state (open / locked / authed).
        .route("/auth/me", get(crate::auth::me))
        .route("/login", post(crate::auth::login))
        .route("/logout", post(crate::auth::logout))
        // Setup wizard (only works in Open state; the handler checks state)
        .route("/setup/password", post(crate::auth::setup_password))
        // ── Authenticated below ──
        // System
        .route("/state", get(crate::proxy::supervisor_state))
        // Streamer
        .route("/streamer/state", get(crate::proxy::streamer_state))
        .route("/streamer/snapshot", get(crate::proxy::streamer_snapshot))
        .route("/streamer/stream", get(crate::proxy::streamer_stream))
        // v64: H.264 low-latency live view over WebSocket (WebCodecs client).
        .route("/streamer/ws", get(crate::proxy::streamer_ws))
        .route("/streamer/relaunch", post(crate::proxy::streamer_relaunch))
        // P1: on-demand screen recording (proxied to the streamer's record API).
        .route("/streamer/record/start", post(crate::proxy::record_start))
        .route("/streamer/record/stop", post(crate::proxy::record_stop))
        .route("/streamer/record/state", get(crate::proxy::record_state))
        .route("/streamer/recordings", get(crate::proxy::list_recordings))
        .route("/streamer/recordings/:id",
            get(crate::proxy::get_recording).delete(crate::proxy::delete_recording))
        .route("/streamer/recordings/:id/thumb", get(crate::proxy::get_recording_thumb))
        // Agent Dash — Connected Systems registry + SSH key provisioning.
        .route("/agent/pubkey", get(crate::agent_connect::get_pubkey))
        .route("/agent/systems",
            get(crate::agent_connect::list_systems).post(crate::agent_connect::add_system))
        .route("/agent/systems/:id", axum::routing::delete(crate::agent_connect::remove_system))
        .route("/agent/systems/:id/register", post(crate::agent_connect::register_system))
        .route("/agent/systems/:id/test", post(crate::agent_connect::test_system))
        .route("/agent/systems/:id/metrics", get(crate::agent_connect::system_metrics))
        .route("/agent/systems/:id/power", post(crate::agent_connect::power_system))
        .route("/agent/systems/:id/agents", get(crate::agent_connect::system_agents))
        .route("/agent/systems/:id/usage", get(crate::agent_connect::system_usage))
        .route("/agent/systems/:id/agents/:aid/detail", get(crate::agent_connect::agent_detail))
        .route(
            "/agent/systems/:id/agents/:aid/provision",
            post(crate::agent_connect::provision_agent).delete(crate::agent_connect::deprovision_agent),
        )
        .route(
            "/agent/systems/:id/agents/:aid/ssh",
            post(crate::agent_connect::grant_ssh)
                .patch(crate::agent_connect::toggle_ssh_admin)
                .delete(crate::agent_connect::revoke_ssh),
        )
        // E1: per-agent profile photo → Matrix avatar (upload to the gateway's
        // Dendrite media repo + set avatar_url). POST body is base64 image so
        // raise the default 2MB body limit for the JSON payload.
        .route(
            "/agent/systems/:id/agents/:aid/avatar",
            get(crate::agent_connect::agent_avatar_get)
                .post(crate::agent_connect::agent_avatar_set)
                .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        // v63: live streamer tuning. fps + jpeg_quality are tunable
        // from the /system page so operators can dial in latency vs
        // smoothness without ssh'ing.
        .route("/streamer/config",
            get(crate::streamer_config::get_config)
                .put(crate::streamer_config::put_config))
        // HID
        .route("/hid/status", get(crate::proxy::hid_status))
        .route("/hid/type", post(crate::proxy::hid_type))
        .route("/hid/key", post(crate::proxy::hid_key))
        .route("/hid/click", post(crate::proxy::hid_click))
        .route("/hid/button", post(crate::proxy::hid_button))
        .route("/hid/move", post(crate::proxy::hid_move))
        .route("/hid/move_abs", post(crate::proxy::hid_move_abs))
        .route("/hid/scroll", post(crate::proxy::hid_scroll))
        .route("/hid/persona", post(crate::proxy::hid_persona))
        .route("/hid/release_all", post(crate::proxy::hid_release_all))
        // Macros (named action sequences)
        .route("/macros", get(crate::macros::list_macros))
        .route(
            "/macros/:name",
            get(crate::macros::get_macro)
                .put(crate::macros::put_macro)
                .delete(crate::macros::delete_macro),
        )
        .route("/macros/:name/run", post(crate::macros::run_macro))
        // Prompts (agent playbooks)
        .route("/prompts", get(crate::macros::list_prompts))
        .route(
            "/prompts/:name",
            get(crate::macros::get_prompt)
                .put(crate::macros::put_prompt)
                .delete(crate::macros::delete_prompt),
        )
        // Token management (admin-scope only — enforced in auth middleware)
        .route(
            "/auth/tokens",
            get(crate::auth::list_tokens).post(crate::auth::create_token),
        )
        .route("/auth/tokens/:id", axum::routing::delete(crate::auth::revoke_token))
        .route("/auth/change-password", post(crate::auth::change_password))
        // USB ethernet passthrough state
        .route(
            "/network/usb",
            get(crate::network::get_state).put(crate::network::put_state),
        )
        // Encrypted DNS (DNSCrypt v2 / DoH)
        .route(
            "/network/dnscrypt",
            get(crate::network::get_dnscrypt).put(crate::network::put_dnscrypt),
        )
        // VPN (Tailscale / WireGuard / OpenVPN / Tor / I2P)
        .route(
            "/network/vpn",
            get(crate::network::get_vpn).put(crate::network::put_vpn),
        )
        // Live VPN status — polled by the web UI every few seconds while
        // the VPN section is visible. Returns bootstrap %, peer/circuit
        // list, public IP + country.
        .route("/network/vpn/status", get(crate::network::get_vpn_status))
        // Identity rotation — Tor SIGNAL NEWNYM, Tailscale reset, WG/OVPN
        // reconnect. POST with no body.
        .route("/network/vpn/rotate", post(crate::network::post_vpn_rotate))
        // v57: I2P runtime status — installed/running/bind addresses/
        // browser proxy URL. Read-only; outproxy + enable still flow
        // through /network/vpn since I2P is a VPN provider variant.
        .route("/network/i2p/status", get(crate::i2p::get_status))

        // v59: per-provider VPN wizards (Mullvad / IVPN / AzireVPN).
        // Each runs the provider's REST API: validate account/token,
        // generate WG keypair on Pi, register pubkey with provider,
        // fetch server list, render wg-quick config when user picks
        // a server. Existing vpn.provider=wireguard flow stays for
        // custom configs.
        .route("/network/vpn/providers/catalog",
            get(crate::vpn_providers::api::get_catalog))
        .route("/network/vpn/providers/:id/setup",
            post(crate::vpn_providers::api::setup))
        .route("/network/vpn/providers/:id/state",
            get(crate::vpn_providers::api::get_state))
        .route("/network/vpn/providers/:id/select",
            post(crate::vpn_providers::api::select))
        .route("/network/vpn/providers/:id/pick-fastest",
            post(crate::vpn_providers::api::pick_fastest))
        .route("/network/vpn/providers/:id/refresh",
            post(crate::vpn_providers::api::refresh_servers))
        // v77: AirVPN-only — auto-pull a mode's config package from AirVPN's
        // generator (download=zip) and store it per-mode. No manual paste.
        .route("/network/vpn/providers/:id/generate",
            post(crate::vpn_providers::api::generate))
        // WiFi management — scan, connect, current state. Used by the
        // /setup-wifi captive-portal page during AP-fallback mode and
        // by the authenticated /wifi panel for ongoing management.
        .route("/wifi/scan", get(crate::wifi::scan))
        .route("/wifi/connect", post(crate::wifi::connect))
        .route("/wifi/state", get(crate::wifi::state))
        .route("/wifi/disconnect", post(crate::wifi::disconnect))
        // v63: known-network management + AP-mode + radio toggle.
        .route("/wifi/known", get(crate::wifi::list_known)
            .delete(crate::wifi::forget))
        .route("/wifi/autoconnect", post(crate::wifi::set_autoconnect))
        .route("/wifi/ap", get(crate::wifi::ap_get)
            .put(crate::wifi::ap_set))
        .route("/wifi/radio", post(crate::wifi::radio))
        // Mass storage — manage ISOs uploaded for the USB-CDROM
        // gadget function. Upload endpoint streams to disk; max body
        // limit is raised below.
        .route("/storage", get(crate::storage::list))
        .route("/storage/active", axum::routing::put(crate::storage::put_active))
        .route("/storage/upload",
            post(crate::storage::upload)
                // Disable axum's default 2MB body limit — ISOs are GB-scale
                .layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/storage/:slug", axum::routing::delete(crate::storage::delete))
        // Firewall + NAT + port-forward rules. State is /etc/aeon/firewall.toml;
        // apply layer regenerates iptables-restore rules + executes.
        .route("/firewall/rules",
            get(crate::firewall::list_rules)
                .post(crate::firewall::add_rule))
        .route("/firewall/system-rules",
            get(crate::firewall::list_system_rules))
        .route("/firewall/rules/:id",
            axum::routing::delete(crate::firewall::delete_rule))
        .route("/firewall/rules/:id/move",
            post(crate::firewall::move_rule))
        // SSH key trust store (admin user's authorized_keys).
        .route("/ssh/keys",
            get(crate::ssh_keys::list_keys)
                .post(crate::ssh_keys::add_key))
        .route("/ssh/keys/:id",
            axum::routing::delete(crate::ssh_keys::remove_key))
        // DNS blacklist + query log.
        .route("/dns/log",
            get(crate::dns_log::get_log)
                .put(crate::dns_log::put_log))
        .route("/dns/blacklist",
            get(crate::dns_log::get_blacklist)
                .put(crate::dns_log::put_blacklist))
        .route("/dns/blacklist/import", post(crate::dns_log::import_csv))
        // Subscription sources for the blacklist (StevenBlack, OISD, etc.)
        .route("/dns/sources",
            get(crate::dns_log::list_sources)
                .post(crate::dns_log::add_source))
        .route("/dns/sources/:id",
            axum::routing::put(crate::dns_log::update_source)
                .delete(crate::dns_log::delete_source))
        .route("/dns/sources/:id/refresh", post(crate::dns_log::refresh_source))
        // Security console — throughput + blocked counters + top clients.
        .route("/security/metrics",
            get(crate::security_metrics::get_metrics))
        // Recent blocked packets — populated by the AEON_DROP chain's
        // LOG entries, parsed from the kernel journal on demand.
        .route("/security/blocked",
            get(crate::blocked_log::list))
        // Audit log — recent admin actions and authentication events.
        .route("/audit",
            get(crate::audit::list)
                .delete(crate::audit::clear))
        // System controls — Pi-side reboot / poweroff / health info. Admin scope.
        // v53: renamed reboot/poweroff to pi-reboot/pi-poweroff to disambiguate
        // from /api/target/* (which controls the USB-connected target machine).
        // The old paths stay as aliases for any external scripts that hit them.
        .route("/system/info",        get(crate::system::info))
        .route("/system/pi-reboot",   post(crate::system::reboot))
        .route("/system/pi-poweroff", post(crate::system::poweroff))
        .route("/system/reboot",      post(crate::system::reboot))
        .route("/system/poweroff",    post(crate::system::poweroff))

        // Target (USB-connected machine) power controls. Soft tap +
        // forced hold via HID Consumer Power button; wake via WoL
        // magic packet over usb0; reboot is hold + 5s + wake.
        .route("/target/info",        get(crate::target::get_info))
        .route("/target/config",      axum::routing::put(crate::target::put_config))
        .route("/target/power-tap",   post(crate::target::power_tap))
        .route("/target/power-hold",  post(crate::target::power_hold))
        .route("/target/wake",        post(crate::target::wake))
        .route("/target/reboot",      post(crate::target::reboot))
        // File transfer to/from target via /var/lib/aeon/files/.
        // Target-facing public server (off by default) spawned in main.rs.
        .route("/files",
            get(crate::file_xfer::list_files))
        .route("/files/config",
            get(crate::file_xfer::get_config)
                .put(crate::file_xfer::put_config))
        .route("/files/upload",
            post(crate::file_xfer::upload)
                .layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/files/:name",
            get(crate::file_xfer::download)
                .delete(crate::file_xfer::delete_file))
        // Shared clipboard — single text snippet stored on the Pi,
        // type-on-target button calls /api/hid/type with the buffer.
        .route("/clipboard",
            get(crate::clipboard::get_clipboard)
                .put(crate::clipboard::put_clipboard)
                .delete(crate::clipboard::delete_clipboard))
        .route("/clipboard/type-on-target",
            post(crate::clipboard::type_on_target))
        // MCP (Model Context Protocol) — Streamable HTTP transport
        .route("/mcp", post(crate::mcp::handle));

    // Apply the auth middleware to all /api routes. The middleware lets
    // /api/auth/me, /api/login, /api/logout, /api/setup/password through
    // without an identity (so the SPA can probe state + do setup), and
    // rejects everything else when the request doesn't carry a valid
    // session cookie / token / Basic auth.
    let api = api.route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth_middleware,
    ));

    // SPA-mode static serving with **200-status** fallback.
    //
    // tower-http's `ServeDir::not_found_service(...)` preserves the outer
    // 404 status even when serving index.html as the fallback body. Chrome
    // treats `404 + HTML` as an error and won't honor the SPA's
    // `history.pushState` navigation on hard reload, so the setup wizard
    // never gets a chance to bootstrap.
    //
    // Fix: serve SvelteKit's hashed asset directory `/_app` via ServeDir
    // (real files → 200 + correct MIME), and register an axum `fallback`
    // handler for everything else. The fallback reads index.html and
    // always returns `200 OK + text/html`. SvelteKit's history router then
    // takes over and renders /setup, /tokens, /login client-side.
    let web_root = cfg.web_root.clone();
    let app_assets = web_root.join("_app");

    Router::new()
        .nest("/api", api)
        // SvelteKit's hashed JS/CSS bundles — real files on disk.
        .nest_service("/_app", ServeDir::new(&app_assets))
        // Always-200 fallback for all SPA routes.
        .fallback(spa_fallback)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// SPA fallback handler: always returns `200 OK` with the body of
/// `<web_root>/index.html` so client-side routes (/setup, /tokens, …) load
/// cleanly on direct URL hits or hard-refresh. SvelteKit's history router
/// then takes over rendering.
async fn spa_fallback(State(state): State<AppState>) -> Response {
    use axum::http::header;
    use tokio::fs;

    let path = state.cfg.web_root.join("index.html");
    match fs::read(&path).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            }),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "web_root/index.html missing — is /usr/share/aeon/web populated?",
        )
            .into_response(),
    }
}

/// The single chokepoint that enforces authentication for /api/*.
///
/// Allow-list of unauthenticated endpoints:
///   GET  /api/auth/me            (SPA needs this to know if it should
///                                  show login vs setup vs main UI)
///   POST /api/login              (Basic auth check, sets cookie)
///   POST /api/logout             (clears cookie)
///   POST /api/setup/password     (only valid while state=Open — the
///                                  handler itself enforces that)
///
/// Everything else demands an authenticated identity:
///   - cookie `aeon_session=<hmac-signed>`
///   - `Authorization: Bearer aeon_tok_<...>`
///   - `Authorization: Basic <base64(admin:password)>`
///   - `X-Aeon-Token: aeon_tok_<...>`
///
/// Token scopes are then enforced against the request method + path.
async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let method = req.method().clone();

    // 1. Public allow-list — let these through unconditionally.
    if path == "/auth/me"
        || path == "/login"
        || path == "/logout"
        || path == "/setup/password"
    {
        return next.run(req).await;
    }

    // 1b. WiFi endpoints are public in Open state ONLY. During first-
    //     boot from the captive portal, the user hasn't set a password
    //     yet, so they need to be able to configure WiFi before they
    //     even reach the password-setup wizard. Once the device is
    //     Locked (a password has been set), WiFi management requires
    //     auth like everything else.
    if state.auth.is_open() && path.starts_with("/wifi/") {
        return next.run(req).await;
    }

    // 2. In Open state, nothing else works — caller must finish setup first.
    if state.auth.is_open() {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "ok": false,
                "err": "device not yet configured — POST /api/setup/password first",
            })),
        )
            .into_response();
    }

    // 3. Identify the caller.
    let Some(identity) = crate::auth::identify(&state.auth, req.headers()) else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "ok": false,
                "err": "authentication required",
            })),
        )
            .into_response();
    };

    // 4. Enforce scope. The middleware sees the path WITHOUT the /api prefix
    // because we're mounted under nest("/api", ...). Re-add it for scope_allows
    // so the rules can refer to the canonical /api/auth/tokens etc.
    let full_path = format!("/api{}", path);
    if !crate::auth::scope_allows(&identity, &method, &full_path) {
        crate::audit::log(
            &crate::audit::actor_for(&identity),
            "scope_denied",
            &format!("{} {}", method, full_path),
            "fail",
            Some("scope insufficient"),
        );
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "ok": false,
                "err": format!("scope `{}` not permitted for {} {}",
                    identity.scope.as_str(), method, full_path),
            })),
        )
            .into_response();
    }

    // 5. Decide if this request should land in the audit log. Major
    // mutations only — never GETs, never the chatty HID surface
    // (/hid/click, /hid/move, /hid/type, /hid/scroll, /hid/key,
    // /hid/release_all), never the snapshot/stream feed. Things we DO
    // audit: any non-GET on /network, /storage, /firewall, /ssh,
    // /dns, /wifi, plus persona swaps.
    let auditable_action = if method == axum::http::Method::GET {
        None
    } else if path.starts_with("/network/")
        || path.starts_with("/storage")
        || path.starts_with("/firewall/")
        || path.starts_with("/ssh/")
        || path.starts_with("/dns/")
        || path.starts_with("/wifi/")
    {
        // Construct a stable action label from method + path leaf.
        // e.g. "PUT /api/network/vpn" → "network_vpn_set"
        let leaf = full_path.trim_start_matches("/api/").replace('/', "_");
        Some(format!("{}_{}", method_verb(&method), leaf))
    } else if path == "/hid/persona" && method == axum::http::Method::POST {
        Some("hid_persona_set".to_string())
    } else {
        None
    };

    let actor = crate::audit::actor_for(&identity);
    let detail = format!("{} {}", method, full_path);

    let resp = next.run(req).await;

    if let Some(action) = auditable_action {
        let status = resp.status();
        let (result, err) = if status.is_success() {
            ("ok", None)
        } else {
            ("fail", Some(format!("HTTP {}", status.as_u16())))
        };
        crate::audit::log(&actor, &action, &detail, result, err.as_deref());
    }

    resp
}

/// Map HTTP verb to an audit-action prefix. PUT/POST → "set", DELETE →
/// "delete", POST → "set" / "create" depending on path — we pick the
/// generic "set" for PUT, "create" for POST.
fn method_verb(m: &axum::http::Method) -> &'static str {
    match *m {
        axum::http::Method::PUT => "set",
        axum::http::Method::POST => "set",
        axum::http::Method::DELETE => "delete",
        _ => "call",
    }
}
