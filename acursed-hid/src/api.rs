//! HTTP API for HID actions. Only logical operations — never raw press/release.

use crate::input;
use crate::state::SharedState;
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixListener;
use tracing::info;

pub async fn serve(state: SharedState) -> Result<()> {
    let sock = state.0.cfg.api_sock.clone();
    if let Some(p) = sock.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o660)).ok();
    info!(sock = %sock.display(), "hid API listening");

    let app = Router::new()
        .route("/status", get(get_status))
        .route("/type", post(post_type))
        .route("/key", post(post_key))
        .route("/click", post(post_click))
        .route("/move", post(post_move))
        .route("/scroll", post(post_scroll))
        .route("/release_all", post(post_release_all))
        .with_state(state);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn get_status(State(state): State<SharedState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "persona": state.0.cfg.persona,
    }))
}

#[derive(Deserialize)]
struct TypeReq { text: String }

async fn post_type(
    State(state): State<SharedState>,
    Json(req): Json<TypeReq>,
) -> impl IntoResponse {
    match state.0.hid.type_str(&req.text) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "typed": req.text.len()}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct KeyReq {
    keys: Vec<String>,
    #[serde(default = "default_hold")]
    hold_ms: u32,
}
fn default_hold() -> u32 { 50 }

async fn post_key(
    State(state): State<SharedState>,
    Json(req): Json<KeyReq>,
) -> impl IntoResponse {
    let mut modifier = 0u8;
    let mut keys: Vec<u8> = Vec::new();
    for name in &req.keys {
        if let Some(m) = input::modifier_from_name(name) {
            modifier |= m;
        } else if let Some(k) = input::keycode_from_name(name) {
            keys.push(k);
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!("unknown key {name}")})),
            );
        }
    }
    match state.0.hid.chord(modifier, &keys, req.hold_ms) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "modifier": modifier, "keys": keys}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct ClickReq {
    #[serde(default = "default_button")]
    button: String,
    #[serde(default = "default_count")]
    count: u32,
}
fn default_button() -> String { "left".to_string() }
fn default_count() -> u32 { 1 }

async fn post_click(
    State(state): State<SharedState>,
    Json(req): Json<ClickReq>,
) -> impl IntoResponse {
    let mask = match req.button.as_str() {
        "left" => 0x01,
        "right" => 0x02,
        "middle" => 0x04,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "err": "button must be left/right/middle"}))),
    };
    match state.0.hid.click(mask, req.count) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "button": req.button, "count": req.count}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct MoveReq { dx: i32, dy: i32 }

async fn post_move(
    State(state): State<SharedState>,
    Json(req): Json<MoveReq>,
) -> impl IntoResponse {
    // Clamp into the i8 range that the boot-mouse descriptor allows. If the
    // caller wants a bigger move they should split it.
    let dx = req.dx.clamp(-127, 127) as i8;
    let dy = req.dy.clamp(-127, 127) as i8;
    match state.0.hid.move_rel(dx, dy) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "dx": dx, "dy": dy}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct ScrollReq { dy: i32 }

async fn post_scroll(
    State(state): State<SharedState>,
    Json(req): Json<ScrollReq>,
) -> impl IntoResponse {
    let dy = req.dy.clamp(-127, 127) as i8;
    match state.0.hid.scroll(dy) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true, "dy": dy}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

async fn post_release_all(State(state): State<SharedState>) -> impl IntoResponse {
    let _ = state.0.hid.release_all();
    (StatusCode::OK, Json(json!({"ok": true, "released": true})))
}
