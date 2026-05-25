//! Top-level router for the supervisor's HTTPS endpoint. Combines:
//!   - Static SvelteKit asset serving (under /)
//!   - JSON REST API (under /api/...)
//!   - Streamer proxy (snapshot/stream/state)
//!   - HID proxy (logical input ops)

use anyhow::{Context, Result};
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:443".into(),
            web_root: PathBuf::from("/usr/share/acursed/web"),
            streamer_sock: PathBuf::from("/run/acursed/streamer.sock"),
            hid_sock: PathBuf::from("/run/acursed/hid.sock"),
            cert_path: PathBuf::from("/etc/acursed/cert.pem"),
            key_path: PathBuf::from("/etc/acursed/key.pem"),
            auth_path: PathBuf::from("/etc/acursed/auth.toml"),
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
}

pub fn build_router(cfg: Config) -> Router {
    let auth = crate::auth::AuthStore::load(&cfg.auth_path);
    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        auth: Arc::new(auth),
    };

    let api = Router::new()
        // System
        .route("/state", get(crate::proxy::supervisor_state))
        .route("/login", post(crate::auth::login))
        .route("/logout", post(crate::auth::logout))
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
        .route("/hid/release_all", post(crate::proxy::hid_release_all));

    let web_root = cfg.web_root.clone();
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(&web_root))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
