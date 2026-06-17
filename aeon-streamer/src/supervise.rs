//! Manages the capture subprocess: spawns either `ustreamer` (fast path,
//! when the device offers a format ustreamer can ingest natively) or
//! `ffmpeg` (fallback for Cam Link at 4K which offers only NV12/YU12).
//! Monitors for exit, relaunches on signal from the watchdog.

use crate::capture::{self, Pipeline};
use crate::config::{Platform, Source};
use crate::state::{chrono, SharedState};
use anyhow::Result;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{info, warn};

pub async fn run(state: SharedState) -> Result<()> {
    loop {
        // ── CAMERA-CSI branch (Pi 5 + a Raspberry Pi camera via libcamera) ──
        // A Pi camera has no plain-v4l2 device to open or enumerate (its node
        // is raw Bayer needing the ISP), so rpicam-vid owns the sensor and
        // feeds ffmpeg. Kept in its own branch so the v4l2 detect/spawn path
        // below stays the exact Cam Link / HDMI-CSI path. The pipeline still
        // terminates in ffmpeg's H.264 stdout → h264_pipe, so the watchdog's
        // counter-based liveness (pipeline_kind "libcamera-h264") works the
        // same as the Cam Link H.264 path.
        if state.0.cfg.capture.source == Source::CameraCsi {
            let (res, cam_id) = {
                let out = &state.0.cfg.output;
                // Report the post-rotation geometry: a 90/270 turn swaps W↔H.
                let (w, h) = if state.0.cfg.capture.swaps_dims() {
                    (out.height, out.width)
                } else {
                    (out.width, out.height)
                };
                (format!("{w}x{h}"), state.0.cfg.capture.camera_id)
            };
            info!(camera_id = cam_id, %res,
                  "camera-csi: spawning rpicam-vid → ffmpeg (H.264 + JPEG snapshot)");
            let (mut child, mut sidecar) = match spawn_libcamera_h264(&state) {
                Ok(pair) => pair,
                Err(e) => {
                    warn!(?e, "libcamera spawn failed, retrying in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };
            state.mutate(|s| {
                s.mode = Some(capture::CaptureMode {
                    format: Box::leak(format!("h264 (libcamera cam{cam_id})").into_boxed_str()),
                    resolution: res.clone(),
                });
                s.enum_hash = None;
                s.last_relaunch = Some(chrono::DateTime::now());
                s.relaunch_count += 1;
                s.online = false;
                s.captured_fps = 0;
                s.pipeline_kind = Some("libcamera-h264");
            });
            // ffmpeg's H.264 stdout → broadcast (same reader as the Cam Link
            // H.264 pipeline: bumps frames_published for watchdog liveness).
            if let Some(stdout) = child.stdout.take() {
                let counter = std::sync::Arc::clone(&state.0.frames_published);
                let tx = state.0.h264_tx.clone();
                tokio::spawn(crate::h264_pipe::run(stdout, tx, counter));
                info!("h264_pipe reader spawned for libcamera run");
            } else {
                warn!("libcamera ffmpeg child has no stdout pipe — frames won't reach webapi");
            }
            tokio::select! {
                status = child.wait() => {
                    warn!(?status, "libcamera ffmpeg exited; relaunching");
                }
                _ = state.0.relaunch_signal.notified() => {
                    info!("relaunch signaled, killing libcamera pipeline");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                _ = state.0.shutdown_signal.notified() => {
                    info!("shutdown signaled, killing libcamera pipeline");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = sidecar.kill().await; // release the camera
                    return Ok(());
                }
            }
            // Always reap the rpicam sidecar so it releases the camera before
            // the next spawn (else the relaunch hits "camera in use").
            let _ = sidecar.kill().await;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }

        // Wait until the device exists.
        wait_for_device(&state).await;

        // Decide which pipeline to use.
        //
        // HDMI-CSI (Pi 5 X1301 / TC358743) needs a FIXED pipeline: the rp1-cfe
        // CSI capture node doesn't enumerate discrete sizes, so detect_pipeline
        // can't pick a mode (it errors "no ffmpeg-ingestible v4l2 format"). The
        // aeon-hdmi-csi service has already pinned the node to UYVY at the
        // source resolution and enabled the csi2→CFE link, so we build a fixed
        // UYVY FfmpegRescale at the node's current resolution and honour the
        // configured output.format ("mjpeg" = browser-native <img>, "h264" =
        // WebCodecs). The other sources keep the auto-detect path.
        let pipeline = if state.0.cfg.capture.source == Source::HdmiCsi {
            // Use the source's REAL locked mode (resolution + framerate) from the
            // bridge DV-timings, so `-r target_fps` decimates from the true input
            // rate (1080p30 vs 1080p60 etc.) instead of always assuming 60 — and
            // /state reports what's actually coming in. Falls back to the node's
            // current resolution @ 60 when no signal is locked yet.
            let (res, src_fps) = match capture::current_dv_timings() {
                Some((w, h, fps)) => (format!("{w}x{h}"), fps),
                None => (
                    capture::current_resolution(&state.0.cfg.device)
                        .unwrap_or_else(|| "1920x1080".to_string()),
                    60,
                ),
            };
            info!(%res, src_fps, target_fps = state.0.cfg.output.fps,
                  match_source = state.0.cfg.output.match_source,
                  fmt = %state.0.cfg.output.format, "hdmi-csi: source mode");
            Pipeline::FfmpegRescale {
                source_format: "uyvy422".to_string(),
                source_resolution: res,
                source_fps: src_fps,
                target_width: state.0.cfg.output.width,
                target_height: state.0.cfg.output.height,
                target_fps: state.0.cfg.output.fps,
                target_format: state.0.cfg.output.format.clone(),
                scale_algorithm: state.0.cfg.output.scale_algorithm.clone(),
            }
        } else {
            match capture::detect_pipeline(&state.0.cfg.device, &state.0.cfg.output) {
                Ok(p) => p,
                Err(e) => {
                    warn!(?e, "capture pipeline select failed, retrying in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
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
            } => {
                // With match_source the real output tracks the (capped)
                // source resolution, so report that instead of the unused
                // config target. (Crop is applied in spawn; the common
                // no-crop case is exact.)
                let res = if state.0.cfg.output.match_source {
                    parse_wxh(source_resolution)
                        .map(|(w, h)| cap_1080p(w, h))
                        .map(|(w, h)| format!("{w}x{h}"))
                        .unwrap_or_else(|| format!("{target_width}x{target_height}"))
                } else {
                    format!("{target_width}x{target_height}")
                };
                (
                    format!("{target_format} (ffmpeg {source_format}@{source_resolution})"),
                    res,
                )
            }
        };
        // A 90/270 display rotation swaps the reported resolution (W↔H) so
        // /state introspection matches what clients actually receive; the
        // H.264 SPS and JPEG headers already carry the true post-rotation
        // geometry, so this is cosmetic-only.
        let display_res = if state.0.cfg.capture.swaps_dims() {
            parse_wxh(&display_res)
                .map(|(w, h)| format!("{h}x{w}"))
                .unwrap_or(display_res)
        } else {
            display_res
        };
        let kind_tag: &'static str = match &pipeline {
            Pipeline::Ustreamer(_) => "ustreamer",
            Pipeline::FfmpegRescale { target_format, .. } if target_format == "h264" => {
                "ffmpeg-h264"
            }
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
            Pipeline::FfmpegRescale { target_format, .. } if target_format == "h264" => {
                info!(
                    mode = "ffmpeg-h264",
                    target = %display_res,
                    "spawning ffmpeg (H.264 → stdout pipe + atomic JPEG snapshot)"
                );
                spawn_ffmpeg_h264(&state, &pipeline)
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
        if let Pipeline::FfmpegRescale { target_format, .. } = &pipeline {
            if let Some(stdout) = child.stdout.take() {
                let counter = std::sync::Arc::clone(&state.0.frames_published);
                if target_format == "h264" {
                    // H.264: parse NAL units → access units → broadcast.
                    let tx = state.0.h264_tx.clone();
                    tokio::spawn(crate::h264_pipe::run(stdout, tx, counter));
                    info!("h264_pipe reader spawned for this ffmpeg run");
                } else {
                    // MJPEG: parse SOI/EOI frames → latest-frame watch.
                    let tx = state.0.frame_tx.clone();
                    tokio::spawn(crate::jpeg_pipe::run(stdout, tx, counter));
                    info!("jpeg_pipe reader spawned for this ffmpeg run");
                }
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
        // NOTE: deliberately NOT --notify-parent. That flag makes ustreamer
        // send SIGUSR2 to us on every stream online/offline transition, but
        // aeon-streamer installs no SIGUSR2 handler — the default disposition
        // is "terminate", so the first notify killed the supervisor and the
        // service crash-looped (status=12/USR2). It was masked for as long as
        // ustreamer never started (the old invalid --encoder value made it
        // exit before notifying); fixing the encoder unmasked the kill. We
        // don't need the notify anyway — watchdog.rs independently detects
        // ustreamer online/offline via the API socket and signals relaunch.
        .arg("--no-log-colors");

    // Cap the capture/encode rate. ustreamer's HW encoder only handles
    // (M)JPEG input — our capture is raw YUYV, so ustreamer silently falls
    // back to its CPU encoder and software-encodes EVERY grabbed frame.
    // Left uncapped it grabs at the source rate (the Cam Link offers 60fps),
    // which at 720p pegs ~2.3 CPU cores and starves the supervisor (sluggish
    // web UI, dropped sessions, HTTP/2 stream resets). `--desired-fps`
    // throttles the grab+encode rate; 0 means "unlimited" to ustreamer, so
    // only pass it when a positive cap is configured. This finally wires up
    // output.fps for the direct-MJPEG path — previously it affected only the
    // ffmpeg/H.264 pipeline (capture.rs `-r target_fps`), never this one.
    if cfg.output.fps > 0 {
        cmd.arg(format!("--desired-fps={}", cfg.output.fps));
    }

    // Encoder selection. CRITICAL — and counter-intuitive:
    //
    // The shipped ustreamer 4.9 (Debian/Pi OS `ustreamer` package) is built
    // WITHOUT M2M support. `ustreamer --help` lists only: CPU, HW, NOOP.
    // (`m2m-image`/`M2M-IMAGE` → "Unknown encoder type" → exit 1, which is
    // what crash-looped the whole ≤1080p path before.)
    //
    // `HW` here is NOT the Pi's hardware JPEG block — it's ustreamer's
    // "device hardware internal encoder", i.e. a (M)JPEG→(M)JPEG passthrough.
    // Our capture is raw YUYV, so ustreamer logs "Switching to CPU encoder:
    // the input format is not (M)JPEG" and software-encodes anyway. So on
    // this build HW ≈ CPU for our input — there is NO hardware JPEG path.
    // (True HW offload would need ustreamer rebuilt WITH_M2M, or the ffmpeg
    // h264_v4l2m2m pipeline — output.format="h264" — which DOES use the Pi's
    // hardware H.264 encoder. That's the real low-CPU answer; see capture.rs.)
    //
    // We keep HW for Pi4 (harmless passthrough if a future device ever hands
    // us MJPEG; identical to CPU otherwise) and explicit multi-worker CPU
    // elsewhere. The actual CPU containment is the --desired-fps cap above
    // plus the CPUQuota ceiling on aeon-streamer.service.
    match cfg.platform {
        Platform::Pi4 => {
            cmd.arg("--encoder=HW");
        }
        _ => {
            cmd.arg("--encoder=CPU").arg("--workers=4");
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
    //
    // HDMI-CSI (rp1-cfe) captures a desktop — no pillarbox to crop, and the
    // low-probesize cropdetect pass stalls on that node — so skip it there.
    let detected = if cfg.capture.source == Source::HdmiCsi {
        Ok(None)
    } else {
        capture::detect_content_crop(
            &cfg.ffmpeg_bin,
            &cfg.device,
            source_format,
            source_resolution,
            *source_fps,
        )
    };
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
        // in_range=full:out_range=jpeg — CRITICAL for correct color.
        // The Cam Link (capturing a computer's HDMI) delivers FULL-range
        // YUV (NV12/YU12 at 4K), but swscale defaults to assuming
        // limited/TV range on these planar formats → a heavy green/
        // magenta cast (ffmpeg even warns "deprecated pixel format used,
        // make sure you did set range correctly"). Declaring the input
        // full-range and keeping it full-range for the JPEG output fixes
        // the cast. Verified on-device against a 4K MacBook source.
        format!(
            "scale={w}:{h}:flags={alg}:in_range=full:out_range=jpeg",
            w = out_w,
            h = out_h,
            alg = scale_algorithm
        )
    };
    // Decimate before scale (same reasoning as the h264 path): swscale runs
    // per input frame, so without this it processes the full 60fps capture
    // even when target_fps is lower. fps filter at the head fixes that.
    // Append the configured display rotation/flip last (after crop+scale), so
    // the served MJPEG snapshot/stream carries the same orientation as the
    // H.264 path. `None` for the identity orientation.
    let orient = cfg.capture.orientation_vf();
    let scale_filter = {
        let core = match &crop_filter {
            Some(c) => format!("fps={target_fps},{c},{scale_part}"),
            None => format!("fps={target_fps},{scale_part}"),
        };
        match orient {
            Some(rot) => format!("{core},{rot}"),
            None => core,
        }
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

    // The rp1-cfe CSI node (HDMI-CSI) has no frame-rate ioctl and needs a real
    // probe before it delivers frames — the Cam-Link-tuned 32-byte probe just
    // stalls it ("not enough frames to estimate rate"). The Cam Link keeps the
    // minimal probe for near-zero startup latency. [Confirmed on Pi 5 + X1301.]
    let (probesize, analyzeduration) = if cfg.capture.source == Source::HdmiCsi {
        ("20M", "5M")
    } else {
        ("32", "0")
    };
    let mut cmd = Command::new(&cfg.ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("warning")
        .arg("-y")
        // ── Latency-cutting INPUT flags (kept from v46) ──
        // These attack ffmpeg's start-up and steady-state buffering
        // without altering the encoder/muxer side. They're safe.
        //
        //   -fflags nobuffer    Skip frame buffering inside the
        //                       demuxer.
        //   -flags  low_delay   Generic "minimize latency" hint.
        //   -avioflags direct   Bypass libavformat's I/O buffering.
        //   -probesize  32      Skip stream auto-detection (we know
        //                       the format already). Saves ~5s of
        //                       startup latency.
        //   -analyzeduration 0  Ditto.
        //
        // (v46's `-thread_queue_size 4` removed — default 8 is fine
        // and 4 caused back-pressure into the v4l2 demuxer when the
        // software MJPEG encoder couldn't keep up, dropping the
        // visible fps to ~7. Default is the right call.)
        .arg("-fflags").arg("nobuffer")
        .arg("-flags").arg("low_delay")
        .arg("-avioflags").arg("direct")
        .arg("-probesize").arg(probesize)
        .arg("-analyzeduration").arg(analyzeduration)
        // Input
        .arg("-f").arg("v4l2")
        .arg("-input_format").arg(source_format)
        .arg("-video_size").arg(source_resolution)
        .arg("-framerate").arg(source_fps.to_string())
        .arg("-i").arg(&cfg.device)
        // Filter — scale to target resolution
        .arg("-vf").arg(&scale_filter)
        // ── Output / encoding ──
        //
        // -r 30 (NOT -fps_mode passthrough): force ffmpeg to emit at
        // a steady 30 fps. v46 tried `-fps_mode passthrough` to skip
        // re-timing — that exposed the real source rate (sometimes
        // 7-8 fps when the Cam Link delivers slowly) and resulted in
        // visible stutter. Steady 30 fps with frame duplication on
        // slow input is smoother for the human eye + agent screen-
        // grabs at the cost of a few extra bytes per second over the
        // LAN (which is plenty fast).
        //
        // -flush_packets removed: it was causing the muxer to split
        // JPEG frames across multiple writes more aggressively, which
        // exposed a latent bug in the SOI/EOI parser when ffmpeg
        // wrote partial frames AND it interacted with `nobuffer` to
        // produce torn frames on the wire ("top segment only" was the
        // symptom). image2pipe's default behavior emits one whole
        // JPEG per write — which is what jpeg_pipe::run expects.
        .arg("-r").arg(target_fps.to_string())
        .arg("-c:v").arg("mjpeg")
        .arg("-q:v").arg(&qv)
        .arg("-f").arg("image2pipe")
        .arg("pipe:1")
        // Pipe stdout so jpeg_pipe::run can read frames. stderr stays
        // inherited so ffmpeg's warnings/errors land in our journal.
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    Ok(cmd.spawn()?)
}

/// H.264 pipeline (v64). One ffmpeg, one capture-device open, TWO outputs
/// fed from a single scaled source via the `split` filter:
///
///   output 1: H.264 Annex-B → stdout pipe → `h264_pipe::run` → broadcast
///             → `/h264` → supervisor WebSocket → browser WebCodecs. This
///             is the low-latency live path replacing the buffered
///             multipart-MJPEG `<img>`.
///
///   output 2: a single JPEG, atomically rewritten every frame, at the
///             snapshot path. `/snapshot` reads it (agents depend on that),
///             and the MJPEG `/stream` fallback re-reads it for browsers
///             without WebCodecs.
///
/// NOTE (hardware-in-the-loop): the encoder name, `-bsf:v dump_extra`,
/// `-atomic_writing`, and the exact low-latency flags all want validation
/// against the Pi 4's `h264_v4l2m2m`. This is the apple-mt-style frontier
/// where on-device truth beats theory.
/// Parse a "WxH" resolution string (e.g. "1920x1080") into (w, h).
fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Cap a resolution to the Pi 4 hardware H.264 encoder's 1920×1080 ceiling,
/// preserving aspect ratio (even dimensions for chroma). Returns the input
/// unchanged when it already fits — the no-rescale `match_source` case.
fn cap_1080p(w: u32, h: u32) -> (u32, u32) {
    if w <= 1920 && h <= 1080 {
        return (w, h);
    }
    let factor = f32::min(1920.0 / w as f32, 1080.0 / h as f32);
    let cw = (((w as f32 * factor).round() as u32) / 2) * 2;
    let ch = (((h as f32 * factor).round() as u32) / 2) * 2;
    (cw.max(2), ch.max(2))
}

fn spawn_ffmpeg_h264(state: &SharedState, pipeline: &Pipeline) -> Result<Child> {
    let cfg = &state.0.cfg;
    let out = &cfg.output;

    let Pipeline::FfmpegRescale {
        source_format,
        source_resolution,
        source_fps,
        target_width,
        target_height,
        target_fps,
        scale_algorithm,
        ..
    } = pipeline
    else {
        anyhow::bail!("spawn_ffmpeg_h264 called with non-ffmpeg pipeline");
    };

    if let Some(parent) = out.snapshot_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Reuse the same pillarbox/letterbox crop detection as the MJPEG path
    // so HID coordinates still map 1:1 to the host's logical display.
    // (Skip it for HDMI-CSI: a desktop has no pillarbox + the cropdetect pass
    // stalls on the rp1-cfe node.)
    let detected = if cfg.capture.source == Source::HdmiCsi {
        Ok(None)
    } else {
        capture::detect_content_crop(
            &cfg.ffmpeg_bin,
            &cfg.device,
            source_format,
            source_resolution,
            *source_fps,
        )
    };
    let crop_filter = match &detected {
        Ok(Some(c)) => {
            info!(w = c.width, h = c.height, x = c.x, y = c.y, "h264: content crop detected");
            Some(c.as_filter())
        }
        _ => None,
    };
    // The frame resolution *after* the optional content-crop — the "natural"
    // res we'd emit with no rescaling at all.
    let (nat_w, nat_h) = if let Ok(Some(c)) = &detected {
        (c.width, c.height)
    } else {
        parse_wxh(source_resolution).unwrap_or((*target_width, *target_height))
    };

    // Output dimensions. With `match_source`, emit the natural res capped at
    // the encoder's 1920×1080 ceiling (≤1080p → native; >1080p → fit 1080p).
    // Otherwise keep the configured target (width derived from cropped aspect).
    let (out_w, out_h) = if out.match_source {
        cap_1080p(nat_w, nat_h)
    } else if let Ok(Some(c)) = &detected {
        let h = *target_height;
        let w = ((h as f32 * c.aspect()).round() as u32 / 2) * 2; // even for chroma
        (w, h)
    } else {
        (*target_width, *target_height)
    };

    // Skip the scale filter when the output already equals the natural res —
    // no point resampling 1:1 (saves CPU and avoids resample softness). This
    // is the common `match_source` path for a ≤1080p source.
    let need_scale = (out_w, out_h) != (nat_w, nat_h);
    let scale_part = if !need_scale {
        // match_source, no resample. Still force full→limited range so the
        // Cam Link's full-range NV12/YU12 doesn't get mis-mapped (same
        // green/magenta cast the MJPEG path hit). `scale` with no size
        // change is a cheap way to attach the range conversion; h264
        // conventionally carries limited (tv) range.
        format!("scale=in_range=full:out_range=tv")
    } else if out.hw_accel {
        format!("scale_v4l2m2m={out_w}:{out_h}")
    } else {
        // See the MJPEG path for why in_range=full matters. h264 carries
        // limited (tv) range by convention, so out_range=tv here.
        format!("scale={out_w}:{out_h}:flags={scale_algorithm}:in_range=full:out_range=tv")
    };
    // Assemble crop + (optional) scale; either may be absent.
    // Decimate to the target rate FIRST, before the (software) scale/convert.
    // The Cam Link delivers 60fps and we ingest at 60 (source_fps), but the
    // swscale NV12→yuv420p + full→tv range conversion is the dominant CPU
    // cost — and it runs per *input* frame. Dropping `-r` only at the encoder
    // leaves swscale chewing all 60fps, so lowering output fps did NOT reduce
    // CPU (measured on-device: 20fps and 30fps both pegged ~2 cores). Putting
    // an `fps` filter at the HEAD of the graph means crop/scale/convert only
    // touch target_fps frames — roughly halving CPU at 30fps and making the
    // fps knob actually control cost. The `fps` filter itself is cheap (it
    // selects frames by PTS; no per-pixel work).
    let fps_part = format!("fps={target_fps}");
    // Display rotation/flip applied AFTER scale (in the landscape target geometry);
    // a 90/270 transpose then yields the portrait frame the encoder + split both
    // see, so H.264 and the JPEG snapshot stay in lock-step.
    let orient = cfg.capture.orientation_vf();
    let base = [
        fps_part.as_str(),
        crop_filter.as_deref().unwrap_or(""),
        scale_part.as_str(),
        orient.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(",");
    // One filtered source split into two outputs (H.264 + JPEG snapshot).
    let filter_complex = format!("[0:v]{base},split=2[vh][vj]");

    // MJPEG snapshot quality (same 1–100 → q:v 2–15 mapping as the MJPEG path).
    let qv = {
        let inverted = 31u32.saturating_sub(((cfg.jpeg_quality as u32) * 31) / 100);
        inverted.clamp(2, 15).to_string()
    };
    // Snapshot runs at a modest fps — agents grab it occasionally, and the
    // software MJPEG encoder (1080p JPEG per frame) shouldn't compete with
    // HW H.264 for CPU. Capped well below the live fps: a screenshot grabs
    // the latest frame regardless, and the MJPEG <img> fallback only needs
    // to be usable, not smooth.
    let snap_fps = (*target_fps).clamp(1, 6).to_string();

    let venc = match cfg.platform {
        Platform::Pi4 => "h264_v4l2m2m",
        _ => "libx264",
    };
    let bitrate = format!("{}k", out.h264_bitrate_kbps.max(500));
    let gop = out.h264_gop.max(1).to_string();
    let snapshot_path = out.snapshot_path.display().to_string();

    // HDMI-CSI (rp1-cfe) needs a real probe to start; Cam Link keeps the
    // minimal one for low startup latency. (See spawn_ffmpeg.)
    let (probesize, analyzeduration) = if cfg.capture.source == Source::HdmiCsi {
        ("20M", "5M")
    } else {
        ("32", "0")
    };
    let mut cmd = Command::new(&cfg.ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("warning")
        .arg("-y")
        // Latency-cutting input flags (same as the MJPEG pipeline).
        .arg("-fflags").arg("nobuffer")
        .arg("-flags").arg("low_delay")
        .arg("-avioflags").arg("direct")
        .arg("-probesize").arg(probesize)
        .arg("-analyzeduration").arg(analyzeduration)
        // Input
        .arg("-f").arg("v4l2")
        .arg("-input_format").arg(source_format)
        .arg("-video_size").arg(source_resolution)
        .arg("-framerate").arg(source_fps.to_string())
        .arg("-i").arg(&cfg.device)
        .arg("-filter_complex").arg(&filter_complex)
        // ── output 1: H.264 Annex-B → stdout ──
        .arg("-map").arg("[vh]")
        .arg("-r").arg(target_fps.to_string())
        .arg("-c:v").arg(venc)
        .arg("-b:v").arg(&bitrate)
        .arg("-g").arg(&gop)
        .arg("-bf").arg("0") // no B-frames → no reorder latency
        // KEEP yuv420p. Feeding the Pi's h264_v4l2m2m encoder nv12 directly
        // (to skip the swscale repack) produced badly corrupted output —
        // green macroblocks + ghosting — so the small CPU win isn't worth
        // it. swscale's yuv420p conversion path is clean.
        .arg("-pix_fmt").arg("yuv420p");
    if venc == "libx264" {
        // Dev-box / Pi 5 software path: make it as low-latency as possible.
        cmd.arg("-preset").arg("ultrafast").arg("-tune").arg("zerolatency");
    }
    // Inline SPS/PPS ahead of every keyframe so a client connecting
    // mid-stream can configure WebCodecs from the next IDR.
    cmd.arg("-bsf:v").arg("dump_extra=freq=keyframe")
        .arg("-f").arg("h264")
        .arg("pipe:1")
        // ── output 2: atomic single-frame JPEG for /snapshot + fallback ──
        .arg("-map").arg("[vj]")
        .arg("-r").arg(&snap_fps)
        .arg("-c:v").arg("mjpeg")
        .arg("-q:v").arg(&qv)
        .arg("-update").arg("1")
        .arg("-atomic_writing").arg("1")
        .arg("-f").arg("image2")
        .arg(&snapshot_path)
        // stdout carries the H.264 stream for h264_pipe; stderr → journal.
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    Ok(cmd.spawn()?)
}

/// Camera-CSI pipeline (Pi 5 + a Raspberry Pi camera, e.g. the HQ Camera /
/// IMX477). A Pi camera's v4l2 node is raw Bayer needing the ISP, so ffmpeg
/// can't `-f v4l2` it directly — we go through libcamera. `rpicam-vid` emits
/// an **MJPEG** stream (self-describing, so ffmpeg parses frame boundaries
/// with no width/height/stride assumptions — robust against the buffer
/// alignment that bites the raw-yuv420 route at non-16-aligned heights like
/// 1080). ffmpeg ingests it and splits into the SAME two sinks as the Cam
/// Link H.264 path:
///
///   output 1: H.264 Annex-B → stdout → `h264_pipe::run` → WebSocket/WebCodecs
///   output 2: a single JPEG, atomically rewritten → `/snapshot` (+ the MJPEG
///             `<img>` fallback)
///
/// Returns `(ffmpeg child, rpicam-vid child)`. The caller owns BOTH; it must
/// kill the rpicam sidecar on relaunch/shutdown so the sensor is released
/// before the next spawn.
fn spawn_libcamera_h264(state: &SharedState) -> Result<(Child, Child)> {
    let cfg = &state.0.cfg;
    let out = &cfg.output;

    if let Some(parent) = out.snapshot_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let w = out.width.max(2);
    let h = out.height.max(2);
    let fps = out.fps.max(1);
    let cam_id = cfg.capture.camera_id;

    // ── Stage 1: rpicam-vid → MJPEG on stdout ──
    // NOTE: orientation (rotation/flip) is realised ONLY in the ffmpeg filter
    // graph below (see `orientation_vf` in the filter_complex). Do NOT also add
    // rpicam-vid `--rotation`/`--hflip`/`--vflip` here — that would rotate the
    // frame twice. The single-layer (ffmpeg) model is deliberate: it keeps the
    // H.264 stream, the JPEG snapshot, recordings, and the vision tap identical
    // and dodges rpicam's 0/180-only `--rotation` limitation.
    let mut rpicam = Command::new("rpicam-vid");
    rpicam
        .arg("--camera").arg(cam_id.to_string())
        .arg("-t").arg("0") // run until killed
        .arg("--nopreview")
        .arg("--flush") // flush each frame for lower latency
        .arg("--width").arg(w.to_string())
        .arg("--height").arg(h.to_string())
        .arg("--framerate").arg(fps.to_string())
        .arg("--codec").arg("mjpeg")
        .arg("--quality").arg("90")
        .arg("-o").arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut rpicam_child = rpicam.spawn().map_err(|e| {
        anyhow::anyhow!("spawning rpicam-vid (is rpicam-apps installed + a camera attached?): {e}")
    })?;
    // Hand rpicam's stdout pipe to ffmpeg's stdin (the tokio documented
    // ChildStdout → Stdio handoff).
    let rpicam_stdio: Stdio = rpicam_child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("rpicam-vid produced no stdout pipe"))?
        .try_into()
        .map_err(|e| anyhow::anyhow!("rpicam stdout → Stdio: {e}"))?;

    // ── Stage 2: ffmpeg ingests MJPEG, splits to H.264 + JPEG snapshot ──
    // Pi 5 has no HW H.264 encoder, so libx264 (already the non-Pi4 default).
    let venc = match cfg.platform {
        Platform::Pi4 => "h264_v4l2m2m",
        _ => "libx264",
    };
    let bitrate = format!("{}k", out.h264_bitrate_kbps.max(500));
    let gop = out.h264_gop.max(1).to_string();
    let qv = {
        let inverted = 31u32.saturating_sub(((cfg.jpeg_quality as u32) * 31) / 100);
        inverted.clamp(2, 15).to_string()
    };
    let snap_fps = fps.clamp(1, 6).to_string();
    let snapshot_path = out.snapshot_path.display().to_string();
    // No crop (a camera has no pillarbox) and no scale (rpicam already emits
    // w×h). Decimate to target fps at the head, apply the configured display
    // rotation/flip, then split — so the H.264 stream AND the JPEG snapshot are
    // rotated identically (Pi-camera mounts are often physically turned).
    let filter_complex = match cfg.capture.orientation_vf() {
        Some(rot) => format!("[0:v]fps={fps},{rot},split=2[vh][vj]"),
        None => format!("[0:v]fps={fps},split=2[vh][vj]"),
    };

    let mut cmd = Command::new(&cfg.ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("warning")
        .arg("-y")
        .arg("-fflags").arg("nobuffer")
        .arg("-flags").arg("low_delay")
        // Input: MJPEG stream from rpicam-vid's stdout.
        .arg("-f").arg("mjpeg")
        .arg("-i").arg("pipe:0")
        .arg("-filter_complex").arg(&filter_complex)
        // output 1: H.264 Annex-B → stdout
        .arg("-map").arg("[vh]")
        .arg("-r").arg(fps.to_string())
        .arg("-c:v").arg(venc)
        .arg("-b:v").arg(&bitrate)
        .arg("-g").arg(&gop)
        .arg("-bf").arg("0")
        .arg("-pix_fmt").arg("yuv420p");
    if venc == "libx264" {
        cmd.arg("-preset").arg("ultrafast").arg("-tune").arg("zerolatency");
    }
    cmd.arg("-bsf:v").arg("dump_extra=freq=keyframe")
        .arg("-f").arg("h264")
        .arg("pipe:1")
        // output 2: atomic single-frame JPEG snapshot
        .arg("-map").arg("[vj]")
        .arg("-r").arg(&snap_fps)
        .arg("-c:v").arg("mjpeg")
        .arg("-q:v").arg(&qv)
        .arg("-update").arg("1")
        .arg("-atomic_writing").arg("1")
        .arg("-f").arg("image2")
        .arg(&snapshot_path)
        .stdin(rpicam_stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let ffmpeg_child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning ffmpeg (libcamera pipeline): {e}"))?;
    Ok((ffmpeg_child, rpicam_child))
}
