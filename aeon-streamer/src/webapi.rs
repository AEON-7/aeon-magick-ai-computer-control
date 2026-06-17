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
        .route("/h264", get(h264_stream))
        .route("/record/start", post(record_start))
        .route("/record/stop", post(record_stop))
        .route("/record/state", get(record_state))
        .route("/recordings", get(list_recordings))
        .route("/recordings/:id", get(get_recording).delete(delete_recording))
        .route("/recordings/:id/thumb", get(get_thumb))
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
    if kind == "ffmpeg-h264" || kind == "libcamera-h264" {
        // H.264 mode: ffmpeg writes an atomic JPEG to the snapshot path.
        return serve_snapshot_from_file(&state).await;
    }
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
    if kind == "ffmpeg-h264" || kind == "libcamera-h264" {
        // H.264 mode: the low-latency live view is /h264 (WebSocket +
        // WebCodecs). This MJPEG /stream stays available as a fallback for
        // browsers without WebCodecs, synthesized from the snapshot file.
        return stream_mjpeg_from_file(&state).await;
    }
    if kind == "ffmpeg" {
        return proxy_to_ffmpeg_tcp(&state, "/").await;
    }
    proxy_to_ustreamer(&state, "/stream").await
}

/// Serve the latest JPEG frame from the in-memory watch channel populated
/// by jpeg_pipe::run (which reads ffmpeg's stdout pipe). Zero filesystem.
async fn serve_snapshot_file(state: &SharedState) -> Response<Body> {
    let current = state.0.frame_rx.borrow().clone();
    match current {
        Some(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "no-store")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            }),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no frame captured yet — ffmpeg still starting up",
        )
            .into_response(),
    }
}

/// Synthesize a multipart MJPEG stream straight from the in-memory
/// frame watch channel that jpeg_pipe::run keeps current. Each browser
/// connection gets a clone of state.frame_rx and emits on every change.
///
/// v24 design: zero filesystem. The watch channel publishes complete
/// JPEG frames (validated by SOI/EOI parser in jpeg_pipe), so there's
/// no possibility of forwarding a torn / partial frame. The previous
/// v23 design polled a tmpfs file's mtime and read its contents — it
/// worked once -atomic_writing was set, but this path eliminates the
/// race entirely and drops latency from ~1-2ms (file round-trip) to
/// the watch channel's wake-up cost (~50µs).
///
/// Browsers and multipart parsers interpret "no part received for 3-5
/// seconds" as "stream ended", so we still re-emit the last frame as a
/// heartbeat every 1.5s when the source goes idle.
async fn proxy_to_ffmpeg_tcp(state: &SharedState, _path: &str) -> Response<Body> {
    use axum::body::Body;
    use futures::stream::StreamExt;
    use std::time::Duration;

    let boundary = "aeonframe";
    let heartbeat = Duration::from_millis(1500);

    // Each client gets its OWN receiver. watch::Receiver::clone()
    // creates a fresh subscription with its own seen-version counter,
    // so concurrent /stream requests don't steal each other's wake-ups.
    let mut rx = state.0.frame_rx.clone();

    let body_stream = async_stream::stream! {
        let mut last_bytes: Option<bytes::Bytes> = None;

        // Helper closure to grab the current frame and IMMEDIATELY drop
        // the watch::Ref before any potential .await. Holding a Ref
        // across an await point would force the async_stream future to
        // be !Send.
        let snapshot = |rx: &tokio::sync::watch::Receiver<Option<bytes::Bytes>>| -> Option<bytes::Bytes> {
            rx.borrow().clone()
        };

        // Send the current frame immediately if there is one (otherwise
        // a brand-new connection has to wait for the next ffmpeg frame,
        // and that's awkward UX).
        let initial = snapshot(&rx);
        if let Some(b) = initial {
            let header = format!(
                "\r\n--{boundary}\r\n\
                 Content-Type: image/jpeg\r\n\
                 Content-Length: {}\r\n\r\n",
                b.len()
            );
            yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
            yield Ok::<_, std::io::Error>(b.clone());
            last_bytes = Some(b);
        }

        loop {
            // Wait for the next frame, OR fire the heartbeat timer if
            // no new frame arrives within 1.5s.
            let next = tokio::time::timeout(heartbeat, rx.changed()).await;
            match next {
                Ok(Ok(())) => {
                    // New frame published by jpeg_pipe::run.
                    let current = snapshot(&rx);
                    if let Some(b) = current {
                        let header = format!(
                            "\r\n--{boundary}\r\n\
                             Content-Type: image/jpeg\r\n\
                             Content-Length: {}\r\n\r\n",
                            b.len()
                        );
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
                        yield Ok::<_, std::io::Error>(b.clone());
                        last_bytes = Some(b);
                    }
                }
                Ok(Err(_)) => {
                    // Sender dropped — only happens if state is being
                    // torn down. End the stream cleanly.
                    return;
                }
                Err(_) => {
                    // Heartbeat timer fired — re-emit the last frame so
                    // the browser's multipart parser stays alive.
                    if let Some(b) = &last_bytes {
                        let header = format!(
                            "\r\n--{boundary}\r\n\
                             Content-Type: image/jpeg\r\n\
                             Content-Length: {}\r\n\r\n",
                            b.len()
                        );
                        yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
                        yield Ok::<_, std::io::Error>(b.clone());
                    }
                }
            }
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

/// v64: stream H.264 access units to the supervisor, which forwards them
/// onto the browser WebSocket. Per-AU framing: `[1 byte flags][4 byte BE
/// length][AU bytes]`; flags bit0 = keyframe. AU payload is Annex-B.
///
/// Only meaningful in the `ffmpeg-h264` pipeline; in other modes nothing is
/// ever published to `h264_tx`, so this long-lived response simply idles.
async fn h264_stream(State(state): State<SharedState>) -> Response<Body> {
    let mut rx = state.0.h264_tx.subscribe();
    let body_stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(au) => {
                    let mut hdr = [0u8; 5];
                    hdr[0] = if au.key { 1 } else { 0 };
                    hdr[1..5].copy_from_slice(&(au.data.len() as u32).to_be_bytes());
                    yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&hdr));
                    yield Ok::<_, std::io::Error>(au.data);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "h264 subscriber lagged; resyncing at next keyframe");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .header("Cache-Control", "no-store")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response())
}

/// Serve the latest JPEG that the H.264 pipeline's ffmpeg writes atomically
/// to the snapshot path. Agents depend on /snapshot returning a frame even
/// when the live view is H.264.
async fn serve_snapshot_from_file(state: &SharedState) -> Response<Body> {
    let path = state.0.cfg.output.snapshot_path.clone();
    // Tiny tmpfs file; a blocking read here is microseconds. ffmpeg writes
    // with -atomic_writing (temp + rename), so a reader never catches a
    // torn frame.
    match std::fs::read(&path) {
        Ok(b) if !b.is_empty() => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "no-store")
            .body(Body::from(b))
            .unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response()
            }),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "no snapshot yet — ffmpeg still starting up",
        )
            .into_response(),
    }
}

/// Fallback MJPEG /stream for the H.264 pipeline: re-read the atomic
/// snapshot file at the configured fps and emit multipart. Laggy compared
/// to the /h264 WebSocket path — this exists only for browsers without
/// WebCodecs. Atomic writes on the producer side mean each read is a whole
/// frame.
async fn stream_mjpeg_from_file(state: &SharedState) -> Response<Body> {
    let path = state.0.cfg.output.snapshot_path.clone();
    let fps = state.0.cfg.output.fps.clamp(1, 30) as u64;
    let interval = std::time::Duration::from_millis((1000 / fps).max(33));
    let boundary = "aeonframe";
    let body_stream = async_stream::stream! {
        loop {
            if let Ok(b) = std::fs::read(&path) {
                if !b.is_empty() {
                    let header = format!(
                        "\r\n--{boundary}\r\n\
                         Content-Type: image/jpeg\r\n\
                         Content-Length: {}\r\n\r\n",
                        b.len()
                    );
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(header));
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(b));
                }
            }
            tokio::time::sleep(interval).await;
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Content-Type",
            format!("multipart/x-mixed-replace; boundary={boundary}"),
        )
        .header("Cache-Control", "no-store")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response())
}

// ── Screen recording (v1) ──────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct RecordStartReq {
    /// Auto-stop after this many seconds. Omit → 30 s default; 0 → open-ended
    /// (manual stop, capped at 1 h).
    #[serde(default)]
    duration_s: Option<u64>,
}

/// POST /record/start {duration_s?} — begin recording the live H.264 stream to
/// MP4. Requires the ffmpeg-h264 pipeline (the only one that publishes AUs).
async fn record_start(State(state): State<SharedState>, body: bytes::Bytes) -> impl IntoResponse {
    if !matches!(state.read().pipeline_kind, Some("ffmpeg-h264") | Some("libcamera-h264")) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "err": "recording requires H.264 stream mode — set the stream format to h264 first"})),
        );
    }
    let duration_s = serde_json::from_slice::<RecordStartReq>(&body)
        .ok()
        .and_then(|r| r.duration_s);
    let fps = state.0.cfg.output.fps;
    let ffmpeg = state.0.cfg.ffmpeg_bin.clone();
    // Mux the configured ALSA capture (BrainCraft mic for camera-csi, or the
    // tc358743 HDMI-audio card for hdmi-csi) into the MP4; empty = video-only.
    let cap = &state.0.cfg.capture;
    let (adev, arate, ach) =
        (cap.audio_device.clone(), cap.audio_rate, cap.audio_channels);
    match state
        .0
        .record
        .start(&state.0.h264_tx, ffmpeg, fps, adev, arate, ach, duration_s)
    {
        Ok(info) => (StatusCode::OK, Json(json!({"ok": true, "recording": info}))),
        Err(e) => (StatusCode::CONFLICT, Json(json!({"ok": false, "err": e}))),
    }
}

/// POST /record/stop — finalize the in-progress recording.
async fn record_stop(State(state): State<SharedState>) -> impl IntoResponse {
    match state.0.record.stop() {
        Ok(info) => (StatusCode::OK, Json(json!({"ok": true, "recording": info}))),
        Err(e) => (StatusCode::CONFLICT, Json(json!({"ok": false, "err": e}))),
    }
}

/// GET /record/state — active recording (if any) + the finished list.
async fn record_state(State(state): State<SharedState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "active": state.0.record.active_info(),
        "recordings": state.0.record.list(),
        "note": state.0.record.note(),
    }))
}

/// GET /recordings/:id/thumb — first-frame JPEG thumbnail.
async fn get_thumb(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response<Body> {
    let Some(path) = state.0.record.thumb_path(&id) else {
        return (StatusCode::NOT_FOUND, "no thumbnail").into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(b) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "max-age=86400")
            .body(Body::from(b))
            .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "x").into_response()),
        Err(_) => (StatusCode::NOT_FOUND, "no thumbnail").into_response(),
    }
}

/// GET /recordings — finished recordings, newest first.
async fn list_recordings(State(state): State<SharedState>) -> impl IntoResponse {
    Json(json!({"ok": true, "recordings": state.0.record.list()}))
}

/// DELETE /recordings/:id
async fn delete_recording(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.0.record.delete(&id) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "deleted": id}))),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "err": e}))),
    }
}

/// GET /recordings/:id — download the MP4 (streamed in chunks, bounded memory).
async fn get_recording(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response<Body> {
    let Some(path) = state.0.record.path(&id) else {
        return (StatusCode::NOT_FOUND, "no such recording").into_response();
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "open failed").into_response(),
    };
    let len = file.metadata().await.map(|m| m.len()).ok();
    let body_stream = async_stream::stream! {
        use tokio::io::AsyncReadExt;
        let mut file = file;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[..n])),
                Err(_) => break,
            }
        }
    };
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "video/mp4")
        .header(
            "Content-Disposition",
            format!("attachment; filename=\"{id}.mp4\""),
        )
        .header("Cache-Control", "no-store");
    if let Some(l) = len {
        builder = builder.header("Content-Length", l);
    }
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "body build").into_response())
}
