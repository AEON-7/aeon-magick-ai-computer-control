//! AzireVPN integration.
//!
//! AzireVPN's actual API flow (confirmed against
//! https://www.azirevpn.com/docs/api — GL.iNet's router firmware uses
//! the same path):
//!
//!   1. User generates an API token from the AzireVPN dashboard
//!      (https://www.azirevpn.com → settings → "API keys")
//!   2. Pi generates a local WG keypair
//!   3. POST /v3/ips with the WG pubkey + Bearer <token> → response
//!      includes the assigned IPv4/IPv6, DNS servers, and a `device_name`
//!      AzireVPN auto-assigns. The endpoint is called "IPs" rather than
//!      "keys" — that confusing rename is why every "/v3/keys" or
//!      "/v3/users/keys" probe returns 404. (Each IP supports up to
//!      three pubkeys, so you can re-key on a different device without
//!      losing the IP.)
//!   4. GET /v3/locations + Bearer <token> → server list with `name`,
//!      `city`, `country`, `iso`, `pool` (hostname like se-sto.azirevpn.net),
//!      `pubkey`. Port is the WireGuard default 51820.
//!   5. User picks a server, supervisor renders wg config using stored
//!      private key + assigned IPv4/IPv6 + the chosen server's pool +
//!      pubkey.
//!
//! v63.1: prior implementation hit `/v3/keys` (404) because that was
//! my best guess from the v59 work — AzireVPN's docs call the endpoint
//! "IPs" since you're allocating an IP address as a side effect, with
//! key registration being a parameter. Now wired through correctly so
//! the wizard works just like Mullvad/IVPN.

use super::{Server, eyes_for_country, compute_server_score, http_get_json, http_post_json};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/azirevpn.toml";
const API_BASE: &str = "https://api.azirevpn.com/v3";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AzireState {
    /// AzireVPN API token from the user dashboard.
    pub api_token: String,
    /// Allocated client IP (returned from /v3/ips).
    pub peer_ipv4: String,
    #[serde(default)]
    pub peer_ipv6: String,
    /// DNS servers AzireVPN wants us to use (returned by /v3/ips).
    /// Defaults to AzireVPN's pair if the response somehow omits them.
    #[serde(default)]
    pub dns_servers: Vec<String>,
    /// AzireVPN's auto-assigned device name. Cosmetic; surfaced in the
    /// UI so the user can match a row in their dashboard with what
    /// aeon registered.
    #[serde(default)]
    pub device_name: String,
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

/// POST /v3/ips — allocate an IP with our pubkey attached.
///
/// Confusingly named — AzireVPN's "IPs" endpoint IS the key-
/// registration call. Body shape:
///
///   { "key": "<base64-pubkey>" }
///
/// Response (verbatim from their docs):
///
/// ```json
/// {
///   "id": "IJiaUdc2",
///   "ipv4_address": "10.0.1.136",
///   "ipv4_netmask": 32,
///   "ipv6_address": "2a0e:1c80:1337:1:10:0:1:136",
///   "ipv6_netmask": 128,
///   "dns": ["10.0.0.1", "2a0e:1c80:1337:1:10::1"],
///   "device_name": "camel",
///   "keys": [{"key": "...", "created_at": 1691499313}]
/// }
/// ```
pub fn register_key(token: &str, public_key: &str) -> Result<IpAllocation, String> {
    let url = format!("{API_BASE}/ips");
    let payload = serde_json::json!({ "key": public_key });
    let v = http_post_json(&url, Some(token), &payload)?;

    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let ipv4 = v.get("ipv4_address").and_then(|x| x.as_str())
        .ok_or_else(|| "no ipv4_address in /v3/ips response".to_string())?
        .to_string();
    let ipv6 = v.get("ipv6_address").and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let dns: Vec<String> = v.get("dns")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect())
        .unwrap_or_default();
    let device_name = v.get("device_name").and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    Ok(IpAllocation { id, ipv4, ipv6, dns, device_name })
}

#[derive(Debug, Clone)]
pub struct IpAllocation {
    pub id: String,
    pub ipv4: String,
    pub ipv6: String,
    pub dns: Vec<String>,
    pub device_name: String,
}

/// GET /v3/locations — server list. Each entry shape (verbatim):
///
/// ```json
/// {
///   "name": "se-sto",
///   "city": "Stockholm",
///   "country": "Sweden",
///   "iso": "se",
///   "pool": "se-sto.azirevpn.net",
///   "pubkey": "VYRJwI6n2Rpvh/gmYnUoyMJQDrUSdxls0JX9/6JlOEw="
/// }
/// ```
///
/// Note `pool` is a hostname, not an IP — WireGuard accepts hostnames
/// in `Endpoint =` and the OS resolver picks an IP at connect time.
/// That matches GL.iNet's behavior; we use the pool string verbatim.
/// Port isn't in the response — AzireVPN uses the WireGuard default 51820.
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
        let country = l.get("iso").and_then(|x| x.as_str())
            .unwrap_or("")
            .to_uppercase();
        let country_name = l.get("country").and_then(|x| x.as_str())
            .unwrap_or(&country)
            .to_string();
        let city = l.get("city").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let pubkey = l.get("pubkey")
            .or_else(|| l.get("public_key"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let pool = l.get("pool").and_then(|x| x.as_str()).unwrap_or("");
        if id.is_empty() || pubkey.is_empty() || pool.is_empty() {
            continue;
        }
        let eyes = eyes_for_country(&country);
        let score = compute_server_score(provider_trust, eyes);
        out.push(Server {
            id: id.to_string(),
            label: format!("{country_name} — {city}"),
            country: country.clone(),
            country_name,
            city,
            hostname: id.to_string(),
            // We store the hostname pool in endpoint_ip; the WG config
            // line `Endpoint = <pool>:51820` resolves at connect time.
            endpoint_ip: pool.to_string(),
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

    // Address line: ipv4 + optional ipv6. WireGuard accepts both
    // comma-separated.
    let mut addr = format!("{}/32", state.peer_ipv4);
    if !state.peer_ipv6.is_empty() {
        addr.push_str(&format!(", {}/128", state.peer_ipv6));
    }

    // DNS — use the values AzireVPN handed us at registration time.
    // Fallback to their public clearnet resolver if the state file
    // somehow lost the array (e.g. migration from an older format).
    let dns = if state.dns_servers.is_empty() {
        "91.231.153.2".to_string()
    } else {
        state.dns_servers.join(", ")
    };

    Ok(format!(r#"# Managed by aeon-supervisor (AzireVPN provider).
# Re-generated on every save — do not edit by hand.

[Interface]
PrivateKey = {priv}
Address    = {addr}
DNS        = {dns}
MTU        = 1420

[Peer]
PublicKey  = {peer_pub}
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint   = {endpoint}:{endpoint_port}
PersistentKeepalive = 25
"#,
        priv = state.wg_private_key,
        addr = addr,
        dns = dns,
        peer_pub = server.public_key,
        endpoint = server.endpoint_ip,
        endpoint_port = server.endpoint_port,
    ))
}
