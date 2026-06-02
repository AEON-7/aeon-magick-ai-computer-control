//! Thin proxies that translate `/api/streamer/*` → streamer unix socket
//! and `/api/hid/*` → hid unix socket.

use crate::api::AppState;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

async fn proxy(
    state: &AppState,
    sock: &Path,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Response<Body> {
    if !sock.exists() {
        return (StatusCode::SERVICE_UNAVAILABLE, "daemon socket missing").into_response();
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();

    let mut req_builder = hyper::Request::builder().method(method).uri(uri);
    if body.is_some() {
        req_builder = req_builder.header("Content-Type", "application/json");
    }

    // Use long-lived AppState clients — see api.rs for the rationale.
    // Per-request Clients drop their connection pool when the function
    // returns, killing in-flight streaming response bodies a few seconds
    // later (after the buffered head of the stream drains). Most
    // visible on /api/streamer/stream which is a long-lived multipart
    // MJPEG.
    let req_result = match body {
        Some(b) => {
            state
                .uds_client_full
                .request(req_builder.body(Full::new(b.into())).unwrap())
                .await
        }
        None => {
            state
                .uds_client_empty
                .request(req_builder.body(Empty::new()).unwrap())
                .await
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
    proxy(&state, &state.cfg.streamer_sock, Method::GET, "/state", None).await
}
pub async fn streamer_snapshot(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, "/snapshot", None).await
}
pub async fn streamer_stream(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, "/stream", None).await
}
pub async fn streamer_relaunch(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::POST, "/relaunch", Some(vec![])).await
}

// ── Screen recording (proxied to the streamer's record API) ─────────────────
pub async fn record_start(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::POST, "/record/start", Some(body.to_vec())).await
}
pub async fn record_stop(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::POST, "/record/stop", Some(vec![])).await
}
pub async fn record_state(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, "/record/state", None).await
}
pub async fn list_recordings(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, "/recordings", None).await
}
pub async fn get_recording(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, &format!("/recordings/{id}"), None).await
}
pub async fn delete_recording(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::DELETE, &format!("/recordings/{id}"), None).await
}
pub async fn get_recording_thumb(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response<Body> {
    proxy(&state, &state.cfg.streamer_sock, Method::GET, &format!("/recordings/{id}/thumb"), None).await
}

// ── v64: H.264 low-latency WebSocket bridge ─────────────────────────────
//
// Browser (WebCodecs) ⇄ WSS /api/streamer/ws ⇄ streamer unix-socket /h264.
// The streamer frames each access unit as [1B flags][4B BE len][AU bytes];
// we re-emit each AU as ONE binary WS message = [1B flags] ++ Annex-B AU
// (the WebSocket frame boundary replaces the length prefix). flags bit0 = key.
//
// Auth: the upgrade is a GET behind the same auth_middleware as every /api
// route. Browsers can't set Authorization headers on a WebSocket, but the
// same-origin `aeon_session` cookie rides along automatically — that's how
// the web UI authenticates. (Token-in-query for headless agents is a future
// add; agents use /snapshot over plain HTTP today.)
pub async fn streamer_ws(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response<Body> {
    ws.on_upgrade(move |socket| bridge_h264(socket, state))
}

async fn bridge_h264(mut socket: WebSocket, state: AppState) {
    let sock = &state.cfg.streamer_sock;
    if !sock.exists() {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    // Long-lived GET to the streamer's /h264 access-unit stream.
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, "/h264").into();
    let req = match hyper::Request::builder().method("GET").uri(uri).body(Empty::new()) {
        Ok(r) => r,
        Err(_) => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let resp = match state.uds_client_empty.request(req).await {
        Ok(r) => r,
        Err(_) => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let body = resp.into_body();

    // ── Drop-to-newest bridge (v64.1) ──────────────────────────────────
    // Previously this read one AU and `socket.send().await`'d it inline, so
    // a browser slower than the source applied backpressure all the way up
    // the unix socket into the streamer's 256-deep broadcast — letting live
    // latency silently accumulate to *seconds* and never recover (the lag we
    // chased; it surfaced on a congested Ethernet path). Now a reader task
    // drains /h264 as fast as it arrives (so the broadcast + socket never
    // back up) and stages access units in `pending`; when a backlog forms it
    // discards everything before the most recent keyframe. The writer
    // forwards that keyframe-anchored tail, so a slow/congested link drops
    // stale frames and resyncs at the next keyframe instead of running
    // seconds behind.
    let pending: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let notify = Arc::new(Notify::new());
    let done = Arc::new(AtomicBool::new(false));

    let pending_r = pending.clone();
    let notify_r = notify.clone();
    let done_r = done.clone();
    let reader = tokio::spawn(async move {
        let mut body = body;
        // Accumulates streamer-framed bytes until whole AUs can be split out.
        let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
        while !done_r.load(Ordering::Relaxed) {
            match body.frame().await {
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        buf.extend_from_slice(&data);
                    }
                }
                // EOF (streamer relaunched / pipeline changed) or error.
                _ => break,
            }
            let mut produced = false;
            // Split out every complete [1B flags][4B BE len][AU] record.
            loop {
                if buf.len() < 5 {
                    break;
                }
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if buf.len() < 5 + len {
                    break;
                }
                let mut msg = Vec::with_capacity(1 + len);
                msg.push(buf[0]); // flags: bit0 = keyframe
                msg.extend_from_slice(&buf[5..5 + len]);
                buf.drain(..5 + len);
                {
                    let mut p = pending_r.lock().unwrap();
                    p.push(msg);
                    // Hard cap (e.g. browser hung but not yet disconnected):
                    // keep only the latest keyframe-anchored run so memory
                    // stays bounded.
                    if p.len() > 60 {
                        if let Some(kf) = p.iter().rposition(|m| m[0] & 1 == 1) {
                            if kf > 0 {
                                p.drain(..kf);
                            }
                        }
                    }
                }
                produced = true;
            }
            if produced {
                notify_r.notify_one();
            }
        }
        done_r.store(true, Ordering::Relaxed);
        notify_r.notify_one();
    });

    // Writer: forward the keyframe-anchored tail; watch for client close.
    loop {
        tokio::select! {
            _ = notify.notified() => {}
            ws_in = socket.recv() => {
                match ws_in {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                }
            }
        }
        let batch: Vec<Vec<u8>> = {
            let mut p = pending.lock().unwrap();
            // Frames piled up while we were sending ⇒ browser is behind the
            // source: skip to the most recent keyframe, dropping the stale
            // run. (No keyframe staged yet ⇒ forward as-is; client waits for
            // one.) The `> 3` guard avoids dropping during normal cadence.
            if p.len() > 3 {
                if let Some(kf) = p.iter().rposition(|m| m[0] & 1 == 1) {
                    if kf > 0 {
                        p.drain(..kf);
                    }
                }
            }
            std::mem::take(&mut *p)
        };
        for msg in batch {
            if socket.send(Message::Binary(msg)).await.is_err() {
                done.store(true, Ordering::Relaxed);
                break;
            }
        }
        if done.load(Ordering::Relaxed) && pending.lock().unwrap().is_empty() {
            break;
        }
    }
    done.store(true, Ordering::Relaxed);
    reader.abort();
    let _ = socket.send(Message::Close(None)).await;
}

// ── HID endpoints ───────────────────────────────────────────────────────

pub async fn hid_status(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::GET, "/status", None).await
}
pub async fn hid_type(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/type", Some(body.to_vec())).await
}
pub async fn hid_key(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/key", Some(body.to_vec())).await
}
pub async fn hid_click(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/click", Some(body.to_vec())).await
}
pub async fn hid_button(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/button", Some(body.to_vec())).await
}
pub async fn hid_move(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/move", Some(body.to_vec())).await
}
pub async fn hid_move_abs(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/move_abs", Some(body.to_vec())).await
}
pub async fn hid_scroll(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/scroll", Some(body.to_vec())).await
}
pub async fn hid_release_all(State(state): State<AppState>) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/release_all", Some(vec![])).await
}
pub async fn hid_persona(State(state): State<AppState>, body: bytes::Bytes) -> Response<Body> {
    proxy(&state, &state.cfg.hid_sock, Method::POST, "/persona", Some(body.to_vec())).await
}

// ── Helpers used by the macro runner and the MCP server ─────────────────

/// POST one JSON body to the HID daemon. Returns Err with a short
/// human-readable reason on transport failure or non-2xx status.
pub async fn post_hid(state: &AppState, path: &str, body: Vec<u8>) -> Result<(), String> {
    post_hid_json(state, path, body).await.map(|_| ())
}

/// Like `post_hid`, but returns the parsed JSON response body so
/// callers can read fields like `typed` / `skipped` from the HID
/// daemon. Errors with a short human-readable reason on transport
/// failure or non-2xx status.
pub async fn post_hid_json(
    state: &AppState,
    path: &str,
    body: Vec<u8>,
) -> Result<serde_json::Value, String> {
    let sock = &state.cfg.hid_sock;
    if !sock.exists() {
        return Err("hid socket missing".into());
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new())
        .build::<_, Full<bytes::Bytes>>(connector);
    let req = hyper::Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Full::new(body.into()))
        .map_err(|e| e.to_string())?;
    let resp = client.request(req).await.map_err(|e| format!("hid: {e}"))?;
    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("hid body: {e}"))?
        .to_bytes();
    if !status.is_success() {
        // Try to extract a useful error message from the HID daemon's
        // JSON body (it returns {"ok": false, "err": "..."}). Fall
        // back to the raw status if parsing fails.
        let body_str = String::from_utf8_lossy(&body_bytes);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_str) {
            if let Some(e) = v.get("err").and_then(|x| x.as_str()) {
                return Err(format!("hid status {status}: {e}"));
            }
        }
        return Err(format!("hid status {status}"));
    }
    serde_json::from_slice(&body_bytes)
        .map_err(|e| format!("hid response parse: {e}"))
}

/// POST a JSON body to the streamer's API socket, returning its parsed JSON
/// response. Used by the recording MCP tools.
pub async fn post_streamer_json(
    state: &AppState,
    path: &str,
    body: Vec<u8>,
) -> Result<serde_json::Value, String> {
    streamer_request_json(state, Method::POST, path, Some(body)).await
}

/// GET JSON from the streamer's API socket.
pub async fn fetch_streamer_json(state: &AppState, path: &str) -> Result<serde_json::Value, String> {
    streamer_request_json(state, Method::GET, path, None).await
}

async fn streamer_request_json(
    state: &AppState,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<serde_json::Value, String> {
    let sock = &state.cfg.streamer_sock;
    if !sock.exists() {
        return Err("streamer socket missing".into());
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let client = Client::builder(TokioExecutor::new())
        .build::<_, Full<bytes::Bytes>>(hyperlocal::UnixConnector);
    let req = hyper::Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/json")
        .body(Full::new(body.unwrap_or_default().into()))
        .map_err(|e| e.to_string())?;
    let resp = client.request(req).await.map_err(|e| format!("streamer: {e}"))?;
    let status = resp.status();
    let body_bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("streamer body: {e}"))?
        .to_bytes();
    if !status.is_success() {
        let body_str = String::from_utf8_lossy(&body_bytes);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_str) {
            if let Some(e) = v.get("err").and_then(|x| x.as_str()) {
                return Err(format!("streamer status {status}: {e}"));
            }
        }
        return Err(format!("streamer status {status}"));
    }
    serde_json::from_slice(&body_bytes).map_err(|e| format!("streamer response parse: {e}"))
}

/// Fetch one snapshot from the streamer and write it to `out`.
pub async fn write_snapshot(state: &AppState, out: &std::path::Path) -> Result<(), String> {
    let sock = &state.cfg.streamer_sock;
    if !sock.exists() {
        return Err("streamer socket missing".into());
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, "/snapshot").into();
    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new())
        .build::<_, Empty<bytes::Bytes>>(connector);
    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())
        .map_err(|e| e.to_string())?;
    let resp = client.request(req).await.map_err(|e| format!("snap: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("snap status {}", resp.status()));
    }
    let bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("snap body: {e}"))?
        .to_bytes();
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("snap mkdir: {e}"))?;
    }
    std::fs::write(out, &bytes).map_err(|e| format!("snap write: {e}"))?;
    Ok(())
}

/// Fetch one snapshot and return the raw JPEG bytes (for MCP image responses).
pub async fn fetch_snapshot_bytes(state: &AppState) -> Result<bytes::Bytes, String> {
    let sock = &state.cfg.streamer_sock;
    if !sock.exists() {
        return Err("streamer socket missing".into());
    }
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, "/snapshot").into();
    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new())
        .build::<_, Empty<bytes::Bytes>>(connector);
    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())
        .map_err(|e| e.to_string())?;
    let resp = client.request(req).await.map_err(|e| format!("snap: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("snap status {}", resp.status()));
    }
    let bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("snap body: {e}"))?
        .to_bytes();
    Ok(bytes)
}

/// GET combined supervisor state as a `serde_json::Value` (for MCP).
pub async fn fetch_supervisor_state(state: &AppState) -> Value {
    let streamer = fetch_json(&state.cfg.streamer_sock, "/state").await;
    let hid = fetch_json(&state.cfg.hid_sock, "/status").await;
    serde_json::json!({ "ok": true, "streamer": streamer, "hid": hid })
}
