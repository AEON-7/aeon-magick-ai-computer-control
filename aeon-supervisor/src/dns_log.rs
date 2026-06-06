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
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory we keep refreshed source lists in. One file per source, named
/// `<id>.list`, one domain per line (sorted + deduped at write time).
const SOURCES_DIR: &str = "/var/lib/aeon/dns-sources";

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
    /// Upstream subscription sources (StevenBlack, Ultimate Hosts, OISD, …).
    /// Cached to /var/lib/aeon/dns-sources/<id>.list; merged into the final
    /// dnsmasq drop-in alongside manual domains + regex-extracted hosts.
    #[serde(default)]
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Source {
    /// Random short id — stable across edits so cache files don't churn.
    pub id: String,
    pub name: String,
    pub url: String,
    /// "hosts" → `0.0.0.0 domain.com` lines; strip the IP. (StevenBlack,
    /// Ultimate Hosts.)
    /// "domains" → one bare domain per line. (OISD, hagezi domain-only.)
    /// "adblock" → AdBlock Plus syntax `||domain.com^`. (Most ad-block lists.)
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_refresh_hours")]
    pub refresh_hours: u64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub last_fetched_ms: u64,
    #[serde(default)]
    pub last_attempt_ms: u64,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub entry_count: u64,
    #[serde(default)]
    pub sha256: String,
}

fn default_format() -> String { "hosts".to_string() }
fn default_refresh_hours() -> u64 { 24 }
fn default_enabled() -> bool { true }

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// Render the dnsmasq drop-in from the blacklist + all enabled source
/// caches. Dedupe across sources so a domain appearing in 4 different
/// lists only emits one `address=` line. dnsmasq's `address=/foo/IP`
/// matches `foo` AND `*.foo`, so we don't have to expand subdomains.
fn render_blacklist_conf(b: &Blacklist) -> String {
    // Use BTreeSet so output is sorted + deduped. For million-entry
    // ad-block lists this is the bottleneck; on a Pi 4 it's still
    // under a second.
    let mut all: BTreeSet<String> = BTreeSet::new();
    for d in &b.domains {
        let d = d.trim().to_lowercase();
        if safe_domain(&d) {
            all.insert(d);
        }
    }
    // Pull each enabled source's cached list.
    for s in &b.sources {
        if !s.enabled {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(source_cache_path(&s.id)) {
            for line in text.lines() {
                let d = line.trim().to_lowercase();
                if safe_domain(&d) {
                    all.insert(d);
                }
            }
        }
    }
    // Regex literals.
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
                    let trimmed = current.trim_matches('.').to_lowercase();
                    if safe_domain(&trimmed) {
                        all.insert(trimmed);
                    }
                }
                current.clear();
            }
        }
        if current.len() >= 3 && current.contains('.') {
            let trimmed = current.trim_matches('.').to_lowercase();
            if safe_domain(&trimmed) {
                all.insert(trimmed);
            }
        }
    }

    let mut out = String::with_capacity(all.len() * 32);
    out.push_str(
        "# Managed by aeon-supervisor — do not edit by hand.\n\
         # Edit via /api/dns/blacklist and /api/dns/sources.\n\n",
    );
    for d in &all {
        out.push_str("address=/");
        out.push_str(d);
        out.push_str("/0.0.0.0\n");
    }
    out
}

fn source_cache_path(id: &str) -> String {
    format!("{SOURCES_DIR}/{id}.list")
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

// ── Subscription sources ─────────────────────────────────────────────

/// Curated presets the UI offers as one-click subscribes. Trusted, well-
/// maintained lists; users can paste any other URL via the custom add
/// form. Keep this list short — we want recognizable names + diverse
/// scope (everything from "tiny + lightweight" to "the kitchen sink").
pub fn curated_presets() -> Value {
    json!([
        {
            "name": "StevenBlack — unified hosts",
            "url": "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts",
            "format": "hosts",
            "blurb": "The de-facto starter list. Ads + trackers + malware. ~150k entries. Updated weekly.",
            "category": "general"
        },
        {
            "name": "Ultimate Hosts Blacklist",
            "url": "https://hosts.ubuntu101.co.za/hosts",
            "format": "hosts",
            "blurb": "Aggregated from 100+ source lists via the maintainer's published mirror (consolidated, daily-rebuilt). Very thorough (~1.4M). Heavier RAM footprint.",
            "category": "comprehensive"
        },
        {
            "name": "OISD — full",
            "url": "https://big.oisd.nl/domainswild",
            "format": "domains",
            "blurb": "Curated mega-list with low false positives. ~200k. The community favorite.",
            "category": "general"
        },
        {
            "name": "hagezi — pro",
            "url": "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/wildcard/pro.txt",
            "format": "domains",
            "blurb": "Multi-tier collection, 'pro' tier balances coverage vs false positives. ~250k.",
            "category": "general"
        },
        {
            "name": "1Hosts — Lite",
            "url": "https://raw.githubusercontent.com/badmojr/1Hosts/master/Lite/domains.txt",
            "format": "domains",
            "blurb": "Minimal-breakage ad + tracker list (~70k). Good for performance-constrained setups.",
            "category": "lite"
        },
        {
            "name": "Phishing Army — extended",
            "url": "https://phishing.army/download/phishing_army_blocklist_extended.txt",
            "format": "domains",
            "blurb": "Phishing-only feed updated daily. ~30k. Safe to combine with any general list.",
            "category": "security"
        },
        {
            "name": "AdGuard — DNS filter",
            "url": "https://adguardteam.github.io/AdGuardSDNSFilter/Filters/filter.txt",
            "format": "adblock",
            "blurb": "AdGuard's own DNS filter in Adblock-Plus syntax, via their GitHub Pages distribution. ~70k. Modest, well-maintained.",
            "category": "general"
        },
        {
            "name": "NoCoin — crypto-jacking",
            "url": "https://raw.githubusercontent.com/hoshsadiq/adblock-nocoin-list/master/hosts.txt",
            "format": "hosts",
            "blurb": "Just blocks in-browser cryptocurrency miners. ~1k. Light + targeted.",
            "category": "security"
        },
    ])
}

/// GET /api/dns/sources
pub async fn list_sources(State(_state): State<AppState>) -> Json<Value> {
    let b = read_blacklist();
    let sources: Vec<Value> = b.sources.iter().map(|s| {
        json!({
            "id": s.id,
            "name": s.name,
            "url": s.url,
            "format": s.format,
            "refresh_hours": s.refresh_hours,
            "enabled": s.enabled,
            "last_fetched_ms": s.last_fetched_ms,
            "last_attempt_ms": s.last_attempt_ms,
            "last_error": s.last_error,
            "entry_count": s.entry_count,
            "sha256": s.sha256,
            // Useful UX hint: is this list stale and due for refresh?
            "stale": is_stale(s),
        })
    }).collect();
    Json(json!({
        "ok": true,
        "sources": sources,
        "presets": curated_presets(),
    }))
}

fn is_stale(s: &Source) -> bool {
    if !s.enabled || s.last_fetched_ms == 0 { return true; }
    let interval_ms = s.refresh_hours.saturating_mul(60 * 60 * 1000);
    now_ms().saturating_sub(s.last_fetched_ms) >= interval_ms
}

#[derive(Deserialize)]
pub struct SourceAddReq {
    pub name: String,
    pub url: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_refresh_hours")]
    pub refresh_hours: u64,
}

fn gen_source_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}", now_ms().wrapping_add(n))
}

fn safe_source_url(u: &str) -> bool {
    let u = u.trim();
    (u.starts_with("https://") || u.starts_with("http://"))
        && u.len() <= 512
        && !u.contains(' ')
        && !u.contains('\n')
        && !u.contains('\r')
}

const VALID_FORMATS: &[&str] = &["hosts", "domains", "adblock"];

/// POST /api/dns/sources — add a subscription source.
pub async fn add_source(
    State(state): State<AppState>,
    Json(req): Json<SourceAddReq>,
) -> impl IntoResponse {
    if !safe_source_url(&req.url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "url must be http:// or https://"})),
        ).into_response();
    }
    if !VALID_FORMATS.contains(&req.format.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": format!("format must be one of {VALID_FORMATS:?}")})),
        ).into_response();
    }
    if req.refresh_hours == 0 || req.refresh_hours > 24 * 30 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "refresh_hours must be 1..720"})),
        ).into_response();
    }
    let cleaned_name: String = req
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let mut b = read_blacklist();
    let id = gen_source_id();
    b.sources.push(Source {
        id: id.clone(),
        name: if cleaned_name.is_empty() {
            req.url.clone()
        } else {
            cleaned_name
        },
        url: req.url.trim().to_string(),
        format: req.format,
        refresh_hours: req.refresh_hours,
        enabled: true,
        last_fetched_ms: 0,
        last_attempt_ms: 0,
        last_error: String::new(),
        entry_count: 0,
        sha256: String::new(),
    });
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        ).into_response();
    }
    // Kick an immediate refresh — the user just added it, they want
    // to see it populate. Done off-thread so the response returns quickly.
    let id_clone = id.clone();
    tokio::spawn(async move {
        let _ = refresh_one(&id_clone).await;
    });
    let _ = state; // unused but keeps signature stable
    Json(json!({"ok": true, "id": id})).into_response()
}

#[derive(Deserialize)]
pub struct SourceUpdateReq {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub refresh_hours: Option<u64>,
}

/// PUT /api/dns/sources/:id — toggle / change refresh interval.
pub async fn update_source(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SourceUpdateReq>,
) -> impl IntoResponse {
    let mut b = read_blacklist();
    let Some(s) = b.sources.iter_mut().find(|s| s.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "err": "no such source"})),
        ).into_response();
    };
    if let Some(v) = req.enabled {
        s.enabled = v;
    }
    if let Some(v) = req.refresh_hours {
        if v == 0 || v > 24 * 30 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "refresh_hours must be 1..720"})),
            ).into_response();
        }
        s.refresh_hours = v;
    }
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        ).into_response();
    }
    apply_blacklist(&b);
    Json(json!({"ok": true})).into_response()
}

/// DELETE /api/dns/sources/:id — remove subscription + its cache file.
pub async fn delete_source(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut b = read_blacklist();
    let before = b.sources.len();
    b.sources.retain(|s| s.id != id);
    if b.sources.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "err": "no such source"})),
        ).into_response();
    }
    let _ = std::fs::remove_file(source_cache_path(&id));
    if let Err(e) = write_blacklist(&b) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        ).into_response();
    }
    apply_blacklist(&b);
    Json(json!({"ok": true})).into_response()
}

/// POST /api/dns/sources/:id/refresh — force an immediate refresh.
pub async fn refresh_source(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match refresh_one(&id).await {
        Ok((count, sha)) => Json(json!({
            "ok": true,
            "entry_count": count,
            "sha256": sha,
        })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e})),
        ).into_response(),
    }
}

/// Fetch one source and rewrite its cache. Returns (entry_count, sha256).
/// Called both from the explicit refresh endpoint and from the periodic
/// background task in main.rs.
pub async fn refresh_one(id: &str) -> Result<(u64, String), String> {
    let mut b = read_blacklist();
    let Some(s) = b.sources.iter().find(|s| s.id == id).cloned() else {
        return Err(format!("no such source: {id}"));
    };
    // Mark attempt timestamp regardless of outcome so the UI sees we tried.
    let now = now_ms();
    if let Some(rec) = b.sources.iter_mut().find(|x| x.id == id) {
        rec.last_attempt_ms = now;
    }
    let _ = write_blacklist(&b);

    // Shell out to curl — handles HTTPS / redirects / gzip transparently
    // and is already on the image. Spawn on blocking pool so the axum
    // worker isn't tied up while we download.
    let url = s.url.clone();
    let raw_result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .args([
                "-fsSL",                  // fail on 4xx/5xx, follow redirects, silent
                "--retry", "2",
                "--max-time", "60",
                "--max-filesize", "268435456", // 256MB hard cap (blacklists are <50MB) — a
                                               // streaming/oversized URL would otherwise read
                                               // unbounded into memory and OOM the supervisor
                "--compressed",            // accept gzip
                "-A", "aeon-magick/0.1",  // some hosts (GitHub) require UA
                &url,
            ])
            .output()
    }).await;

    let output = match raw_result {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => {
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            return record_failure(id, format!("curl exit {:?}: {err}", o.status.code()));
        }
        Ok(Err(e)) => return record_failure(id, format!("curl spawn: {e}")),
        Err(e) => return record_failure(id, format!("task join: {e}")),
    };

    // Guard the streaming / no-Content-Length case that --max-filesize can't
    // catch mid-download: refuse to parse an oversized blob rather than let it
    // (and from_utf8_lossy's copy) balloon the supervisor's heap.
    if output.stdout.len() > 256 * 1024 * 1024 {
        return record_failure(id, format!("blacklist too large: {} bytes", output.stdout.len()));
    }

    // Parse format → set of normalized domains.
    let text = String::from_utf8_lossy(&output.stdout);
    let domains = parse_list(&text, &s.format);

    // Compute sha256 of the canonical (sorted-dedup) joined output.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut sorted: Vec<&String> = domains.iter().collect();
    sorted.sort();
    for d in &sorted {
        hasher.update(d.as_bytes());
        hasher.update(b"\n");
    }
    let sha = hex::encode(hasher.finalize());

    // Write cache file atomically.
    let _ = std::fs::create_dir_all(SOURCES_DIR);
    let path = source_cache_path(id);
    let tmp = format!("{path}.tmp");
    let mut buf = String::with_capacity(domains.len() * 24);
    for d in &sorted {
        buf.push_str(d);
        buf.push('\n');
    }
    if let Err(e) = std::fs::write(&tmp, &buf) {
        return record_failure(id, format!("write tmp: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        return record_failure(id, format!("rename: {e}"));
    }

    let count = domains.len() as u64;

    // Record success in the source row.
    let mut b = read_blacklist();
    if let Some(rec) = b.sources.iter_mut().find(|x| x.id == id) {
        rec.last_fetched_ms = now_ms();
        rec.last_error = String::new();
        rec.entry_count = count;
        rec.sha256 = sha.clone();
    }
    let _ = write_blacklist(&b);
    // Regenerate the dnsmasq drop-in so the new entries take effect.
    apply_blacklist(&b);
    Ok((count, sha))
}

fn record_failure(id: &str, err: String) -> Result<(u64, String), String> {
    let mut b = read_blacklist();
    if let Some(rec) = b.sources.iter_mut().find(|x| x.id == id) {
        rec.last_error = err.chars().take(200).collect();
    }
    let _ = write_blacklist(&b);
    Err(err)
}

/// Parse a fetched blob into normalized domains. Robust enough to swallow
/// blank lines, comments (`#` or `!` for adblock), IP-prefix host lines,
/// and trailing comments.
fn parse_list(text: &str, format: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for raw in text.lines() {
        // Strip trailing comments.
        let line = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Adblock metadata lines start with ! or [
        if line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        let domain_opt: Option<String> = match format {
            "hosts" => {
                // "0.0.0.0 evil.com" or "127.0.0.1 evil.com"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 2 {
                    None
                } else {
                    // last token is the domain in well-formed hosts files
                    Some(parts.last().unwrap().to_lowercase())
                }
            }
            "adblock" => {
                // "||domain.com^"  or  "||domain.com^$third-party"
                let s = line.trim_start_matches("||");
                let s = s.split('^').next().unwrap_or("");
                let s = s.split('$').next().unwrap_or("");
                if s.is_empty() || s.contains('/') || s.contains('*') {
                    None
                } else {
                    Some(s.to_lowercase())
                }
            }
            _ /* "domains" + fallback */ => {
                // Some "domain" lists still include a leading wildcard
                // (e.g. OISD's `domainswild` ships `domain.com` per line
                // already, but defensive trim covers the variant).
                let s = line
                    .trim_start_matches("*.")
                    .trim_start_matches(".");
                if s.contains(char::is_whitespace) {
                    None
                } else {
                    Some(s.to_lowercase())
                }
            }
        };
        if let Some(d) = domain_opt {
            // Filter localhost / broadcast / "this-host" placeholders
            // common in hosts-files.
            if d == "localhost"
                || d == "localhost.localdomain"
                || d == "broadcasthost"
                || d == "ip6-localhost"
                || d == "ip6-loopback"
                || d == "0.0.0.0"
            {
                continue;
            }
            if safe_domain(&d) {
                out.insert(d);
            }
        }
    }
    out
}

/// Background task: every 5 minutes, walk sources, refresh any whose
/// `last_fetched_ms + refresh_hours` is in the past. Called once at
/// startup by main.rs.
pub async fn run_refresh_loop() {
    // Initial settle delay so the first tick doesn't fire during boot
    // when networking is still coming up.
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    loop {
        let b = read_blacklist();
        let to_refresh: Vec<String> = b
            .sources
            .iter()
            .filter(|s| s.enabled && is_stale(s))
            .map(|s| s.id.clone())
            .collect();
        for id in to_refresh {
            // Serialize fetches — these can be big (1M+ entries) and the
            // Pi has limited RAM; don't parallelize.
            let _ = refresh_one(&id).await;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5 * 60)).await;
    }
}
