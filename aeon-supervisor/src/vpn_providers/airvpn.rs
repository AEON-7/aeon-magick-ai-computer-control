//! AirVPN integration.
//!
//! AirVPN differs from Mullvad/IVPN: its API does NOT mint client
//! credentials. The user generates a config in AirVPN's Config Generator
//! (client area) and pastes it once; we extract what we need. AirVPN's
//! WireGuard uses ONE server public key network-wide — you switch servers
//! by changing only the endpoint IP — so with the shared key + the user's
//! private key/address we can offer a full per-server picker driven by the
//! `status` API's entry IPs (ip_v4_in1).
//!
//! The API key powers the server list / No-Eyes / pick-fastest. Four
//! connection modes are tracked (wireguard, openvpn, openvpn_ssl,
//! openvpn_ssh); WireGuard is rendered from the parsed creds, the OpenVPN
//! family from the pasted .ovpn bundle (the SSL/SSH obfuscation wrappers
//! are applied by aeon-net-services).
//!
//! Credentials live at /etc/aeon/vpn-secrets/airvpn.toml (mode 0600).

use super::{Server, eyes_for_country, compute_server_score, http_get_json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const SECRETS_PATH: &str = "/etc/aeon/vpn-secrets/airvpn.toml";
/// Per-mode generated packages land here, one subdir per mode, so net-services
/// can pick up exactly the files AirVPN's generator produced for that mode.
const PACKAGE_ROOT: &str = "/etc/aeon/vpn-secrets/airvpn";
const GENERATOR_URL: &str = "https://airvpn.org/api/generator/";

/// Record of a config package fetched from AirVPN's generator for one mode.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GeneratedInfo {
    pub server: String,
    pub server_label: String,
    pub generated_ms: i64,
    pub files: Vec<String>,
}
// status service — the form verified against the live API (returns the
// full `servers` array). Key is appended as a query param.
const STATUS_URL: &str = "https://airvpn.org/api/?service=status&format=json";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AirvpnState {
    /// AirVPN API key (64-char, from the member area → API). Drives the
    /// status server list. The only field that is strictly a secret here
    /// besides the WG private key.
    pub api_key: String,
    /// Connection mode: "wireguard" | "openvpn" | "openvpn_ssl" | "openvpn_ssh".
    #[serde(default = "default_wg_mode")]
    pub mode: String,

    // ── WireGuard creds, extracted from the pasted Config-Generator file ──
    pub wg_private_key: String,
    /// AirVPN's network-wide shared WireGuard server public key.
    pub wg_public_key: String,
    pub wg_preshared_key: String,
    pub peer_ipv4: String,
    pub peer_ipv6: String,
    #[serde(default = "default_wg_port")]
    pub wg_port: u16,
    #[serde(default = "default_mtu")]
    pub mtu: u16,

    // ── OpenVPN / SSL / SSH modes: the pasted .ovpn bundle (certs inline) ──
    #[serde(default)]
    pub openvpn_config: String,

    pub selected_server: String,
    #[serde(default = "default_mode")]
    pub selection_mode: String,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub servers_updated_ms: i64,

    /// v77: per-mode config packages auto-pulled from AirVPN's generator
    /// (key = mode). The actual files live under PACKAGE_ROOT/<mode>/; this
    /// just tracks which modes are ready + for which server. Replaces the
    /// manual paste flow — each mode is independently generated + stored.
    #[serde(default)]
    pub generated: BTreeMap<String, GeneratedInfo>,
}

fn default_mode() -> String { "manual".into() }
fn default_wg_mode() -> String { "wireguard".into() }
fn default_wg_port() -> u16 { 1637 }   // AirVPN WireGuard default entry port
fn default_mtu() -> u16 { 1320 }       // AirVPN WireGuard recommended MTU

pub fn read_state() -> AirvpnState {
    std::fs::read_to_string(SECRETS_PATH)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_state(s: &AirvpnState) -> std::io::Result<()> {
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

/// Parse a pasted AirVPN WireGuard config (the .conf from the Config
/// Generator) and fill the WG credential fields on `state`. AirVPN's
/// generated file is a standard wg-quick config — we pull PrivateKey,
/// Address (split into v4/v6, CIDR stripped), the shared [Peer] PublicKey,
/// an optional PresharedKey, the MTU, and the endpoint port (host:port).
pub fn parse_wg_config(state: &mut AirvpnState, config: &str) -> Result<(), String> {
    let mut have_priv = false;
    let mut have_pub = false;
    // PresharedKey resets per parse so re-pasting a no-PSK config clears it.
    state.wg_preshared_key.clear();
    state.peer_ipv4.clear();
    state.peer_ipv6.clear();
    for raw in config.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let (k, val) = match line.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_string()),
            None => continue,
        };
        match k.as_str() {
            "privatekey" => { state.wg_private_key = val; have_priv = true; }
            "publickey" => { state.wg_public_key = val; have_pub = true; }
            "presharedkey" => { state.wg_preshared_key = val; }
            "address" => {
                for part in val.split(',') {
                    let ip = part.trim().split('/').next().unwrap_or("").trim();
                    if ip.is_empty() {
                        continue;
                    } else if ip.contains(':') {
                        state.peer_ipv6 = ip.to_string();
                    } else {
                        state.peer_ipv4 = ip.to_string();
                    }
                }
            }
            "mtu" => { if let Ok(m) = val.parse::<u16>() { state.mtu = m; } }
            "endpoint" => {
                if let Some(port) = val.rsplit(':').next() {
                    if let Ok(p) = port.parse::<u16>() { state.wg_port = p; }
                }
            }
            _ => {}
        }
    }
    if !have_priv || state.wg_private_key.is_empty() {
        return Err("no PrivateKey found in the pasted WireGuard config".into());
    }
    if !have_pub || state.wg_public_key.is_empty() {
        return Err("no [Peer] PublicKey found in the pasted WireGuard config".into());
    }
    if state.peer_ipv4.is_empty() {
        return Err("no Interface Address (IPv4) found in the pasted WireGuard config".into());
    }
    Ok(())
}

/// GET the AirVPN status server list. AirVPN's WireGuard shares one peer
/// public key network-wide, so we stamp `shared_pubkey` + `wg_port` onto
/// every Server — switching servers changes only the endpoint IP, which is
/// exactly what render_wg_config does.
///
/// status server shape (verbatim fields probed live): public_name,
/// country_code, country_name, location, ip_v4_in1..4, ip_v6_in1..4,
/// currentload, health, bw, users. No per-server WG key (it's shared).
pub fn fetch_servers(
    api_key: &str,
    shared_pubkey: &str,
    wg_port: u16,
    provider_trust: u8,
) -> Result<Vec<Server>, String> {
    if api_key.is_empty() {
        return Err("no AirVPN API key configured — add it in setup".into());
    }
    let url = format!("{STATUS_URL}&key={api_key}");
    let v = http_get_json(&url, None)?;
    if v.get("result").and_then(|x| x.as_str()) == Some("fail") {
        let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("AirVPN API rejected the key");
        return Err(format!("AirVPN status: {msg}"));
    }
    let servers = v.get("servers").and_then(|x| x.as_array())
        .ok_or_else(|| "no servers array in AirVPN status response".to_string())?;
    let mut out = Vec::new();
    for s in servers {
        let name = s.get("public_name").and_then(|x| x.as_str()).unwrap_or("");
        let cc = s.get("country_code").and_then(|x| x.as_str()).unwrap_or("").to_uppercase();
        let cname = s.get("country_name").and_then(|x| x.as_str()).unwrap_or(&cc).to_string();
        let city = s.get("location").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ip = s.get("ip_v4_in1").and_then(|x| x.as_str()).unwrap_or("");
        if name.is_empty() || ip.is_empty() {
            continue;
        }
        let eyes = eyes_for_country(&cc);
        let score = compute_server_score(provider_trust, eyes);
        out.push(Server {
            id: name.to_string(),
            label: format!("{cname} — {city} ({name})"),
            country: cc.clone(),
            country_name: cname.clone(),
            city,
            hostname: name.to_string(),
            endpoint_ip: ip.to_string(),
            endpoint_port: wg_port,
            public_key: shared_pubkey.to_string(),
            eyes,
            server_score: score,
        });
    }
    if out.is_empty() {
        return Err("parsed 0 AirVPN servers from the status response".into());
    }
    Ok(out)
}

/// Render /etc/wireguard/aeon0.conf for the selected AirVPN WireGuard
/// server. Uses the network-wide shared peer key (stamped onto each
/// cached Server) + the user's private key/address, swapping the endpoint.
pub fn render_wg_config(state: &AirvpnState) -> Result<String, String> {
    let server = state.servers.iter()
        .find(|s| s.id == state.selected_server)
        .ok_or_else(|| format!("selected_server '{}' not in cache — refresh server list", state.selected_server))?;
    let mut addr = format!("{}/32", state.peer_ipv4);
    if !state.peer_ipv6.is_empty() {
        addr.push_str(&format!(", {}/128", state.peer_ipv6));
    }
    // Optional preshared key line (AirVPN configs may or may not include one).
    let psk_line = if state.wg_preshared_key.is_empty() {
        String::new()
    } else {
        format!("PresharedKey = {}\n", state.wg_preshared_key)
    };
    // No `DNS =` line: wg-quick applies it via resolvconf, absent on Pi OS
    // (see mullvad.rs / ivpn.rs). DNSCrypt provides tunneled DNS instead.
    Ok(format!(r#"# Managed by aeon-supervisor (AirVPN provider).
# Re-generated on every save — do not edit by hand.

[Interface]
PrivateKey = {priv}
Address    = {addr}
MTU        = {mtu}

[Peer]
PublicKey  = {peer_pub}
{psk}AllowedIPs = 0.0.0.0/0, ::/0
Endpoint   = {endpoint_ip}:{endpoint_port}
PersistentKeepalive = 25
"#,
        priv = state.wg_private_key,
        addr = addr,
        mtu = state.mtu,
        peer_pub = server.public_key,
        psk = psk_line,
        endpoint_ip = server.endpoint_ip,
        endpoint_port = server.endpoint_port,
    ))
}

// ── v77: config-generator auto-pull ─────────────────────────────────
// AirVPN's /api/generator/ produces the exact, ready-to-run files for any
// mode (WireGuard .conf; OpenVPN .ovpn; SSL = .ovpn + stunnel .ssl + CA;
// SSH = .ovpn + launcher .sh + sshtunnel.key). We fetch download=zip (a
// uniform package across modes) and store the files per mode so net-services
// can run AirVPN's own configs verbatim — no manual paste, no guessed
// endpoints. Token format is <proto>_<entry>_<transport>_<port>, where the
// entry is the server's ip_v4_inN index; SSL only lives on entry 2.

fn protocol_token(mode: &str) -> Result<&'static str, String> {
    Ok(match mode {
        "wireguard"   => "wireguard_1_udp_1637",
        "openvpn"     => "openvpn_1_tcp_443",
        "openvpn_ssl" => "openvpn_2_ssl_443",
        "openvpn_ssh" => "openvpn_1_ssh_22",
        other => return Err(format!("unknown AirVPN mode '{other}' (expected wireguard|openvpn|openvpn_ssl|openvpn_ssh)")),
    })
}

pub fn package_dir(mode: &str) -> String {
    format!("{PACKAGE_ROOT}/{mode}")
}

/// Fetch + extract AirVPN's generated config package for (server, mode).
/// `server` is a public_name (e.g. "Ainalrami") or region. Always uses
/// download=zip so we get the complete file set AirVPN ships for the mode.
pub fn generate_package(api_key: &str, server: &str, mode: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    if api_key.is_empty() {
        return Err("no AirVPN API key configured — add it in setup".into());
    }
    if server.is_empty() {
        return Err("no server selected — pick one first".into());
    }
    let token = protocol_token(mode)?;
    let url = format!("{GENERATOR_URL}?system=linux&download=zip&servers={server}&protocols={token}");
    let resp = ureq::get(&url)
        .set("API-KEY", api_key)
        .set("User-Agent", "AEON-Magick/1.0")
        .call()
        .map_err(|e| format!("AirVPN generator request failed: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    resp.into_reader().read_to_end(&mut buf)
        .map_err(|e| format!("reading generator response: {e}"))?;
    if buf.is_empty() {
        return Err("AirVPN generator returned an empty response".into());
    }
    // A JSON body (starts with '{') is an error/manifest, not a zip package.
    if buf.first() == Some(&b'{') {
        let msg = serde_json::from_slice::<serde_json::Value>(&buf).ok()
            .and_then(|v| v.get("error").or_else(|| v.get("result"))
                .and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "request rejected".into());
        return Err(format!("AirVPN generator: {msg}"));
    }
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(buf))
        .map_err(|e| format!("AirVPN generator response was not a zip: {e}"))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        if !f.is_file() { continue; }
        let name = f.name().rsplit(['/', '\\']).next().unwrap_or("").to_string();
        if name.is_empty() { continue; }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).map_err(|e| format!("reading {name}: {e}"))?;
        out.push((name, bytes));
    }
    if out.is_empty() {
        return Err("AirVPN generator package contained no files".into());
    }
    Ok(out)
}

/// Persist a package under PACKAGE_ROOT/<mode>/ (0700 dir, 0600 files),
/// replacing any prior package for that mode. Returns the stored filenames.
pub fn store_package(mode: &str, files: &[(String, Vec<u8>)]) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(PACKAGE_ROOT)?;
    let _ = std::fs::set_permissions(PACKAGE_ROOT, PermissionsExt::from_mode(0o700));
    let dir = package_dir(mode);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o700))?;
    let mut names = Vec::new();
    for (name, bytes) in files {
        let safe: String = name.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .collect();
        if safe.is_empty() { continue; }
        let path = format!("{dir}/{safe}");
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600))?;
        names.push(safe);
    }
    Ok(names)
}
