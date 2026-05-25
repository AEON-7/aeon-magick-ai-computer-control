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

    loop {
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = state.0.shutdown_signal.notified() => return Ok(()),
        }

        // 1. enum-hash check
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

        // 2. ustreamer online check
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
                // Couldn't reach socket — ustreamer may be starting. Don't count.
                offline_streak = 0;
            }
            Err(e) => {
                tracing::debug!(?e, "query_ustreamer_online failed");
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
