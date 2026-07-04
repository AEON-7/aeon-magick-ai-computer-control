//! /api/nas — optional LAN file sharing (Samba/SMB). OFF by default.
//!
//! Enabling installs Samba on first use and exposes two READ/WRITE shares —
//! the model library and a general "Aeon Share" folder — both authenticated as
//! the `admin` account with a password set here (nothing is served
//! unauthenticated). Privileged work lives in the `aeon-nas` helper.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

use crate::api::AppState;

const SCRIPT: &str = "/usr/local/bin/aeon-nas";

fn run(args: &[&str]) -> Result<String, String> {
    let out = Command::new(SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .lines()
            .last()
            .unwrap_or("nas op failed")
            .to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// GET /api/nas/status — installed? running? share paths + host.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| match run(&["status"]) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({"ok": false, "err": s})),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct EnableReq {
    pub password: String,
}

/// POST /api/nas/enable — install (first time) + expose the shares, protected by
/// the given SMB password. Installing Samba can take a while, so run it detached
/// and let the UI poll status.
pub async fn enable(State(_s): State<AppState>, Json(req): Json<EnableReq>) -> Json<Value> {
    if req.password.len() < 4 {
        return Json(json!({"ok": false, "err": "choose a password of at least 4 characters"}));
    }
    // Reject anything that could break the argv boundary into the helper.
    if req.password.contains('\n') || req.password.contains('\0') {
        return Json(json!({"ok": false, "err": "invalid password"}));
    }
    let pw = req.password.clone();
    let v = tokio::task::spawn_blocking(move || match run(&["enable", &pw]) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "enable task failed"}));
    Json(v)
}

/// POST /api/nas/disable — stop + disable smbd (config left in place).
pub async fn disable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| match run(&["disable"]) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "disable task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct PassReq {
    pub password: String,
}

/// POST /api/nas/password — change the SMB password without touching config.
pub async fn set_password(State(_s): State<AppState>, Json(req): Json<PassReq>) -> Json<Value> {
    if req.password.len() < 4 || req.password.contains('\n') || req.password.contains('\0') {
        return Json(json!({"ok": false, "err": "invalid password (min 4 chars)"}));
    }
    let pw = req.password.clone();
    let v = tokio::task::spawn_blocking(move || match run(&["set-pass", &pw]) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "password task failed"}));
    Json(v)
}
