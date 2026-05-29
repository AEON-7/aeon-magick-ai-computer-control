//! IVPN integration.
//!
//! IVPN account flow:
//!   1. User pastes account ID (e.g. "ivpn-XXXX-XXXX-XXXX")
//!   2. POST /v5/session/new with account_id → session token
//!   3. Generate local WG keypair on Pi
//!   4. POST /v5/wireguard/connect with WG pubkey → server peer config
//!      (allocated client IP + server list with pubkeys)
//!   5. User picks a server, supervisor renders wg config
//!
//! Slightly different from Mullvad: IVPN binds the WG pubkey at
//! session establishment, then re-uses that key across all server
//! switches. Mullvad treats each pubkey as a device record.

use super::{EyesTier, Server, eyes_for_country, compute_server_score, http_get_json, http_post_json};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/ivpn.toml";
const API_BASE: &str = "https://api.ivpn.net/v5";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IvpnState {
    /// IVPN account ID. Format usually "ivpn-XXXX-XXXX-XXXX".
    pub account_id: String,
    /// Session token issued by /session/new, used as Bearer on
    /// subsequent requests. Refreshed when it expires (~12h).
    pub session_token: String,
    pub session_expires_ms: i64,
    /// Allocated client IP (from /wireguard/connect).
    pub peer_ipv4: String,
    pub wg_private_key: String,
    pub wg_public_key: String,
    pub selected_server: String,
    #[serde(default = "default_mode")]
    pub selection_mode: String,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub servers_updated_ms: i64,
}

fn default_mode() -> String { "manual".into() }

pub fn read_state() -> IvpnState {
    std::fs::read_to_string(SECRETS_PATH)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_state(s: &IvpnState) -> std::io::Result<()> {
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

/// POST /v5/session/new — exchange account_id for a session token.
pub fn new_session(account_id: &str, wg_pubkey: &str) -> Result<SessionResult, String> {
    let url = format!("{API_BASE}/session/new");
    let payload = serde_json::json!({
        "username": account_id,
        "wireguard_public_key": wg_pubkey,
    });
    let v = http_post_json(&url, None, &payload)?;
    // Response shape (documented):
    //   {"status":200, "token":"...", "session_token":"...",
    //    "wireguard": {"ipv4_address":"172.x.x.x", ...}, ...}
    let token = v.get("token").and_then(|x| x.as_str())
        .or_else(|| v.get("session_token").and_then(|x| x.as_str()))
        .ok_or_else(|| "no session token in response".to_string())?
        .to_string();
    let ipv4 = v.get("wireguard")
        .and_then(|w| w.get("ipv4_address"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(SessionResult { token, ipv4 })
}

#[derive(Debug, Clone)]
pub struct SessionResult {
    pub token: String,
    pub ipv4: String,
}

/// GET /v5/servers — full server list with country + city + WG peer
/// info. Public (no auth needed for listing).
pub fn fetch_servers(provider_trust: u8) -> Result<Vec<Server>, String> {
    let url = format!("{API_BASE}/servers/stats");
    let v = http_get_json(&url, None)?;
    let servers = v.get("servers")
        .or_else(|| v.get("wireguard"))
        .and_then(|x| x.as_array())
        .ok_or_else(|| "no servers array in response".to_string())?;
    let mut out = Vec::with_capacity(servers.len());
    for s in servers {
        let hostname = s.get("hostnames").and_then(|h| h.as_array())
            .and_then(|h| h.first())
            .and_then(|h| h.as_str())
            .or_else(|| s.get("hostname").and_then(|h| h.as_str()))
            .unwrap_or("");
        let country = s.get("country_code").and_then(|x| x.as_str())
            .unwrap_or("")
            .to_uppercase();
        let country_name = s.get("country").and_then(|x| x.as_str())
            .unwrap_or(&country)
            .to_string();
        let city = s.get("city").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let pubkey = s.get("public_key").and_then(|x| x.as_str()).unwrap_or("");
        let endpoint_ip = s.get("ip_address").and_then(|x| x.as_str())
            .or_else(|| s.get("ipv4").and_then(|x| x.as_str()))
            .unwrap_or("");
        if hostname.is_empty() || pubkey.is_empty() || endpoint_ip.is_empty() {
            continue;
        }
        let eyes = eyes_for_country(&country);
        let score = compute_server_score(provider_trust, eyes);
        out.push(Server {
            id: hostname.to_string(),
            label: format!("{country_name} — {city}"),
            country: country.clone(),
            country_name,
            city,
            hostname: hostname.to_string(),
            endpoint_ip: endpoint_ip.to_string(),
            endpoint_port: 51820,
            public_key: pubkey.to_string(),
            eyes,
            server_score: score,
        });
    }
    Ok(out)
}

pub fn render_wg_config(state: &IvpnState) -> Result<String, String> {
    let server = state.servers.iter()
        .find(|s| s.id == state.selected_server)
        .ok_or_else(|| format!("selected_server '{}' not in cache — refresh server list", state.selected_server))?;
    Ok(format!(r#"# Managed by aeon-supervisor (IVPN provider).
# Re-generated on every save — do not edit by hand.

[Interface]
PrivateKey = {priv}
Address    = {ipv4}/32
DNS        = 172.16.0.1
MTU        = 1420

[Peer]
PublicKey  = {peer_pub}
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint   = {endpoint_ip}:{endpoint_port}
PersistentKeepalive = 25
"#,
        priv = state.wg_private_key,
        ipv4 = state.peer_ipv4,
        peer_pub = server.public_key,
        endpoint_ip = server.endpoint_ip,
        endpoint_port = server.endpoint_port,
    ))
}
