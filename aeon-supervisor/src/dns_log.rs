//! DNS query log + blacklist management.
//!
//! Blacklist is persisted to /etc/aeon/dns-blacklist.toml and rendered
//! to /etc/dnsmasq.d/aeon-blacklist.conf as `address=/<domain>/0.0.0.0`
//! lines (which makes dnsmasq return 0.0.0.0 — effectively a block
//! since browsers can't connect there). Regex patterns are expanded to
//! a sorted/deduplicated wildcard list dnsmasq DOES understand
//! (`address=/.example.com/...` matches all subdomains).
//!
//! Activity log: when enabled, we write a `log-queries` drop-in into
//! dnsmasq.d/ and read recent queries via `journalctl -t dnsmasq -n N`.
//! Returns last-N entries on GET.

use crate::api::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;

/// Parse "YYYY-MM-DDTHH:MM:SS±HHMM" → epoch millis. Returns None on
/// anything we can't decode. Naïve but dependency-free: we treat the
/// timestamp as UTC if no offset is parseable. Good enough for sort
/// order in the UI; not used for anything safety-critical.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    // "2026-05-27T15:00:00"  ← bare
    // "2026-05-27T15:00:00-0700"
    // "2026-05-27T15:00:00+02:00"
    let core = s.get(0..19)?;
    let mut it = core.split(|c| c == '-' || c == 'T' || c == ':');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let h: i64 = it.next()?.parse().ok()?;
    let mi: i64 = it.next()?.parse().ok()?;
    let se: i64 = it.next()?.parse().ok()?;
    // Days from civil date — Howard Hinnant's algorithm.
    let y = y - if mo <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let unix_secs = days * 86_400 + h * 3600 + mi * 60 + se;
    // Apply offset if present
    let mut offset_min: i64 = 0;
    if s.len() >= 24 {
        let off = &s[19..];
        let sign: i64 = if off.starts_with('+') { -1 }
                        else if off.starts_with('-') { 1 }
                        else { 0 };
        if sign != 0 {
            let off = &off[1..];
            // accept "HHMM" or "HH:MM"
            let cleaned: String = off.chars().filter(|c| c.is_ascii_digit()).collect();
            if cleaned.len() >= 4 {
                let oh: i64 = cleaned[..2].parse().unwrap_or(0);
                let om: i64 = cleaned[2..4].parse().unwrap_or(0);
                offset_min = sign * (oh * 60 + om);
            }
        }
    }
    Some((unix_secs + offset_min * 60) * 1000)
}

const BLACKLIST_TOML: &str = "/etc/aeon/dns-blacklist.toml";
const DNSMASQ_BLACKLIST_CONF: &str = "/etc/dnsmasq.d/aeon-blacklist.conf";
const DNSMASQ_LOGGING_CONF: &str = "/etc/dnsmasq.d/aeon-logging.conf";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Blacklist {
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub regexes: Vec<String>,
    #[serde(default)]
    pub log_enabled: bool,
}

fn read_blacklist() -> Blacklist {
    std::fs::read_to_string(BLACKLIST_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_blacklist(b: &Blacklist) -> std::io::Result<()> {
    let text = toml::to_string_pretty(b)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(parent) = std::path::Path::new(BLACKLIST_TOML).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = format!("{BLACKLIST_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, BLACKLIST_TOML)?;
    Ok(())
}

/// Validate a domain — alphanumeric + dash + dot, no schemes / paths.
fn safe_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= 253
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Render the dnsmasq drop-in from the blacklist. dnsmasq supports a
/// dot-prefix wildcard: `address=/.evil.example/0.0.0.0` blocks all
/// subdomains of evil.example. We map exact entries verbatim and
/// regex entries → best-effort wildcards (anything before the first
/// literal alphanumeric run is treated as the dotted root).
fn render_blacklist_conf(b: &Blacklist) -> String {
    let mut out = String::from(
        "# Managed by aeon-supervisor — do not edit by hand.\n\
         # Edit via PUT /api/dns/blacklist.\n\n",
    );
    for d in &b.domains {
        if !safe_domain(d) {
            continue;
        }
        // `address=/foo.com/0.0.0.0` matches foo.com AND *.foo.com.
        out.push_str(&format!("address=/{}/0.0.0.0\n", d));
    }
    for r in &b.regexes {
        // dnsmasq doesn't speak regex. Heuristic: extract literal
        // domain-like substrings of length >= 3 and treat each as a
        // wildcard root. This catches common patterns like
        // `.*\.adsfor\.us` → blocks `*.adsfor.us`. Anything more
        // complex requires a real regex DNS proxy (out of scope).
        let mut current = String::new();
        for c in r.chars() {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                current.push(c);
            } else {
                if current.len() >= 3 && current.contains('.') {
                    let trimmed = current.trim_matches('.');
                    if safe_domain(trimmed) {
                        out.push_str(&format!("address=/{}/0.0.0.0\n", trimmed));
                    }
                }
                current.clear();
            }
        }
        if current.len() >= 3 && current.contains('.') {
            let trimmed = current.trim_matches('.');
            if safe_domain(trimmed) {
                out.push_str(&format!("address=/{}/0.0.0.0\n", trimmed));
            }
        }
    }
    out
}

fn apply_blacklist(b: &Blacklist) {
    let conf = render_blacklist_conf(b);
    let _ = std::fs::write(DNSMASQ_BLACKLIST_CONF, conf);
    // Toggle query-logging drop-in.
    if b.log_enabled {
        let _ = std::fs::write(
            DNSMASQ_LOGGING_CONF,
            "# Managed by aeon-supervisor.\nlog-queries\n",
        );
    } else {
        let _ = std::fs::remove_file(DNSMASQ_LOGGING_CONF);
    }
    // Restart NetworkManager-managed dnsmasq via nmcli — the shared
    // dnsmasq picks up drop-ins on connection re-up.
    let _ = Command::new("nmcli")
        .args(["con", "up", "aeon-usb0"])
        .status();
}

/// GET /api/dns/blacklist
pub async fn get_blacklist(State(_state): State<AppState>) -> Json<Value> {
    let b = read_blacklist();
    Json(json!({
        "ok": true,
        "blacklist": {
            "domains": b.domains,
            "regexes": b.regexes,
        },
    }))
}

#[derive(Deserialize)]
pub struct BlacklistPutReq {
    #[serde(default)]
    pub domains: Option<Vec<String>>,
    #[serde(default)]
    pub regexes: Option<Vec<String>>,
}

/// PUT /api/dns/blacklist
pub async fn put_blacklist(
    State(_state): State<AppState>,
    Json(req): Json<BlacklistPutReq>,
) -> impl IntoResponse {
    let mut b = read_blacklist();
    if let Some(ds) = req.domains {
        let mut dedup: Vec<String> = ds
            .into_iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| safe_domain(s))
            .collect();
        dedup.sort();
        dedup.dedup();
        b.domains = dedup;
    }
    if let Some(rs) = req.regexes {
        let cleaned: Vec<String> = rs
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .collect();
        b.regexes = cleaned;
    }
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    apply_blacklist(&b);
    Json(json!({"ok": true, "domain_count": b.domains.len()})).into_response()
}

#[derive(Deserialize)]
pub struct CsvImportReq {
    pub csv: String,
}

/// POST /api/dns/blacklist/import — bulk-add from CSV (one domain per
/// line, or comma-separated). Domains-only — regexes still go via PUT.
pub async fn import_csv(
    State(_state): State<AppState>,
    Json(req): Json<CsvImportReq>,
) -> impl IntoResponse {
    let mut b = read_blacklist();
    let before = b.domains.len();
    for line in req.csv.lines() {
        for tok in line.split(',') {
            let d = tok.trim().trim_start_matches("0.0.0.0").trim().to_lowercase();
            // pi-hole / hosts-style "0.0.0.0 evil.com" rows: strip
            // anything before the last whitespace-separated token.
            let d = d
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string();
            if safe_domain(&d) && !b.domains.contains(&d) {
                b.domains.push(d);
            }
        }
    }
    b.domains.sort();
    b.domains.dedup();
    let added = b.domains.len() - before;
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    apply_blacklist(&b);
    Json(json!({"ok": true, "added": added})).into_response()
}

#[derive(Deserialize)]
pub struct LogToggleReq {
    pub enabled: bool,
}

/// PUT /api/dns/log — turn query logging on/off.
pub async fn put_log(
    State(_state): State<AppState>,
    Json(req): Json<LogToggleReq>,
) -> impl IntoResponse {
    let mut b = read_blacklist();
    b.log_enabled = req.enabled;
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    apply_blacklist(&b);
    Json(json!({"ok": true, "enabled": req.enabled})).into_response()
}

/// GET /api/dns/log — last 500 dnsmasq queries via journalctl. Light
/// on the Pi (one journalctl invocation per request; we don't
/// continuously tail).
pub async fn get_log(State(_state): State<AppState>) -> Json<Value> {
    let b = read_blacklist();
    let mut entries: Vec<Value> = Vec::new();
    let mut blocked_total = 0u64;
    let mut allowed_total = 0u64;
    if b.log_enabled {
        let out = Command::new("journalctl")
            .args([
                "-t", "dnsmasq",
                "-n", "500",
                "--no-pager",
                "-o", "short-iso",
            ])
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines().rev() {
                // Format: "2026-05-27T15:00:00 hostname dnsmasq[pid]: query[A] foo.com from 10.55.0.10"
                let Some(content) = line.split("dnsmasq[").nth(1) else { continue };
                let Some(rest) = content.split_once(": ") else { continue };
                let msg = rest.1;
                // Only count actual queries; ignore "forwarded …" etc.
                if !msg.starts_with("query[") {
                    continue;
                }
                let body = &msg["query[".len()..];
                let Some(close) = body.find(']') else { continue };
                let qtype = &body[..close];
                let after_qtype = &body[close + 1..].trim_start();
                let mut parts = after_qtype.split_whitespace();
                let domain = parts.next().unwrap_or("").to_string();
                let _ = parts.next(); // "from"
                let client = parts.next().unwrap_or("").to_string();
                // Match against blacklist for the action label.
                let action = if b.domains.iter().any(|d| {
                    domain == *d || domain.ends_with(&format!(".{}", d))
                }) {
                    blocked_total += 1;
                    "block"
                } else {
                    allowed_total += 1;
                    "allow"
                };
                // Parse the iso timestamp into millis. Be tolerant —
                // journalctl emits "YYYY-MM-DDTHH:MM:SS±HHMM". We
                // extract just the date+time without bringing in a
                // full chrono dep.
                let ts_ms = line
                    .split_whitespace()
                    .next()
                    .and_then(parse_iso8601_ms)
                    .unwrap_or(0);
                entries.push(json!({
                    "ts_ms": ts_ms,
                    "client": client,
                    "domain": domain,
                    "qtype": qtype,
                    "action": action,
                    "source": "dnsmasq",
                }));
                if entries.len() >= 500 {
                    break;
                }
            }
        }
    }
    Json(json!({
        "ok": true,
        "enabled": b.log_enabled,
        "entries": entries,
        "blocked_total": blocked_total,
        "allowed_total": allowed_total,
    }))
}
