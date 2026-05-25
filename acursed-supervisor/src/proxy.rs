//! Thin proxies that translate `/api/streamer/*` → streamer unix socket
//! and `/api/hid/*` → hid unix socket.

use crate::api::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use http_body_util::BodyExt;
use http_body_util::Empty;
use http_body_util::Full;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;
use std::path::Path;

async fn proxy(
    sock: &Path,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Response<Body> {
    if !sock.exists() {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon socket missing").into_response();
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let connector = hyperlocal::UnixConnector;

    let mut req_builder = hyper::Request::builder().method(method).uri(uri);
    if body.is_some() {
        req_builder = req_builder.header("Content-Type", "application/json");
    }

    let req_result = match body {
        Some(b) => {
            let client = Client::builder(TokioExecutor::new())
                .build::<_, Full<bytes::Bytes>>(connector);
            client
                .request(req_builder.body(Full::new(b.into())).unwrap())
                .await
        }
        None => {
            let client = Client::builder(TokioExecutor::new())
                .build::<_, Empty<bytes::Bytes>>(connector);
            client.request(req_builder.body(Empty::new()).unwrap()).await
        }
    };

    match req_result {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            let mut b = Response::builder().status(parts.status);
            for (k, v) in parts.headers.iter() {
                b = b.header(k, v);
            }
            b.body(Body::new(body)).unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            })
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "daemon unreachable").into_response(),
    }
}

// ── unified system status (combines streamer + hid) ─────────────────────

pub async fn supervisor_state(State(state): State<AppState>) -> Response<Body> {
    let streamer = fetch_json(&state.cfg.streamer_sock, "/state").await;
    let hid = fetch_json(&state.cfg.hid_sock, "/status").await;
    let combined = serde_json::json!({
        "ok": true,
        "streamer": streamer,
        "hid": hid,
    });
    Json(combined).into_response()
}

async fn fetch_json(sock: &Path, path: &str) -> Value {
    if !sock.exists() {
        return Value::Null;
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new())
        .build::<_, Empty<bytes::Bytes>>(connector);
    let Ok(req) = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())
    else {
        return Value::Null;
    };
    let Ok(resp) = client.request(req).await else {
        return Value::Null;
    };
    let Ok(bytes) = resp.into_body().collect().await else {
        return Value::Null;
    };
    serde_json::from_slice(&bytes.to_bytes()).unwrap_or(Value::Null)
}

// ── streamer endpoints ──────────────────────────────────────────────────

pub async fn streamer_state(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.streamer_sock, Method::GET, "/state", None).await
}
pub async fn streamer_snapshot(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.streamer_sock, Method::GET, "/snapshot", None).await
}
pub async fn streamer_stream(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.streamer_sock, Method::GET, "/stream", None).await
}
pub async fn streamer_relaunch(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.streamer_sock, Method::POST, "/relaunch", Some(vec![])).await
}

// ── HID endpoints ───────────────────────────────────────────────────────

pub async fn hid_status(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::GET, "/status", None).await
}
pub async fn hid_type(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/type", Some(body.to_vec())).await
}
pub async fn hid_key(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/key", Some(body.to_vec())).await
}
pub async fn hid_click(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/click", Some(body.to_vec())).await
}
pub async fn hid_move(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/move", Some(body.to_vec())).await
}
pub async fn hid_scroll(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/scroll", Some(body.to_vec())).await
}
pub async fn hid_release_all(State(state): State<AppState>) -> Response<Body> {
    proxy(&state.cfg.hid_sock, Method::POST, "/release_all", Some(vec![])).await
}
