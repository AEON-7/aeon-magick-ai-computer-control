//! Streamer configuration. Loaded from TOML; sensible defaults baked in.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    /// Pi 4. Hardware H.264 encoder available via h264_v4l2m2m.
    Pi4,
    /// Pi 5. No HW H.264. Software libx264 or MJPEG.
    Pi5,
    /// Unknown (e.g., running on x86 dev box). MJPEG only.
    Other,
}

impl Platform {
    pub fn detect() -> Self {
        match std::fs::read_to_string("/proc/device-tree/model").ok().as_deref() {
            Some(m) if m.contains("Raspberry Pi 4") => Self::Pi4,
            Some(m) if m.contains("Raspberry Pi 5") => Self::Pi5,
            _ => Self::Other,
        }
    }
}

/// What to do with the captured frame before serving it to clients. When
/// the source capture device offers a format ustreamer can ingest natively
/// (YUYV / UYVY / MJPG), `Output` is ignored — frames flow straight through
/// at the source resolution. When the source offers only NV12/YU12 (Cam
/// Link 4K's only modes when fed a 4K HDMI signal), the streamer falls
/// back to an ffmpeg pipeline that ingests NV12, rescales to
/// `width`×`height`, and serves the result. This is how you make a Mac
/// running at logical 1512×982 + mirroring + Cam-Link-receives-4K turn
/// into an agent-friendly 1512×982 MJPEG stream where pixel coordinates
/// map 1:1 with the Mac's screen.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Output {
    /// Target stream resolution. Pick this to match the host's *logical*
    /// (post-Retina-scaling) pixel grid so agent move_cursor(x,y) maps
    /// 1:1 with what the host sees. Examples:
    ///   M1 MBP 13" default scale  → 1440×900
    ///   M2/M3 MBP 14" default     → 1512×982
    ///   M2/M3 MBP 16" default     → 1728×1117
    ///   Pure 1080p host           → 1920×1080
    pub width: u32,
    pub height: u32,

    /// Target frame rate. 30 is plenty for agent use; 60 doubles CPU on Pi 4.
    pub fps: u32,

    /// Encoder format. "mjpeg" (most compatible) or "h264" (lower bandwidth,
    /// requires player support — Pi 4 uses h264_v4l2m2m hardware encoder).
    pub format: String,

    /// Scale filter used by ffmpeg. Best quality → "lanczos". Fastest →
    /// "neighbor". Reasonable default → "bicubic".
    pub scale_algorithm: String,

    /// Where ffmpeg writes the latest single-frame JPEG. /api/streamer/snapshot
    /// reads this file.
    pub snapshot_path: PathBuf,

    /// TCP port for ffmpeg's mpjpeg live stream. Bound to 127.0.0.1 only;
    /// aeon-streamer proxies it out via the unix-socket API.
    pub mjpeg_tcp_port: u16,

    /// If true, use the Pi 4 GPU's hardware scaler (`scale_v4l2m2m`)
    /// instead of CPU-bound `scale=`. Saves ~300–500 mA on a Pi 4 under
    /// active streaming. Default `false` for compatibility — the hw
    /// scaler is finicky about pixel formats and we want a safe fallback
    /// on first boot. Toggle to `true` in /etc/aeon/streamer.toml when
    /// you've validated streaming works.
    pub hw_accel: bool,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            format: "mjpeg".into(),
            scale_algorithm: "bicubic".into(),
            snapshot_path: PathBuf::from("/run/aeon/snapshots/live.jpg"),
            mjpeg_tcp_port: 8002,
            hw_accel: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// V4L2 device. Default `/dev/kvmd-video` for compatibility with PiKVM
    /// udev conventions; fall back to `/dev/video0`.
    pub device: PathBuf,

    /// Where to listen for the HTTP API (unix socket).
    pub api_sock: PathBuf,

    /// ustreamer's unix socket. We launch it; we proxy snapshot requests to it.
    pub ustreamer_sock: PathBuf,

    /// Quality knob for software JPEG encoding (1-100).
    pub jpeg_quality: u8,

    /// Drop-same-frames N to save bandwidth and CPU when the source is static.
    pub drop_same_frames: u32,

    /// Detected platform (auto-set; can be overridden in config).
    pub platform: Platform,

    /// Path to ustreamer binary.
    pub ustreamer_bin: PathBuf,

    /// Path to ffmpeg binary. Used for the NV12/YU12 fallback pipeline that
    /// ustreamer can't handle directly.
    pub ffmpeg_bin: PathBuf,

    /// Run ustreamer as this user (uid lookup happens at start).
    pub run_as: String,

    /// Output (post-process) settings — applied only in the ffmpeg fallback
    /// pipeline. The ustreamer fast path serves frames at the source
    /// resolution unchanged.
    pub output: Output,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: PathBuf::from("/dev/kvmd-video"),
            api_sock: PathBuf::from("/run/aeon/streamer.sock"),
            ustreamer_sock: PathBuf::from("/run/aeon/ustreamer.sock"),
            jpeg_quality: 80,
            drop_same_frames: 30,
            platform: Platform::detect(),
            ustreamer_bin: PathBuf::from("/usr/bin/ustreamer"),
            ffmpeg_bin: PathBuf::from("/usr/bin/ffmpeg"),
            run_as: "aeon".to_string(),
            output: Output::default(),
        }
    }
}

pub fn load_with_overrides(
    explicit: Option<PathBuf>,
    device_override: Option<PathBuf>,
    api_sock_override: Option<PathBuf>,
) -> Result<Config> {
    let candidates: Vec<PathBuf> = match explicit {
        Some(p) => vec![p],
        None => vec![
            PathBuf::from("/etc/aeon/streamer.toml"),
            PathBuf::from("streamer.toml"),
        ],
    };

    let mut cfg = Config::default();
    for path in &candidates {
        if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            cfg = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            tracing::info!(path = %path.display(), "loaded config");
            break;
        }
    }

    if let Some(d) = device_override {
        cfg.device = d;
    }
    if let Some(s) = api_sock_override {
        cfg.api_sock = s;
    }

    // Fall back from /dev/kvmd-video to /dev/video0 if the udev symlink isn't present.
    if !cfg.device.exists() && cfg.device == Path::new("/dev/kvmd-video") {
        let v0 = PathBuf::from("/dev/video0");
        if v0.exists() {
            tracing::info!("/dev/kvmd-video missing, falling back to /dev/video0");
            cfg.device = v0;
        }
    }

    Ok(cfg)
}
