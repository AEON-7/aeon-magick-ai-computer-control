//! Mullvad VPN integration.
//!
//! Mullvad's account model is a single 16-digit number — no email,
//! no password. The supervisor flow:
//!
//!   1. User pastes account number into the UI → POST to
//!      /api/network/vpn/providers/mullvad/setup
//!   2. Generate a local WG keypair on the Pi
//!   3. POST account + local pubkey to api.mullvad.net to register
//!      a "device" (Mullvad tracks per-key device usage)
//!   4. Mullvad returns the device record incl. the IP they
//!      assigned to our peer
//!   5. Pull the full WireGuard relay list (cached for 24h)
//!   6. User picks a server → supervisor writes
//!      /etc/wireguard/aeon0.conf and brings up wg-quick@aeon0
//!
//! Credentials live at /etc/aeon/vpn-secrets/mullvad.toml (mode 0600).
//! Account number is the only real secret; the rest is derivable
//! from it via Mullvad's API.

use super::{EyesTier, Server, eyes_for_country, compute_server_score, http_get_json, http_post_json};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/mullvad.toml";
const API_BASE: &str = "https://api.mullvad.net";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MullvadState {
    /// 16-digit Mullvad account number. The only real secret here.
    pub account_number: String,
    /// Device ID returned by Mullvad's API after we registered our
    /// pubkey with their device endpoint.
    pub device_id: String,
    pub device_name: String,
    /// IPv4 they assigned us as a peer (we PUT this into our
    /// WireGuard config's Interface.Address field).
    pub peer_ipv4: String,
    pub peer_ipv6: String,
    /// Locally-generated WG private key (base64). Kept here so
    /// re-running setup doesn't churn the device list on their side.
    pub wg_private_key: String,
    pub wg_public_key: String,
    /// Selected server hostname (e.g. "se-got-wg-001"). Empty until
    /// the user picks one.
    pub selected_server: String,
    /// "manual" or "auto" (latency-picked).
    #[serde(default = "default_mode")]
    pub selection_mode: String,
    /// Cached server list with our trust scoring. Refreshed on
    /// demand via /api/network/vpn/providers/mullvad/refresh-servers.
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub servers_updated_ms: i64,
}

fn default_mode() -> String { "manual".into() }

pub fn read_state() -> MullvadState {
    std::fs::read_to_string(SECRETS_PATH)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_state(s: &MullvadState) -> std::io::Result<()> {
    if let Some(parent) = Path::new(SECRETS_PATH).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(s)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    let tmp = format!("{SECRETS_PATH}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::set_permissions(&tmp,
        std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    std::fs::rename(tmp, SECRETS_PATH)?;
    Ok(())
}

/// POST /accounts/v1/devices — register our local public key as a
/// new device on the Mullvad account. Returns the device record
/// with the assigned IPv4/IPv6.
pub fn register_device(
    account_number: &str,
    public_key: &str,
    device_name: &str,
) -> Result<DeviceRecord, String> {
    // Mullvad's account-token-as-bearer-with-leading-mvauth scheme.
    // Documented at https://github.com/mullvad/mullvadvpn-app/blob/main/docs/api.md
    let token = login_account(account_number)?;
    let url = format!("{API_BASE}/accounts/v1/devices");
    let payload = serde_json::json!({
        "pubkey": public_key,
        "hijack_dns": false,
        "name": device_name,
    });
    let v = http_post_json(&url, Some(&token), &payload)?;
    Ok(DeviceRecord {
        id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        name: v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ipv4: v.get("ipv4_address").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ipv6: v.get("ipv6_address").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        pubkey: v.get("pubkey").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

/// POST /auth/v1/token — exchange account number for an
/// access token usable as a Bearer on subsequent requests.
fn login_account(account_number: &str) -> Result<String, String> {
    let url = format!("{API_BASE}/auth/v1/token");
    let payload = serde_json::json!({
        "account_number": account_number,
    });
    let v = http_post_json(&url, None, &payload)?;
    v.get("access_token")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no access_token in response".into())
}

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub ipv4: String,
    pub ipv6: String,
    pub pubkey: String,
}

/// GET /public/relays/wireguard/v1 — fetch the live WireGuard relay
/// list. Public (no auth needed). Refreshed on demand.
///
/// v67.6: the response is NOT a flat `{"relays":[…]}` — it's nested
/// `{"countries":[{name,code,cities:[{name,code,relays:[…]}]}]}`. The
/// old flat lookup found no top-level `relays` array → "no relays array
/// in response" and setup failed. Traverse countries→cities→relays.
///
/// Relay shape (verbatim):
///   {"hostname":"al-tia-wg-003","ipv4_addr_in":"103.124.165.130",
///    "ipv6_addr_in":"2a04:…","public_key":"rWiQ…","multihop_port":3574}
/// Country/city names + codes come from the enclosing objects.
pub fn fetch_relays(provider_trust: u8) -> Result<Vec<Server>, String> {
    let url = format!("{API_BASE}/public/relays/wireguard/v1");
    let v = http_get_json(&url, None)?;
    let countries = v.get("countries").and_then(|x| x.as_array())
        .ok_or_else(|| "no countries array in Mullvad relay response".to_string())?;
    let mut out = Vec::new();
    for country in countries {
        let cc = country.get("code").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
        let cname = country.get("name").and_then(|x| x.as_str()).unwrap_or(&cc).to_string();
        let cities = match country.get("cities").and_then(|x| x.as_array()) {
            Some(c) => c,
            None => continue,
        };
        for city in cities {
            let city_name = city.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let relays = match city.get("relays").and_then(|x| x.as_array()) {
                Some(r) => r,
                None => continue,
            };
            for r in relays {
                let hostname = r.get("hostname").and_then(|x| x.as_str()).unwrap_or("");
                let ipv4 = r.get("ipv4_addr_in").and_then(|x| x.as_str()).unwrap_or("");
                let pubkey = r.get("public_key").and_then(|x| x.as_str()).unwrap_or("");
                if hostname.is_empty() || ipv4.is_empty() || pubkey.is_empty() {
                    continue;
                }
                let eyes = eyes_for_country(&cc);
                let score = compute_server_score(provider_trust, eyes);
                out.push(Server {
                    id: hostname.to_string(),
                    label: format!("{cname} — {city_name} ({hostname})"),
                    country: cc.clone(),
                    country_name: cname.clone(),
                    city: city_name.clone(),
                    hostname: hostname.to_string(),
                    endpoint_ip: ipv4.to_string(),
                    endpoint_port: 51820, // Mullvad WireGuard standard port
                    public_key: pubkey.to_string(),
                    eyes,
                    server_score: score,
                });
            }
        }
    }
    if out.is_empty() {
        return Err("parsed 0 Mullvad WireGuard relays from countries/cities/relays".into());
    }
    Ok(out)
}

/// Render the supervisor's /etc/wireguard/aeon0.conf so wg-quick@aeon0
/// can bring the tunnel up. Mullvad uses standard WireGuard — the
/// config is the same shape as any other WG peer.
pub fn render_wg_config(state: &MullvadState) -> Result<String, String> {
    let server = state.servers.iter()
        .find(|s| s.id == state.selected_server)
        .ok_or_else(|| format!("selected_server '{}' not in cache — refresh server list", state.selected_server))?;
    Ok(format!(r#"# Managed by aeon-supervisor (Mullvad provider).
# Re-generated on every save — do not edit by hand.

[Interface]
PrivateKey = {priv}
Address    = {ipv4}/32, {ipv6}/128
DNS        = 10.64.0.1
MTU        = 1380
# (LAN bypass + kill-switch live in aeon-net-services iptables, not here.)

[Peer]
PublicKey  = {peer_pub}
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint   = {endpoint_ip}:{endpoint_port}
PersistentKeepalive = 25
"#,
        priv = state.wg_private_key,
        ipv4 = state.peer_ipv4,
        ipv6 = state.peer_ipv6,
        peer_pub = server.public_key,
        endpoint_ip = server.endpoint_ip,
        endpoint_port = server.endpoint_port,
    ))
}
