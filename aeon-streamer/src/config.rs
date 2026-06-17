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

/// Which capture input the streamer uses. `Auto` preserves the historical
/// behavior (Cam Link USB). The CSI sources target a Pi 5 carrying the
/// Geekworm X1301 HDMI-to-CSI bridge (TC358743) on one connector and/or a
/// Raspberry Pi camera (e.g. the HQ Camera / IMX477) on the other — so a
/// vision agent can watch a live camera OR view the HDMI of a system it
/// controls, by flipping `source`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Resolve by platform — today that means the Cam Link USB device
    /// (`/dev/kvmd-video`), exactly the path the streamer has always used.
    #[default]
    Auto,
    /// Elgato Cam Link / generic UVC HDMI capture over USB (`/dev/kvmd-video`).
    CamLinkUsb,
    /// HDMI-over-CSI via the X1301 (TC358743) on a Pi 5. The device is the
    /// stable `/dev/aeon-hdmi` symlink maintained by `aeon-hdmi-csi.service`
    /// (which discovers the post-renumber `/dev/videoN` every boot). Capture
    /// then flows through the existing v4l2 → ffmpeg H.264 pipeline (the
    /// TC358743 offers UYVY, already supported).
    HdmiCsi,
    /// A Raspberry Pi camera on a CSI port, captured through libcamera
    /// (`rpicam-vid`) rather than a raw v4l2 open — a Pi camera's video node
    /// is Bayer needing the ISP, so ffmpeg can't read it directly.
    CameraCsi,
}

/// Capture-source selection plus per-source knobs. The defaults reproduce the
/// historical Cam Link path byte-for-byte (`source = "auto"`), so an old
/// `streamer.toml` with no `[capture]` block behaves exactly as before.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Capture {
    /// `auto` | `cam-link-usb` | `hdmi-csi` | `camera-csi`.
    pub source: Source,
    /// rpicam `--camera N` selector, used only when `source = "camera-csi"`.
    pub camera_id: u32,
    /// Clockwise display rotation in degrees: `0` | `90` | `180` | `270`.
    /// Applied in the ffmpeg filter graph (a `transpose` for 90/270, an
    /// `hflip,vflip` for 180) BEFORE the H.264 / JPEG split, so the live
    /// stream, the `/snapshot`, recordings, and the vision/OCR tap all see
    /// the same corrected orientation. A physically rotated Pi-camera mount
    /// is the common case. Any other value is normalised to the nearest of
    /// the four by [`Capture::orientation_vf`].
    pub rotation: u16,
    /// Mirror left↔right (applied after `rotation`, in the rotated frame's
    /// coordinate space).
    pub hflip: bool,
    /// Mirror top↔bottom (applied after `rotation`).
    pub vflip: bool,
    /// ALSA capture device to MUX into RECORDINGS as an audio track — e.g.
    /// `plughw:CARD=wm8960soundcard,DEV=0` (BrainCraft mic) for `camera-csi`,
    /// or the TC358743 HDMI-audio card `hw:N,0` for `hdmi-csi`. Empty disables
    /// audio (video-only recordings — the historical behaviour). This ONLY
    /// affects /record MP4 output; the live H.264 pipe stays pure video NALs.
    pub audio_device: String,
    /// Sample rate + channels for the ALSA capture above (WM8960 + tc358743
    /// both run 48 kHz stereo).
    pub audio_rate: u32,
    pub audio_channels: u32,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            source: Source::Auto,
            camera_id: 0,
            rotation: 0,
            hflip: false,
            vflip: false,
            audio_device: String::new(),
            audio_rate: 48000,
            audio_channels: 2,
        }
    }
}

impl Capture {
    /// The ffmpeg filter fragment that realises the configured orientation,
    /// or `None` when it is the identity (no rotation, no flips). Spliced into
    /// each pipeline's filter graph immediately before the H.264/JPEG split so
    /// every downstream consumer is rotated identically.
    ///
    /// Rotation uses `transpose` for 90/270 (the only way to turn a quarter)
    /// and `hflip,vflip` for 180 (one cheaper pass than two transposes).
    /// `transpose=1` is 90° clockwise, `transpose=2` is 90° counter-clockwise
    /// (= 270° clockwise). User flips are appended last.
    pub fn orientation_vf(&self) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        match self.rotation % 360 {
            90 => parts.push("transpose=1"),
            180 => {
                parts.push("hflip");
                parts.push("vflip");
            }
            270 => parts.push("transpose=2"),
            _ => {} // 0 (and any non-quarter value) → no rotation
        }
        if self.hflip {
            parts.push("hflip");
        }
        if self.vflip {
            parts.push("vflip");
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }

    /// True when the rotation turns a quarter (90/270) and therefore swaps the
    /// frame's width and height — used to report the post-rotation resolution
    /// in `/state`. The encoders and self-describing streams (H.264 SPS, JPEG)
    /// handle the swapped geometry on their own; this is only for accurate
    /// introspection.
    pub fn swaps_dims(&self) -> bool {
        matches!(self.rotation % 360, 90 | 270)
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

    /// H.264 target bitrate (kbps). Only used when `format = "h264"`.
    /// 6000 ≈ 6 Mbps is ample for 1080p desktop content on a LAN and keeps
    /// the Pi 4 hardware encoder comfortable. Tunable live via /system.
    pub h264_bitrate_kbps: u32,

    /// H.264 keyframe interval (GOP) in frames. A WebSocket client can only
    /// begin decoding at an IDR, so keep this near ~1s of frames (30 @ 30fps).
    /// Smaller = faster client sync + better packet-loss recovery, at a
    /// bitrate cost.
    pub h264_gop: u32,

    /// Dynamically match the output resolution to the *source* resolution
    /// (capped at the Pi 4 HW encoder's 1920×1080 ceiling), skipping the
    /// scale filter entirely when they're equal — a ≤1080p source streams
    /// at native res with no resampling; a >1080p source (e.g. 4K) is still
    /// downscaled to fit 1080p (the encoder can't exceed it). When true,
    /// `width`/`height` are ignored, and source-resolution changes are
    /// tracked live via the streamer's re-detect/respawn.
    pub match_source: bool,
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
            h264_bitrate_kbps: 6000,
            h264_gop: 30,
            match_source: false,
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

    /// Capture-source selection (Cam Link USB vs. Pi 5 CSI HDMI / camera).
    pub capture: Capture,
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
            capture: Capture::default(),
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

    let had_device_override = device_override.is_some();
    if let Some(d) = device_override {
        cfg.device = d;
    }
    if let Some(s) = api_sock_override {
        cfg.api_sock = s;
    }

    // Resolve the capture source → device, unless an explicit --device won.
    //   Auto / CamLinkUsb → keep the /dev/kvmd-video default (+ the fallback
    //                       below). This is the unchanged historical path.
    //   HdmiCsi           → the stable /dev/aeon-hdmi symlink that
    //                       aeon-hdmi-csi.service maintains (it discovers the
    //                       post-renumber /dev/videoN each boot); if the
    //                       symlink isn't up yet, read the indirection file it
    //                       also writes, else keep the symlink path so
    //                       wait_for_device blocks until it appears.
    //   CameraCsi         → no v4l2 device; supervise.rs drives rpicam-vid.
    if !had_device_override {
        match cfg.capture.source {
            Source::Auto | Source::CamLinkUsb | Source::CameraCsi => {}
            Source::HdmiCsi => {
                let sym = PathBuf::from("/dev/aeon-hdmi");
                if sym.exists() {
                    cfg.device = sym;
                } else if let Ok(p) = std::fs::read_to_string("/run/aeon/hdmi-video") {
                    let p = p.trim();
                    cfg.device = if p.is_empty() { sym } else { PathBuf::from(p) };
                } else {
                    cfg.device = sym;
                }
            }
        }
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
