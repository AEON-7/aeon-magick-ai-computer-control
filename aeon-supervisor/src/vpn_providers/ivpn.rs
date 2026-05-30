//! IVPN integration.
//!
//! IVPN account flow:
//!   1. User pastes account ID (e.g. "ivpn-XXXX-XXXX-XXXX")
//!   2. POST /v4/session/new with account_id → session token + wg peer IP
//!   3. Generate local WG keypair on Pi (done client-side before step 2,
//!      the pubkey is bound at session establishment)
//!   4. User picks a server from the cached list, supervisor renders
//!      wg config
//!
//! Slightly different from Mullvad: IVPN binds the WG pubkey at
//! session establishment, then re-uses that key across all server
//! switches. Mullvad treats each pubkey as a device record.
//!
//! v63.1: switched session endpoint v5 → v4. Initial v59 implementation
//! used /v5/session/new because some IVPN docs reference v5, but the
//! live API only serves session/* under /v4 (v5 returns 404). The
//! server-listing endpoint /servers/stats works on both v4 and v5 —
//! kept on v4 so everything routes through the same base URL.

use super::{EyesTier, Server, eyes_for_country, compute_server_score, http_get_json, http_post_json};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/ivpn.toml";
const API_BASE: &str = "https://api.ivpn.net/v4";

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

/// POST /v4/session/new — exchange account_id for a session token.
///
/// Live API contract observed from probing api.ivpn.net + cross-checking
/// with IVPN's open-source desktop client (github.com/ivpn/desktop-app):
///   * field name is `wg_public_key`, NOT `wireguard_public_key`
///   * response top-level keys are `token` and `wg_ip` (not nested under
///     a `wireguard` object the way Mullvad's response is)
///   * 400 response with `error_code` in body when the account is
///     invalid or already over its device cap
pub fn new_session(account_id: &str, wg_pubkey: &str) -> Result<SessionResult, String> {
    let url = format!("{API_BASE}/session/new");
    let payload = serde_json::json!({
        "username": account_id,
        "wg_public_key": wg_pubkey,
    });
    let v = http_post_json(&url, None, &payload)?;
    // IVPN returns 200 with a `status` field — sometimes a non-200
    // status sneaks through with a friendlier message. Surface that
    // explicitly so the wizard can show "your account is over its
    // 5-device limit" rather than "no session token in response".
    if let Some(status) = v.get("status").and_then(|x| x.as_i64()) {
        if status != 200 {
            let msg = v.get("message").and_then(|x| x.as_str())
                .unwrap_or("unknown IVPN error");
            return Err(format!("ivpn API status {status}: {msg}"));
        }
    }
    let token = v.get("token").and_then(|x| x.as_str())
        .or_else(|| v.get("session_token").and_then(|x| x.as_str()))
        .ok_or_else(|| "no session token in response".to_string())?
        .to_string();
    // Response shape:
    //   {"status":200,"token":"...","wg_ip":"172.30.x.x","vpn_username":"..."}
    // Some older builds nested it under `wireguard.ipv4_address` —
    // try both for resilience across future API revisions.
    let ipv4 = v.get("wg_ip").and_then(|x| x.as_str())
        .or_else(|| v.get("wireguard").and_then(|w| w.get("ipv4_address")).and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    Ok(SessionResult { token, ipv4 })
}

#[derive(Debug, Clone)]
pub struct SessionResult {
    pub token: String,
    pub ipv4: String,
}

/// GET /v4/servers.json — full WireGuard server list. Public (no auth).
///
/// v67.4: previously hit /v4/servers/stats and looked for `public_key` +
/// `ip_address` per entry — but stats names the key `wg_public_key` and
/// carries NO per-server IP, so every server failed the
/// non-empty-pubkey-and-IP check and the cache came up empty ("0 servers
/// in cache"). servers.json is the right source: a `wireguard` array of
/// gateways, each with a `hosts[]` list giving the actual endpoint
/// `host` (IPv4), `public_key`, and `hostname`. We flatten hosts into
/// individual selectable Servers.
///
/// Response shape (verbatim):
///   { "wireguard": [ { "gateway":"ro.wg.ivpn.net", "country_code":"RO",
///       "country":"Romania", "city":"Bucharest",
///       "hosts":[ { "hostname":"ro1.wg.ivpn.net", "host":"37.120.206.53",
///                   "public_key":"F2uQ…", "local_ip":"172.16.0.1/12" } ] } ],
///     "openvpn":[…], "config":{…} }
pub fn fetch_servers(provider_trust: u8) -> Result<Vec<Server>, String> {
    let url = format!("{API_BASE}/servers.json");
    let v = http_get_json(&url, None)?;
    let gateways = v.get("wireguard")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "no wireguard array in /v4/servers.json".to_string())?;
    let mut out = Vec::new();
    for gw in gateways {
        let country = gw.get("country_code").and_then(|x| x.as_str())
            .unwrap_or("")
            .to_uppercase();
        let country_name = gw.get("country").and_then(|x| x.as_str())
            .unwrap_or(&country)
            .to_string();
        let city = gw.get("city").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let hosts = match gw.get("hosts").and_then(|x| x.as_array()) {
            Some(h) => h,
            None => continue,
        };
        for host in hosts {
            let hostname = host.get("hostname").and_then(|x| x.as_str()).unwrap_or("");
            let ip = host.get("host").and_then(|x| x.as_str()).unwrap_or("");
            let pubkey = host.get("public_key").and_then(|x| x.as_str()).unwrap_or("");
            if hostname.is_empty() || ip.is_empty() || pubkey.is_empty() {
                continue;
            }
            let eyes = eyes_for_country(&country);
            let score = compute_server_score(provider_trust, eyes);
            out.push(Server {
                id: hostname.to_string(),
                label: format!("{country_name} — {city}"),
                country: country.clone(),
                country_name: country_name.clone(),
                city: city.clone(),
                hostname: hostname.to_string(),
                endpoint_ip: ip.to_string(),
                // IVPN WireGuard single-hop listens on UDP 2049 (their
                // config generator's default Endpoint port).
                endpoint_port: 2049,
                public_key: pubkey.to_string(),
                eyes,
                server_score: score,
            });
        }
    }
    if out.is_empty() {
        return Err("parsed 0 IVPN WireGuard servers from /v4/servers.json".into());
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
