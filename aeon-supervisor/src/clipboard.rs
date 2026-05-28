//! Shared clipboard / type-to-target text buffer.
//!
//! The web UI has a persistent text area whose contents live on the
//! Pi at /var/lib/aeon/clipboard.txt. Two interactions:
//!
//!   1. The user pastes / types a snippet into the textarea →
//!      `PUT /api/clipboard` saves it.
//!   2. The user clicks "type on target" → the UI calls
//!      `POST /api/clipboard/type-on-target`. The backend forwards
//!      the saved string to /api/hid/type via the existing HID
//!      proxy. The target sees a sequence of keystrokes as if the
//!      user typed it directly.
//!
//! Off by default (the clipboard.txt simply doesn't exist). The web
//! UI's textarea is shown unconditionally — the "store" is just a
//! string on disk, no toggle to disable it. The target-facing aspect
//! is the "type on target" button, which only fires when explicitly
//! clicked.
//!
//! Capped at 64 kB. Larger text won't fit the buffer; the UI shows
//! "trimmed to 64kB" if so.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

const CLIPBOARD_FILE: &str = "/var/lib/aeon/clipboard.txt";
const MAX_BYTES: usize = 64 * 1024;

pub async fn get_clipboard(State(_state): State<AppState>) -> Json<Value> {
    let text = std::fs::read_to_string(CLIPBOARD_FILE).unwrap_or_default();
    Json(json!({
        "ok": true,
        "text": text,
        "size_bytes": text.len(),
        "max_bytes": MAX_BYTES,
    }))
}

#[derive(Deserialize)]
pub struct ClipboardPut {
    pub text: String,
}

pub async fn put_clipboard(
    State(_state): State<AppState>,
    Json(req): Json<ClipboardPut>,
) -> impl IntoResponse {
    let mut text = req.text;
    let trimmed = text.len() > MAX_BYTES;
    if trimmed {
        text.truncate(MAX_BYTES);
    }
    // Ensure parent dir exists.
    if let Some(parent) = std::path::Path::new(CLIPBOARD_FILE).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(CLIPBOARD_FILE, &text) {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("write: {e}")}))).into_response();
    }
    Json(json!({
        "ok": true,
        "size_bytes": text.len(),
        "trimmed": trimmed,
    })).into_response()
}

pub async fn delete_clipboard(State(_state): State<AppState>) -> impl IntoResponse {
    let _ = std::fs::remove_file(CLIPBOARD_FILE);
    Json(json!({"ok": true})).into_response()
}

/// POST /api/clipboard/type-on-target — fire the saved text through
/// the HID `/type` endpoint. Forwards over the same unix socket the
/// supervisor uses for all HID proxying.
pub async fn type_on_target(State(state): State<AppState>) -> impl IntoResponse {
    let text = match std::fs::read_to_string(CLIPBOARD_FILE) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "clipboard is empty"}))).into_response(),
    };
    if text.is_empty() {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "clipboard is empty"}))).into_response();
    }
    let body = serde_json::to_vec(&json!({ "text": text })).unwrap();
    match crate::proxy::post_hid(&state, "/type", body).await {
        Ok(_) => Json(json!({"ok": true, "bytes_typed": text.len()})).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": format!("hid daemon: {e}")}))).into_response(),
    }
}
