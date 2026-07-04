//! /api/disks — external USB/SSD storage: detect drives, format a new one
//! ("Prepare Device"), and adopt a prepared drive as the Orb's data store
//! (the IPFS repo + model library relocate onto it via bind mounts).
//!
//! All privileged block work — lsblk, wipefs, parted, mkfs, mount, fstab —
//! lives in the `aeon-storage` helper (run as root, mirroring `aeon-ipfs`).
//! This module is the HTTP surface. The DESTRUCTIVE `prepare` is guarded on
//! BOTH sides: the helper refuses the boot disk / any non-removable or
//! system-mounted device, and this layer requires the caller to echo the exact
//! `FORMAT` confirmation string.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Command;

use crate::api::AppState;

const SCRIPT: &str = "/usr/local/bin/aeon-storage";

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
            .unwrap_or("storage op failed")
            .to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn run_json(args: &[&str]) -> Value {
    match run(args) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({"ok": false, "err": s})),
        Err(e) => json!({"ok": false, "err": e}),
    }
}

/// A real block-device path, no shell metacharacters. (Command doesn't invoke a
/// shell, but this keeps obviously-bogus input out of the helper.)
fn valid_dev(d: &str) -> bool {
    d.starts_with("/dev/")
        && d.len() > 5
        && d[5..].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-'))
}

/// GET /api/disks — external drives + per-drive status (available / needs_prepare / in_use).
pub async fn list(State(_s): State<AppState>) -> Json<Value> {
    Json(
        tokio::task::spawn_blocking(|| run_json(&["list"]))
            .await
            .unwrap_or_else(|_| json!({"ok": false, "err": "list task failed"})),
    )
}

/// GET /api/disks/status — the currently adopted drive (if any) + free/used.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    Json(
        tokio::task::spawn_blocking(|| run_json(&["status"]))
            .await
            .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"})),
    )
}

#[derive(Deserialize)]
pub struct PrepareReq {
    pub dev: String,
    /// Must be the literal "FORMAT" — a deliberate, server-side wipe gate.
    #[serde(default)]
    pub confirm: String,
}

/// POST /api/disks/prepare — DESTRUCTIVE: wipe + one ext4 partition (AEON-DATA).
pub async fn prepare(State(_s): State<AppState>, Json(req): Json<PrepareReq>) -> Json<Value> {
    if req.confirm != "FORMAT" {
        return Json(json!({"ok": false, "err": "type FORMAT to confirm — this erases the entire drive"}));
    }
    if !valid_dev(&req.dev) {
        return Json(json!({"ok": false, "err": "invalid device path"}));
    }
    let dev = req.dev.clone();
    let v = tokio::task::spawn_blocking(move || match run(&["prepare", &dev, "FORMAT"]) {
        Ok(part) => json!({"ok": true, "partition": part}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "prepare task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct DevReq {
    pub dev: String,
}

/// POST /api/disks/use — adopt a prepared drive as the Orb's data store.
pub async fn adopt(State(_s): State<AppState>, Json(req): Json<DevReq>) -> Json<Value> {
    if !valid_dev(&req.dev) {
        return Json(json!({"ok": false, "err": "invalid device path"}));
    }
    let dev = req.dev.clone();
    let v = tokio::task::spawn_blocking(move || match run(&["use", &dev]) {
        Ok(mnt) => json!({"ok": true, "mount": mnt}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "adopt task failed"}));
    Json(v)
}

/// POST /api/disks/release — revert to internal storage (unbind + drop fstab).
pub async fn release(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| match run(&["release"]) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "release task failed"}));
    Json(v)
}
