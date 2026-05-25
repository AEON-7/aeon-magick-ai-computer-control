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

    /// Run ustreamer as this user (uid lookup happens at start).
    pub run_as: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: PathBuf::from("/dev/kvmd-video"),
            api_sock: PathBuf::from("/run/acursed/streamer.sock"),
            ustreamer_sock: PathBuf::from("/run/acursed/ustreamer.sock"),
            jpeg_quality: 80,
            drop_same_frames: 30,
            platform: Platform::detect(),
            ustreamer_bin: PathBuf::from("/usr/bin/ustreamer"),
            run_as: "acursed".to_string(),
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
            PathBuf::from("/etc/acursed/streamer.toml"),
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
