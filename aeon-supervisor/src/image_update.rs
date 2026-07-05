//! Image version stamp + the "a newer Orb image is published" check.
//!
//! The flashed image stamps `/etc/aeon-image-version` (KEY=value) at build time —
//! deliberately OUTSIDE `/etc/aeon` so a config backup/restore can't carry a stale
//! version onto a freshly-flashed newer image (the config backup grabs all of
//! `/etc/aeon`; this file must reflect the IMAGE, not the restored config).
//!
//! We read that stamp, then compare the installed per-track version against a
//! manifest published in the GitHub repo (raw.githubusercontent.com — no auth, no
//! API rate limit, already a proven-reachable host in this codebase for DNS
//! blocklists) to decide whether to nudge the operator to grab a newer image from
//! Patreon. This is NOTIFY-ONLY — the Orb never self-flashes; the button just
//! guides a backup + opens the Patreon download page.

use crate::api::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

const STAMP_PATH: &str = "/etc/aeon-image-version";
const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/AEON-7/aeon-magick-ai-computer-control/main/image-manifest.json";
const FALLBACK_PATREON: &str = "https://www.patreon.com/AeonForge7";

/// Runtime hardware track from the board model (mirrors `fleet::pi_model()`).
fn board_track() -> &'static str {
    match std::fs::read_to_string("/proc/device-tree/model") {
        Ok(m) if m.contains("Raspberry Pi 5") => "pi5",
        Ok(m) if m.contains("Raspberry Pi 4") => "pi4",
        _ => "other",
    }
}

/// Parsed `/etc/aeon-image-version`. `version` is None on a legacy (unstamped)
/// image — in which case we don't compare (no false "up to date"/"behind" claim).
pub struct Stamp {
    pub version: Option<u64>,
    pub track: String,
    pub codename: Option<String>,
    pub built: Option<String>,
}

pub fn read_stamp() -> Stamp {
    let mut version = None;
    let mut track = None;
    let mut codename = None;
    let mut built = None;
    if let Ok(txt) = std::fs::read_to_string(STAMP_PATH) {
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').to_string();
                match k.trim() {
                    "version" => version = v.parse::<u64>().ok(),
                    "track" if !v.is_empty() => track = Some(v),
                    "codename" if !v.is_empty() => codename = Some(v),
                    "built" if !v.is_empty() => built = Some(v),
                    _ => {}
                }
            }
        }
    }
    // Track falls back to the live board model if the stamp lacks it (legacy).
    let track = track.unwrap_or_else(|| board_track().to_string());
    Stamp { version, track, codename, built }
}

/// The local stamp as JSON — folded into `system::info` so the /system page can
/// show "Image v112 (pi5 · trixie)" without an extra round trip.
pub fn stamp_fields() -> Value {
    let s = read_stamp();
    json!({
        "image_version": s.version,
        "track": s.track,
        "codename": s.codename,
        "image_built": s.built,
    })
}

/// Blocking GET → JSON, for the manifest fetch. Kept local (the codebase has four
/// bespoke ureq helpers already; this one carries the Orb UA + a short timeout).
fn http_get_json(url: &str) -> Result<Value, String> {
    let resp = ureq::get(url)
        .set("User-Agent", "aeon-magick-orb")
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(12))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            ureq::Error::Transport(t) => format!("network: {t}"),
        })?;
    resp.into_json::<Value>().map_err(|e| format!("bad manifest JSON: {e}"))
}

/// GET /api/system/image-updates — is a newer image published for THIS track?
/// Compares the stamped version against the repo manifest. `update_available` is
/// true only when BOTH numbers are known and the manifest's is strictly newer.
pub async fn image_updates(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| {
        let stamp = read_stamp();
        let installed = stamp.version;
        let track = stamp.track.clone();
        let manifest = match http_get_json(MANIFEST_URL) {
            Ok(m) => m,
            Err(e) => {
                return json!({
                    "ok": false,
                    "err": e,
                    "track": track,
                    "installed": installed,
                    "installed_known": installed.is_some(),
                    "update_available": false,
                    "patreon_url": FALLBACK_PATREON,
                })
            }
        };
        let top_patreon = manifest
            .get("patreon")
            .and_then(|v| v.as_str())
            .unwrap_or(FALLBACK_PATREON);
        let t = manifest.get("tracks").and_then(|m| m.get(&track));
        let latest = t.and_then(|x| x.get("version")).and_then(|v| v.as_u64());
        let latest_name = t.and_then(|x| x.get("name")).and_then(|v| v.as_str()).map(String::from);
        let published = t.and_then(|x| x.get("published")).and_then(|v| v.as_str()).map(String::from);
        let notes = t.and_then(|x| x.get("notes")).and_then(|v| v.as_str()).map(String::from);
        let patreon_url = t
            .and_then(|x| x.get("patreon_url"))
            .and_then(|v| v.as_str())
            .unwrap_or(top_patreon)
            .to_string();
        let update_available = matches!((installed, latest), (Some(i), Some(l)) if l > i);
        json!({
            "ok": true,
            "track": track,
            "installed": installed,                 // null on a legacy (unstamped) image
            "installed_known": installed.is_some(),
            "latest": latest,
            "latest_name": latest_name,
            "update_available": update_available,
            "published": published,
            "notes": notes,
            "patreon_url": patreon_url,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "image-updates task failed", "update_available": false}));
    Json(v)
}
