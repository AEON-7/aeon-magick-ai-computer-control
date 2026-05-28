//! Manages the capture subprocess: spawns either `ustreamer` (fast path,
//! when the device offers a format ustreamer can ingest natively) or
//! `ffmpeg` (fallback for Cam Link at 4K which offers only NV12/YU12).
//! Monitors for exit, relaunches on signal from the watchdog.

use crate::capture::{self, Pipeline};
use crate::config::Platform;
use crate::state::{chrono, SharedState};
use anyhow::Result;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{info, warn};

pub async fn run(state: SharedState) -> Result<()> {
    loop {
        // Wait until the device exists.
        wait_for_device(&state).await;

        // Decide which pipeline to use based on the v4l2 enum + user's
        // configured output preferences.
        let pipeline =
            match capture::detect_pipeline(&state.0.cfg.device, &state.0.cfg.output) {
                Ok(p) => p,
                Err(e) => {
                    warn!(?e, "capture pipeline select failed, retrying in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

        let enum_hash = capture::enum_signature(&state.0.cfg.device).ok();

        // Stash the "current mode" in shared state for /state introspection.
        let (display_fmt, display_res) = match &pipeline {
            Pipeline::Ustreamer(cap) => {
                (cap.format.to_string(), cap.resolution.clone())
            }
            Pipeline::FfmpegRescale {
                source_format,
                source_resolution,
                target_width,
                target_height,
                target_format,
                ..
            } => (
                format!("{target_format} (ffmpeg {source_format}@{source_resolution})"),
                format!("{target_width}x{target_height}"),
            ),
        };
        let kind_tag: &'static str = match &pipeline {
            Pipeline::Ustreamer(_) => "ustreamer",
            Pipeline::FfmpegRescale { .. } => "ffmpeg",
        };
        state.mutate(|s| {
            s.mode = Some(capture::CaptureMode {
                // Leak the format string so it can be 'static (the existing
                // CaptureMode struct stores &'static str). Boxing the string
                // into a leak is fine for a couple-of-bytes-per-relaunch
                // overhead; we rarely change modes.
                format: Box::leak(display_fmt.into_boxed_str()),
                resolution: display_res.clone(),
            });
            s.enum_hash = enum_hash.clone();
            s.last_relaunch = Some(chrono::DateTime::now());
            s.relaunch_count += 1;
            s.online = false;
            s.captured_fps = 0;
            s.pipeline_kind = Some(kind_tag);
        });

        let spawn_res = match &pipeline {
            Pipeline::Ustreamer(cap) => {
                info!(
                    format = cap.format,
                    resolution = %cap.resolution,
                    "spawning ustreamer"
                );
                spawn_ustreamer(&state, cap)
            }
            Pipeline::FfmpegRescale { .. } => {
                info!(
                    mode = "ffmpeg-rescale",
                    target = %display_res,
                    "spawning ffmpeg (NV12/YU12 source → rescale)"
                );
                spawn_ffmpeg(&state, &pipeline)
            }
        };
        let mut child = match spawn_res {
            Ok(c) => c,
            Err(e) => {
                warn!(?e, "spawn failed");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        let label = match &pipeline {
            Pipeline::Ustreamer(_) => "ustreamer",
            Pipeline::FfmpegRescale { .. } => "ffmpeg",
        };

        // ── ffmpeg-mode: capture stdout and parse JPEG frames ──
        // Take ffmpeg's stdout pipe (set by spawn_ffmpeg with
        // Stdio::piped()) and hand it to the JPEG parser, which
        // publishes each complete frame on state.0.frame_tx. The
        // webapi reads from the matching watch::Receiver, so /snapshot
        // and /stream serve frames straight from memory with no
        // filesystem in the path.
        //
        // The parser task lives only as long as this ffmpeg instance —
        // when the child exits / is killed / signals relaunch, the
        // stdout pipe closes and jpeg_pipe::run returns. A new task
        // gets spawned on the next iteration.
        if matches!(pipeline, Pipeline::FfmpegRescale { .. }) {
            if let Some(stdout) = child.stdout.take() {
                let tx = state.0.frame_tx.clone();
                let counter = std::sync::Arc::clone(&state.0.frames_published);
                tokio::spawn(crate::jpeg_pipe::run(stdout, tx, counter));
                info!("jpeg_pipe reader spawned for this ffmpeg run");
            } else {
                warn!("ffmpeg child has no stdout pipe — frames won't reach webapi");
            }
        }

        tokio::select! {
            status = child.wait() => {
                warn!(target = label, ?status, "capture child exited; relaunching");
            }
            _ = state.0.relaunch_signal.notified() => {
                info!(target = label, "relaunch signaled, killing capture child");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            _ = state.0.shutdown_signal.notified() => {
                info!(target = label, "shutdown signaled, killing capture child");
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            }
        }

        // Small backoff to avoid hot-looping if the child fails immediately.
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

/// ffmpeg pipeline. Ingests NV12 or YU12 (planar/semi-planar 4:2:0) at the
/// source resolution Cam Link advertises (typically 3840x2160 when the host
/// is mirroring at 4K), rescales to the user's configured output size
/// (`streamer.toml` `[output]` width × height), and produces TWO sinks in
/// one ffmpeg invocation:
///
///   sink 1: a single JPEG file updated atomically every frame
///           (`/run/aeon/snapshots/live.jpg` by default) — this is what
///           the /api/streamer/snapshot endpoint serves.
///
///   sink 2: an mpjpeg HTTP server on 127.0.0.1:<mjpeg_tcp_port> — this is
///           what /api/streamer/stream proxies to.
fn spawn_ffmpeg(state: &SharedState, pipeline: &Pipeline) -> Result<Child> {
    let cfg = &state.0.cfg;
    let out = &cfg.output;

    let Pipeline::FfmpegRescale {
        source_format,
        source_resolution,
        source_fps,
        target_width,
        target_height,
        target_fps,
        target_format,
        scale_algorithm,
    } = pipeline
    else {
        anyhow::bail!("spawn_ffmpeg called with non-ffmpeg pipeline");
    };

    // Make sure the snapshot directory and the parent of the API socket exist.
    if let Some(parent) = out.snapshot_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Auto-detect content area (pillarbox/letterbox) on the HDMI signal.
    // A 14" MBP rendering at 1512×982 (16:10.4) outputs over a 4K HDMI
    // line (16:9) with ~257-px black bars left+right. cropdetect finds
    // those bars; we then crop them out before scaling so the agent's
    // view has the same aspect ratio as the host's actual screen and
    // HID coords map 1:1.
    let detected = capture::detect_content_crop(
        &cfg.ffmpeg_bin,
        &cfg.device,
        source_format,
        source_resolution,
        *source_fps,
    );
    let crop_filter = match &detected {
        Ok(Some(c)) => {
            info!(
                w = c.width, h = c.height, x = c.x, y = c.y,
                aspect = c.aspect(),
                "content crop detected — cropping pillarbox/letterbox",
            );
            Some(c.as_filter())
        }
        Ok(None) => {
            info!("no pillarbox detected, streaming the full source frame");
            None
        }
        Err(e) => {
            warn!(?e, "cropdetect failed, streaming full frame");
            None
        }
    };

    // Build the final -vf filter chain: crop (if any) → scale.
    // If a crop was detected, also override the target dims to match the
    // cropped aspect ratio at the configured height. That preserves the
    // host's actual aspect in the output (avoids a stretched stream when
    // the user picked, say, output.width=1920 but content is 16:10.4).
    let (out_w, out_h) = if let Ok(Some(c)) = &detected {
        // Pick output height = configured target_height, width derived from cropped aspect
        let h = *target_height;
        let w = ((h as f32 * c.aspect()).round() as u32 / 2) * 2; // round to even (yuv chroma)
        (w, h)
    } else {
        (*target_width, *target_height)
    };
    // Build the scale filter. HW path uses the Pi 4 GPU's bcm2835-codec
    // v4l2m2m scaler — saves ~300–500 mA vs the CPU-bound software scaler
    // on a Pi 4 actively streaming. Falls back to software if disabled in
    // config (default) or if the m2m device isn't available.
    let scale_part = if out.hw_accel {
        info!("hw_accel=true — using scale_v4l2m2m (Pi GPU scaler)");
        // scale_v4l2m2m doesn't take a flags= option; quality is implicit.
        format!("scale_v4l2m2m={w}:{h}", w = out_w, h = out_h)
    } else {
        format!(
            "scale={w}:{h}:flags={alg}",
            w = out_w,
            h = out_h,
            alg = scale_algorithm
        )
    };
    let scale_filter = match crop_filter {
        Some(c) => format!("{c},{scale_part}"),
        None => scale_part,
    };

    // Q:v for ffmpeg MJPEG: 1 = best, 31 = worst. We map our 1–100 jpeg_quality
    // (higher = better) into this inverted scale, clamped to 2..=15 for a sane
    // band. (jpeg_quality 80 → q:v ≈ 5, the visually-lossless sweet spot.)
    let qv = {
        let inverted = 31u32.saturating_sub(((cfg.jpeg_quality as u32) * 31) / 100);
        inverted.clamp(2, 15).to_string()
    };

    let encoder = match (target_format.as_str(), cfg.platform) {
        ("mjpeg", _) => "mjpeg",
        ("h264", Platform::Pi4) => "h264_v4l2m2m",
        ("h264", _) => "libx264",
        _ => "mjpeg",
    };

    let mjpeg_listen_url = format!("http://127.0.0.1:{}/", out.mjpeg_tcp_port);

    // Two-output ffmpeg: split the post-scale stream, send one branch to
    // the always-overwriting snapshot file, the other to the mpjpeg HTTP
    // server. ffmpeg's `tee` muxer is the standard way to do this.
    //
    //   -map 0:v -update 1 -f image2 /run/aeon/snapshots/live.jpg
    //   -map 0:v -f mpjpeg -listen 1 http://127.0.0.1:8002/
    //
    // We use two `-map` outputs (no `tee`); ffmpeg natively allows multiple
    // output files in one invocation.

    // Single output: continuously overwrite a single JPEG file at target
    // resolution. The webapi reads the file for /snapshot, and synthesizes
    // a multipart MJPEG /stream by re-reading at the target fps.
    //
    // Why not also serve via ffmpeg's `-f mpjpeg -listen 1`? Because the
    // listen mode blocks ALL of ffmpeg's outputs until a client connects.
    // That deadlocks the image2 sink and we never get a frame. File-based
    // hand-off is simpler and the snapshot file IS the live stream.
    let _ = (encoder, mjpeg_listen_url); // silence unused warnings — kept above
                                          // because the encoder string IS used
                                          // when we add an h264 file output.

    let mut cmd = Command::new(&cfg.ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("warning")
        .arg("-y")
        // ── Low-latency input flags (v46) ──
        // Without these, ffmpeg's default behavior buffers 2-3 seconds
        // of video for "smoothing" — devastating for a live KVM where
        // every extra frame of latency is visible to the user typing
        // on the target. Each flag explained:
        //
        //   -fflags +nobuffer    Skip frame buffering inside the
        //                        demuxer. Each captured frame fires
        //                        downstream immediately.
        //   -flags  low_delay    Generic "minimize latency" mode for
        //                        codecs that honor it.
        //   -avioflags direct    Bypass libavformat's I/O buffering
        //                        layer entirely.
        //   -probesize  32       Use only 32 bytes for stream
        //                        detection. Default is 5 MB which
        //                        buffers ~5 seconds before producing
        //                        the first frame.
        //   -analyzeduration 0   Don't sample N seconds of input to
        //                        figure out stream parameters — we
        //                        already told ffmpeg the format /
        //                        resolution / framerate via flags.
        //   -thread_queue_size 4 Tiny input queue. Default is 8;
        //                        smaller = less buffered latency.
        .arg("-fflags").arg("nobuffer")
        .arg("-flags").arg("low_delay")
        .arg("-avioflags").arg("direct")
        .arg("-probesize").arg("32")
        .arg("-analyzeduration").arg("0")
        .arg("-thread_queue_size").arg("4")
        // Input
        .arg("-f").arg("v4l2")
        .arg("-input_format").arg(source_format)
        .arg("-video_size").arg(source_resolution)
        .arg("-framerate").arg(source_fps.to_string())
        .arg("-i").arg(&cfg.device)
        // Filter — scale to target resolution
        .arg("-vf").arg(&scale_filter)
        // ── Output / encoding (v46) ──
        //
        // -fps_mode passthrough  (the modern name for -vsync 0) tells
        //   ffmpeg to emit each input frame at the time it arrives,
        //   instead of re-timing to a fixed output framerate. Without
        //   this, `-r 30` would force ffmpeg to BUFFER frames waiting
        //   for the 33.3 ms tick — a frame that arrives early sits in
        //   limbo until its scheduled slot. Passthrough mode skips
        //   the timing layer entirely, shaving ~1-2 frames of latency
        //   in the common case.
        //
        // -flush_packets 1  Make the muxer flush each frame out of
        //   the demuxer the instant it's encoded. Default would let
        //   ffmpeg coalesce small writes.
        //
        // image2pipe: write each frame to stdout as a back-to-back
        // sequence of JPEG bytes. The jpeg_pipe::run task reads
        // stdout, parses SOI/EOI boundaries, and publishes complete
        // frames on a tokio::sync::watch channel. No filesystem, no
        // atomic-write gymnastics, no race.
        .arg("-fps_mode").arg("passthrough")
        .arg("-c:v").arg("mjpeg")
        .arg("-q:v").arg(&qv)
        .arg("-flush_packets").arg("1")
        .arg("-f").arg("image2pipe")
        .arg("pipe:1")
        // Pipe stdout so jpeg_pipe::run can read frames. stderr stays
        // inherited so ffmpeg's warnings/errors land in our journal.
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let _ = target_fps; // we no longer use it; we just pass frames through
                         // at source rate. Drop it from compile-time silence.

    Ok(cmd.spawn()?)
}
