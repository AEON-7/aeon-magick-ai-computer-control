//! Manages the `ustreamer` subprocess: spawn with adaptive args, monitor for
//! exit, relaunch on signal from the watchdog.

use crate::capture;
use crate::config::Platform;
use crate::state::{SharedState, chrono};
use anyhow::Result;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{info, warn};

pub async fn run(state: SharedState) -> Result<()> {
    loop {
        // Wait until the device exists.
        wait_for_device(&state).await;

        // Detect Cam Link's current best mode.
        let mode = match capture::detect(&state.0.cfg.device) {
            Ok(m) => m,
            Err(e) => {
                warn!(?e, "capture mode detect failed, retrying in 2s");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        let enum_hash = capture::enum_signature(&state.0.cfg.device).ok();
        state.mutate(|s| {
            s.mode = Some(mode.clone());
            s.enum_hash = enum_hash.clone();
            s.last_relaunch = Some(chrono::DateTime::now());
            s.relaunch_count += 1;
            s.online = false;
            s.captured_fps = 0;
        });

        info!(
            format = mode.format,
            resolution = mode.resolution,
            "spawning ustreamer"
        );

        let child = spawn_ustreamer(&state, &mode);
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                warn!(?e, "spawn failed");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        tokio::select! {
            status = child.wait() => {
                warn!(?status, "ustreamer exited; relaunching");
            }
            _ = state.0.relaunch_signal.notified() => {
                info!("relaunch signaled, killing ustreamer");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            _ = state.0.shutdown_signal.notified() => {
                info!("shutdown signaled, killing ustreamer");
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            }
        }

        // Small backoff to avoid hot-looping if ustreamer fails immediately.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn wait_for_device(state: &SharedState) {
    let path = &state.0.cfg.device;
    if path.exists() {
        return;
    }
    info!(device = %path.display(), "waiting for capture device to appear");
    loop {
        if path.exists() {
            info!(device = %path.display(), "device present");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

fn spawn_ustreamer(state: &SharedState, mode: &capture::CaptureMode) -> Result<Child> {
    let cfg = &state.0.cfg;

    // Make sure the API socket dir exists.
    if let Some(parent) = cfg.ustreamer_sock.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut cmd = Command::new(&cfg.ustreamer_bin);
    cmd.arg(format!("--device={}", cfg.device.display()))
        .arg(format!("--format={}", mode.format))
        .arg(format!("--resolution={}", mode.resolution))
        .arg("--persistent")
        .arg(format!("--quality={}", cfg.jpeg_quality))
        .arg(format!("--drop-same-frames={}", cfg.drop_same_frames))
        .arg(format!("--unix={}", cfg.ustreamer_sock.display()))
        .arg("--unix-rm")
        .arg("--unix-mode=0660")
        .arg("--exit-on-parent-death")
        .arg("--notify-parent")
        .arg("--no-log-colors");

    // Hardware encoding path: on Pi 4 we can use the M2M H.264 encoder for
    // a JPEG-MJPEG-equivalent latency reduction. ustreamer's --encoder takes
    // "cpu" or "m2m-image" (Pi-specific). We use m2m-image on Pi 4. Pi 5
    // doesn't have H.264 HW encode, so we stick with CPU.
    match cfg.platform {
        Platform::Pi4 => {
            cmd.arg("--encoder=m2m-image");
        }
        _ => {
            cmd.arg("--encoder=cpu").arg("--workers=4");
        }
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    Ok(cmd.spawn()?)
}
