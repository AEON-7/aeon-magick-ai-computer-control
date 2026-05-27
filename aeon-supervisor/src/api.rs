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
        .route("/streamer/relaunch", post(crate::proxy::streamer_relaunch))
        // HID
        .route("/hid/status", get(crate::proxy::hid_status))
        .route("/hid/type", post(crate::proxy::hid_type))
        .route("/hid/key", post(crate::proxy::hid_key))
        .route("/hid/click", post(crate::proxy::hid_click))
        .route("/hid/move", post(crate::proxy::hid_move))
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
        // WiFi management — scan, connect, current state. Used by the
        // /setup-wifi captive-portal page during AP-fallback mode and
        // by the authenticated WiFi panel for ongoing management.
        .route("/wifi/scan", get(crate::wifi::scan))
        .route("/wifi/connect", post(crate::wifi::connect))
        .route("/wifi/state", get(crate::wifi::state))
        .route("/wifi/disconnect", post(crate::wifi::disconnect))
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

    next.run(req).await
}
