//! Per-provider VPN setup wizards (v59+).
//!
//! Each provider sub-module talks to its vendor's REST API to:
//!   - validate an account number / token
//!   - fetch the live server list
//!   - register a local WireGuard public key
//!   - hand back the peer config the supervisor writes to
//!     /etc/wireguard/aeon0.conf and brings up via wg-quick@aeon0
//!
//! All providers share the same downstream pipeline — the only
//! per-provider work is auth + server-list shape + key-registration
//! endpoint. Everything else (running the WG tunnel, kill-switch,
//! lan_bypass) is the existing apply_vpn_wireguard() machinery.
//!
//! Trust + privacy ratings live in a static table per provider
//! (see PROVIDER_META). Server-level scoring combines the provider's
//! trust tier with the server country's Eyes-tier so users can pick
//! "the no-log Mullvad server in Romania" not just "Mullvad".

use serde::{Deserialize, Serialize};

pub mod mullvad;
pub mod ivpn;
pub mod azirevpn;
pub mod airvpn;
pub mod api;

/// Eyes-alliance tier for the country a server runs in. Reuses the
/// same enum shape as the DNSCrypt relay / resolver catalogs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EyesTier {
    None,
    Five,
    Nine,
    Fourteen,
    Unknown,
}

impl EyesTier {
    /// Stable lowercase tag matching the serde representation — handy for
    /// embedding in hand-built json! values (e.g. the pick-fastest ranking).
    pub fn as_str(&self) -> &'static str {
        match self {
            EyesTier::None => "none",
            EyesTier::Five => "five",
            EyesTier::Nine => "nine",
            EyesTier::Fourteen => "fourteen",
            EyesTier::Unknown => "unknown",
        }
    }
}

/// A single VPN endpoint as exposed by the provider's server-list API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    /// Canonical identifier the provider uses (e.g. "se-got-wg-001").
    pub id: String,
    /// User-facing label ("Gothenburg #1").
    pub label: String,
    pub country: String,        // ISO 3166 alpha-2
    pub country_name: String,
    pub city: String,
    pub hostname: String,       // FQDN of the WireGuard endpoint
    pub endpoint_ip: String,    // resolved IPv4 (or hostname if unresolved)
    pub endpoint_port: u16,
    pub public_key: String,     // server WireGuard pubkey, base64
    pub eyes: EyesTier,         // computed from country
    /// 0-5 server score = privacy + jurisdiction. UI shows alongside
    /// the country flag so users can sort intelligently.
    pub server_score: u8,
}

/// Provider-level metadata (static — baked in at compile time).
#[derive(Debug, Clone, Serialize)]
pub struct ProviderMeta {
    pub id: &'static str,
    pub label: &'static str,
    pub headquarters_country: &'static str,
    pub headquarters_eyes: EyesTier,
    pub audited: bool,
    pub last_audit_year: Option<u16>,
    pub last_audit_firm: Option<&'static str>,
    pub anonymous_signup: bool,
    pub accepts_cash: bool,
    pub accepts_crypto: bool,
    /// 0-5 trust score reflecting public reputation + audits. Subjective.
    pub trust_score: u8,
    pub website: &'static str,
    pub notes: &'static str,
}

/// Static catalog of supported providers. Exposed via
/// /api/network/vpn/providers/catalog.
pub const PROVIDERS: &[ProviderMeta] = &[
    ProviderMeta {
        id: "mullvad",
        label: "Mullvad VPN",
        headquarters_country: "SE",
        headquarters_eyes: EyesTier::Fourteen,
        audited: true,
        last_audit_year: Some(2023),
        last_audit_firm: Some("Cure53 + Assured AB"),
        anonymous_signup: true,
        accepts_cash: true,
        accepts_crypto: true,
        trust_score: 5,
        website: "https://mullvad.net/",
        notes: "Account = 16-digit number with no email tied to it. Multiple independent audits. Pays its lawyers to fight subpoenas. Server jurisdictions vary widely.",
    },
    ProviderMeta {
        id: "ivpn",
        label: "IVPN",
        headquarters_country: "GI",     // Gibraltar
        headquarters_eyes: EyesTier::None,
        audited: true,
        last_audit_year: Some(2022),
        last_audit_firm: Some("Cure53"),
        anonymous_signup: false,        // requires email
        accepts_cash: true,
        accepts_crypto: true,
        trust_score: 5,
        website: "https://www.ivpn.net/",
        notes: "Gibraltar HQ keeps it outside major intelligence-sharing pacts. Audited code + infra. Smaller server fleet than Mullvad.",
    },
    ProviderMeta {
        id: "airvpn",
        label: "AirVPN",
        headquarters_country: "IT",      // Italy
        headquarters_eyes: EyesTier::Fourteen,
        audited: false,
        last_audit_year: None,
        last_audit_firm: None,
        anonymous_signup: false,         // email required at signup
        accepts_cash: false,
        accepts_crypto: true,            // BTC / Monero / vouchers
        trust_score: 4,
        website: "https://airvpn.org/",
        notes: "Activist-run (Italy, 14-Eyes HQ); crypto/voucher payment, no formal third-party audit. Uniquely Tor-friendly: native OpenVPN-over-SSL (stunnel) and OpenVPN-over-SSH to defeat DPI/blocking. WireGuard shares one network-wide server key — switch servers by endpoint. Config comes from AirVPN's Config Generator (paste once); the API key drives the server list.",
    },
    // AzireVPN removed (v67.9): acquired, trust dropped, and crucially
    // it does NOT take crypto — a hard misfit for the at-risk-user /
    // journalist threat model this device targets. The azirevpn.rs module
    // + api.rs match arms remain compiled but are unreachable now that
    // it's out of this catalog (provider_meta() gates every entry point),
    // so re-adding it later is a one-line revert.
];

pub fn provider_meta(id: &str) -> Option<&'static ProviderMeta> {
    PROVIDERS.iter().find(|p| p.id == id)
}

// ── Country → Eyes mapping (reused logic) ────────────────────────────

pub fn eyes_for_country(country: &str) -> EyesTier {
    let c = country.to_uppercase();
    const EYES_5: &[&str] = &["US", "GB", "CA", "AU", "NZ"];
    const EYES_9_EXTRA: &[&str] = &["FR", "DK", "NL", "NO"];
    const EYES_14_EXTRA: &[&str] = &["DE", "BE", "IT", "ES", "SE"];
    if EYES_5.contains(&c.as_str()) { return EyesTier::Five; }
    if EYES_9_EXTRA.contains(&c.as_str()) { return EyesTier::Nine; }
    if EYES_14_EXTRA.contains(&c.as_str()) { return EyesTier::Fourteen; }
    if c.is_empty() { return EyesTier::Unknown; }
    EyesTier::None
}

/// 0-5 server score = provider trust + jurisdiction bonus. Used for
/// the auto-pick ranking when the user doesn't specify a server.
pub fn compute_server_score(provider_trust: u8, eyes: EyesTier) -> u8 {
    let eyes_bonus: u8 = match eyes {
        EyesTier::None => 2,
        EyesTier::Fourteen => 1,
        EyesTier::Nine => 0,
        EyesTier::Five => 0,
        EyesTier::Unknown => 1,
    };
    (provider_trust.saturating_add(eyes_bonus)).min(5)
}

// ── HTTP helper (shared by all provider modules) ─────────────────────

/// UTF-8-safe truncation for error bodies. Slicing a &str at an
/// arbitrary byte index panics if it lands mid-codepoint — and a
/// panic *inside the error-formatting path* would mask the real
/// failure. Walk back to the nearest char boundary.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Turn a non-2xx HTTP response into a human-readable error string.
///
/// v65.1: this is the fix for the opaque "status code 404 / 400"
/// errors the VPN wizard used to show. ureq 2.x returns
/// `Err(Error::Status(code, resp))` for any non-2xx, so the previous
/// `req.call().map_err(|e| format!("http: {e}"))?` short-circuited
/// BEFORE reading the response body — discarding the provider's
/// actual explanation. Providers stash that explanation under
/// different keys, so try the common ones:
///   - IVPN / AzireVPN: `{"message": "..."}`
///   - Mullvad:         `{"error": "...", "code": "..."}`
/// Fall back to the raw (truncated) body if none match.
fn api_error_message(code: u16, body: &str) -> String {
    // Human gloss for status codes whose raw meaning trips people up.
    // 402 in particular: every provider returns it for "valid token,
    // but the account isn't paid up" — the token authenticated (else
    // you'd get 401), so the actionable cause is billing, not creds.
    // (AzireVPN returns 402 on /v3/ips when the subscription lapsed.)
    let hint = match code {
        402 => " — your VPN subscription appears inactive or expired; renew with the provider, then retry (the token itself is valid)",
        403 => " — the account is authenticated but not permitted this action (subscription tier, device/key limit, or token scope)",
        429 => " — rate-limited by the provider; wait a moment and retry",
        _ => "",
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        for key in ["message", "error", "detail", "description", "error_message"] {
            if let Some(msg) = v.get(key).and_then(|x| x.as_str()) {
                if !msg.trim().is_empty() {
                    // Include a machine code if the provider gave one
                    // (Mullvad's INVALID_ACCOUNT, etc.) — helps support.
                    let code_hint = v.get("code")
                        .and_then(|x| x.as_str())
                        .filter(|c| !c.is_empty())
                        .map(|c| format!(" [{c}]"))
                        .unwrap_or_default();
                    return format!("HTTP {code}: {}{code_hint}{hint}", msg.trim());
                }
            }
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        format!("HTTP {code}{hint}")
    } else {
        format!("HTTP {code}: {}{hint}", truncate_str(trimmed, 300))
    }
}

/// Minimal blocking HTTP client. All provider API calls are
/// infrequent (setup wizard + occasional refresh), so the simple
/// blocking path beats async machinery for clarity.
pub fn http_get_json(url: &str, bearer: Option<&str>) -> Result<serde_json::Value, String> {
    let mut req = ureq::get(url)
        .set("User-Agent", "AEON-Magick/1.0")
        .set("Accept", "application/json");
    if let Some(t) = bearer {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.call() {
        Ok(resp) => {
            let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
            serde_json::from_str(&body)
                .map_err(|e| format!("parse: {e} (body: {})", truncate_str(&body, 200)))
        }
        // Non-2xx — read the body so the provider's actual error
        // surfaces instead of a bare "status code NNN".
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(api_error_message(code, &body))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("network error reaching {url}: {t}"))
        }
    }
}

pub fn http_post_json(url: &str, bearer: Option<&str>, payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    let mut req = ureq::post(url)
        .set("User-Agent", "AEON-Magick/1.0")
        .set("Accept", "application/json")
        .set("Content-Type", "application/json");
    if let Some(t) = bearer {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    match req.send_json(payload.clone()) {
        Ok(resp) => {
            let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
            serde_json::from_str(&body)
                .map_err(|e| format!("parse: {e} (body: {})", truncate_str(&body, 200)))
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(api_error_message(code, &body))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("network error reaching {url}: {t}"))
        }
    }
}

// ── WireGuard keygen helper ──────────────────────────────────────────

/// Generate a new Curve25519 keypair for WireGuard. Returns
/// (private_key_base64, public_key_base64).
///
/// We shell out to the `wg` CLI since the supervisor already
/// depends on wireguard-tools for the runtime stack. Doing it
/// in-process would need x25519-dalek which is fine but adds
/// another crate just for keygen.
pub fn generate_wg_keypair() -> Result<(String, String), String> {
    use std::process::{Command, Stdio};
    let priv_out = Command::new("wg").arg("genkey").output()
        .map_err(|e| format!("wg genkey: {e}"))?;
    if !priv_out.status.success() {
        return Err("wg genkey returned non-zero".into());
    }
    let private_key = String::from_utf8_lossy(&priv_out.stdout).trim().to_string();

    let mut pub_proc = Command::new("wg").arg("pubkey")
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .spawn().map_err(|e| format!("wg pubkey: {e}"))?;
    use std::io::Write;
    pub_proc.stdin.as_mut().unwrap()
        .write_all(private_key.as_bytes())
        .map_err(|e| format!("wg pubkey stdin: {e}"))?;
    let pub_out = pub_proc.wait_with_output()
        .map_err(|e| format!("wg pubkey wait: {e}"))?;
    if !pub_out.status.success() {
        return Err("wg pubkey returned non-zero".into());
    }
    let public_key = String::from_utf8_lossy(&pub_out.stdout).trim().to_string();
    Ok((private_key, public_key))
}
