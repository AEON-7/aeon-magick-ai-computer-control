//! GET /api/ups — Waveshare UPS HAT (E) battery state.
//!
//! The `aeon-ups` daemon owns the I2C bus (the HAT's MCU at 0x2D) and publishes
//! the whole power picture to `/run/aeon/ups.json` every 2s (atomic write). We
//! just relay that file, so the dashboard + an AI agent can read charge %,
//! charging state, per-cell voltages, VBUS, and minutes-to-empty without anyone
//! else touching the bus. Returns `{"present": false}` when the HAT isn't
//! attached or the daemon isn't running.

use crate::api::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

const UPS_JSON: &str = "/run/aeon/ups.json";

pub async fn get_ups(State(_state): State<AppState>) -> impl IntoResponse {
    match tokio::fs::read_to_string(UPS_JSON).await {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => Json(v).into_response(),
            Err(e) => Json(json!({
                "present": false, "ok": false,
                "err": format!("parse {UPS_JSON}: {e}")
            }))
            .into_response(),
        },
        // No file → daemon not running / no HAT on this device.
        Err(_) => Json(json!({"present": false})).into_response(),
    }
}
