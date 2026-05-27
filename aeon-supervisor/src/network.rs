//! /api/network/* — read & write USB-ethernet, DNSCrypt, and VPN config.
//!
//! All state lives in /etc/aeon/network.toml. PUTs write the file then
//! ask systemd to reload the matching service (aeon-usb-net for usb_ethernet,
//! aeon-net-services for dnscrypt + vpn), which re-applies the runtime
//! state idempotently.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

const NETWORK_TOML: &str = "/etc/aeon/network.toml";

// ──────────────────────────────────────────────────────────────────────
// On-disk shape
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct NetFile {
    #[serde(default)]
    usb_ethernet: UsbEthernet,
    #[serde(default)]
    dnscrypt: Dnscrypt,
    #[serde(default)]
    vpn: Vpn,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct UsbEthernet {
    enabled: bool,
    mode: String,
    #[serde(default = "default_subnet")]
    subnet: String,
    #[serde(default = "default_pi_addr")]
    pi_addr: String,
    #[serde(default = "default_dhcp_start")]
    dhcp_start: String,
    #[serde(default = "default_dhcp_end")]
    dhcp_end: String,
    #[serde(default = "default_host_mac")]
    host_mac: String,
    #[serde(default = "default_dev_mac")]
    dev_mac: String,
}

impl Default for UsbEthernet {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "isolation".into(),
            subnet: default_subnet(),
            pi_addr: default_pi_addr(),
            dhcp_start: default_dhcp_start(),
            dhcp_end: default_dhcp_end(),
            host_mac: default_host_mac(),
            dev_mac: default_dev_mac(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Dnscrypt {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_dns_provider")]
    provider: String,
    #[serde(default = "default_dns_location")]
    location: String,
}

impl Default for Dnscrypt {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_dns_provider(),
            location: default_dns_location(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Vpn {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_vpn_provider")]
    provider: String,
    #[serde(default)]
    kill_switch: bool,
    #[serde(default = "default_lan_bypass")]
    lan_bypass: String,
    #[serde(default)]
    tailscale: TailscaleCfg,
    #[serde(default)]
    wireguard: WireguardCfg,
    #[serde(default)]
    openvpn: OpenvpnCfg,
    #[serde(default)]
    tor: TorCfg,
    #[serde(default)]
    i2p: I2pCfg,
}

impl Default for Vpn {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_vpn_provider(),
            kill_switch: false,
            lan_bypass: default_lan_bypass(),
            tailscale: TailscaleCfg::default(),
            wireguard: WireguardCfg::default(),
            openvpn: OpenvpnCfg::default(),
            tor: TorCfg::default(),
            i2p: I2pCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct TorCfg {
    #[serde(default)]
    bridges: String,
    #[serde(default)]
    exit_country: String,
    #[serde(default)]
    meek_mode: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct I2pCfg {
    #[serde(default)]
    outproxy: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct TailscaleCfg {
    #[serde(default)]
    auth_key: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    exit_node: bool,
    #[serde(default)]
    advertise_exit_node: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct WireguardCfg {
    #[serde(default)]
    config: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct OpenvpnCfg {
    #[serde(default)]
    config: String,
    #[serde(default)]
    auth_username: String,
    #[serde(default)]
    auth_password: String,
}

fn default_subnet() -> String { "10.55.0.0/24".to_string() }
fn default_pi_addr() -> String { "10.55.0.1".to_string() }
fn default_dhcp_start() -> String { "10.55.0.10".to_string() }
fn default_dhcp_end() -> String { "10.55.0.50".to_string() }
fn default_host_mac() -> String { "02:42:ae:00:55:01".to_string() }
fn default_dev_mac() -> String { "02:42:ae:00:55:02".to_string() }
fn default_dns_provider() -> String { "cloudflare".to_string() }
fn default_dns_location() -> String { "auto".to_string() }
fn default_vpn_provider() -> String { "none".to_string() }
fn default_lan_bypass() -> String { "192.168.0.0/16".to_string() }

fn read_state() -> NetFile {
    std::fs::read_to_string(NETWORK_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_state(nf: &NetFile) -> std::io::Result<()> {
    let text = toml::to_string_pretty(nf)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(parent) = Path::new(NETWORK_TOML).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(NETWORK_TOML, text)?;
    Ok(())
}

fn reload_service(name: &str) {
    let _ = Command::new("systemctl").arg("reload").arg(name).status();
}

fn restart_service(name: &str) {
    let _ = Command::new("systemctl").arg("restart").arg(name).status();
}

// ──────────────────────────────────────────────────────────────────────
// /api/network/usb
// ──────────────────────────────────────────────────────────────────────

/// GET /api/network/usb — current USB-ethernet state.
pub async fn get_state(State(_state): State<AppState>) -> Json<Value> {
    let s = read_state();
    Json(json!({
        "ok": true,
        "enabled": s.usb_ethernet.enabled,
        "mode": s.usb_ethernet.mode,
        "subnet": s.usb_ethernet.subnet,
        "pi_addr": s.usb_ethernet.pi_addr,
        "dhcp_range": format!("{}-{}", s.usb_ethernet.dhcp_start, s.usb_ethernet.dhcp_end),
    }))
}

#[derive(Deserialize)]
pub struct PutReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub mode: Option<String>,
}

/// PUT /api/network/usb — update enabled and/or mode. Persists then
/// `systemctl reload aeon-usb-net` to re-apply NM profile + iptables.
///
/// `mode` must be "isolation", "sharing", or "restricted". Toggling
/// `enabled` requires an aeon-hid restart because the USB gadget
/// composite has to be rebuilt with/without the ECM function.
pub async fn put_state(
    State(_state): State<AppState>,
    Json(req): Json<PutReq>,
) -> impl IntoResponse {
    let mut nf = read_state();
    let mut hid_restart_needed = false;

    if let Some(mode) = req.mode.as_deref() {
        if mode != "isolation" && mode != "sharing" && mode != "restricted" {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "mode must be 'isolation', 'sharing', or 'restricted'"})),
            )
                .into_response();
        }
        nf.usb_ethernet.mode = mode.to_string();
    }
    if let Some(enabled) = req.enabled {
        if enabled != nf.usb_ethernet.enabled {
            hid_restart_needed = true;
        }
        nf.usb_ethernet.enabled = enabled;
    }

    if let Err(e) = write_state(&nf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }

    reload_service("aeon-usb-net.service");

    if hid_restart_needed {
        restart_service("aeon-hid.service");
    }

    Json(json!({
        "ok": true,
        "enabled": nf.usb_ethernet.enabled,
        "mode": nf.usb_ethernet.mode,
        "hid_restarted": hid_restart_needed,
    }))
    .into_response()
}

// ──────────────────────────────────────────────────────────────────────
// /api/network/dnscrypt
// ──────────────────────────────────────────────────────────────────────

/// GET /api/network/dnscrypt — current DNSCrypt state.
pub async fn get_dnscrypt(State(_state): State<AppState>) -> Json<Value> {
    let s = read_state();
    Json(json!({
        "ok": true,
        "enabled": s.dnscrypt.enabled,
        "provider": s.dnscrypt.provider,
        "location": s.dnscrypt.location,
        "providers": [
            {"id": "cloudflare", "label": "Cloudflare", "blurb": "Cloudflare's 1.1.1.1 over DoH. Fast anycast, claims zero-logging."},
            {"id": "cloudflare-fam", "label": "Cloudflare for Families", "blurb": "Cloudflare 1.1.1.3 — adds malware + adult-content filtering."},
            {"id": "quad9", "label": "Quad9", "blurb": "Swiss-based, blocks known-malicious domains via threat intel."},
            {"id": "adguard", "label": "AdGuard DNS", "blurb": "DoH with ad + tracker blocking built in."},
            {"id": "nextdns", "label": "NextDNS", "blurb": "Anycast DoH; free tier; per-user dashboard for filtering."},
            {"id": "mullvad", "label": "Mullvad", "blurb": "Privacy-focused DoH; no logging; pairs well with Mullvad VPN."},
        ],
        "locations": [
            {"id": "auto", "label": "Auto (pick by latency)"},
            {"id": "us", "label": "United States"},
            {"id": "eu", "label": "Europe"},
            {"id": "asia", "label": "Asia"},
        ],
    }))
}

#[derive(Deserialize)]
pub struct DnscryptPutReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
}

/// PUT /api/network/dnscrypt — update DNSCrypt config + apply.
pub async fn put_dnscrypt(
    State(_state): State<AppState>,
    Json(req): Json<DnscryptPutReq>,
) -> impl IntoResponse {
    const VALID_PROVIDERS: &[&str] = &[
        "cloudflare",
        "cloudflare-fam",
        "quad9",
        "adguard",
        "nextdns",
        "mullvad",
    ];
    const VALID_LOCATIONS: &[&str] = &["auto", "us", "eu", "asia"];

    let mut nf = read_state();

    if let Some(p) = req.provider.as_deref() {
        if !VALID_PROVIDERS.contains(&p) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!("unknown provider '{p}'")})),
            )
                .into_response();
        }
        nf.dnscrypt.provider = p.to_string();
    }
    if let Some(loc) = req.location.as_deref() {
        if !VALID_LOCATIONS.contains(&loc) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!("unknown location '{loc}'")})),
            )
                .into_response();
        }
        nf.dnscrypt.location = loc.to_string();
    }
    if let Some(enabled) = req.enabled {
        nf.dnscrypt.enabled = enabled;
    }

    if let Err(e) = write_state(&nf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }

    reload_service("aeon-net-services.service");

    Json(json!({
        "ok": true,
        "enabled": nf.dnscrypt.enabled,
        "provider": nf.dnscrypt.provider,
        "location": nf.dnscrypt.location,
    }))
    .into_response()
}

// ──────────────────────────────────────────────────────────────────────
// /api/network/vpn
// ──────────────────────────────────────────────────────────────────────

/// GET /api/network/vpn — current VPN state.
///
/// SECURITY: deliberately does NOT echo secrets back (auth_key,
/// auth_password, raw config blobs). Returns booleans indicating whether
/// each provider has a configured payload. The web UI re-uploads the
/// blob on update if the user wants to change it.
pub async fn get_vpn(State(_state): State<AppState>) -> Json<Value> {
    let s = read_state();
    Json(json!({
        "ok": true,
        "enabled": s.vpn.enabled,
        "provider": s.vpn.provider,
        "kill_switch": s.vpn.kill_switch,
        "lan_bypass": s.vpn.lan_bypass,
        "tailscale": {
            "hostname": s.vpn.tailscale.hostname,
            "exit_node": s.vpn.tailscale.exit_node,
            "advertise_exit_node": s.vpn.tailscale.advertise_exit_node,
            "has_auth_key": !s.vpn.tailscale.auth_key.is_empty(),
        },
        "wireguard": {
            "has_config": !s.vpn.wireguard.config.is_empty(),
        },
        "openvpn": {
            "has_config": !s.vpn.openvpn.config.is_empty(),
            "auth_username": s.vpn.openvpn.auth_username,
            "has_auth_password": !s.vpn.openvpn.auth_password.is_empty(),
        },
        "tor": {
            "has_bridges": !s.vpn.tor.bridges.is_empty(),
            "exit_country": s.vpn.tor.exit_country,
            "meek_mode": s.vpn.tor.meek_mode,
        },
        "i2p": {
            "outproxy": s.vpn.i2p.outproxy,
        },
        "providers": [
            {"id": "none", "label": "None", "blurb": "No VPN. WAN traffic exits via the Pi's normal upstream (eth0/wlan0)."},
            {"id": "tailscale", "label": "Tailscale", "blurb": "WireGuard mesh. Bring an auth-key from the Tailscale admin console. Optionally turn this Pi into an exit-node for your tailnet."},
            {"id": "wireguard", "label": "WireGuard", "blurb": "Paste a working WireGuard .conf. We'll run it via wg-quick@aeon0."},
            {"id": "openvpn", "label": "OpenVPN", "blurb": "Paste a working .ovpn config. Username/password optional."},
            {"id": "tor", "label": "Tor", "blurb": "Route all outbound TCP + DNS through Tor transparently. UDP can't traverse Tor — it's dropped while this is active. Slower than a real VPN but harder to deanonymize."},
            {"id": "i2p", "label": "I2P", "blurb": "Garlic-routed overlay; reaches .i2p sites natively. Configure an outproxy to also reach the regular internet through I2P (slower, less anonymous than Tor for clearnet)."},
        ],
    }))
}

#[derive(Deserialize)]
pub struct VpnPutReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub kill_switch: Option<bool>,
    #[serde(default)]
    pub lan_bypass: Option<String>,
    #[serde(default)]
    pub tailscale: Option<TailscalePut>,
    #[serde(default)]
    pub wireguard: Option<WireguardPut>,
    #[serde(default)]
    pub openvpn: Option<OpenvpnPut>,
    #[serde(default)]
    pub tor: Option<TorPut>,
    #[serde(default)]
    pub i2p: Option<I2pPut>,
}

#[derive(Deserialize)]
pub struct TorPut {
    #[serde(default)]
    pub bridges: Option<String>,
    #[serde(default)]
    pub exit_country: Option<String>,
    #[serde(default)]
    pub meek_mode: Option<bool>,
}

#[derive(Deserialize)]
pub struct I2pPut {
    #[serde(default)]
    pub outproxy: Option<String>,
}

#[derive(Deserialize)]
pub struct TailscalePut {
    #[serde(default)]
    pub auth_key: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub exit_node: Option<bool>,
    #[serde(default)]
    pub advertise_exit_node: Option<bool>,
}

#[derive(Deserialize)]
pub struct WireguardPut {
    #[serde(default)]
    pub config: Option<String>,
}

#[derive(Deserialize)]
pub struct OpenvpnPut {
    #[serde(default)]
    pub config: Option<String>,
    #[serde(default)]
    pub auth_username: Option<String>,
    #[serde(default)]
    pub auth_password: Option<String>,
}

/// PUT /api/network/vpn — update VPN config + apply.
///
/// Fields not provided are left unchanged. Pass an empty string to clear
/// a secret (e.g. `tailscale.auth_key=""` to remove a stale auth key).
pub async fn put_vpn(
    State(_state): State<AppState>,
    Json(req): Json<VpnPutReq>,
) -> impl IntoResponse {
    const VALID_PROVIDERS: &[&str] = &[
        "none", "tailscale", "wireguard", "openvpn", "tor", "i2p",
    ];

    let mut nf = read_state();

    if let Some(p) = req.provider.as_deref() {
        if !VALID_PROVIDERS.contains(&p) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!("unknown vpn provider '{p}'")})),
            )
                .into_response();
        }
        nf.vpn.provider = p.to_string();
    }
    if let Some(enabled) = req.enabled {
        nf.vpn.enabled = enabled;
    }
    if let Some(ks) = req.kill_switch {
        nf.vpn.kill_switch = ks;
    }
    if let Some(lb) = req.lan_bypass {
        nf.vpn.lan_bypass = lb;
    }
    if let Some(ts) = req.tailscale {
        if let Some(v) = ts.auth_key { nf.vpn.tailscale.auth_key = v; }
        if let Some(v) = ts.hostname { nf.vpn.tailscale.hostname = v; }
        if let Some(v) = ts.exit_node { nf.vpn.tailscale.exit_node = v; }
        if let Some(v) = ts.advertise_exit_node { nf.vpn.tailscale.advertise_exit_node = v; }
    }
    if let Some(wg) = req.wireguard {
        if let Some(v) = wg.config { nf.vpn.wireguard.config = v; }
    }
    if let Some(ov) = req.openvpn {
        if let Some(v) = ov.config { nf.vpn.openvpn.config = v; }
        if let Some(v) = ov.auth_username { nf.vpn.openvpn.auth_username = v; }
        if let Some(v) = ov.auth_password { nf.vpn.openvpn.auth_password = v; }
    }
    if let Some(tor) = req.tor {
        if let Some(v) = tor.bridges { nf.vpn.tor.bridges = v; }
        if let Some(v) = tor.exit_country {
            // Country codes are 2-letter ISO-3166. Reject anything else
            // to avoid torrc injection (the value lands in a config file).
            let v = v.trim().to_lowercase();
            if !v.is_empty() && !(v.len() == 2 && v.chars().all(|c| c.is_ascii_alphabetic())) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "exit_country must be a 2-letter ISO code or empty"})),
                ).into_response();
            }
            nf.vpn.tor.exit_country = v;
        }
        if let Some(v) = tor.meek_mode { nf.vpn.tor.meek_mode = v; }
    }
    if let Some(i2p) = req.i2p {
        if let Some(v) = i2p.outproxy { nf.vpn.i2p.outproxy = v; }
    }

    if let Err(e) = write_state(&nf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }

    reload_service("aeon-net-services.service");

    Json(json!({
        "ok": true,
        "enabled": nf.vpn.enabled,
        "provider": nf.vpn.provider,
    }))
    .into_response()
}

// ──────────────────────────────────────────────────────────────────────
// /api/network/vpn/status — live introspection of the active VPN
// ──────────────────────────────────────────────────────────────────────

/// GET /api/network/vpn/status — runs the aeon-vpn-status helper and
/// returns its JSON verbatim. The helper takes ~50ms when not connected
/// to a tunnel, up to ~8s when connected (curl to ifconfig.co for the
/// public IP lookup). The UI polls this on a 2-3s interval while the
/// VPN section is visible.
pub async fn get_vpn_status(State(_state): State<AppState>) -> impl IntoResponse {
    let join_result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("/usr/local/bin/aeon-vpn-status").output()
    })
    .await;

    let output = match join_result {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("status spawn: {e}")})),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("status task join: {e}")})),
            )
                .into_response();
        }
    };

    if !output.status.success() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "err": format!("status exit {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr).trim()),
            })),
        )
            .into_response();
    }

    match serde_json::from_slice::<Value>(&output.stdout) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "ok": false,
                "err": format!("status parse: {e}"),
                "raw": String::from_utf8_lossy(&output.stdout).to_string(),
            })),
        )
            .into_response(),
    }
}

/// POST /api/network/vpn/rotate — triggers an identity / circuit refresh
/// (Tor SIGNAL NEWNYM, Tailscale reset, WG/OVPN reconnect, i2pd tunnel
/// rebuild). Synchronous; returns when the rotation command finishes.
pub async fn post_vpn_rotate(State(_state): State<AppState>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(|| {
        std::process::Command::new("/usr/local/bin/aeon-vpn-rotate")
            .output()
    })
    .await;

    match result {
        Ok(Ok(out)) => {
            let body = serde_json::from_slice::<Value>(&out.stdout)
                .unwrap_or_else(|_| json!({
                    "ok": false,
                    "err": "rotate produced non-JSON",
                    "raw": String::from_utf8_lossy(&out.stdout).to_string(),
                }));
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rotate spawn: {e}")})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rotate join: {e}")})),
        )
            .into_response(),
    }
}
