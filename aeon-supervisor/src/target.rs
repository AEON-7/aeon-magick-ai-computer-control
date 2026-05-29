//! Remote power control for the USB-connected target machine.
//!
//! Three orthogonal mechanisms, exposed under /api/target/*:
//!
//!   1. **Soft tap** (POST /api/target/power-tap)
//!      A 200ms press on the USB HID Consumer Power button (usage
//!      0x30). Most modern OSes interpret this the same as a tap on
//!      the physical chassis power button — so Windows shows the
//!      power menu, macOS shows the shutdown dialog, Linux usually
//!      kicks off a clean shutdown. Quiet and graceful.
//!
//!   2. **Hard hold** (POST /api/target/power-hold)
//!      An 8-second press on the same button. Every modern board
//!      treats this as a forced hardware power-off — bypasses the OS
//!      entirely. Useful when the target is wedged or the OS won't
//!      let go. Comes back via WoL or a manual button press.
//!
//!   3. **Wake** (POST /api/target/wake)
//!      Wake-on-LAN magic packet over usb0 to the target's MAC. Works
//!      when the target is in S5 (full off) ONLY IF WoL is enabled in
//!      its BIOS/UEFI AND the OS left the NIC armed for wake. Lots of
//!      laptops disable WoL on battery; desktops are usually fine.
//!
//! And one orchestrated convenience:
//!
//!   4. **Reboot** (POST /api/target/reboot)
//!      Hard hold → 5-second pause → wake. The orchestrated sequence
//!      most users want for the "load an ISO, reboot, enter boot
//!      menu, install OS" workflow.
//!
//! MAC discovery: we try to read the target's MAC from the kernel's
//! ARP/neighbour table (`/proc/net/arp` and `ip neigh show dev usb0`)
//! since the supervisor-driven dnsmasq leases live in NM's runtime
//! state and aren't easy to parse cleanly. The user can override the
//! discovered MAC via /api/target/config — saved to `[target] mac`
//! in network.toml so it survives across boots.
//!
//! Why not also use the Sleep / Wake consumer codes (0x32 / 0x83)?
//! Most OSes ignore HID Sleep events on principle (they're rarely
//! useful and easy to misfire). Power button is the universal hammer.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::sleep;

const NETWORK_TOML: &str = "/etc/aeon/network.toml";

// ── Persisted config (lives in [target] in network.toml) ─────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TargetConfig {
    /// Override the auto-discovered target MAC. Format
    /// "aa:bb:cc:dd:ee:ff" (lowercase, colon-separated). Empty means
    /// "use whatever ARP/neighbour discovery turns up".
    #[serde(default)]
    pub mac: String,
    /// The interface to send WoL packets on. Default usb0. Override
    /// for users who have non-standard USB-net setups.
    #[serde(default = "default_iface")]
    pub iface: String,
}
fn default_iface() -> String { "usb0".into() }

impl Default for TargetConfig {
    fn default() -> Self {
        // Match the serde default path. Without this, when network.toml
        // doesn't have a [target] section at all, the supervisor falls
        // back to Default::default() (which gives iface="" on the
        // String field) instead of running the #[serde(default = ...)]
        // function. Same bug we hit in v48's file_xfer module.
        Self {
            mac: String::new(),
            iface: default_iface(),
        }
    }
}

#[derive(Deserialize)]
struct ConfigRootForTarget {
    #[serde(default)]
    target: TargetConfig,
}

fn read_config() -> TargetConfig {
    let mut cfg = std::fs::read_to_string(NETWORK_TOML)
        .ok()
        .and_then(|t| toml::from_str::<ConfigRootForTarget>(&t).ok())
        .map(|r| r.target)
        .unwrap_or_default();
    // Legacy: TOMLs written before the Default fix can have iface=""
    // saved on disk. Treat empty as the canonical default rather than
    // letting WoL silently try to send via no interface.
    if cfg.iface.trim().is_empty() {
        cfg.iface = default_iface();
    }
    cfg
}

// ── MAC discovery ─────────────────────────────────────────────────────

/// Walk /proc/net/arp looking for a usable target MAC on the
/// configured interface. Skips the "incomplete" entries (MAC of all
/// zeros) and our own MAC. Returns None if nothing found — caller
/// should fall back to the persisted override or surface a clear UI
/// hint to the user.
fn discover_target_mac(iface: &str) -> Option<String> {
    let arp = std::fs::read_to_string("/proc/net/arp").ok()?;
    // /proc/net/arp format:
    //   IP address       HW type     Flags       HW address            Mask     Device
    //   10.55.0.2        0x1         0x2         f4:5c:89:aa:bb:cc     *        usb0
    for line in arp.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        let mac = cols[3];
        let dev = cols[5];
        if dev != iface {
            continue;
        }
        if mac == "00:00:00:00:00:00" {
            continue;
        }
        return Some(mac.to_string());
    }
    None
}

fn target_mac(cfg: &TargetConfig) -> Option<String> {
    if !cfg.mac.is_empty() {
        return Some(cfg.mac.clone());
    }
    discover_target_mac(&cfg.iface)
}

// ── WoL magic packet ──────────────────────────────────────────────────

fn parse_mac(s: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = s.split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return Err(format!("malformed MAC '{s}' — want aa:bb:cc:dd:ee:ff"));
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16)
            .map_err(|_| format!("bad hex byte '{p}' in MAC"))?;
    }
    Ok(out)
}

/// Build a 102-byte Wake-on-LAN magic payload: 6 bytes of 0xFF
/// followed by the target MAC repeated 16 times.
fn build_magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut buf = [0u8; 102];
    buf[0..6].fill(0xFF);
    for i in 0..16 {
        let off = 6 + i * 6;
        buf[off..off + 6].copy_from_slice(&mac);
    }
    buf
}

/// Fire the WoL magic packet over the configured interface to the
/// configured (or discovered) MAC. Sent as UDP to 255.255.255.255:9
/// (and :7 as a courtesy — some BIOSes listen there instead).
fn send_wol(cfg: &TargetConfig, mac_str: &str) -> Result<(), String> {
    let mac = parse_mac(mac_str)?;
    let magic = build_magic_packet(mac);

    // We bind to 0.0.0.0:0 with broadcast enabled. The kernel routes
    // the broadcast out the interface that owns the matching route —
    // which for 10.55.0.x is usb0 in our setup. We don't try to bind
    // to a specific interface (SO_BINDTODEVICE needs CAP_NET_RAW).
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| format!("bind: {e}"))?;
    sock.set_broadcast(true)
        .map_err(|e| format!("set_broadcast: {e}"))?;
    // Direct to subnet broadcast (10.55.0.255) and to the limited
    // broadcast (255.255.255.255). The subnet form is the one most
    // BIOSes actually listen on through their NIC's wake logic.
    for target_addr in &["10.55.0.255:9", "10.55.0.255:7", "255.255.255.255:9"] {
        if let Err(e) = sock.send_to(&magic, *target_addr) {
            tracing::warn!("wol: send to {target_addr} failed: {e}");
        }
    }
    tracing::info!("wol: sent magic packet (mac={mac_str}, iface={})", cfg.iface);
    Ok(())
}

// ── HID power button ──────────────────────────────────────────────────

/// HID Consumer Page usage code for the Power button.
const HID_CONSUMER_POWER: u16 = 0x0030;

async fn press_power_button(state: &AppState, hold_ms: u32) -> Result<(), String> {
    let body = serde_json::to_vec(&json!({
        "usage": HID_CONSUMER_POWER,
        "hold_ms": hold_ms,
    }))
    .map_err(|e| format!("serialize: {e}"))?;
    crate::proxy::post_hid(state, "/consumer", body).await
}

// ── Public API handlers ──────────────────────────────────────────────

/// GET /api/target/info — current target config + discovery state.
pub async fn get_info(State(_state): State<AppState>) -> Json<Value> {
    let cfg = read_config();
    let discovered = discover_target_mac(&cfg.iface);
    let effective = if !cfg.mac.is_empty() {
        Some(cfg.mac.clone())
    } else {
        discovered.clone()
    };
    Json(json!({
        "ok": true,
        "mac_override": cfg.mac,
        "iface": cfg.iface,
        "mac_discovered": discovered,
        "mac_effective": effective,
        // Surface the canonical operating modes so the UI knows what
        // labels to render without hardcoding them on the client.
        "modes": [
            { "id": "tap",   "label": "Tap power button",  "hint": "Short press; OS-managed" },
            { "id": "hold",  "label": "Force power off",   "hint": "8-second hold; hardware-level off" },
            { "id": "wake",  "label": "Wake / power on",   "hint": "WoL magic packet" },
            { "id": "reboot","label": "Reboot",            "hint": "Force off + wait 5s + WoL" },
        ],
    }))
}

#[derive(Deserialize)]
pub struct ConfigPutReq {
    #[serde(default)] pub mac: Option<String>,
    #[serde(default)] pub iface: Option<String>,
}

/// PUT /api/target/config — persist MAC override + interface choice.
pub async fn put_config(
    State(_state): State<AppState>,
    Json(req): Json<ConfigPutReq>,
) -> impl IntoResponse {
    let mut cfg = read_config();
    if let Some(m) = req.mac {
        let trimmed = m.trim().to_ascii_lowercase();
        if !trimmed.is_empty() {
            if let Err(e) = parse_mac(&trimmed) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": e})),
                )
                    .into_response();
            }
        }
        cfg.mac = trimmed;
    }
    if let Some(i) = req.iface {
        // Reject anything with shell-injection-y characters; iface
        // names are short ASCII identifiers.
        if !i.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "iface name has invalid characters"})),
            )
                .into_response();
        }
        if i.len() > 16 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "iface name too long"})),
            )
                .into_response();
        }
        cfg.iface = i;
    }
    if let Err(e) = persist_config(&cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "mac": cfg.mac, "iface": cfg.iface})).into_response()
}

/// Read-modify-write the [target] section of network.toml without
/// stomping on the other sections. Done as a string substitution
/// because the rest of network.toml has unparsed dynamic content
/// (vpn provider stanzas, etc.) that round-trips poorly through
/// the toml crate.
fn persist_config(cfg: &TargetConfig) -> std::io::Result<()> {
    let text = std::fs::read_to_string(NETWORK_TOML).unwrap_or_default();
    let new_section = format!(
        "[target]\nmac = \"{}\"\niface = \"{}\"\n",
        cfg.mac, cfg.iface
    );

    // Either replace the existing [target] block in place, or append
    // it. We assume the section runs until the next [section] header
    // or end-of-file.
    let out = if text.contains("[target]") {
        // Capture from "[target]" through (but not including) the
        // next "[" at start-of-line, or end-of-string.
        let mut new_text = String::with_capacity(text.len());
        let mut i = 0;
        while i < text.len() {
            if text[i..].starts_with("[target]") {
                // Find the next [section] header
                let after = &text[i..];
                let end_rel = after.lines().enumerate().skip(1)
                    .find(|(_, l)| l.starts_with('['))
                    .map(|(line_idx, _)| {
                        after.split('\n').take(line_idx).map(|l| l.len() + 1).sum::<usize>()
                    })
                    .unwrap_or(after.len());
                new_text.push_str(&new_section);
                if end_rel < after.len() && !new_text.ends_with('\n') {
                    new_text.push('\n');
                }
                i += end_rel;
            } else {
                new_text.push(text.as_bytes()[i] as char);
                i += 1;
            }
        }
        new_text
    } else {
        let mut out = text;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&new_section);
        out
    };
    std::fs::write(NETWORK_TOML, out)
}

/// POST /api/target/power-tap — short press (200ms).
pub async fn power_tap(State(state): State<AppState>) -> impl IntoResponse {
    crate::audit::log("admin (session)", "target_power_tap",
        "HID power button short press", "ok", None);
    match press_power_button(&state, 200).await {
        Ok(_) => Json(json!({"ok": true, "mode": "tap", "hold_ms": 200})).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": format!("hid: {e}")}))).into_response(),
    }
}

/// POST /api/target/power-hold — 8-second hold (force hardware off).
pub async fn power_hold(State(state): State<AppState>) -> impl IntoResponse {
    crate::audit::log("admin (session)", "target_power_hold",
        "HID power button 8s hold (force off)", "ok", None);
    match press_power_button(&state, 8000).await {
        Ok(_) => Json(json!({"ok": true, "mode": "hold", "hold_ms": 8000})).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": format!("hid: {e}")}))).into_response(),
    }
}

/// POST /api/target/wake — Wake-on-LAN magic packet.
pub async fn wake(State(_state): State<AppState>) -> impl IntoResponse {
    let cfg = read_config();
    let Some(mac) = target_mac(&cfg) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "err": "no target MAC — either bring usb0 up + let the target DHCP (so ARP populates), or set a MAC override via PUT /api/target/config",
            })),
        )
            .into_response();
    };
    match send_wol(&cfg, &mac) {
        Ok(_) => {
            crate::audit::log("admin (session)", "target_wake",
                &format!("WoL magic packet (mac={mac})"), "ok", None);
            Json(json!({"ok": true, "mac": mac, "iface": cfg.iface})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e})),
        )
            .into_response(),
    }
}

/// POST /api/target/reboot — orchestrated: hard hold → 5s → WoL.
/// The 5-second gap is enough for most boards to finish their
/// post-poweroff transition before re-arming WoL.
pub async fn reboot(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = read_config();
    let Some(mac) = target_mac(&cfg) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "err": "no target MAC — reboot needs WoL after the force-off; configure mac via PUT /api/target/config",
            })),
        )
            .into_response();
    };
    crate::audit::log("admin (session)", "target_reboot",
        &format!("force-off + WoL (mac={mac})"), "starting", None);

    // Hard power-hold first.
    if let Err(e) = press_power_button(&state, 8000).await {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({"ok": false, "err": format!("hid: {e}")})),
        )
            .into_response();
    }

    // Pause for the board to finish powering down + transitioning to
    // S5. Doing this inline keeps the API call a single round-trip
    // for the UI — the user clicks "reboot" and waits for one OK.
    sleep(Duration::from_secs(5)).await;

    if let Err(e) = send_wol(&cfg, &mac) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("wol: {e}")})),
        )
            .into_response();
    }

    Json(json!({
        "ok": true,
        "mode": "reboot",
        "mac": mac,
        "iface": cfg.iface,
        "phases": ["power-hold:8s", "wait:5s", "wol"],
    }))
    .into_response()
}
