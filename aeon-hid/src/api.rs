//! HTTP API for HID actions. Only logical operations — never raw press/release.

use crate::input;
use crate::state::SharedState;
use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as ConnBuilder;
use serde::Deserialize;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use tokio::net::UnixListener;
use tower::ServiceExt;
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

    let app: Router = Router::new()
        .route("/status", get(get_status))
        .route("/type", post(post_type))
        .route("/key", post(post_key))
        .route("/click", post(post_click))
        .route("/move", post(post_move))
        .route("/scroll", post(post_scroll))
        .route("/release_all", post(post_release_all))
        .route("/persona", post(post_persona))
        .route("/consumer", post(post_consumer))
        .with_state(state);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let app = app.clone();
        tokio::spawn(async move {
            let svc = service_fn(move |req: hyper::Request<Incoming>| {
                let app = app.clone();
                async move { app.oneshot(req).await }
            });
            if let Err(e) = ConnBuilder::new(TokioExecutor::new())
                .serve_connection(io, svc).await
            {
                tracing::debug!(?e, "connection ended");
            }
        });
    }
}

async fn get_status(State(state): State<SharedState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "persona": state.0.cfg.persona.as_slug(),
        "available_personas": [
            "generic-composite",
            "logitech-mx",
            "apple-magic-stable",
            "apple-magic",
        ],
    }))
}

#[derive(Deserialize)]
struct TypeReq { text: String }

async fn post_type(
    State(state): State<SharedState>,
    Json(req): Json<TypeReq>,
) -> impl IntoResponse {
    let input_chars = req.text.chars().count();
    match state.0.hid.type_str(&req.text) {
        Ok((typed, skipped)) => (
            StatusCode::OK,
            // `typed` and `skipped` are character counts (not bytes) — the
            // UI message reads "typed 24 chars, skipped 2 unmappable".
            // `input_chars` lets the caller cross-check the total.
            Json(json!({
                "ok": true,
                "typed": typed,
                "skipped": skipped,
                "input_chars": input_chars,
            })),
        ),
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

#[derive(Deserialize)]
struct ConsumerReq {
    /// HID Consumer Page usage code (16-bit). Examples:
    ///   0x0030 = Power
    ///   0x0032 = Sleep
    ///   0x0083 = Wake
    ///   0x00CD = Play/Pause
    ///   0x00E2 = Mute
    ///   0x00E9 = Volume Up
    ///   0x00EA = Volume Down
    /// See HID Usage Tables §15 for the full list.
    usage: u16,
    /// Hold the press for this many milliseconds before releasing.
    /// Defaults to 200ms (short tap). For force-power-off use 8000ms.
    #[serde(default = "default_consumer_hold")]
    hold_ms: u32,
}
fn default_consumer_hold() -> u32 { 200 }

/// POST /consumer — emit a Consumer Page usage event over the
/// /dev/hidg2 consumer-control function. Press, hold, release.
///
/// This is the primary mechanism for remote-controlling the
/// USB-connected target machine's power button: most modern
/// motherboards register the HID power button (usage 0x30) as
/// equivalent to a physical power button, so a short tap triggers
/// the OS power dialog and an 8-second hold forces hardware shutdown.
async fn post_consumer(
    State(state): State<SharedState>,
    Json(req): Json<ConsumerReq>,
) -> impl IntoResponse {
    match state.0.hid.consumer_press(req.usage, req.hold_ms) {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "usage": req.usage,
                "hold_ms": req.hold_ms.min(30_000),
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        ),
    }
}

#[derive(Deserialize)]
struct PersonaReq {
    persona: String,
}

/// Hot-swap the USB HID persona.
///
/// Implementation: write the new slug to /etc/aeon/persona.state, return
/// 200 to the caller, then exit the process with code 0 after a short
/// delay. systemd's `Restart=on-failure` is configured to also restart on
/// clean exit by means of `Restart=always` in the unit (see
/// aeon-hid.service). When aeon-hid respawns, `config::load()` reads the
/// state file and the new persona is applied.
///
/// Why "exit + respawn" instead of in-process hot-swap? Tearing down and
/// rebuilding the configfs gadget tree from the same running process has
/// nasty race conditions with the HID writer threads and pending writes
/// on /dev/hidg0/1/2 — a fresh process gets a clean slate.
async fn post_persona(
    State(state): State<crate::state::SharedState>,
    Json(req): Json<PersonaReq>,
) -> impl IntoResponse {
    use crate::config::Persona;
    let Some(new_persona) = Persona::from_slug(&req.persona) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "err": format!(
                    "unknown persona '{}'; expected generic-composite, logitech-mx, apple-magic-stable, or apple-magic",
                    req.persona
                ),
            })),
        );
    };

    if new_persona == state.0.cfg.persona {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "no_op": true,
                "persona": new_persona.as_slug(),
            })),
        );
    }

    // Persist the new selection before the process exits — so the next
    // boot (and the next systemd-managed respawn after our exit) picks it
    // up.
    let path = &state.0.cfg.persona_state_path;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, new_persona.as_slug()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist failed: {e}")})),
        );
    }

    let response_body = Json(json!({
        "ok": true,
        "persona": new_persona.as_slug(),
        "note": "USB re-enumerates in ~1s",
    }));

    // Schedule a process exit AFTER this HTTP response flushes. We can't
    // exit immediately because that would close the connection mid-write.
    // A short sleep + std::process::exit(0) is the simplest reliable path
    // — far cleaner than trying to in-place tear down + rebuild configfs.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        tracing::info!(
            persona = new_persona.as_slug(),
            "exiting for systemd restart with new persona",
        );
        std::process::exit(0);
    });

    (StatusCode::OK, response_body)
}
