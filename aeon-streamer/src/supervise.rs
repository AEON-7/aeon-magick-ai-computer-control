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
        .arg("-probesize").arg("32")
        .arg("-analyzeduration").arg("0")
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
    let detected = capture::detect_content_crop(
        &cfg.ffmpeg_bin,
        &cfg.device,
        source_format,
        source_resolution,
        *source_fps,
    );
    let crop_filter = match &detected {
        Ok(Some(c)) => {
            info!(w = c.width, h = c.height, x = c.x, y = c.y, "h264: content crop detected");
            Some(c.as_filter())
        }
        _ => None,
    };
    let (out_w, out_h) = if let Ok(Some(c)) = &detected {
        let h = *target_height;
        let w = ((h as f32 * c.aspect()).round() as u32 / 2) * 2; // even for chroma
        (w, h)
    } else {
        (*target_width, *target_height)
    };
    let scale_part = if out.hw_accel {
        format!("scale_v4l2m2m={out_w}:{out_h}")
    } else {
        format!("scale={out_w}:{out_h}:flags={scale_algorithm}")
    };
    let base = match crop_filter {
        Some(c) => format!("{c},{scale_part}"),
        None => scale_part,
    };
    // One filtered source split into two outputs (H.264 + JPEG snapshot).
    let filter_complex = format!("[0:v]{base},split=2[vh][vj]");

    // MJPEG snapshot quality (same 1–100 → q:v 2–15 mapping as the MJPEG path).
    let qv = {
        let inverted = 31u32.saturating_sub(((cfg.jpeg_quality as u32) * 31) / 100);
        inverted.clamp(2, 15).to_string()
    };
    // Snapshot runs at a modest fps — agents grab it occasionally, and the
    // software MJPEG encoder shouldn't compete with HW H.264 for CPU.
    let snap_fps = (*target_fps).clamp(1, 12).to_string();

    let venc = match cfg.platform {
        Platform::Pi4 => "h264_v4l2m2m",
        _ => "libx264",
    };
    let bitrate = format!("{}k", out.h264_bitrate_kbps.max(500));
    let gop = out.h264_gop.max(1).to_string();
    let snapshot_path = out.snapshot_path.display().to_string();

    let mut cmd = Command::new(&cfg.ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("warning")
        .arg("-y")
        // Latency-cutting input flags (same as the MJPEG pipeline).
        .arg("-fflags").arg("nobuffer")
        .arg("-flags").arg("low_delay")
        .arg("-avioflags").arg("direct")
        .arg("-probesize").arg("32")
        .arg("-analyzeduration").arg("0")
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
