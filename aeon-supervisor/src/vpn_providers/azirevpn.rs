//! AzireVPN integration.
//!
//! AzireVPN's API is the least-documented of the three but follows
//! a similar shape:
//!   1. User generates an API token from their dashboard
//!      (https://www.azirevpn.com/manager/devices)
//!   2. Generate local WG keypair
//!   3. POST /v3/keys with pubkey → register the key, get peer config
//!   4. GET /v3/locations → server list
//!   5. User picks a server, supervisor renders wg config

use super::{EyesTier, Server, eyes_for_country, compute_server_score, http_get_json, http_post_json};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/azirevpn.toml";
const API_BASE: &str = "https://api.azirevpn.com/v3";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AzireState {
    /// AzireVPN API token from the user dashboard.
    pub api_token: String,
    /// Allocated client IP.
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

pub fn read_state() -> AzireState {
    std::fs::read_to_string(SECRETS_PATH)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_state(s: &AzireState) -> std::io::Result<()> {
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

/// POST /v3/keys — register our pubkey, get back the peer IP +
/// initial server endpoint info.
pub fn register_key(token: &str, public_key: &str) -> Result<KeyRecord, String> {
    let url = format!("{API_BASE}/keys");
    let payload = serde_json::json!({
        "key": public_key,
    });
    let v = http_post_json(&url, Some(token), &payload)?;
    let ipv4 = v.get("ipv4_address")
        .or_else(|| v.get("address"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let device_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(KeyRecord { id: device_id, ipv4 })
}

#[derive(Debug, Clone)]
pub struct KeyRecord {
    pub id: String,
    pub ipv4: String,
}

/// GET /v3/locations — server list. Each entry has country + city +
/// WireGuard peer pubkey + endpoint.
pub fn fetch_servers(token: &str, provider_trust: u8) -> Result<Vec<Server>, String> {
    let url = format!("{API_BASE}/locations");
    let v = http_get_json(&url, Some(token))?;
    let locations = v.get("locations")
        .or_else(|| v.as_array().map(|_| &v))
        .and_then(|x| x.as_array())
        .ok_or_else(|| "no locations array in response".to_string())?;
    let mut out = Vec::with_capacity(locations.len());
    for l in locations {
        let id = l.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let country = l.get("country_code").and_then(|x| x.as_str())
            .unwrap_or("")
            .to_uppercase();
        let city = l.get("city").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let pubkey = l.get("pubkey")
            .or_else(|| l.get("public_key"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let endpoint = l.get("endpoint")
            .or_else(|| l.get("ipv4"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if id.is_empty() || pubkey.is_empty() || endpoint.is_empty() {
            continue;
        }
        let eyes = eyes_for_country(&country);
        let score = compute_server_score(provider_trust, eyes);
        out.push(Server {
            id: id.to_string(),
            label: format!("{country} — {city}"),
            country: country.clone(),
            country_name: country.clone(),
            city,
            hostname: id.to_string(),
            endpoint_ip: endpoint.to_string(),
            endpoint_port: 51820,
            public_key: pubkey.to_string(),
            eyes,
            server_score: score,
        });
    }
    Ok(out)
}

pub fn render_wg_config(state: &AzireState) -> Result<String, String> {
    let server = state.servers.iter()
        .find(|s| s.id == state.selected_server)
        .ok_or_else(|| format!("selected_server '{}' not in cache — refresh server list", state.selected_server))?;
    Ok(format!(r#"# Managed by aeon-supervisor (AzireVPN provider).
# Re-generated on every save — do not edit by hand.

[Interface]
PrivateKey = {priv}
Address    = {ipv4}/32
DNS        = 91.231.153.2
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
