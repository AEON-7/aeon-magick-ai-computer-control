//! Lockdown + granular API/MCP exposure controls.
//!
//! LOCKDOWN MODE is the failsafe killswitch: flip it on and every external API
//! token + MCP call is refused — the Orb becomes a single-user jump box + KVM
//! that only the human admin session (and the local web UI) can drive. The
//! admin can always turn it back off; agents cannot.
//!
//! Short of the full killswitch, individual API categories (HID, vision,
//! network, files, MCP, OrbNet, …) can be disabled independently so you expose
//! exactly the surface you want.
//!
//! Enforcement lives in `auth::scope_allows`: admin short-circuits first, then
//! non-admin identities are checked against `is_blocked(path)`. State is cached
//! (2 s TTL) so the hot auth path never hits the disk twice in a row.

use crate::api::AppState;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CONFIG_TOML: &str = "/etc/aeon/lockdown.toml";

/// The toggleable API categories (path-prefix → category). The dashboard renders
/// one switch per category; `mcp` also gates the whole MCP surface.
pub const CATEGORIES: &[&str] = &[
    "hid", "vision", "macros", "network", "files", "hardware", "target", "orbnet", "mcp",
];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LockdownConfig {
    /// Full killswitch — refuse ALL non-admin API + MCP.
    #[serde(default)]
    pub enabled: bool,
    /// Categories disabled individually (subset of CATEGORIES).
    #[serde(default)]
    pub disabled_categories: Vec<String>,
}
impl Default for LockdownConfig {
    fn default() -> Self {
        Self { enabled: false, disabled_categories: vec![] }
    }
}

fn read_config() -> LockdownConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &LockdownConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

static CACHE: OnceLock<Mutex<(Instant, LockdownConfig)>> = OnceLock::new();

/// Current lockdown config, cached for 2 s so the per-request auth check is cheap.
pub fn current() -> LockdownConfig {
    let m = CACHE.get_or_init(|| Mutex::new((Instant::now(), read_config())));
    let mut g = m.lock().unwrap();
    if g.0.elapsed() > Duration::from_secs(2) {
        g.1 = read_config();
        g.0 = Instant::now();
    }
    g.1.clone()
}

fn invalidate(new: &LockdownConfig) {
    if let Some(m) = CACHE.get() {
        let mut g = m.lock().unwrap();
        g.1 = new.clone();
        g.0 = Instant::now();
    }
}

/// Map an API path to its toggle category, if any.
pub fn path_category(path: &str) -> Option<&'static str> {
    let p = path.strip_prefix("/api/")?;
    let head = p.split('/').next()?;
    Some(match head {
        "hid" => "hid",
        "streamer" => "vision",
        "state" | "snapshot" => "vision",
        "macros" => "macros",
        "network" | "dns" | "dnscrypt" => "network",
        "files" | "clipboard" => "files",
        "hardware" => "hardware",
        "target" => "target",
        "orbnet" => "orbnet",
        "mcp" => "mcp",
        _ => return None,
    })
}

/// Should this path be refused for a NON-admin caller right now? (Admin is
/// short-circuited before this is reached.)
pub fn is_blocked(path: &str) -> bool {
    let cfg = current();
    if cfg.enabled {
        return true; // full lockdown: kill all non-admin
    }
    match path_category(path) {
        Some(cat) => cfg.disabled_categories.iter().any(|c| c == cat),
        None => false,
    }
}

// ── handlers (admin-only via the auth deny-list) ─────────────────────────────

/// GET /api/lockdown — current lockdown + per-category exposure state.
pub async fn get_lockdown(State(_s): State<AppState>) -> Json<Value> {
    let cfg = current();
    Json(json!({
        "ok": true,
        "enabled": cfg.enabled,
        "disabled_categories": cfg.disabled_categories,
        "categories": CATEGORIES,
    }))
}

#[derive(Deserialize)]
pub struct SetReq {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub disabled_categories: Vec<String>,
}

/// POST /api/lockdown — set the killswitch + per-category toggles. Admin-only.
pub async fn set_lockdown(State(_s): State<AppState>, Json(req): Json<SetReq>) -> Json<Value> {
    let cfg = LockdownConfig {
        enabled: req.enabled,
        disabled_categories: req
            .disabled_categories
            .into_iter()
            .filter(|c| CATEGORIES.contains(&c.as_str()))
            .collect(),
    };
    match write_config(&cfg) {
        Ok(_) => {
            invalidate(&cfg);
            crate::audit::log(
                "admin",
                "lockdown_set",
                &format!("enabled={} disabled={:?}", cfg.enabled, cfg.disabled_categories),
                "ok",
                None,
            );
            Json(json!({"ok": true, "enabled": cfg.enabled, "disabled_categories": cfg.disabled_categories}))
        }
        Err(e) => Json(json!({"ok": false, "err": e.to_string()})),
    }
}
