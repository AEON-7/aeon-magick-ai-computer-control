//! HTTP API the supervisor (and dev tools) talk to over a unix socket.
//!
//! Endpoints:
//!   GET  /state          → JSON: mode, online, fps, relaunch_count, etc.
//!   POST /relaunch       → force ustreamer relaunch
//!   GET  /snapshot       → JPEG, proxied from ustreamer's /snapshot
//!   GET  /stream         → MJPEG, proxied from ustreamer's /stream

use crate::state::SharedState;
use anyhow::Result;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixListener;
use tracing::info;

pub async fn serve(state: SharedState) -> Result<()> {
    let sock_path = state.0.cfg.api_sock.clone();
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o660)).ok();

    info!(sock = %sock_path.display(), "streamer API listening");

    let app = Router::new()
        .route("/state", get(get_state))
        .route("/relaunch", post(force_relaunch))
        .route("/snapshot", get(snapshot_proxy))
        .route("/stream", get(stream_proxy))
        .with_state(state.clone());

    let shutdown = async move {
        state.0.shutdown_signal.notified().await;
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn get_state(State(state): State<SharedState>) -> impl IntoResponse {
    let s = state.read();
    Json(json!({
        "ok": true,
        "mode": s.mode.map(|m| json!({"format": m.format, "resolution": m.resolution})),
        "enum_hash": s.enum_hash,
        "online": s.online,
        "captured_fps": s.captured_fps,
        "last_relaunch_ms": s.last_relaunch.map(|t| t.epoch_ms),
        "relaunch_count": s.relaunch_count,
    }))
}

async fn force_relaunch(State(state): State<SharedState>) -> impl IntoResponse {
    state.signal_relaunch("api /relaunch");
    Json(json!({"ok": true, "kicked": true}))
}

async fn snapshot_proxy(State(state): State<SharedState>) -> Response<Body> {
    proxy_to_ustreamer(&state, "/snapshot").await
}

async fn stream_proxy(State(state): State<SharedState>) -> Response<Body> {
    proxy_to_ustreamer(&state, "/stream").await
}

async fn proxy_to_ustreamer(state: &SharedState, path: &str) -> Response<Body> {
    use http_body_util::Empty;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let sock = &state.0.cfg.ustreamer_sock;
    if !sock.exists() {
        return (StatusCode::SERVICE_UNAVAILABLE, "streamer socket missing").into_response();
    }

    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new()).build::<_, Empty<bytes::Bytes>>(connector);
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let req = match hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())
    {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "request build").into_response(),
    };

    match client.request(req).await {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            let mut builder = Response::builder().status(parts.status);
            for (k, v) in parts.headers.iter() {
                builder = builder.header(k, v);
            }
            builder.body(Body::new(body)).unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            })
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "ustreamer unreachable").into_response(),
    }
}
