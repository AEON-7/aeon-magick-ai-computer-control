//! Sniff `/dev/video0`'s current v4l2 capabilities to decide which
//! `--format` and `--resolution` to pass to ustreamer. The Cam Link 4K
//! UVC enumeration changes based on the source signal:
//!   * source ≤1080p60: offers YUYV, NV12, YU12
//!   * source 4K30:     offers ONLY NV12 + YU12 (no YUYV!)
//!
//! ustreamer can capture YUYV / UYVY / YUV420 (= YU12) / MJPEG. It can NOT
//! capture NV12 in stock builds (it's semi-planar; ustreamer expects
//! planar/packed).
//!
//! Strategy: prefer YU12/YUV420 because Cam Link offers it at every
//! resolution. Fall back to YUYV / UYVY / MJPEG if YU12 isn't present
//! (other USB capture sticks may not offer it).

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureMode {
    pub format: &'static str,    // ustreamer's --format value
    pub resolution: String,      // "1920x1080"
}

/// Run `v4l2-ctl --list-formats-ext` and pick the best ustreamer-compatible mode.
///
/// Legacy entry point kept around for tooling / tests. The supervise loop
/// uses `detect_pipeline` which returns either an ustreamer- or
/// ffmpeg-flavored pipeline.
#[allow(dead_code)]
pub fn detect(device: &Path) -> Result<CaptureMode> {
    let out = Command::new("v4l2-ctl")
        .args(["-d"])
        .arg(device)
        .arg("--list-formats-ext")
        .output()
        .context("running v4l2-ctl --list-formats-ext")?;
    if !out.status.success() {
        return Err(anyhow!(
            "v4l2-ctl --list-formats-ext exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let modes = parse_modes(&text);
    pick_best(modes)
}

/// Lower-overhead than `--list-formats-ext`: gets the device's CURRENT
/// configured format. Useful to keep the size we ask for matching what the
/// device wants to give us, even when the enum has multiple options.
#[allow(dead_code)] // utility for future supervise.rs tuning
pub fn current_resolution(device: &Path) -> Option<String> {
    let out = Command::new("v4l2-ctl")
        .args(["-d"])
        .arg(device)
        .arg("--get-fmt-video")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Width/Height") {
            // "Width/Height      : 1920/1080"
            let val = rest.split(':').nth(1)?.trim();
            let (w, h) = val.split_once('/')?;
            return Some(format!("{}x{}", w.trim(), h.trim()));
        }
    }
    None
}

/// MD5 of the device's enumerated format list. We hash this and re-detect
/// on changes — that's our cheap "source signal changed" signal for USB UVC
/// devices that don't support V4L2 DV-timings.
pub fn enum_signature(device: &Path) -> Result<String> {
    let out = Command::new("v4l2-ctl")
        .args(["-d"])
        .arg(device)
        .arg("--list-formats-ext")
        .output()
        .context("running v4l2-ctl --list-formats-ext")?;
    if !out.status.success() {
        return Err(anyhow!("v4l2-ctl failed"));
    }
    Ok(format!("{:x}", md5::compute(&out.stdout)))
}

/// Result of an ffmpeg-cropdetect pass over a few seconds of the live HDMI
/// signal. Tells us how the source is pillarboxed / letterboxed so we can
/// crop out the black bars and present the agent with an aspect-correct
/// frame matching the host's logical display.
#[derive(Debug, Clone, Copy)]
pub struct ContentCrop {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

impl ContentCrop {
    /// Format as ffmpeg `crop=W:H:X:Y` filter argument.
    pub fn as_filter(&self) -> String {
        format!("crop={}:{}:{}:{}", self.width, self.height, self.x, self.y)
    }
    /// Aspect ratio of the cropped content.
    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }
}

/// Run ffmpeg with `cropdetect` for ~2 seconds against the capture device,
/// then parse the resulting `crop=W:H:X:Y` line from its stderr. ffmpeg's
/// cropdetect filter analyzes pixel rows/columns at the edges to identify
/// black bars (pillarbox or letterbox). What it returns is the rectangle
/// that excludes those bars.
///
/// Returns `Ok(None)` when detection found nothing better than the full
/// frame (no bars present) so the caller can skip a no-op crop filter.
pub fn detect_content_crop(
    ffmpeg_bin: &Path,
    device: &Path,
    source_format: &str,
    source_resolution: &str,
    source_fps: u32,
) -> Result<Option<ContentCrop>> {
    use std::time::Duration;

    let (sw, sh) = source_resolution
        .split_once('x')
        .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
        .ok_or_else(|| anyhow!("bad source_resolution: {}", source_resolution))?;

    // ffmpeg cropdetect args: limit=24 (treat pixel <= 24 as black), round=2
    // (snap crop dims to even numbers — needed for chroma subsampled YUV),
    // reset_count=0 (one shot, don't re-detect on every frame).
    let mut cmd = Command::new(ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel").arg("info")
        .arg("-f").arg("v4l2")
        .arg("-input_format").arg(source_format)
        .arg("-video_size").arg(source_resolution)
        .arg("-framerate").arg(source_fps.to_string())
        .arg("-i").arg(device)
        .arg("-vf").arg("cropdetect=24:2:0")
        .arg("-frames:v").arg("60") // ~2s at 30fps
        .arg("-f").arg("null")
        .arg("-")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("spawning ffmpeg cropdetect")?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("ffmpeg stderr"))?;

    // Read with a 5-second timeout cap. Don't block forever if ffmpeg hangs.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut last_crop: Option<String> = None;
        for line in BufReader::new(stderr).lines().flatten() {
            // Look for "crop=W:H:X:Y" in cropdetect lines.
            if let Some(idx) = line.find("crop=") {
                let tail = &line[idx + 5..];
                let crop = tail
                    .split(|c: char| c == ' ' || c == ']')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !crop.is_empty() {
                    last_crop = Some(crop);
                }
            }
        }
        let _ = tx.send(last_crop);
    });

    // Wait up to 5s for ffmpeg to finish + parse.
    let wait_start = std::time::Instant::now();
    while wait_start.elapsed() < Duration::from_secs(5) {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(anyhow!("cropdetect wait: {e}")),
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    let parsed = match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Some(s)) => s,
        _ => return Ok(None),
    };

    // Parse "W:H:X:Y"
    let parts: Vec<&str> = parsed.split(':').collect();
    if parts.len() != 4 {
        return Ok(None);
    }
    let w: u32 = parts[0].parse().context("crop W")?;
    let h: u32 = parts[1].parse().context("crop H")?;
    let x: u32 = parts[2].parse().context("crop X")?;
    let y: u32 = parts[3].parse().context("crop Y")?;

    // If the detected crop equals the full frame (no bars), skip.
    if w >= sw && h >= sh && x == 0 && y == 0 {
        return Ok(None);
    }
    Ok(Some(ContentCrop { width: w, height: h, x, y }))
}

#[derive(Debug, Clone)]
struct ParsedMode {
    fourcc: String,
    resolutions: Vec<String>,
}

fn parse_modes(text: &str) -> Vec<ParsedMode> {
    // We're scraping output that looks like:
    //   [0]: 'YUYV' (YUYV 4:2:2)
    //       Size: Discrete 1920x1080
    //           Interval: Discrete 0.017s (60.000 fps)
    let mut modes: Vec<ParsedMode> = Vec::new();
    let mut current: Option<ParsedMode> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("[").and_then(|s| s.split_once(']')) {
            // new format block
            if let Some(m) = current.take() {
                modes.push(m);
            }
            if let Some((_, after)) = rest.1.split_once('\'') {
                if let Some((fourcc, _)) = after.split_once('\'') {
                    current = Some(ParsedMode {
                        fourcc: fourcc.to_string(),
                        resolutions: Vec::new(),
                    });
                }
            }
        } else if let Some(c) = current.as_mut() {
            if let Some(rest) = line.strip_prefix("Size: Discrete ") {
                c.resolutions.push(rest.trim().to_string());
            }
        }
    }
    if let Some(m) = current {
        modes.push(m);
    }
    modes
}

/// What execution model the supervise loop should use for this device.
///
/// `Ustreamer` is the fast happy path: capture device offers something
/// ustreamer can ingest (YUYV / UYVY / MJPG), we just pipe it through.
///
/// `FfmpegRescale` is the fallback for Cam Link at 4K, which only offers
/// NV12/YU12. ustreamer can't speak those, so we spawn ffmpeg to ingest
/// NV12, rescale to the user's configured target resolution, and serve
/// the result. Output goes to both a single-frame JPEG file (for
/// /snapshot) and an mpjpeg TCP server (for /stream).
#[derive(Debug, Clone)]
pub enum Pipeline {
    Ustreamer(CaptureMode),
    FfmpegRescale {
        /// V4L2 fourcc in lowercase, as ffmpeg expects: "nv12" or "yuv420p".
        source_format: String,
        /// e.g. "3840x2160".
        source_resolution: String,
        /// e.g. 30.
        source_fps: u32,
        /// Output width × height (from streamer.toml).
        target_width: u32,
        target_height: u32,
        target_fps: u32,
        /// "mjpeg" or "h264".
        target_format: String,
        /// "lanczos" | "bicubic" | "neighbor" | ...
        scale_algorithm: String,
    },
}

/// Pipeline-level selection. Tries the ustreamer-compatible formats first;
/// falls back to ffmpeg rescale for NV12/YU12-only sources (Cam Link at 4K).
/// Called via `detect_pipeline` (the public entry point).
fn pick_pipeline(modes: Vec<ParsedMode>, output: &crate::config::Output) -> Result<Pipeline> {
    // Try the fast path first.
    if let Ok(cap) = pick_best(modes.clone()) {
        return Ok(Pipeline::Ustreamer(cap));
    }
    // Look for NV12 or YU12 to feed ffmpeg.
    let candidate = modes
        .iter()
        .find(|m| m.fourcc == "NV12")
        .or_else(|| modes.iter().find(|m| m.fourcc == "YU12"));
    if let Some(m) = candidate {
        if let Some(res) = m.resolutions.first() {
            // V4L2 "NV12" → ffmpeg "-input_format nv12"
            // V4L2 "YU12" → ffmpeg "-input_format yuv420p" (the planar variant)
            let src_fmt = match m.fourcc.as_str() {
                "NV12" => "nv12",
                "YU12" => "yuv420p",
                other => other,
            };
            return Ok(Pipeline::FfmpegRescale {
                source_format: src_fmt.into(),
                source_resolution: res.clone(),
                source_fps: 30,
                target_width: output.width,
                target_height: output.height,
                target_fps: output.fps,
                target_format: output.format.clone(),
                scale_algorithm: output.scale_algorithm.clone(),
            });
        }
    }
    let offered: Vec<&String> = modes.iter().map(|m| &m.fourcc).collect();
    Err(anyhow!(
        "no compatible v4l2 format found. enum: {:?}",
        offered
    ))
}

/// Convenience: run the full detect + pipeline pick in one call.
pub fn detect_pipeline(device: &Path, output: &crate::config::Output) -> Result<Pipeline> {
    let out = Command::new("v4l2-ctl")
        .args(["-d"])
        .arg(device)
        .arg("--list-formats-ext")
        .output()
        .context("running v4l2-ctl --list-formats-ext")?;
    if !out.status.success() {
        return Err(anyhow!(
            "v4l2-ctl --list-formats-ext exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    pick_pipeline(parse_modes(&text), output)
}

fn pick_best(modes: Vec<ParsedMode>) -> Result<CaptureMode> {
    // ustreamer's --format flag only accepts: YUYV, UYVY, RGB565, RGB24,
    // MJPEG, JPEG. Anything else (YU12 / NV12 / YV12 — Cam Link's 4K-only
    // formats) is not actually ingestible by ustreamer; passing the name
    // anyway makes ustreamer immediately exit and us crash-loop spawning it.
    //
    // So we ONLY map v4l2 fourccs that ustreamer truly supports. If a
    // device offers nothing else (Cam Link at 4K-only NV12/YU12 is the
    // canonical case), error out with a helpful pointer so the user knows
    // to drop the source's resolution to 1080p — at 1080p Cam Link
    // re-advertises YUYV alongside the YUV planar formats.
    let pref: &[(&str, &str)] = &[
        ("YUYV", "YUYV"),
        ("UYVY", "UYVY"),
        ("MJPG", "MJPEG"),
    ];
    for (fourcc, ustream_fmt) in pref {
        if let Some(m) = modes.iter().find(|m| m.fourcc == *fourcc) {
            if let Some(res) = m.resolutions.first() {
                return Ok(CaptureMode {
                    format: ustream_fmt,
                    resolution: res.clone(),
                });
            }
        }
    }
    let offered: Vec<&String> = modes.iter().map(|m| &m.fourcc).collect();
    Err(anyhow!(
        "no ustreamer-compatible format offered by capture device. v4l2 enum: {:?}. \
         If you see only NV12/YU12, the upstream HDMI source is at 4K — \
         drop its resolution to 1080p (macOS: System Settings → Displays, \
         hold Option, pick `1920×1080` without 'Looks like') so Cam Link \
         re-advertises YUYV.",
        offered
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_1080P: &str = "ioctl: VIDIOC_ENUM_FMT
	Type: Video Capture

	[0]: 'YUYV' (YUYV 4:2:2)
		Size: Discrete 1920x1080
			Interval: Discrete 0.017s (60.000 fps)
	[1]: 'NV12' (Y/UV 4:2:0)
		Size: Discrete 1920x1080
			Interval: Discrete 0.017s (60.000 fps)
	[2]: 'YU12' (Planar YUV 4:2:0)
		Size: Discrete 1920x1080
			Interval: Discrete 0.017s (60.000 fps)
";

    const SAMPLE_4K: &str = "ioctl: VIDIOC_ENUM_FMT
	Type: Video Capture

	[0]: 'NV12' (Y/UV 4:2:0)
		Size: Discrete 3840x2160
			Interval: Discrete 0.033s (30.000 fps)
	[1]: 'NV12' (Y/UV 4:2:0)
		Size: Discrete 3840x2160
			Interval: Discrete 0.033s (30.000 fps)
	[2]: 'YU12' (Planar YUV 4:2:0)
		Size: Discrete 3840x2160
			Interval: Discrete 0.033s (30.000 fps)
";

    #[test]
    fn picks_yuyv_at_1080p() {
        let m = pick_best(parse_modes(SAMPLE_1080P)).unwrap();
        assert_eq!(m.format, "YUYV");
        assert_eq!(m.resolution, "1920x1080");
    }

    #[test]
    fn errors_on_4k_only_nv12_yu12() {
        // ustreamer can't ingest NV12/YU12 — the 4K-only Cam Link case must
        // error so the supervisor surfaces a useful message instead of
        // crash-looping the way it did before.
        let err = pick_best(parse_modes(SAMPLE_4K)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no ustreamer-compatible format"), "got: {msg}");
        assert!(msg.contains("1920×1080"), "should hint at the fix: {msg}");
    }
}
