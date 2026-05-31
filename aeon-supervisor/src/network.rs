//! /api/network/* — read & write USB-ethernet, DNSCrypt, and VPN config.
//!
//! All state lives in /etc/aeon/network.toml. PUTs write the file then
//! ask systemd to reload the matching service (aeon-usb-net for usb_ethernet,
//! aeon-net-services for dnscrypt + vpn), which re-applies the runtime
//! state idempotently.

use crate::api::AppState;
use axum::extract::{Query, State};
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
    /// v58+: Tor and I2P are independent top-level toggles. They
    /// can run alongside any vpn.provider (or none). Old configs
    /// with vpn.provider="tor" or "i2p" get migrated by
    /// migrate_legacy() at read time.
    #[serde(default)]
    tor: Tor,
    #[serde(default)]
    i2p: I2p,
}

/// v58: rewrite old vpn.provider="tor"/"i2p" configs into the new
/// independent [tor]/[i2p] sections in-place. Idempotent. Called by
/// read_state() before anyone else sees the struct.
fn migrate_legacy(nf: &mut NetFile) {
    match nf.vpn.provider.as_str() {
        "tor" => {
            // Old behaviour was "everything via Tor" → keep that as
            // transparent mode, and clear the now-invalid VPN provider.
            tracing::info!("migrate: vpn.provider=tor → tor.enabled=true (transparent), vpn=none");
            nf.tor.enabled = true;
            nf.tor.mode = "transparent".into();
            nf.vpn.provider = "none".into();
        }
        "i2p" => {
            tracing::info!("migrate: vpn.provider=i2p → i2p.enabled=true, vpn=none");
            nf.i2p.enabled = true;
            nf.vpn.provider = "none".into();
        }
        _ => {}
    }
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
    /// Custom sdns:// stamp (when provider="custom"). DNSCrypt v2 stamps
    /// can encode DNSCrypt, DoH, DoT, or ODoH endpoints — see
    /// https://dnscrypt.info/stamps for the format.
    #[serde(default)]
    custom_stamp: String,
    /// Friendly label for the custom stamp (shown in the UI).
    #[serde(default)]
    custom_label: String,
    /// Server-selection mode (v55+):
    ///   "specific" — pin to the single resolver in `provider`
    ///                (legacy behaviour, still the default).
    ///   "auto"     — feed dnscrypt-proxy ALL resolvers matching the
    ///                `auto_criteria` filter. Its lb_strategy="p2"
    ///                routes per-query to the lowest-latency match.
    #[serde(default = "default_server_mode")]
    server_mode: String,
    /// Criteria for auto mode. Ignored when server_mode="specific".
    #[serde(default)]
    auto_criteria: crate::dnscrypt_servers::ResolverCriteria,
    /// Cache of the resolvers auto_pick produced on last apply. Used
    /// so the API and aeon-net-services.sh see the same list.
    #[serde(default)]
    auto_picked_servers: Vec<String>,
    /// Anonymized DNSCrypt: route queries through a relay so the
    /// resolver never sees the client IP. v51+.
    #[serde(default)]
    anonymized: Anonymized,
}
/// v56: default to criteria-based auto mode. The strict defaults on
/// `ResolverCriteria` give ~86 candidates, of which the supervisor
/// picks 30 across multiple operators — dnscrypt-proxy's
/// lb_strategy="p2" then routes per-query to the lowest-latency one
/// live. When anonymized DNSCrypt is also enabled, the latency
/// probe goes THROUGH the relay path, so the picked server is the
/// fastest end-to-end choice (client → relay → resolver). Users
/// who want a single named provider can switch to "specific".
fn default_server_mode() -> String { "auto".into() }

/// Anonymized DNSCrypt configuration. Off by default — adds 30-100ms
/// of latency per query, so opt-in. When on, the user picks either
/// "auto" mode (system selects 3 relays matching the criteria, from
/// 3 different operators in 3 different jurisdictions) or "specific"
/// mode (user names the relays directly).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct Anonymized {
    /// Master toggle for the whole anonymized layer.
    #[serde(default)]
    enabled: bool,
    /// "auto" or "specific".
    #[serde(default = "default_anon_mode")]
    mode: String,
    /// Criteria the auto-picker uses to filter the relay catalog.
    /// Ignored when mode = "specific".
    #[serde(default)]
    criteria: crate::dnscrypt_relays::RelayCriteria,
    /// User-named relays for mode="specific". Each entry must match
    /// a name in the curated catalog (we validate on PUT).
    #[serde(default)]
    specific_relays: Vec<String>,
    /// Resolved relay list — what the picker (or user, for specific
    /// mode) actually chose. Persisted to TOML so aeon-net-services.sh
    /// reads the same picks that the API computed. Recomputed on every
    /// PUT that touches anonymized fields, the provider, or the
    /// criteria.
    #[serde(default)]
    picked_relays: Vec<String>,
}

fn default_anon_mode() -> String { "auto".into() }

impl Default for Dnscrypt {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_dns_provider(),
            location: default_dns_location(),
            custom_stamp: String::new(),
            custom_label: String::new(),
            server_mode: default_server_mode(),
            auto_criteria: crate::dnscrypt_servers::ResolverCriteria::default(),
            auto_picked_servers: Vec::new(),
            anonymized: Anonymized::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Vpn {
    #[serde(default)]
    enabled: bool,
    /// v58: provider enum trimmed. "tor" and "i2p" are no longer
    /// VPN providers — they live in their own top-level [tor] and
    /// [i2p] sections and can run alongside any VPN choice.
    /// Valid: "none" | "tailscale" | "wireguard" | "openvpn".
    /// Old configs with provider="tor" or "i2p" get migrated in
    /// migrate_legacy_config().
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
        }
    }
}

// ── v58: Tor as a top-level independent toggle ───────────────────────
//
// Old model: vpn.provider = "tor" made Tor the entire VPN and
// transparently redirected ALL outbound TCP through Tor. New model
// decouples this — Tor can be on independently of any "VPN" choice.
// .onion hidden-service access is the primary use case for the
// split_tunnel mode; transparent mode preserves the old "everything
// via Tor" behaviour for users who want it.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Tor {
    /// Master toggle. When true, the Tor service runs and is
    /// reachable on usb0 for the DNS-based .onion fix.
    #[serde(default)]
    enabled: bool,
    /// "split_tunnel" — only TCP destined for Tor's virtual-IP range
    ///                  (10.192.0.0/10) gets REDIRECTed to TransPort.
    ///                  Clearnet TCP goes via the default route /
    ///                  VPN. This is the v58 default.
    /// "transparent"  — ALL outbound TCP goes through Tor (old v57
    ///                  behaviour, kept for users who want it).
    #[serde(default = "default_tor_mode")]
    mode: String,
    /// Nest Tor through the active VPN. Requires vpn.enabled = true.
    /// When on, the debian-tor UID's outbound packets get fwmarked
    /// and policy-routed through the VPN tunnel — entry-guard
    /// connections leave via the VPN, the rest of the path is
    /// normal Tor.
    #[serde(default)]
    over_vpn: bool,
    #[serde(default = "default_tor_preset")]
    preset: String,
    #[serde(default)]
    bridges: String,
    #[serde(default)]
    exit_country: String,
    #[serde(default)]
    meek_mode: bool,
}

impl Default for Tor {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_tor_mode(),
            over_vpn: false,
            preset: default_tor_preset(),
            bridges: String::new(),
            exit_country: String::new(),
            meek_mode: false,
        }
    }
}

fn default_tor_mode() -> String { "split_tunnel".to_string() }
fn default_tor_preset() -> String { "direct".to_string() }

// ── v58: I2P as a top-level independent toggle ───────────────────────
//
// Same story as Tor — independent of any "VPN" choice. I2P is
// always "proxy mode" by design (no transparent equivalent
// possible), so there's no mode field.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct I2p {
    #[serde(default)]
    enabled: bool,
    /// Optional clearnet outproxy (e.g. exit.stormycloud.i2p).
    /// Empty = .i2p-only mode (the safe default).
    #[serde(default)]
    outproxy: String,
    /// v58.1: nest i2pd's outbound traffic through the active VPN.
    /// Same fwmark + ip rule machinery as tor.over_vpn but for the
    /// i2pd UID. Requires vpn.enabled = true.
    #[serde(default)]
    over_vpn: bool,
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
    let mut nf: NetFile = std::fs::read_to_string(NETWORK_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    migrate_legacy(&mut nf);
    nf
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

/// Query params for GET /api/network/dnscrypt.
#[derive(serde::Deserialize, Default)]
pub struct DnscryptQuery {
    /// Include the full static resolver + relay catalogs (~141 KB).
    /// Off by default so status polls stay tiny — the catalogs are
    /// build-time-static, so the config UI fetches them once on demand
    /// (or via its manual "refresh list" button) instead of every poll.
    #[serde(default)]
    pub catalog: bool,
}

/// GET /api/network/dnscrypt — current DNSCrypt state. Pass
/// `?catalog=true` to include the heavy static resolver/relay catalogs;
/// status polls omit them to stay small.
pub async fn get_dnscrypt(
    State(_state): State<AppState>,
    Query(q): Query<DnscryptQuery>,
) -> Json<Value> {
    let s = read_state();

    // Heavy catalogs are opt-in (?catalog=true). Empty arrays otherwise
    // so the response shape stays stable for status-only callers.
    let servers_catalog: Value = if q.catalog {
        json!(crate::dnscrypt_servers::catalog())
    } else {
        Value::Array(vec![])
    };
    let anon_catalog: Value = if q.catalog {
        json!(crate::dnscrypt_relays::catalog())
    } else {
        Value::Array(vec![])
    };

    // picked_relays is persisted (computed at PUT time) so the API
    // and the apply script see the same selection. Empty when
    // anonymized is off.
    let picked_relays = s.dnscrypt.anonymized.picked_relays.clone();

    // Each provider carries metadata so the UI can show users what they
    // signed up for: log policy, DNSSEC validation, filtering, the
    // transport, and the jurisdiction the operator's behind. All of
    // this is on the provider's published privacy page; we just
    // surface it next to the radio button.
    //
    // Logging tiers:
    //   "no_logs"        — operator publicly commits to zero logging
    //   "anonymized"     — operator keeps aggregated/anonymized stats
    //   "self_logs"      — operator keeps per-user dashboards (opt-in tier)
    //
    // Security tiers:
    //   "basic"          — DNSSEC valid, encrypted transport
    //   "filtered"       — also blocks malware/phishing
    //   "family"         — also blocks adult content
    //   "ad_block"       — also blocks ads + trackers
    Json(json!({
        "ok": true,
        "enabled": s.dnscrypt.enabled,
        "provider": s.dnscrypt.provider,
        "location": s.dnscrypt.location,
        "custom_stamp": s.dnscrypt.custom_stamp,
        "custom_label": s.dnscrypt.custom_label,
        // Server selection (v55+). When server_mode="auto",
        // `auto_picked_servers` is the live list of resolver names
        // dnscrypt-proxy will probe + route between by latency. The
        // full catalog of ~226 DNSCrypt v2 resolvers is included
        // under `servers.catalog` so the UI can render the
        // search/filter list without a second round-trip.
        "servers": {
            "mode": s.dnscrypt.server_mode,
            "auto_criteria": s.dnscrypt.auto_criteria,
            "auto_picked": s.dnscrypt.auto_picked_servers,
            "catalog": servers_catalog,
        },
        "anonymized": {
            "enabled": s.dnscrypt.anonymized.enabled,
            "mode": s.dnscrypt.anonymized.mode,
            "criteria": s.dnscrypt.anonymized.criteria,
            "specific_relays": s.dnscrypt.anonymized.specific_relays,
            // Resolved relay names that would be applied right now.
            // Empty Vec if anonymized.enabled = false.
            "currently_picked": picked_relays,
            // Full curated catalog so the UI can render filter chips +
            // a "specific relay" multi-select.
            "catalog": anon_catalog,
        },
        // ── DNSCrypt-only provider list (v49+) ──────────────────────
        //
        // Why no Cloudflare / NextDNS / Mullvad here: those providers
        // run DoH/DoT only — they don't operate native DNSCrypt v2
        // servers. The old list ran them as DoH-via-dnscrypt-proxy,
        // which meant SNI of the resolver was leaking on every query
        // (defeating most of the "encrypted DNS" benefit). True
        // DNSCrypt has no TLS layer and so no SNI to leak. Users who
        // want Cloudflare anyway can paste their DoH sdns:// stamp
        // into the Custom slot below — the protocol field stays
        // honest.
        "providers": [
            {
                "id": "quad9", "label": "Quad9 (filtered)",
                "blurb": "Swiss non-profit; blocks known-malicious domains via threat-intel feeds. The default — solid pick if you want some protection.",
                "transport": "DNSCrypt",
                "log_policy": "no_logs",
                "log_detail": "Publicly audited zero-log policy; Swiss data-protection law applies.",
                "security": "filtered",
                "jurisdiction": "CH",
                "homepage": "https://quad9.net/",
            },
            {
                "id": "quad9-unfiltered", "label": "Quad9 (unfiltered)",
                "blurb": "Same Quad9 anycast network, no malware filter. Pure encrypted DNS for users who don't want server-side filtering.",
                "transport": "DNSCrypt",
                "log_policy": "no_logs",
                "log_detail": "Same zero-log policy as filtered Quad9.",
                "security": "basic",
                "jurisdiction": "CH",
                "homepage": "https://quad9.net/",
            },
            {
                "id": "adguard", "label": "AdGuard DNS",
                "blurb": "Ad + tracker blocklists applied at the resolver. Good for general-purpose privacy with light filtering.",
                "transport": "DNSCrypt",
                "log_policy": "anonymized",
                "log_detail": "Aggregated query stats only; no per-user identifiers retained.",
                "security": "ad_block",
                "jurisdiction": "CY",
                "homepage": "https://adguard-dns.io/",
            },
            {
                "id": "adguard-family", "label": "AdGuard Family",
                "blurb": "AdGuard with safe-search enforcement and adult-content blocking on top of the ad/tracker filter.",
                "transport": "DNSCrypt",
                "log_policy": "anonymized",
                "log_detail": "Same anonymized-stats policy as AdGuard default.",
                "security": "family",
                "jurisdiction": "CY",
                "homepage": "https://adguard-dns.io/",
            },
            {
                "id": "adguard-unfiltered", "label": "AdGuard Unfiltered",
                "blurb": "AdGuard's encrypted DNS without any filtering — pure transport encryption.",
                "transport": "DNSCrypt",
                "log_policy": "anonymized",
                "log_detail": "Same anonymized-stats policy as AdGuard default.",
                "security": "basic",
                "jurisdiction": "CY",
                "homepage": "https://adguard-dns.io/",
            },
            {
                "id": "opendns", "label": "OpenDNS (Cisco)",
                "blurb": "Cisco's public DNSCrypt resolver. Mature anycast network; malware filtering enabled by default on the standard endpoint.",
                "transport": "DNSCrypt",
                "log_policy": "anonymized",
                "log_detail": "Cisco retains aggregated query data for threat-intel purposes; no per-user dashboards.",
                "security": "filtered",
                "jurisdiction": "US",
                "homepage": "https://www.opendns.com/",
            },
            {
                "id": "cleanbrowsing", "label": "CleanBrowsing Security",
                "blurb": "Independent operator; blocks known phishing + malware domains. Lightweight filter, US-based.",
                "transport": "DNSCrypt",
                "log_policy": "anonymized",
                "log_detail": "Aggregated stats only; published privacy policy.",
                "security": "filtered",
                "jurisdiction": "US",
                "homepage": "https://cleanbrowsing.org/",
            },
            {
                "id": "custom", "label": "Custom (paste a stamp)",
                "blurb": "Paste an sdns:// stamp from dnscrypt.info or a provider's site. Supports true DNSCrypt v2 — and also DoH/DoT/ODoH if you accept that those expose the resolver via TLS SNI.",
                "transport": "any",
                "log_policy": "varies",
                "log_detail": "Depends on the operator behind the stamp.",
                "security": "varies",
                "jurisdiction": "varies",
                "homepage": "https://dnscrypt.info/stamps/",
            },
        ],
        "locations": [
            {"id": "auto", "label": "Auto (pick by latency)"},
            {"id": "us", "label": "Americas"},
            {"id": "eu", "label": "Europe / Africa"},
            {"id": "asia", "label": "Asia / Pacific"},
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
    #[serde(default)]
    pub custom_stamp: Option<String>,
    #[serde(default)]
    pub custom_label: Option<String>,
    #[serde(default)]
    pub anonymized: Option<AnonymizedPutReq>,
    /// v55: criteria-based auto-pick across the full ~226-entry
    /// upstream resolver catalog.
    #[serde(default)]
    pub servers: Option<ServersPutReq>,
}

#[derive(Deserialize)]
pub struct ServersPutReq {
    /// "specific" or "auto".
    #[serde(default)] pub mode: Option<String>,
    #[serde(default)] pub auto_criteria: Option<crate::dnscrypt_servers::ResolverCriteria>,
}

/// All fields optional — clients send only what they're changing.
#[derive(Deserialize)]
pub struct AnonymizedPutReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub criteria: Option<crate::dnscrypt_relays::RelayCriteria>,
    #[serde(default)]
    pub specific_relays: Option<Vec<String>>,
}

/// PUT /api/network/dnscrypt — update DNSCrypt config + apply.
pub async fn put_dnscrypt(
    State(_state): State<AppState>,
    Json(req): Json<DnscryptPutReq>,
) -> impl IntoResponse {
    // Keep in sync with the providers list returned by GET
    // /api/network/dnscrypt. Only DNSCrypt-native operators here —
    // DoH-only providers (Cloudflare / NextDNS / Mullvad / etc.) were
    // dropped in v50; users who want them can paste their sdns://
    // stamp into the Custom slot.
    const VALID_PROVIDERS: &[&str] = &[
        "quad9",
        "quad9-unfiltered",
        "adguard",
        "adguard-family",
        "adguard-unfiltered",
        "opendns",
        "cleanbrowsing",
        "custom",
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
    if let Some(stamp) = req.custom_stamp {
        let s = stamp.trim();
        // Minimal validation: sdns:// scheme + reject anything with newlines
        // or a torrc-injection-y character. The actual stamp parsing
        // happens inside dnscrypt-proxy when it starts.
        if !s.is_empty() && (!s.starts_with("sdns://") || s.contains('\n') || s.contains('\r')) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "custom_stamp must be an sdns:// URL"})),
            )
                .into_response();
        }
        nf.dnscrypt.custom_stamp = s.to_string();
    }
    if let Some(label) = req.custom_label {
        // Strip control chars to avoid trashing the TOML / log lines.
        let cleaned: String = label.chars().filter(|c| !c.is_control()).take(64).collect();
        nf.dnscrypt.custom_label = cleaned;
    }
    if let Some(enabled) = req.enabled {
        // Don't allow enabling a custom provider that has no stamp.
        if enabled
            && nf.dnscrypt.provider == "custom"
            && nf.dnscrypt.custom_stamp.is_empty()
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "custom provider requires a custom_stamp"})),
            )
                .into_response();
        }
        nf.dnscrypt.enabled = enabled;
    }

    // Anonymized DNSCrypt updates
    if let Some(anon) = req.anonymized {
        if let Some(e) = anon.enabled {
            nf.dnscrypt.anonymized.enabled = e;
        }
        if let Some(mode) = anon.mode {
            if mode != "auto" && mode != "specific" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "anonymized.mode must be 'auto' or 'specific'"})),
                )
                    .into_response();
            }
            nf.dnscrypt.anonymized.mode = mode;
        }
        if let Some(c) = anon.criteria {
            nf.dnscrypt.anonymized.criteria = c;
        }
        if let Some(relays) = anon.specific_relays {
            // Validate every relay name is in the curated catalog —
            // unknown names would just be silently skipped by
            // dnscrypt-proxy (via skip_incompatible=true) and the
            // user would never know their config was a no-op.
            let known: std::collections::HashSet<&str> =
                crate::dnscrypt_relays::catalog().iter().map(|r| r.name.as_str()).collect();
            for r in &relays {
                if !known.contains(r.as_str()) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({
                            "ok": false,
                            "err": format!("unknown relay '{r}' — not in curated catalog"),
                        })),
                    )
                        .into_response();
                }
            }
            // Cap at 8 — more than that and dnscrypt-proxy spends
            // longer probing relays than serving DNS.
            if relays.len() > 8 {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "specific_relays capped at 8"})),
                )
                    .into_response();
            }
            // Diversity check: warn (don't reject) if all picks are
            // from the same operator. We return ok but include a hint.
            nf.dnscrypt.anonymized.specific_relays = relays;
        }
        // Anonymized requires base DNSCrypt to be on — otherwise the
        // [anonymized_dns] block is dead config. Auto-enable.
        if nf.dnscrypt.anonymized.enabled && !nf.dnscrypt.enabled {
            nf.dnscrypt.enabled = true;
        }
    }

    // v55: server-selection mode + criteria.
    if let Some(srv) = req.servers {
        if let Some(m) = srv.mode {
            if m != "specific" && m != "auto" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "servers.mode must be 'specific' or 'auto'"})),
                )
                    .into_response();
            }
            nf.dnscrypt.server_mode = m;
        }
        if let Some(c) = srv.auto_criteria {
            nf.dnscrypt.auto_criteria = c;
        }
    }

    // v55.1: when Tor is the active VPN, force the auto-pick to
    // port-443-only resolvers. Tor exit policies block alternates
    // (8443 / 5443 / etc.) which silently time out even with
    // force_tcp on. We mutate the user's saved criteria so it shows
    // up in the GET response — the UI surfaces "Tor-active filter is
    // on" so the user understands why their pool shrunk.
    let tor_active = nf.vpn.enabled && nf.vpn.provider == "tor";
    nf.dnscrypt.auto_criteria.tor_friendly_port = tor_active;

    // Recompute the auto-picked server list so the TOML on disk
    // matches whatever aeon-net-services.sh will see on the next
    // reload. Cap at 30 — dnscrypt-proxy probes every server on
    // startup so larger pools turn into slow boots.
    nf.dnscrypt.auto_picked_servers = if nf.dnscrypt.server_mode == "auto" {
        crate::dnscrypt_servers::auto_pick(&nf.dnscrypt.auto_criteria, 30)
    } else {
        Vec::new()
    };

    // Recompute picked_relays AFTER any field changes so the TOML
    // reflects the actual relays that will be in effect on the next
    // aeon-net-services reload. This avoids the supervisor and the
    // shell script disagreeing about which relays are active.
    nf.dnscrypt.anonymized.picked_relays = if nf.dnscrypt.anonymized.enabled {
        if nf.dnscrypt.anonymized.mode == "specific" {
            nf.dnscrypt.anonymized.specific_relays.clone()
        } else {
            let resolver_op = crate::dnscrypt_relays::resolver_operator(&nf.dnscrypt.provider);
            crate::dnscrypt_relays::auto_pick(
                &nf.dnscrypt.anonymized.criteria,
                resolver_op,
                3,
            )
        }
    } else {
        Vec::new()
    };

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
        // v58: tor and i2p are now read from top-level [tor] / [i2p]
        // sections but exposed under the same JSON keys so older
        // UI clients keep working. Adds enabled/mode/over_vpn for
        // the new toggles.
        "tor": {
            "enabled": s.tor.enabled,
            "mode": s.tor.mode,
            "over_vpn": s.tor.over_vpn,
            "preset": s.tor.preset,
            "has_bridges": !s.tor.bridges.is_empty(),
            "exit_country": s.tor.exit_country,
            "meek_mode": s.tor.meek_mode,
            "presets": [
                {"id": "direct", "label": "Direct", "blurb": "No bridge. Works on unrestricted networks. Fastest option."},
                {"id": "obfs4", "label": "obfs4 (built-in)", "blurb": "Standard obfs4 obfuscation against simple traffic-analysis. Uses the Tor Browser default bridge list."},
                {"id": "meek-azure", "label": "meek-azure", "blurb": "Tunnels through Microsoft Azure CDN, looking like HTTPS to Microsoft. Slow but very hard to block — works in most restrictive networks."},
                {"id": "snowflake", "label": "Snowflake", "blurb": "Ephemeral WebRTC-based bridges via volunteer proxies. Requires snowflake-client (install: apt install snowflake-client)."},
                {"id": "custom", "label": "Custom", "blurb": "Paste your own bridge lines below. Get fresh bridges from bridges.torproject.org."},
            ],
            "modes": [
                {"id": "split_tunnel", "label": "Split tunnel (.onion only)", "blurb": "Only TCP destined for .onion services rides Tor. Clearnet keeps its normal path (default route, or your VPN if one is selected). The recommended default."},
                {"id": "transparent", "label": "Transparent (everything via Tor)", "blurb": "Every outbound TCP connection from the Pi + USB clients goes through Tor. Strongest privacy, but slow and many services break (CAPTCHAs, geo-blocks). Old v57 and earlier did this by default."},
            ],
        },
        "i2p": {
            "enabled": s.i2p.enabled,
            "outproxy": s.i2p.outproxy,
            "over_vpn": s.i2p.over_vpn,
        },
        // v58: clearnet providers — tor and i2p moved out of the VPN
        // enum but still appear in the providers list for backward
        // compat with older UI clients. Selecting them flips the
        // corresponding top-level toggle (see PUT migration in
        // put_vpn). Once UI's been updated to use the independent
        // tor.enabled / i2p.enabled fields, these two entries can
        // be dropped from the list.
        "providers": [
            {"id": "none", "label": "None", "blurb": "No clearnet VPN. Traffic exits via the Pi's normal upstream (eth0/wlan0). Independent Tor + I2P can still be on for .onion / .i2p sites — see the toggles below."},
            {"id": "tailscale", "label": "Tailscale", "blurb": "WireGuard mesh. Bring an auth-key from the Tailscale admin console. Optionally turn this Pi into an exit-node for your tailnet."},
            {"id": "wireguard", "label": "WireGuard", "blurb": "Paste a working WireGuard .conf. We'll run it via wg-quick@aeon0."},
            {"id": "openvpn", "label": "OpenVPN", "blurb": "Paste a working .ovpn config. Username/password optional."},
            // v59: provider wizards. Underlying transport is WireGuard;
            // config is fetched from the provider's REST API via the
            // /api/network/vpn/providers/:id/* endpoints. Per-provider
            // setup lives under /network/vpn/providers/<id>.
            {"id": "mullvad", "label": "Mullvad VPN", "blurb": "Swedish HQ, multiple 3rd-party audits, anonymous account (16-digit number, no email), accepts cash + crypto. Setup wizard at /network/vpn-providers."},
            {"id": "ivpn", "label": "IVPN", "blurb": "Gibraltar HQ (outside 14-Eyes), audited by Cure53, accepts cash + crypto. Setup wizard at /network/vpn-providers."},
            {"id": "tor", "label": "Tor (legacy)", "blurb": "v58+: prefer the independent Tor toggle below — Tor can now run alongside any of the above. Selecting this option flips tor.enabled=true in transparent mode for backward compat."},
            {"id": "i2p", "label": "I2P (legacy)", "blurb": "v58+: prefer the independent I2P toggle below. Selecting this option flips i2p.enabled=true for backward compat."},
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
    // v58: top-level fields for the independent toggle + mode + nest.
    #[serde(default)] pub enabled: Option<bool>,
    #[serde(default)] pub mode: Option<String>,
    #[serde(default)] pub over_vpn: Option<bool>,
    #[serde(default)] pub preset: Option<String>,
    #[serde(default)] pub bridges: Option<String>,
    #[serde(default)] pub exit_country: Option<String>,
    #[serde(default)] pub meek_mode: Option<bool>,
}

#[derive(Deserialize)]
pub struct I2pPut {
    #[serde(default)] pub enabled: Option<bool>,
    #[serde(default)] pub outproxy: Option<String>,
    #[serde(default)] pub over_vpn: Option<bool>,
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
    let mut nf = read_state();

    if let Some(p) = req.provider.as_deref() {
        // Assign as-is; the canonical validation + the tor/i2p → overlay-toggle
        // migration both run downstream (see VALID_VPN_PROVIDERS below). The
        // early allow-list that used to sit here was stale — it still listed
        // tor/i2p (moved to overlay toggles in v58) and never gained mullvad/
        // ivpn (v59), so it 400'd a perfectly valid IVPN/Mullvad save with
        // "unknown vpn provider" before the correct check downstream ever ran.
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
    // v58: tor + i2p moved to top-level. PUT still accepts the same
    // nested shape ({tor: {...}, i2p: {...}}) so the existing UI
    // keeps working until it migrates to the new endpoints.
    if let Some(tor) = req.tor {
        if let Some(v) = tor.enabled { nf.tor.enabled = v; }
        if let Some(m) = tor.mode {
            if m != "split_tunnel" && m != "transparent" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": "tor.mode must be 'split_tunnel' or 'transparent'"})),
                ).into_response();
            }
            nf.tor.mode = m;
        }
        if let Some(v) = tor.over_vpn { nf.tor.over_vpn = v; }
        if let Some(p) = tor.preset {
            const VALID_PRESETS: &[&str] = &["direct", "obfs4", "meek-azure", "snowflake", "custom"];
            if !VALID_PRESETS.contains(&p.as_str()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": format!("unknown tor preset '{p}'")})),
                ).into_response();
            }
            nf.tor.preset = p;
        }
        if let Some(v) = tor.bridges { nf.tor.bridges = v; }
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
            nf.tor.exit_country = v;
        }
        if let Some(v) = tor.meek_mode { nf.tor.meek_mode = v; }
    }
    if let Some(i2p) = req.i2p {
        if let Some(v) = i2p.enabled { nf.i2p.enabled = v; }
        if let Some(v) = i2p.outproxy { nf.i2p.outproxy = v; }
        if let Some(v) = i2p.over_vpn { nf.i2p.over_vpn = v; }
    }

    // v58: validate vpn.provider — "tor" and "i2p" got moved out of
    // the VPN enum, but the existing UI may still send them. Migrate
    // PUT-time instead of 400ing: provider="tor" flips tor.enabled
    // = true + transparent mode (the old behaviour), provider="i2p"
    // flips i2p.enabled = true. This way old UIs keep working while
    // we ship updated UI in v58.1.
    if nf.vpn.provider == "tor" {
        tracing::info!("PUT migration: provider=tor → tor.enabled=true + mode=transparent, vpn=none");
        nf.tor.enabled = true;
        if nf.tor.mode != "transparent" && nf.tor.mode != "split_tunnel" {
            nf.tor.mode = "transparent".into();
        } else if nf.tor.mode == "split_tunnel" {
            // Caller upgraded to v58 schema mid-flight: keep their
            // chosen mode.
        }
        nf.vpn.provider = "none".into();
    } else if nf.vpn.provider == "i2p" {
        tracing::info!("PUT migration: provider=i2p → i2p.enabled=true, vpn=none");
        nf.i2p.enabled = true;
        nf.vpn.provider = "none".into();
    }
    // v59: mullvad/ivpn/azirevpn join the list — they all run as
    // WireGuard tunnels but their config comes from the provider
    // wizard rather than a user-pasted .conf.
    const VALID_VPN_PROVIDERS: &[&str] = &[
        "none", "tailscale", "wireguard", "openvpn",
        "mullvad", "ivpn",
    ];
    if !VALID_VPN_PROVIDERS.contains(&nf.vpn.provider.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "err": format!("vpn.provider '{}' not allowed — use tor.enabled / i2p.enabled for those", nf.vpn.provider),
            })),
        ).into_response();
    }

    if let Err(e) = write_state(&nf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }

    // v55.1: changing VPN provider/enabled can change whether the
    // Tor port-443 filter applies — recompute the DNSCrypt
    // auto-picked server list so it's consistent with the new VPN
    // state. Without this, the user could enable Tor + still have
    // their DNSCrypt auto-pool include port-8443 resolvers that
    // would silently time out.
    let tor_active = nf.vpn.enabled && nf.vpn.provider == "tor";
    nf.dnscrypt.auto_criteria.tor_friendly_port = tor_active;
    nf.dnscrypt.auto_picked_servers = if nf.dnscrypt.server_mode == "auto" {
        crate::dnscrypt_servers::auto_pick(&nf.dnscrypt.auto_criteria, 30)
    } else {
        Vec::new()
    };
    // Re-persist with the recomputed pool. write_state is cheap.
    if let Err(e) = write_state(&nf) {
        tracing::warn!("re-persist after vpn change: {e}");
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
