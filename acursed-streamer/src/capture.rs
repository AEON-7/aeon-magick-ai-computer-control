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

fn pick_best(modes: Vec<ParsedMode>) -> Result<CaptureMode> {
    // Preference order: YUYV > YU12 > UYVY > MJPG > anything-else-mapping-via-NV12-fallback
    // YUYV first because ustreamer + downstream sw-jpeg encoder benefit from it
    // (packed 4:2:2 is its native fast path). YU12 second because it's always
    // available on Cam Link 4K including at 4K input.
    let pref: &[(&str, &str)] = &[
        ("YUYV", "YUYV"),
        ("YU12", "YUV420"),
        ("UYVY", "UYVY"),
        ("MJPG", "MJPEG"),
        ("YV12", "YVU420"),
    ];
    for (fourcc, ustream_fmt) in pref {
        if let Some(m) = modes.iter().find(|m| m.fourcc == *fourcc) {
            // Prefer the highest enumerated resolution. (At 1080p input
            // these are all just 1920x1080; at 4K input the 4K entry is
            // first. Either way, take the first.)
            if let Some(res) = m.resolutions.first() {
                return Ok(CaptureMode {
                    format: ustream_fmt,
                    resolution: res.clone(),
                });
            }
        }
    }
    // NV12 fallback: Cam Link offers it at 4K. ustreamer can't capture it
    // directly, BUT it always also lists YU12 in parallel — so this branch
    // should never fire in practice. Keep as defensive default.
    Err(anyhow!(
        "no ustreamer-compatible format in v4l2 enum: {:?}",
        modes.iter().map(|m| &m.fourcc).collect::<Vec<_>>()
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
    fn picks_yu12_at_4k() {
        let m = pick_best(parse_modes(SAMPLE_4K)).unwrap();
        assert_eq!(m.format, "YUV420");
        assert_eq!(m.resolution, "3840x2160");
    }
}
