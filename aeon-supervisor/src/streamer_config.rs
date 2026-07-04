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
    let format = output
        .and_then(|o| o.get("format"))
        .and_then(|v| v.as_str())
        .unwrap_or("mjpeg")
        .to_string();
    let hw_accel = output
        .and_then(|o| o.get("hw_accel"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    Json(json!({
        "ok": true,
        "jpeg_quality": jpeg_quality,
        "fps": fps,
        "width": width,
        "height": height,
        "format": format,
        "hw_accel": hw_accel,
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

    if fps.is_none() && q.is_none() {
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
    if let Some(fps) = fps {
        // The output table must already exist (we ship it in the
        // default config). If it somehow doesn't, create it so we
        // don't error on a fresh image.
        let output_entry = table
            .entry("output".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let Some(out_tbl) = output_entry.as_table_mut() {
            out_tbl.insert("fps".into(), toml::Value::Integer(fps));
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
"#;
        let mut parsed: toml::Value = toml::from_str(text).unwrap();
        let table = parsed.as_table_mut().unwrap();
        table.insert("jpeg_quality".into(), toml::Value::Integer(60));
        let out = table
            .entry("output".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        out.as_table_mut().unwrap().insert("fps".into(), toml::Value::Integer(15));
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
    }
}
