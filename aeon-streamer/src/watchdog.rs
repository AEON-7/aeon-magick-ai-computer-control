//! Two health checks running in one loop. Either firing signals
//! `supervise` to relaunch ustreamer with fresh args:
//!
//!   1. The v4l2 enum hash changed (source signal changed → device caps
//!      changed → ustreamer almost certainly needs different args).
//!   2. ustreamer's own `source.online` reports false for 2+ polls
//!      (capture stuck; happens when source caps change while the device
//!      is held open, since USB UVC devices don't fire DV-timings events).

use crate::capture;
use crate::state::SharedState;
use anyhow::Result;
use std::time::Duration;
use tracing::info;

const POLL: Duration = Duration::from_secs(2);

pub async fn run(state: SharedState) -> Result<()> {
    let mut last_hash: Option<String> = None;
    let mut offline_streak: u32 = 0;
    // v24: track ffmpeg liveness via the frame watch channel instead
    // of `live.jpg` mtime (the file no longer exists — jpeg_pipe::run
    // publishes frames directly from ffmpeg's stdout pipe). Each poll,
    // we call has_changed() / borrow_and_update() to detect whether
    // any new frame arrived since last poll. N consecutive false =
    // ffmpeg stalled.
    let mut frame_rx = state.0.frame_rx.clone();
    let _ = frame_rx.borrow_and_update();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = state.0.shutdown_signal.notified() => return Ok(()),
        }

        // 1. enum-hash check — ONLY in ustreamer mode.
        //
        // In ffmpeg mode, ffmpeg holds /dev/kvmd-video open continuously.
        // Probing it with v4l2-ctl while ffmpeg has it open can yield a
        // different format list than at startup (some UVC drivers report
        // "currently configured" rather than "all available"), making the
        // hash flip-flop. The watchdog would then kill ffmpeg every cycle
        // — exactly the "video works for a few seconds then dies" symptom.
        //
        // The hash check is only valuable for the ustreamer path, where
        // format changes mean ustreamer needs new args.
        let pipeline_kind = state.read().pipeline_kind;
        let in_ffmpeg_mode = pipeline_kind == Some("ffmpeg");
        if !in_ffmpeg_mode {
            if let Ok(h) = capture::enum_signature(&state.0.cfg.device) {
                if let Some(prev) = &last_hash {
                    if &h != prev {
                        info!(prev = %prev, new = %h, "v4l2 enum changed");
                        state.signal_relaunch("v4l2 enum changed");
                        offline_streak = 0;
                    }
                }
                last_hash = Some(h);
            }
        }

        // 2. Online check — different signal source per pipeline.
        if in_ffmpeg_mode {
            // ffmpeg-pipeline mode (v24): "online" = jpeg_pipe::run
            // published at least one new frame since the last poll.
            // The watch channel's has_changed() flag is set by the
            // sender on every send(); borrow_and_update() clears it.
            // 5 consecutive polls with no change (~10s at 2s poll) =
            // ffmpeg stalled, signal a relaunch.
            let fresh = match frame_rx.has_changed() {
                Ok(true) => {
                    // Mark as seen for next iteration.
                    let _ = frame_rx.borrow_and_update();
                    true
                }
                Ok(false) => false,
                Err(_) => false,  // sender dropped — treated as stalled
            };
            if fresh {
                offline_streak = 0;
                state.mutate(|s| s.online = true);
            } else {
                offline_streak += 1;
                state.mutate(|s| s.online = false);
                if offline_streak >= 5 {
                    info!(streak = offline_streak,
                        "no new frame from ffmpeg pipe in {} polls, relaunching",
                        offline_streak);
                    state.signal_relaunch("ffmpeg pipe stalled");
                    offline_streak = 0;
                }
            }
        } else {
            match query_ustreamer_online(&state).await {
                Ok(Some(true)) => {
                    offline_streak = 0;
                    state.mutate(|s| s.online = true);
                }
                Ok(Some(false)) => {
                    offline_streak += 1;
                    state.mutate(|s| s.online = false);
                    if offline_streak >= 2 {
                        info!(streak = offline_streak, "ustreamer source.online=false");
                        state.signal_relaunch("ustreamer offline");
                        offline_streak = 0;
                    }
                }
                Ok(None) => {
                    offline_streak = 0;
                }
                Err(e) => {
                    tracing::debug!(?e, "query_ustreamer_online failed");
                }
            }
        }
    }
}

/// Hit ustreamer's `/state` over its unix socket and parse `source.online`.
async fn query_ustreamer_online(state: &SharedState) -> Result<Option<bool>> {
    use http_body_util::BodyExt;
    use http_body_util::Empty;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let sock = &state.0.cfg.ustreamer_sock;
    if !sock.exists() {
        return Ok(None);
    }

    let connector = hyperlocal::UnixConnector;
    let client = Client::builder(TokioExecutor::new()).build::<_, Empty<bytes::Bytes>>(connector);
    let uri: hyper::Uri = hyperlocal::Uri::new(sock, "/state").into();
    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::new())?;

    let resp = match client.request(req).await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let bytes = resp.into_body().collect().await?.to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    let online = v
        .get("result")
        .and_then(|r| r.get("source"))
        .and_then(|s| s.get("online"))
        .and_then(|o| o.as_bool());
    let fps = v
        .get("result")
        .and_then(|r| r.get("source"))
        .and_then(|s| s.get("captured_fps"))
        .and_then(|o| o.as_u64())
        .unwrap_or(0) as u32;
    state.mutate(|s| s.captured_fps = fps);
    Ok(online)
}
