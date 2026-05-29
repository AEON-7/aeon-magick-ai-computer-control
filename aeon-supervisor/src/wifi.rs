//! WiFi management endpoints — scan, connect, current-state inspection.
//!
//! Used by:
//!   - The /setup-wifi page during AP-setup mode (when the Pi has
//!     fallen back to running its own `aeon-setup` AP because no known
//!     network is reachable; client connects to the AP and uses these
//!     endpoints to configure WiFi credentials, then the Pi reconnects
//!     as a regular client)
//!   - The advanced network panel in the authenticated UI for ongoing
//!     WiFi management after first-boot
//!
//! All four endpoints shell out to `nmcli` — NetworkManager is the
//! source of truth for WiFi state on Pi OS Bookworm.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// GET /api/wifi/scan — list of nearby SSIDs with signal strength.
///
/// Returns:
/// ```
/// { "ok": true, "networks": [
///   {"ssid": "Home WiFi", "signal": 78, "security": "WPA2", "in_use": false},
///   ...
/// ]}
/// ```
pub async fn scan(State(_state): State<AppState>) -> impl IntoResponse {
    // Trigger a scan first (synchronous) — without this, nmcli returns
    // the previous scan's cached results which can be stale.
    let _ = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["device", "wifi", "rescan"])
            .output()
    })
    .await;

    // Now list. `-t -f` mode gives one network per line, colon-separated.
    let result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "device", "wifi", "list"])
            .output()
    })
    .await;

    let output = match result {
        Ok(Ok(out)) if out.status.success() => out,
        Ok(Ok(out)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "ok": false,
                    "err": format!("nmcli wifi list exit {:?}: {}",
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr).trim()),
                })),
            )
                .into_response();
        }
        _ => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": "nmcli scan failed"})),
            )
                .into_response();
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let mut networks: Vec<Value> = Vec::new();
    let mut seen_ssids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in text.lines() {
        // Format: <IN-USE>:<SSID>:<SIGNAL>:<SECURITY>
        // IN-USE is "*" if currently connected, "" otherwise
        // nmcli escapes colons in SSIDs with backslash — split on
        // unescaped colons only.
        let fields: Vec<&str> = line.splitn(4, ':').collect();
        if fields.len() < 4 {
            continue;
        }
        let ssid = fields[1].trim();
        if ssid.is_empty() {
            continue;
        }
        // De-duplicate — nmcli often returns the same SSID multiple
        // times (one per BSSID). Pick the strongest signal.
        if !seen_ssids.insert(ssid.to_string()) {
            continue;
        }
        let signal = fields[2].parse::<u8>().unwrap_or(0);
        let security = fields[3].trim().to_string();
        let in_use = fields[0] == "*";

        networks.push(json!({
            "ssid": ssid,
            "signal": signal,
            "security": if security.is_empty() { "open".to_string() } else { security },
            "in_use": in_use,
        }));
    }

    // Strongest signal first
    networks.sort_by(|a, b| {
        b["signal"].as_u64().unwrap_or(0).cmp(&a["signal"].as_u64().unwrap_or(0))
    });

    Json(json!({"ok": true, "networks": networks})).into_response()
}

#[derive(Deserialize)]
pub struct ConnectReq {
    pub ssid: String,
    #[serde(default)]
    pub password: Option<String>,
}

/// POST /api/wifi/connect — provision a new WiFi connection profile
/// and bring it up. The user posts SSID + password from the setup
/// page; we run `nmcli con add` + `nmcli con up` and report back.
pub async fn connect(
    State(_state): State<AppState>,
    Json(req): Json<ConnectReq>,
) -> impl IntoResponse {
    if req.ssid.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "ssid is required"})),
        )
            .into_response();
    }

    let ssid = req.ssid.clone();
    let password = req.password.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // Delete any existing connection profile with this SSID name so
        // we get a clean slate. `nmcli con delete` is a no-op if the
        // profile doesn't exist.
        let _ = std::process::Command::new("nmcli")
            .args(["connection", "delete", &ssid])
            .output();

        // Create the profile
        let mut add_args = vec![
            "connection".to_string(),
            "add".to_string(),
            "type".to_string(),
            "wifi".to_string(),
            "con-name".to_string(),
            ssid.clone(),
            "ifname".to_string(),
            "wlan0".to_string(),
            "ssid".to_string(),
            ssid.clone(),
        ];
        if let Some(pw) = &password {
            add_args.extend([
                "wifi-sec.key-mgmt".to_string(),
                "wpa-psk".to_string(),
                "wifi-sec.psk".to_string(),
                pw.clone(),
            ]);
        }
        let add_out = std::process::Command::new("nmcli")
            .args(&add_args)
            .output()
            .map_err(|e| format!("nmcli con add spawn: {e}"))?;
        if !add_out.status.success() {
            return Err(format!(
                "nmcli con add: {}",
                String::from_utf8_lossy(&add_out.stderr).trim()
            ));
        }

        // Activate it. Times out after 30s — nmcli will return non-zero
        // and we surface that to the UI.
        let up_out = std::process::Command::new("nmcli")
            .args(["--wait", "30", "connection", "up", &ssid])
            .output()
            .map_err(|e| format!("nmcli con up spawn: {e}"))?;
        if !up_out.status.success() {
            // The profile was added — leave it for the user to retry
            // or delete via the UI. Return the failure detail.
            return Err(format!(
                "nmcli con up: {}",
                String::from_utf8_lossy(&up_out.stderr).trim()
            ));
        }

        Ok(format!("connected to {}", ssid))
    })
    .await;

    match result {
        Ok(Ok(msg)) => Json(json!({"ok": true, "msg": msg})).into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": err})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("task join: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/wifi/state — current WiFi state: connected SSID, signal,
/// AP-fallback mode active or not.
pub async fn state(State(_state): State<AppState>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["-t", "-f", "NAME,TYPE,STATE,DEVICE", "connection", "show", "--active"])
            .output()
    })
    .await;

    let mut state_info = json!({
        "ok": true,
        "connected_ssid": null,
        "ap_mode": false,
        "wifi_radio_on": true,
    });

    if let Ok(Ok(out)) = result {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let fields: Vec<&str> = line.splitn(4, ':').collect();
            if fields.len() < 4 {
                continue;
            }
            let name = fields[0];
            let conn_type = fields[1];
            let device = fields[3];
            // The fallback AP profile is named "aeon-setup" by convention.
            if name == "aeon-setup" {
                state_info["ap_mode"] = json!(true);
            } else if conn_type == "802-11-wireless" && device == "wlan0" {
                state_info["connected_ssid"] = json!(name);
            }
        }
    }

    // Radio on/off
    let radio = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["radio", "wifi"])
            .output()
    })
    .await;
    if let Ok(Ok(out)) = radio {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        state_info["wifi_radio_on"] = json!(s == "enabled");
    }

    Json(state_info).into_response()
}

/// POST /api/wifi/disconnect — tear down the active WiFi connection
/// (the named profile, not the radio). Useful for "forget this
/// network" UX.
pub async fn disconnect(
    State(_state): State<AppState>,
    Json(req): Json<DisconnectReq>,
) -> impl IntoResponse {
    if req.ssid.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "ssid required"})),
        )
            .into_response();
    }
    let ssid = req.ssid.clone();
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("nmcli")
            .args(["connection", "down", &ssid])
            .output()
    })
    .await;
    match result {
        Ok(Ok(out)) if out.status.success() => Json(json!({"ok": true})).into_response(),
        Ok(Ok(out)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "err": String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": "nmcli disconnect failed"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct DisconnectReq {
    pub ssid: String,
}

// ─────────────────────────────────────────────────────────────────────────
// v63: known-network management + AP-mode toggle.
//
// The /setup-wifi page covers first-boot. Once a user is in the
// authenticated UI they want a proper /wifi page that:
//   * lists every saved NetworkManager wifi connection profile
//     (so they can forget / re-prioritize)
//   * can flip a per-profile autoconnect toggle
//   * can switch between client mode and "I want this Pi to BE an AP"
//   * lets them set the AP-mode SSID + password, which also become
//     the credentials used by the no-internet fallback AP that
//     aeon-netwatch spins up at boot
// ─────────────────────────────────────────────────────────────────────────

/// GET /api/wifi/known — list saved wifi connection profiles.
pub async fn list_known(State(_state): State<AppState>) -> impl IntoResponse {
    // Listing NetworkManager profiles + filtering to wifi type, then
    // for each one query autoconnect + priority via `nmcli -g`. Cheap
    // — typical user has <10 saved networks.
    let listing = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["-t", "-f", "NAME,TYPE,DEVICE", "connection", "show"])
            .output()
    })
    .await;

    let text = match listing {
        Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        _ => return Json(json!({"ok": true, "networks": []})).into_response(),
    };

    let mut networks: Vec<Value> = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.splitn(3, ':').collect();
        if fields.len() < 2 || fields[1] != "802-11-wireless" {
            continue;
        }
        let name = fields[0].to_string();
        // Skip the fallback AP profile from the user-visible list —
        // it's managed by aeon-netwatch, not by the user, and "forget"
        // would break first-boot for the next operator.
        if name == "aeon-setup" {
            continue;
        }
        let active = fields.get(2).map(|s| !s.is_empty()).unwrap_or(false);

        // Per-profile metadata via -g (get-value).
        let meta = std::process::Command::new("nmcli")
            .args([
                "-g",
                "connection.autoconnect,connection.autoconnect-priority,802-11-wireless.mode,802-11-wireless.ssid",
                "connection",
                "show",
                &name,
            ])
            .output();
        let (autoconnect, priority, mode, ssid) = match meta {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout);
                let mut it = s.lines();
                let ac = it.next().unwrap_or("yes").trim() == "yes";
                let pri: i64 = it.next().unwrap_or("0").trim().parse().unwrap_or(0);
                let md = it.next().unwrap_or("infrastructure").trim().to_string();
                let sd = it.next().unwrap_or(&name).trim().to_string();
                (ac, pri, md, if sd.is_empty() { name.clone() } else { sd })
            }
            Err(_) => (true, 0, "infrastructure".to_string(), name.clone()),
        };

        networks.push(json!({
            "profile": name,
            "ssid": ssid,
            "autoconnect": autoconnect,
            "priority": priority,
            "mode": mode,
            "active": active,
        }));
    }

    Json(json!({"ok": true, "networks": networks})).into_response()
}

#[derive(Deserialize)]
pub struct ProfileReq {
    pub profile: String,
}

/// DELETE /api/wifi/known — forget a saved profile entirely.
pub async fn forget(
    State(_state): State<AppState>,
    Json(req): Json<ProfileReq>,
) -> impl IntoResponse {
    if req.profile.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "profile required"}))).into_response();
    }
    // Defense against operator footgun — refuse to delete the boot
    // fallback profile via this UI flow.
    if req.profile == "aeon-setup" {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "aeon-setup is the boot-fallback AP — managed by aeon-netwatch, not removable here"}))).into_response();
    }
    let profile = req.profile.clone();
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("nmcli")
            .args(["connection", "delete", &profile])
            .output()
    })
    .await;
    match result {
        Ok(Ok(out)) if out.status.success() => Json(json!({"ok": true})).into_response(),
        Ok(Ok(out)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "ok": false,
            "err": String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }))).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
              Json(json!({"ok": false, "err": "nmcli delete failed"}))).into_response(),
    }
}

#[derive(Deserialize)]
pub struct AutoconnectReq {
    pub profile: String,
    pub enabled: bool,
}

/// POST /api/wifi/autoconnect — flip a profile's autoconnect flag.
pub async fn set_autoconnect(
    State(_state): State<AppState>,
    Json(req): Json<AutoconnectReq>,
) -> impl IntoResponse {
    if req.profile.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "profile required"}))).into_response();
    }
    let val = if req.enabled { "yes" } else { "no" }.to_string();
    let profile = req.profile.clone();
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("nmcli")
            .args([
                "connection", "modify", &profile,
                "connection.autoconnect", &val,
            ])
            .output()
    })
    .await;
    match result {
        Ok(Ok(out)) if out.status.success() => Json(json!({"ok": true, "autoconnect": req.enabled})).into_response(),
        Ok(Ok(out)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "ok": false,
            "err": String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }))).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
              Json(json!({"ok": false, "err": "nmcli modify failed"}))).into_response(),
    }
}

/// GET /api/wifi/ap — current AP-mode config (credentials used in
/// both user-toggled AP mode and the no-internet boot-fallback).
pub async fn ap_get(State(_state): State<AppState>) -> impl IntoResponse {
    // The fallback AP profile is named "aeon-setup". We read its SSID
    // straight back so the UI can show "currently broadcasting as X".
    // Password is NEVER echoed — only "is one set?".
    let result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["-g", "802-11-wireless.ssid,802-11-wireless-security.psk-flags",
                   "connection", "show", "aeon-setup"])
            .output()
    })
    .await;

    // Also figure out whether the AP is currently active.
    let active = tokio::task::spawn_blocking(|| {
        std::process::Command::new("nmcli")
            .args(["-t", "-f", "NAME", "connection", "show", "--active"])
            .output()
    })
    .await;

    let mut ssid = "aeon-setup".to_string();
    let mut has_password = false;
    let mut is_active = false;

    if let Ok(Ok(out)) = result {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let mut it = s.lines();
            if let Some(line) = it.next() {
                let line = line.trim();
                if !line.is_empty() {
                    ssid = line.to_string();
                }
            }
            // psk-flags=0 means "store in plaintext" — password present.
            // psk-flags=1 means agent-owned (NM stores it but won't echo).
            // No psk-flags line = no security set at all (open AP).
            if let Some(flags) = it.next() {
                has_password = !flags.trim().is_empty();
            }
        }
    }

    if let Ok(Ok(out)) = active {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if line.trim() == "aeon-setup" {
                is_active = true;
                break;
            }
        }
    }

    Json(json!({
        "ok": true,
        "ssid": ssid,
        "has_password": has_password,
        "active": is_active,
        // Default password used by aeon-netwatch's first-boot fallback.
        // We expose this so the UI can tell the user what it currently
        // is when they've never customized it.
        "default_ssid": "aeon-setup",
        "default_password": "aeon-setup-pw",
    })).into_response()
}

#[derive(Deserialize)]
pub struct ApSetReq {
    pub ssid: String,
    #[serde(default)]
    pub password: Option<String>,
    /// When true, bring the AP up immediately after writing the
    /// profile. When false (the common case for "I'm using client
    /// mode now, just save the AP creds for the next boot-fallback")
    /// we write the profile and leave activation to aeon-netwatch.
    #[serde(default)]
    pub activate: bool,
}

/// PUT /api/wifi/ap — set AP-mode SSID + password.
///
/// The `aeon-setup` profile is the source of truth for BOTH:
///   * the AP that aeon-netwatch brings up when no client network is
///     reachable at boot (the captive-portal fallback)
///   * the AP the user explicitly switches into via the /wifi page
///
/// So changing these credentials affects both flows. We document this
/// loudly in the UI.
pub async fn ap_set(
    State(_state): State<AppState>,
    Json(req): Json<ApSetReq>,
) -> impl IntoResponse {
    if req.ssid.trim().is_empty() {
        return (StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "ssid required"}))).into_response();
    }
    if let Some(pw) = &req.password {
        // WPA2 PSK constraint — 8 char minimum or NetworkManager refuses.
        if !pw.is_empty() && pw.len() < 8 {
            return (StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "WPA2 password must be at least 8 characters"}))).into_response();
        }
    }

    let ssid = req.ssid.clone();
    let password = req.password.clone();
    let activate = req.activate;

    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        // Idempotent: delete + recreate. NetworkManager's modify-in-place
        // is fiddly for AP-mode profiles (mode + band + ipv4.method all
        // interlock); a clean recreate is simpler and survives upgrades.
        let _ = std::process::Command::new("nmcli")
            .args(["connection", "delete", "aeon-setup"])
            .output();

        let mut args: Vec<String> = vec![
            "connection".into(), "add".into(),
            "type".into(), "wifi".into(),
            "ifname".into(), "wlan0".into(),
            "con-name".into(), "aeon-setup".into(),
            "autoconnect".into(), "no".into(),
            "ssid".into(), ssid.clone(),
            "mode".into(), "ap".into(),
            "ipv4.method".into(), "shared".into(),
            "ipv4.addresses".into(), "10.42.0.1/24".into(),
            "ipv6.method".into(), "ignore".into(),
            "802-11-wireless.band".into(), "bg".into(),
            "802-11-wireless.channel".into(), "6".into(),
        ];
        if let Some(pw) = &password {
            if !pw.is_empty() {
                args.extend([
                    "wifi-sec.key-mgmt".into(), "wpa-psk".into(),
                    "wifi-sec.psk".into(), pw.clone(),
                ]);
            }
        }

        let add_out = std::process::Command::new("nmcli")
            .args(&args)
            .output()
            .map_err(|e| format!("nmcli add: {e}"))?;
        if !add_out.status.success() {
            return Err(format!("nmcli add aeon-setup: {}",
                String::from_utf8_lossy(&add_out.stderr).trim()));
        }

        if activate {
            let up = std::process::Command::new("nmcli")
                .args(["--wait", "15", "connection", "up", "aeon-setup"])
                .output()
                .map_err(|e| format!("nmcli up: {e}"))?;
            if !up.status.success() {
                return Err(format!("nmcli up aeon-setup: {}",
                    String::from_utf8_lossy(&up.stderr).trim()));
            }
        }
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => Json(json!({"ok": true})).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR,
                       Json(json!({"ok": false, "err": e}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
                   Json(json!({"ok": false, "err": format!("task join: {e}")}))).into_response(),
    }
}

#[derive(Deserialize)]
pub struct RadioReq {
    pub on: bool,
}

/// POST /api/wifi/radio — radio on/off. Used to implement the
/// "Disable WiFi entirely" choice on the /wifi page (e.g. operator
/// running ethernet-only and doesn't want a beacon visible at all).
pub async fn radio(
    State(_state): State<AppState>,
    Json(req): Json<RadioReq>,
) -> impl IntoResponse {
    let val = if req.on { "on" } else { "off" }.to_string();
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("nmcli")
            .args(["radio", "wifi", &val])
            .output()
    })
    .await;
    match result {
        Ok(Ok(out)) if out.status.success() => Json(json!({"ok": true, "radio_on": req.on})).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR,
              Json(json!({"ok": false, "err": "nmcli radio failed"}))).into_response(),
    }
}
