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
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixListener;
use tower::ServiceExt;
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

    let app: Router = Router::new()
        .route("/state", get(get_state))
        .route("/relaunch", post(force_relaunch))
        .route("/snapshot", get(snapshot_proxy))
        .route("/stream", get(stream_proxy))
        .with_state(state.clone());

    // axum::serve only takes TcpListener; for unix sockets we run an
    // accept loop and dispatch each connection to a fresh tower service.
    loop {
        tokio::select! {
            _ = state.0.shutdown_signal.notified() => return Ok(()),
            res = listener.accept() => {
                let (stream, _addr) = res?;
                let io = TokioIo::new(stream);
                let app = app.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req: hyper::Request<Incoming>| {
                        let app = app.clone();
                        async move { app.oneshot(req).await }
                    });
                    if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                        .serve_connection(io, svc).await
                    {
                        tracing::debug!(?e, "connection ended");
                    }
                });
            }
        }
    }
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
        "pipeline": s.pipeline_kind,
    }))
}

async fn force_relaunch(State(state): State<SharedState>) -> impl IntoResponse {
    state.signal_relaunch("api /relaunch");
    Json(json!({"ok": true, "kicked": true}))
}

/// Dispatch /snapshot to whichever pipeline is currently active.
/// - "ustreamer" → proxy from ustreamer's unix socket
/// - "ffmpeg"    → read the latest single-frame JPEG file ffmpeg writes
async fn snapshot_proxy(State(state): State<SharedState>) -> Response<Body> {
    let kind = state.read().pipeline_kind.unwrap_or("ustreamer");
    if kind == "ffmpeg" {
        return serve_snapshot_file(&state).await;
    }
    proxy_to_ustreamer(&state, "/snapshot").await
}

/// Dispatch /stream to whichever pipeline is currently active.
/// - "ustreamer" → proxy from ustreamer's unix socket
/// - "ffmpeg"    → proxy from ffmpeg's mpjpeg TCP listener
async fn stream_proxy(State(state): State<SharedState>) -> Response<Body> {
    let kind = state.read().pipeline_kind.unwrap_or("ustreamer");
    if kind == "ffmpeg" {
        return proxy_to_ffmpeg_tcp(&state, "/").await;
    }
    proxy_to_ustreamer(&state, "/stream").await
}

/// Read the single-frame JPEG that ffmpeg's `-update 1` sink keeps fresh.
async fn serve_snapshot_file(state: &SharedState) -> Response<Body> {
    let path = &state.0.cfg.output.snapshot_path;
    match tokio::fs::read(path).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "no-store")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            }),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "ffmpeg snapshot file not ready yet",
        )
            .into_response(),
    }
}

/// Synthesize a multipart MJPEG stream by polling the snapshot JPEG file
/// at the configured target fps. This avoids ffmpeg's `-f mpjpeg -listen 1`
/// behavior (which blocks all sibling outputs until a client connects).
///
/// Client gets a normal `multipart/x-mixed-replace; boundary=aeonframe`
/// stream of JPEG parts at ~30fps (or whatever `output.fps` is). Streams
/// terminate when either the client disconnects or the file goes stale.
async fn proxy_to_ffmpeg_tcp(state: &SharedState, _path: &str) -> Response<Body> {
    use axum::body::Body;
    use futures::stream::StreamExt;
    use std::time::Duration;

    let path = state.0.cfg.output.snapshot_path.clone();
    let fps = state.0.cfg.output.fps.max(1);
    let frame_interval = Duration::from_millis(1000 / (fps as u64).max(1));
    let boundary = "aeonframe";

    // Heartbeat interval: re-emit the latest frame every ~1.5s when the
    // source goes idle. Browsers and multipart-MJPEG decoders interpret
    // "no part received for 3-5 seconds" as "stream ended" and show the
    // broken-image icon, forcing the user to refresh. Re-yielding the
    // current frame keeps the TCP/multipart parser alive even when
    // ffmpeg isn't producing new data (target screen static, frame
    // dedup, brief ffmpeg respawn during reconfigure, etc.).
    let heartbeat = Duration::from_millis(1500);
    let body_stream = async_stream::stream! {
        let mut last_mtime: Option<std::time::SystemTime> = None;
        let mut last_emit = std::time::Instant::now();
        let mut last_bytes: Option<bytes::Bytes> = None;
        loop {
            let meta = tokio::fs::metadata(&path).await;
            let mtime = meta.as_ref().ok().and_then(|m| m.modified().ok());

            let new_frame = matches!(
                (last_mtime, mtime),
                (None, Some(_)) | (Some(_), Some(_)) if last_mtime != mtime
            );

            // Either: source advanced (emit new frame) OR
            //         heartbeat is due (re-emit the previous frame).
            let due_heartbeat = !new_frame
                && last_bytes.is_some()
                && last_emit.elapsed() >= heartbeat;

            if new_frame {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    // Validate JPEG integrity before emitting. ffmpeg
                    // writes live.jpg non-atomically (O_TRUNC + write),
                    // so we sometimes catch the file mid-write — short
                    // file, missing EOI marker, or in the worst case
                    // zero bytes immediately after the truncate. If we
                    // forward a corrupt JPEG, Chrome's image decoder
                    // in a multipart/x-mixed-replace stream eventually
                    // gives up and TEARS DOWN the entire connection
                    // after a few bad frames — which is exactly the
                    // "screen dies after a few dozen frames" symptom
                    // we've been chasing across v15+. Skip corrupt
                    // frames; the heartbeat path will keep the
                    // connection alive with the last-good frame until
                    // a clean read comes through.
                    let valid = bytes.len() >= 4
                        && bytes[0..2] == [0xFF, 0xD8]              // SOI
                        && bytes[bytes.len()-2..] == [0xFF, 0xD9];  // EOI
                    if valid {
                        let b = bytes::Bytes::from(bytes);
                        let header = format!(
                            "\r\n--{boundary}\r\n\
                             Content-Type: image/jpeg\r\n\
                             Content-Length: {}\r\n\r\n",
                            b.len()
                        );
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
                        yield Ok::<_, std::io::Error>(b.clone());
                        last_bytes = Some(b);
                        last_mtime = mtime;
                        last_emit = std::time::Instant::now();
                    } else {
                        // Bad read — don't update last_mtime so the
                        // next iteration retries. Don't yield. The
                        // browser will see this as a brief stall, not
                        // a corrupt frame.
                        tracing::trace!(
                            len = bytes.len(),
                            "skipping malformed live.jpg (mid-write?)"
                        );
                    }
                }
            } else if due_heartbeat {
                if let Some(b) = &last_bytes {
                    let header = format!(
                        "\r\n--{boundary}\r\n\
                         Content-Type: image/jpeg\r\n\
                         Content-Length: {}\r\n\r\n",
                        b.len()
                    );
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
                    yield Ok::<_, std::io::Error>(b.clone());
                    last_emit = std::time::Instant::now();
                }
            }
            tokio::time::sleep(frame_interval).await;
        }
    };

    let body = Body::from_stream(body_stream.map(|r| r.map(|b| b)));
    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Content-Type",
            format!("multipart/x-mixed-replace; boundary={boundary}"),
        )
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response())
}

async fn proxy_to_ustreamer(state: &SharedState, path: &str) -> Response<Body> {
    use http_body_util::Empty;

    let sock = &state.0.cfg.ustreamer_sock;
    if !sock.exists() {
        return (StatusCode::SERVICE_UNAVAILABLE, "streamer socket missing").into_response();
    }

    // Use the long-lived shared Client — per-request Clients drop their
    // connection pool when this function returns, killing in-flight
    // streaming response bodies (multipart MJPEG /stream) a few seconds
    // later. Same lifetime fix the supervisor got in v14; this is the
    // streamer→ustreamer hop.
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, path).into();
    let req = match hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())
    {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "request build").into_response(),
    };

    match state.0.uds_client.request(req).await {
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
