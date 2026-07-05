//! Live tuning of `/etc/aeon/streamer.toml`.
//!
//! v63: the user-visible latency on the live stream comes from a chain
//! of buffers — Cam Link internal queue, ffmpeg / ustreamer encode,
//! axum-server's TLS body streaming, browser-side multipart-MJPEG
//! parser. Most of those are out of our reach, but two big knobs are
//! in our config:
//!
//!   * `output.fps` — how often we *emit* a frame. Browsers and
//!     low-bandwidth links buffer per-frame; cutting from 30 to 24
//!     fps gives the consumer side an extra ~33 ms of breathing room
//!     per frame without making movement feel stuttery.
//!
//!   * `jpeg_quality` — fewer bytes on the wire = less TCP / browser
//!     buffer to drain on each frame. 70 vs 80 is visually
//!     indistinguishable for desktop / UI content.
//!
//! This module exposes a tiny GET/PUT API so the UI can let operators
//! tune live (and try 18 fps if 24 still feels laggy) without
//! editing TOML by hand. After a PUT we restart aeon-streamer via
//! systemctl so the new values take effect on the next frame.
//!
//! Future versions will add H.264 over WebSocket which obsoletes most
//! of this knob set; for now MJPEG + fps/quality is the lever we have.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

const STREAMER_TOML: &str = "/etc/aeon/streamer.toml";

/// Detect which capture sources are physically present, so the UI picker only
/// offers ones that exist. "auto" is always listed (resolves per-platform).
pub fn detect_available_sources() -> Vec<&'static str> {
    let mut v = vec!["auto"];
    // Cam Link / generic USB UVC capture (PiKVM udev symlink).
    if Path::new("/dev/kvmd-video").exists() {
        v.push("cam-link-usb");
    }
    // CSI devices, identified by their v4l2 subdev name.
    let (mut has_bridge, mut has_camera) = (false, false);
    if let Ok(rd) = std::fs::read_dir("/sys/class/video4linux") {
        for e in rd.flatten() {
            if let Ok(name) = std::fs::read_to_string(e.path().join("name")) {
                let n = name.to_ascii_lowercase();
                if n.contains("tc358743") {
                    has_bridge = true; // X1301 HDMI-to-CSI bridge
                } else if n.starts_with("imx") || n.starts_with("ov") {
                    has_camera = true; // a Pi camera sensor (imx477/imx708/ov…)
                }
            }
        }
    }
    if has_bridge {
        v.push("hdmi-csi");
    }
    if has_camera {
        v.push("camera-csi");
    }
    v
}

/// GET /api/streamer/config — read the current fps/quality values
/// from streamer.toml. The TOML has more fields than this; we only
/// surface the ones a non-expert operator would safely tune live.
pub async fn get_config(State(_state): State<AppState>) -> impl IntoResponse {
    let text = match tokio::fs::read_to_string(STREAMER_TOML).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("read streamer.toml: {e}")})),
            )
                .into_response();
        }
    };

    let parsed: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("parse streamer.toml: {e}")})),
            )
                .into_response();
        }
    };

    let jpeg_quality = parsed
        .get("jpeg_quality")
        .and_then(|v| v.as_integer())
        .unwrap_or(70);
    let output = parsed.get("output");
    let fps = output
        .and_then(|o| o.get("fps"))
        .and_then(|v| v.as_integer())
        .unwrap_or(24);
    let width = output
        .and_then(|o| o.get("width"))
        .and_then(|v| v.as_integer())
        .unwrap_or(1920);
    let height = output
        .and_then(|o| o.get("height"))
        .and_then(|v| v.as_integer())
        .unwrap_or(1080);
    let match_source = output
        .and_then(|o| o.get("match_source"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let format = output
        .and_then(|o| o.get("format"))
        .and_then(|v| v.as_str())
        .unwrap_or("mjpeg")
        .to_string();
    let hw_accel = output
        .and_then(|o| o.get("hw_accel"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Capture source (Cam Link USB vs. Pi 5 X1301 HDMI-CSI / Pi camera) plus
    // the display-orientation knobs (server-side rotation/flip baked into the
    // pipeline so snapshot/stream/recording/vision all agree).
    let capture = parsed.get("capture");
    let source = capture
        .and_then(|c| c.get("source"))
        .and_then(|v| v.as_str())
        .unwrap_or("auto")
        .to_string();
    let rotation = capture
        .and_then(|c| c.get("rotation"))
        .and_then(|v| v.as_integer())
        .unwrap_or(0);
    let hflip = capture
        .and_then(|c| c.get("hflip"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let vflip = capture
        .and_then(|c| c.get("vflip"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Which sources are usable depends on the board — hdmi-csi / camera-csi
    // are Pi-5-only. Surface the model so the UI/agent can gate the choices.
    let platform = match std::fs::read_to_string("/proc/device-tree/model") {
        Ok(m) if m.contains("Raspberry Pi 5") => "pi5",
        Ok(m) if m.contains("Raspberry Pi 4") => "pi4",
        _ => "other",
    };

    Json(json!({
        "ok": true,
        "jpeg_quality": jpeg_quality,
        "fps": fps,
        "width": width,
        "height": height,
        "match_source": match_source,
        "format": format,
        "hw_accel": hw_accel,
        "source": source,
        "rotation": rotation,
        "hflip": hflip,
        "vflip": vflip,
        "platform": platform,
        "available_sources": detect_available_sources(),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub struct ConfigPatch {
    /// 1..30 — anything above 30 stresses Pi 4's MJPEG encoder and
    /// usually doesn't help perceived smoothness on a remote viewer.
    pub fps: Option<i64>,
    /// 30..95. Below 30 starts to look mushy; above 90 the encoder
    /// stops dropping any DCT detail and you're paying full bandwidth
    /// for visually identical frames.
    pub jpeg_quality: Option<i64>,
    /// Capture input: "auto" | "cam-link-usb" | "hdmi-csi" | "camera-csi".
    /// `hdmi-csi` (X1301 bridge) and `camera-csi` (Pi camera) are Pi-5-only.
    /// A vision agent flips this to switch between watching a controlled
    /// machine's HDMI and a live camera feed.
    pub source: Option<String>,
    /// Clockwise display rotation in degrees — snapped to 0 | 90 | 180 | 270.
    /// Baked into the pipeline filter graph, so the live stream, snapshot,
    /// recordings, and the vision/OCR tap all rotate together. The common case
    /// is a physically rotated Pi-camera mount.
    pub rotation: Option<i64>,
    /// Mirror the image left↔right (applied after `rotation`).
    pub hflip: Option<bool>,
    /// Mirror the image top↔bottom (applied after `rotation`).
    pub vflip: Option<bool>,
    /// Output encode width/height. Lowering these (e.g. 1280×720) sharply cuts
    /// the Pi 5 software-H.264 (libx264) CPU + power on the hdmi-csi/camera-csi
    /// paths — the Pi 5 has no hardware H.264 encoder. Clamped to even values in
    /// 160..=1920 / 120..=1080. Only takes effect when `match_source = false`.
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// When true the encode tracks the (capped-1080p) source resolution; set
    /// false to force the fixed `width`×`height` above for lower CPU/power.
    pub match_source: Option<bool>,
}

/// PUT /api/streamer/config — update fps and/or jpeg_quality, then
/// restart aeon-streamer so the new values take effect.
///
/// Only those two are exposed for now — width/height/format require
/// matching changes to the Cam Link source format. (The encoder itself
/// is platform-detected by aeon-streamer: hardware h264_v4l2m2m on the
/// Pi 4, software libx264 on the Pi 5.) We'll surface those later
/// once the H.264-over-WebSocket pipe lands.
pub async fn put_config(
    State(_state): State<AppState>,
    Json(patch): Json<ConfigPatch>,
) -> impl IntoResponse {
    // Frame rate is restricted to deterministic divisors of the 60fps
    // capture (15/30/60) so frame-dropping is even — snap any incoming value
    // to the nearest allowed step.
    let fps = patch
        .fps
        .map(|v| [15i64, 30, 60].into_iter().min_by_key(|&f| (f - v).abs()).unwrap_or(30));
    let q = patch.jpeg_quality.map(|v| v.clamp(30, 95));

    // Validate the capture source against the known set before touching disk.
    const SOURCES: [&str; 4] = ["auto", "cam-link-usb", "hdmi-csi", "camera-csi"];
    let source = match &patch.source {
        Some(s) if SOURCES.contains(&s.as_str()) => Some(s.clone()),
        Some(s) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!(
                    "invalid source {s:?}; expected one of {SOURCES:?}"
                )})),
            )
                .into_response();
        }
        None => None,
    };

    // Snap rotation to the nearest quarter turn in [0,360); pipeline only
    // supports 0/90/180/270 (a transpose can't turn an arbitrary angle).
    let rotation = patch.rotation.map(|v| {
        let n = v.rem_euclid(360);
        [0i64, 90, 180, 270]
            .into_iter()
            .min_by_key(|&r| (r - n).abs())
            .unwrap_or(0)
    });
    let hflip = patch.hflip;
    let vflip = patch.vflip;

    // Output resolution: clamp to sane H.264 bounds and force even dimensions
    // (libx264/H.264 require even width+height).
    let width = patch.width.map(|v| v.clamp(160, 1920) & !1);
    let height = patch.height.map(|v| v.clamp(120, 1080) & !1);
    let match_source = patch.match_source;

    if fps.is_none()
        && q.is_none()
        && source.is_none()
        && rotation.is_none()
        && hflip.is_none()
        && vflip.is_none()
        && width.is_none()
        && height.is_none()
        && match_source.is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "nothing to update"})),
        )
            .into_response();
    }

    // Read → parse → mutate → serialize → write. Atomic write so a
    // streamer restart that races a partial write doesn't see truncated
    // TOML.
    let text = match tokio::fs::read_to_string(STREAMER_TOML).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("read streamer.toml: {e}")})),
            )
                .into_response();
        }
    };
    let mut parsed: toml::Value = match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("parse streamer.toml: {e}")})),
            )
                .into_response();
        }
    };
    let table = match parsed.as_table_mut() {
        Some(t) => t,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": "streamer.toml is not a table"})),
            )
                .into_response();
        }
    };

    if let Some(q) = q {
        table.insert("jpeg_quality".into(), toml::Value::Integer(q));
    }
    if fps.is_some() || width.is_some() || height.is_some() || match_source.is_some() {
        // The output table must already exist (we ship it in the
        // default config). If it somehow doesn't, create it so we
        // don't error on a fresh image.
        let output_entry = table
            .entry("output".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let Some(out_tbl) = output_entry.as_table_mut() {
            if let Some(fps) = fps {
                out_tbl.insert("fps".into(), toml::Value::Integer(fps));
            }
            if let Some(w) = width {
                out_tbl.insert("width".into(), toml::Value::Integer(w));
            }
            if let Some(h) = height {
                out_tbl.insert("height".into(), toml::Value::Integer(h));
            }
            if let Some(ms) = match_source {
                out_tbl.insert("match_source".into(), toml::Value::Boolean(ms));
            }
        }
    }
    if source.is_some() || rotation.is_some() || hflip.is_some() || vflip.is_some() {
        // [capture] is shipped in the default config, but create it if a
        // hand-edited file dropped it so we don't error on PUT.
        let cap_entry = table
            .entry("capture".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let Some(cap_tbl) = cap_entry.as_table_mut() {
            if let Some(src) = &source {
                cap_tbl.insert("source".into(), toml::Value::String(src.clone()));
            }
            if let Some(r) = rotation {
                cap_tbl.insert("rotation".into(), toml::Value::Integer(r));
            }
            if let Some(h) = hflip {
                cap_tbl.insert("hflip".into(), toml::Value::Boolean(h));
            }
            if let Some(v) = vflip {
                cap_tbl.insert("vflip".into(), toml::Value::Boolean(v));
            }
        }
    }

    let serialized = match toml::to_string_pretty(&parsed) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("serialize streamer.toml: {e}")})),
            )
                .into_response();
        }
    };

    // Atomic write: write to tmp + rename.
    let tmp_path = format!("{STREAMER_TOML}.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, &serialized).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("write tmp: {e}")})),
        )
            .into_response();
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, STREAMER_TOML).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rename tmp→toml: {e}")})),
        )
            .into_response();
    }

    // Restart aeon-streamer so the new settings take effect. systemd
    // handles the actual process replacement; we just spawn-and-wait.
    let restart = tokio::task::spawn_blocking(|| {
        Command::new("systemctl")
            .args(["restart", "aeon-streamer.service"])
            .output()
    })
    .await;

    match restart {
        Ok(Ok(out)) if out.status.success() => {
            Json(json!({
                "ok": true,
                "fps": fps,
                "jpeg_quality": q,
                "source": source,
                "rotation": rotation,
                "hflip": hflip,
                "vflip": vflip,
                "width": width,
                "height": height,
                "match_source": match_source,
                "restarted": true,
            }))
            .into_response()
        }
        Ok(Ok(out)) => {
            // Config was saved but restart failed — surface both bits.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "ok": false,
                    "err": format!(
                        "config saved but restart failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                })),
            )
                .into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "err": "config saved but systemctl restart task failed",
            })),
        )
            .into_response(),
    }
}

/// Helper for tests / scripts that need to know if the config file
/// exists at all (e.g. should /api/streamer/config 404 because we're
/// running a non-image build that doesn't ship the TOML).
#[allow(dead_code)]
pub fn config_exists() -> bool {
    Path::new(STREAMER_TOML).exists()
}

/// Stub to suppress the `Value` unused-import warning when the only
/// references go through json!{} macro expansions.
#[allow(dead_code)]
fn _typecheck_value(v: Value) -> Value {
    v
}

#[cfg(test)]
mod tests {
    /// Reproduce exactly what put_config does: parse the shipped
    /// streamer.toml, mutate fps + jpeg_quality, re-serialize, then
    /// verify the result is still STRUCTURALLY VALID — i.e. top-level
    /// keys stay top-level and [output] keys stay under [output].
    /// This guards against the toml-Value round-trip reordering keys
    /// such that a top-level scalar gets absorbed into the [output]
    /// table (which would break aeon-streamer on the next slider use).
    #[test]
    fn roundtrip_preserves_structure() {
        let text = r#"device = "/dev/kvmd-video"
jpeg_quality = 70
drop_same_frames = 30
ustreamer_bin = "/usr/bin/ustreamer"
run_as = "aeon"

[output]
match_source = false
width = 1920
height = 1080
fps = 30
format = "mjpeg"
mjpeg_tcp_port = 8002
hw_accel = false

[capture]
source = "camera-csi"
camera_id = 0
rotation = 0
hflip = false
vflip = false
"#;
        let mut parsed: toml::Value = toml::from_str(text).unwrap();
        let table = parsed.as_table_mut().unwrap();
        table.insert("jpeg_quality".into(), toml::Value::Integer(60));
        let out = table
            .entry("output".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        out.as_table_mut().unwrap().insert("fps".into(), toml::Value::Integer(15));
        // Mutate the orientation keys exactly as put_config does — each under
        // [capture] — and confirm below they don't leak to [output]/top-level.
        let cap = table
            .entry("capture".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        {
            let cap_tbl = cap.as_table_mut().unwrap();
            cap_tbl.insert("rotation".into(), toml::Value::Integer(90));
            cap_tbl.insert("hflip".into(), toml::Value::Boolean(true));
            cap_tbl.insert("vflip".into(), toml::Value::Boolean(false));
        }
        let serialized = toml::to_string(&parsed).unwrap();
        eprintln!("--- re-serialized ---\n{serialized}\n---");

        // Must re-parse.
        let rp: toml::Value = toml::from_str(&serialized)
            .expect("re-serialized streamer.toml must re-parse");
        // Top-level keys must remain top-level.
        assert_eq!(rp.get("device").and_then(|x| x.as_str()), Some("/dev/kvmd-video"),
            "device leaked out of top level");
        assert_eq!(rp.get("run_as").and_then(|x| x.as_str()), Some("aeon"),
            "run_as leaked out of top level");
        assert_eq!(rp.get("ustreamer_bin").and_then(|x| x.as_str()), Some("/usr/bin/ustreamer"),
            "ustreamer_bin leaked out of top level");
        // [output] keys must remain under [output].
        let o = rp.get("output").expect("output table missing");
        assert_eq!(o.get("format").and_then(|x| x.as_str()), Some("mjpeg"),
            "output.format missing/moved");
        assert_eq!(o.get("mjpeg_tcp_port").and_then(|x| x.as_integer()), Some(8002),
            "output.mjpeg_tcp_port missing/moved");
        assert_eq!(o.get("fps").and_then(|x| x.as_integer()), Some(15),
            "fps not updated");
        assert_eq!(rp.get("jpeg_quality").and_then(|x| x.as_integer()), Some(60),
            "jpeg_quality not updated");
        // Orientation keys must round-trip under [capture] — never leaking to
        // [output] or the top level (which would silently un-rotate the stream).
        let c = rp.get("capture").expect("capture table missing");
        assert_eq!(c.get("rotation").and_then(|x| x.as_integer()), Some(90),
            "capture.rotation missing/moved");
        assert_eq!(c.get("hflip").and_then(|x| x.as_bool()), Some(true),
            "capture.hflip missing/moved");
        assert_eq!(c.get("vflip").and_then(|x| x.as_bool()), Some(false),
            "capture.vflip missing/moved");
        assert_eq!(c.get("source").and_then(|x| x.as_str()), Some("camera-csi"),
            "capture.source leaked/changed");
        assert!(rp.get("rotation").is_none(),
            "rotation leaked to top level");
        assert!(o.get("rotation").is_none(),
            "rotation leaked into [output]");
    }
}
