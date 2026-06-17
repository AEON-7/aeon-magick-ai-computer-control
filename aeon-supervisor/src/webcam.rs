//! USB webcam passthrough config.
//!
//! Exposes a chosen video source (the Pi camera, the HDMI-CSI bridge, or a USB
//! capture stick) to the OTG-connected target host as a standard UVC webcam.
//! This module just owns `/etc/aeon/uvc.toml`:
//!   * `aeon-hid` reads it at gadget-setup time and adds/removes the `uvc.usb0`
//!     function (so the target enumerates/loses the webcam);
//!   * `aeon-uvc` (the feeder) reads `source` and pumps that device's frames
//!     into the gadget node.
//! A PUT writes the toml then restarts both so the change takes effect.
//!
//! NOTE: the webcam and the *console view* (streamer) are independent, but a
//! given camera is a single resource — selecting the SAME source for both can
//! collide (esp. the libcamera Pi camera, which is single-consumer). The UI
//! should steer users to different sources (e.g. view = hdmi-csi, webcam =
//! camera-csi).

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::process::Command;

const UVC_TOML: &str = "/etc/aeon/uvc.toml";

/// The webcam feeds from a CONCRETE capture source (never "auto").
const WEBCAM_SOURCES: [&str; 3] = ["camera-csi", "hdmi-csi", "cam-link-usb"];

/// Is the UVC gadget function (`usb_f_uvc`) available in this kernel?
fn usb_f_uvc_supported() -> bool {
    let k = match std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        Ok(s) => s.trim().to_string(),
        Err(_) => return false,
    };
    let ko = format!(
        "/lib/modules/{k}/kernel/drivers/usb/gadget/function/usb_f_uvc.ko"
    );
    if Path::new(&ko).exists() || Path::new(&format!("{ko}.xz")).exists() {
        return true;
    }
    std::fs::read_to_string(format!("/lib/modules/{k}/modules.builtin"))
        .map(|s| s.contains("usb_f_uvc"))
        .unwrap_or(false)
}

/// GET /api/webcam — current webcam state + the sources it can use.
pub async fn get_webcam(State(_s): State<AppState>) -> impl IntoResponse {
    let v: toml::Value = std::fs::read_to_string(UVC_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    let enabled = v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false);
    let source = v
        .get("source")
        .and_then(|s| s.as_str())
        .unwrap_or("camera-csi")
        .to_string();
    // Only concrete sources that are physically present.
    let present = crate::streamer_config::detect_available_sources();
    let available: Vec<&str> = WEBCAM_SOURCES
        .iter()
        .copied()
        .filter(|s| present.contains(s))
        .collect();
    Json(json!({
        "ok": true,
        "enabled": enabled,
        "source": if enabled { source } else { "off".to_string() },
        "available_sources": available,
        "supported": usb_f_uvc_supported(),
    }))
}

#[derive(Deserialize)]
pub struct WebcamPatch {
    /// "off" disables the webcam; otherwise one of WEBCAM_SOURCES.
    pub source: String,
}

/// PUT /api/webcam — set the webcam source (or "off") and apply it.
pub async fn put_webcam(
    State(_s): State<AppState>,
    Json(patch): Json<WebcamPatch>,
) -> impl IntoResponse {
    let off = patch.source == "off";
    if !off && !WEBCAM_SOURCES.contains(&patch.source.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": format!(
                "invalid source {:?}; expected \"off\" or one of {:?}", patch.source, WEBCAM_SOURCES
            )})),
        )
            .into_response();
    }

    let text = std::fs::read_to_string(UVC_TOML).unwrap_or_default();
    let mut parsed: toml::Value =
        toml::from_str(&text).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let table = match parsed.as_table_mut() {
        Some(t) => t,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": "uvc.toml is not a table"})),
            )
                .into_response()
        }
    };
    table.insert("enabled".into(), toml::Value::Boolean(!off));
    if !off {
        table.insert("source".into(), toml::Value::String(patch.source.clone()));
    }

    let serialized = match toml::to_string_pretty(&parsed) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("serialize uvc.toml: {e}")})),
            )
                .into_response()
        }
    };
    let tmp = format!("{UVC_TOML}.tmp");
    if let Err(e) = std::fs::write(&tmp, &serialized).and_then(|_| std::fs::rename(&tmp, UVC_TOML)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("write uvc.toml: {e}")})),
        )
            .into_response();
    }

    // aeon-hid re-reads uvc.toml and adds/removes the uvc.usb0 gadget function
    // (a brief USB re-enumeration on the target, like a persona switch); aeon-uvc
    // starts/stops feeding the selected source. Restart both off the async runtime.
    let restart = tokio::task::spawn_blocking(|| {
        let _ = Command::new("systemctl")
            .args(["restart", "aeon-hid.service"])
            .output();
        Command::new("systemctl")
            .args(["restart", "aeon-uvc.service"])
            .output()
    })
    .await;

    let restarted = matches!(restart, Ok(Ok(o)) if o.status.success());
    Json(json!({
        "ok": true,
        "source": if off { "off".to_string() } else { patch.source },
        "restarted": restarted,
    }))
    .into_response()
}
