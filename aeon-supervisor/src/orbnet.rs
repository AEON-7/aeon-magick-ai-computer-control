//! OrbNet — opt-in anonymous Matrix federation between Aeon Magick Orbs over Tor.
//!
//! The heavy infra lifting (Tor onion + Conduit homeserver + self-signed cert)
//! lives in the `aeon-orbnet` shell script; this module is the control plane:
//! it owns `/etc/aeon/orbnet.toml`, drives the script (enable/disable), provisions
//! the Orb owner's Matrix account, and exposes digested endpoints the dashboard
//! polls (status, rooms, activity, send, groups/DMs, personas, moderation).
//!
//! Anonymity model: the homeserver is reachable only as a `.onion` (no port
//! forward, no exposed IP); federation egresses through a dedicated Tor SOCKS;
//! the `.onion` authenticates peers (patched Conduit accepts self-signed certs,
//! so there is no shared CA — fully decentralized).
//!
//! Talking to the local Conduit is done by shelling out to `curl -sk` (the
//! supervisor has no reqwest; curl is on the image and `-k` accepts the
//! throwaway self-signed cert on 127.0.0.1).

use crate::api::AppState;
use axum::extract::{Path as AxPath, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;

const CONFIG_TOML: &str = "/etc/aeon/orbnet.toml";
const RUNTIME_DIR: &str = "/var/lib/aeon/orbnet";
const OWNER_JSON: &str = "/var/lib/aeon/orbnet/owner.json";
const SCRIPT: &str = "/usr/local/bin/aeon-orbnet";
const CS_BASE: &str = "https://127.0.0.1:8448";

// ── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrbNetConfig {
    /// Off by default. When on, the homeserver + onion run and the Orb joins
    /// OrbNet.
    #[serde(default)]
    pub enabled: bool,
    /// The Orb owner's Matrix localpart (e.g. "aurora" → @aurora:<onion>).
    /// Pseudonymous; defaults to a random handle so it isn't identifying.
    #[serde(default)]
    pub handle: String,
    /// Friendly display name shown in chats (separate from the handle).
    #[serde(default)]
    pub display_name: String,
    /// Auto-join the OrbNet community Space + interest rooms on enable.
    #[serde(default = "default_true")]
    pub auto_join_community: bool,
    /// Seed directory onions used to discover other Orbs + the community.
    #[serde(default)]
    pub directory_seeds: Vec<String>,
    /// Per-user moderation: messages matching any keyword/phrase are hidden in
    /// the dashboard (case-insensitive substring match). The owner's own filter.
    #[serde(default)]
    pub moderation_keywords: Vec<String>,
}
fn default_true() -> bool {
    true
}

impl Default for OrbNetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            handle: String::new(),
            display_name: String::new(),
            auto_join_community: true,
            directory_seeds: vec![],
            moderation_keywords: vec![],
        }
    }
}

fn read_config() -> OrbNetConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &OrbNetConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn rand_hex(bytes: usize) -> String {
    std::fs::read("/dev/urandom")
        .ok()
        .map(|b| b.into_iter().take(bytes).map(|x| format!("{x:02x}")).collect())
        .unwrap_or_else(|| "0".repeat(bytes * 2))
}

/// A safe pseudonymous localpart: lowercase ascii alnum + `_.-`, ≤ 40 chars.
fn sanitize_handle(s: &str) -> String {
    let h: String = s
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .take(40)
        .collect();
    if h.is_empty() {
        format!("orb-{}", &rand_hex(3))
    } else {
        h
    }
}

/// Run the `aeon-orbnet` infra script and return trimmed stdout.
fn run_script(args: &[&str]) -> Result<String, String> {
    let out = Command::new(SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{SCRIPT} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn script_status() -> Value {
    run_script(&["status"])
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"onion": "", "homeserver_up": false}))
}

/// Call the local Conduit client-server API. `curl -sk` accepts the self-signed
/// localhost cert; body is JSON.
fn cs_curl(method: &str, path: &str, token: Option<&str>, body: Option<&str>) -> Result<Value, String> {
    let url = format!("{CS_BASE}{path}");
    let mut cmd = Command::new("curl");
    cmd.args(["-sk", "--max-time", "60", "-X", method]);
    if let Some(t) = token {
        cmd.arg("-H").arg(format!("Authorization: Bearer {t}"));
    }
    if let Some(b) = body {
        cmd.arg("-H").arg("Content-Type: application/json").arg("-d").arg(b);
    }
    cmd.arg(&url);
    let out = cmd.output().map_err(|e| format!("curl: {e}"))?;
    let raw = String::from_utf8_lossy(&out.stdout);
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|e| format!("parse {path}: {e}; raw={}", raw.chars().take(200).collect::<String>()))
}

// ── owner account ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Owner {
    user_id: String,
    access_token: String,
    #[serde(default)]
    password: String,
}

fn read_owner() -> Option<Owner> {
    std::fs::read_to_string(OWNER_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn write_owner(o: &Owner) -> std::io::Result<()> {
    std::fs::create_dir_all(RUNTIME_DIR)?;
    let tmp = format!("{OWNER_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(o).unwrap_or_default())?;
    std::fs::rename(tmp, OWNER_JSON)
}

/// The owner access token, if the account is provisioned + the token still
/// validates against the running homeserver.
fn valid_owner_token() -> Option<Owner> {
    let o = read_owner()?;
    let who = cs_curl(
        "GET",
        "/_matrix/client/v3/account/whoami",
        Some(&o.access_token),
        None,
    )
    .ok()?;
    if who.get("user_id").is_some() {
        Some(o)
    } else {
        None
    }
}

/// Register (or re-use) the Orb owner's Matrix account on the local homeserver.
/// Uses the registration token the infra script minted (UIA token flow).
fn provision_owner(handle: &str) -> Result<Owner, String> {
    if let Some(o) = valid_owner_token() {
        return Ok(o);
    }
    let reg_token = run_script(&["reg-token"])?;
    if reg_token.is_empty() {
        return Err("no registration token (is OrbNet up?)".into());
    }
    let password = read_owner().map(|o| o.password).filter(|p| !p.is_empty()).unwrap_or_else(|| rand_hex(16));
    let reg_path = "/_matrix/client/v3/register?kind=user";

    // step 1 — obtain a UIA session
    let r1 = cs_curl("POST", reg_path, None, Some(&json!({"username": handle, "password": password}).to_string()))?;
    if let Some(tok) = r1.get("access_token").and_then(|v| v.as_str()) {
        let o = Owner { user_id: r1["user_id"].as_str().unwrap_or_default().to_string(), access_token: tok.to_string(), password };
        write_owner(&o).map_err(|e| e.to_string())?;
        return Ok(o);
    }
    if r1.get("errcode").and_then(|v| v.as_str()) == Some("M_USER_IN_USE") {
        // account already exists from a prior enable — log in with the stored pw
        let login = cs_curl("POST", "/_matrix/client/v3/login", None, Some(&json!({
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": handle},
            "password": password,
        }).to_string()))?;
        if let Some(tok) = login.get("access_token").and_then(|v| v.as_str()) {
            let o = Owner { user_id: login["user_id"].as_str().unwrap_or_default().to_string(), access_token: tok.to_string(), password };
            write_owner(&o).map_err(|e| e.to_string())?;
            return Ok(o);
        }
        return Err(format!("owner exists but login failed: {login}"));
    }
    let session = r1.get("session").and_then(|v| v.as_str()).ok_or_else(|| format!("register: no session: {r1}"))?;

    // step 2 — complete with the registration token
    let auth = json!({"type": "m.login.registration_token", "token": reg_token, "session": session});
    let r2 = cs_curl("POST", reg_path, None, Some(&json!({"username": handle, "password": password, "auth": auth}).to_string()))?;
    let tok = r2.get("access_token").and_then(|v| v.as_str()).ok_or_else(|| format!("register step2: {r2}"))?;
    let o = Owner { user_id: r2["user_id"].as_str().unwrap_or_default().to_string(), access_token: tok.to_string(), password };
    write_owner(&o).map_err(|e| e.to_string())?;

    // set the friendly display name if configured
    let cfg = read_config();
    if !cfg.display_name.is_empty() {
        let _ = cs_curl(
            "PUT",
            &format!("/_matrix/client/v3/profile/{}/displayname", o.user_id),
            Some(&o.access_token),
            Some(&json!({"displayname": cfg.display_name}).to_string()),
        );
    }
    Ok(o)
}

// ── handlers ─────────────────────────────────────────────────────────────────

/// GET /api/orbnet/status — OrbNet on/off + onion + homeserver/owner health.
/// Admin-gated.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let cfg = read_config();
    let v = tokio::task::spawn_blocking(move || {
        let st = script_status();
        let owner = read_owner();
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "onion": st.get("onion").cloned().unwrap_or(json!("")),
            "homeserver_up": st.get("homeserver_up").cloned().unwrap_or(json!(false)),
            "tor": st.get("tor").cloned().unwrap_or(json!("inactive")),
            "conduit": st.get("conduit").cloned().unwrap_or(json!("inactive")),
            "owner": owner.as_ref().map(|o| o.user_id.clone()),
            "display_name": cfg.display_name,
            "handle": cfg.handle,
            "auto_join_community": cfg.auto_join_community,
            "moderation_keywords": cfg.moderation_keywords,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct EnableReq {
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub display_name: String,
}

/// The heavy OrbNet bring-up: start the onion + homeserver, provision the owner
/// account, set up the community, and peer any configured seeds. Idempotent and
/// safe to re-run — `provision_owner` reuses an existing account and
/// `setup_community` rejoins existing rooms. Runs on the blocking pool, off the
/// request path (the Tor bootstrap can take minutes).
fn reconcile() -> Value {
    let cfg = read_config();
    let onion = match run_script(&["up"]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("orbnet reconcile: up failed: {e}");
            return json!({"ok": false, "err": format!("orbnet up: {e}")});
        }
    };
    let owner = match provision_owner(&cfg.handle) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("orbnet reconcile: provision owner failed: {e}");
            return json!({"ok": false, "err": format!("provision owner: {e}"), "onion": onion});
        }
    };
    let community = if cfg.auto_join_community {
        setup_community(&owner)
    } else {
        json!({"rooms": []})
    };
    // Auto-peer any configured directory seeds (mesh with other Orbs).
    let mut peered = 0;
    for seed in &cfg.directory_seeds {
        peered += peer_seed(&owner, seed);
    }
    json!({"ok": true, "onion": onion, "owner": owner.user_id, "community": community, "peered_rooms": peered})
}

/// POST /api/orbnet/enable — bring OrbNet up. Persists config synchronously
/// (fast) then runs the bring-up in the BACKGROUND and returns immediately, so
/// the request never blocks on the Tor bootstrap — which would otherwise blow
/// past the browser's request timeout (the "Load failed" the operator saw).
/// The dashboard polls /status until the owner account appears. Admin-gated.
pub async fn enable(State(_s): State<AppState>, Json(req): Json<EnableReq>) -> Json<Value> {
    let saved = tokio::task::spawn_blocking(move || -> Value {
        let mut cfg = read_config();
        if !req.handle.is_empty() {
            cfg.handle = sanitize_handle(&req.handle);
        }
        if cfg.handle.is_empty() {
            cfg.handle = sanitize_handle("");
        }
        if !req.display_name.is_empty() {
            cfg.display_name = req.display_name.clone();
        }
        cfg.enabled = true;
        match write_config(&cfg) {
            Ok(_) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "err": format!("write config: {e}")}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "enable task failed"}));
    if saved.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        // Fire-and-forget the slow bring-up; the dashboard polls /status.
        tokio::task::spawn_blocking(reconcile);
        return Json(json!({"ok": true, "started": true}));
    }
    Json(saved)
}

/// On boot, if OrbNet is enabled but the owner account was never provisioned
/// (e.g., a crash/OOM-restart interrupted activation), finish the bring-up.
/// Idempotent — a no-op once the owner exists.
pub async fn reconcile_on_boot() {
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let cfg = read_config();
    if cfg.enabled && read_owner().is_none() {
        eprintln!("orbnet: enabled but owner missing on boot — reconciling");
        let _ = tokio::task::spawn_blocking(reconcile).await;
    }
}

/// POST /api/orbnet/disable — take OrbNet down (homeserver + onion stop). The
/// account + data persist for a later re-enable. Admin-gated.
pub async fn disable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let mut cfg = read_config();
        cfg.enabled = false;
        let _ = write_config(&cfg);
        match run_script(&["down"]) {
            Ok(_) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "disable task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct ModerationReq {
    pub moderation_keywords: Vec<String>,
}

/// PUT /api/orbnet/moderation — set the owner's per-user keyword filter.
/// Admin-gated.
pub async fn set_moderation(State(_s): State<AppState>, Json(req): Json<ModerationReq>) -> Json<Value> {
    let mut cfg = read_config();
    cfg.moderation_keywords = req
        .moderation_keywords
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    match write_config(&cfg) {
        Ok(_) => Json(json!({"ok": true, "moderation_keywords": cfg.moderation_keywords})),
        Err(e) => Json(json!({"ok": false, "err": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct ClientPasswordReq {
    pub password: String,
}

/// POST /api/orbnet/client-password — set a memorable login password on the
/// owner account so the operator can sign into a Matrix client (Element). The
/// account's original password was auto-generated, so this is the only way in.
/// Runs the UIA password-change flow (authed with the stored password), then
/// re-logs-in to refresh the token, and persists both to owner.json so the
/// supervisor's own login fallback stays valid. Admin-gated.
pub async fn set_client_password(State(_s): State<AppState>, Json(req): Json<ClientPasswordReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        if req.password.chars().count() < 8 {
            return json!({"ok": false, "err": "password must be at least 8 characters"});
        }
        let Some(mut owner) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let pw_path = "/_matrix/client/v3/account/password";
        // UIA step 1 — obtain a session
        let r1 = match cs_curl("POST", pw_path, Some(&owner.access_token),
            Some(&json!({"new_password": req.password, "logout_devices": false}).to_string())) {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        if let Some(session) = r1.get("session").and_then(|v| v.as_str()) {
            let auth = json!({
                "type": "m.login.password",
                "identifier": {"type": "m.id.user", "user": owner.user_id},
                "password": owner.password,
                "session": session,
            });
            let r2 = match cs_curl("POST", pw_path, Some(&owner.access_token),
                Some(&json!({"new_password": req.password, "logout_devices": false, "auth": auth}).to_string())) {
                Ok(v) => v,
                Err(e) => return json!({"ok": false, "err": e}),
            };
            if let Some(ec) = r2.get("errcode").and_then(|v| v.as_str()) {
                let msg = r2.get("error").and_then(|v| v.as_str()).unwrap_or("");
                return json!({"ok": false, "err": format!("{ec}: {msg}")});
            }
        } else if r1.get("errcode").is_some() {
            return json!({"ok": false, "err": format!("password change rejected: {r1}")});
        }
        // Re-login with the new password to guarantee a fresh, valid token.
        if let Ok(login) = cs_curl("POST", "/_matrix/client/v3/login", None, Some(&json!({
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": owner.user_id},
            "password": req.password,
        }).to_string())) {
            if let Some(tok) = login.get("access_token").and_then(|v| v.as_str()) {
                owner.access_token = tok.to_string();
            }
        }
        owner.password = req.password.clone();
        if let Err(e) = write_owner(&owner) {
            return json!({"ok": false, "err": format!("password set but owner.json update failed: {e}")});
        }
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "client-password task failed"}));
    Json(v)
}

/// GET /api/orbnet/cert — the Orb's self-signed homeserver cert (PEM, public —
/// NEVER the key) to install + trust on a phone so native Element validates the
/// onion's TLS (the cert's SAN = the .onion). Admin-gated.
pub async fn cert() -> impl IntoResponse {
    use axum::http::{header, StatusCode};
    match std::fs::read_to_string(format!("{RUNTIME_DIR}/tls/cert.pem")) {
        Ok(pem) => (
            [
                (header::CONTENT_TYPE, "application/x-pem-file"),
                (header::CONTENT_DISPOSITION, "attachment; filename=\"orbnet-orb-cert.pem\""),
            ],
            pem,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "cert not found (is OrbNet up?)").into_response(),
    }
}

/// GET /api/orbnet/rooms — the owner's joined rooms (community + groups + DMs)
/// with names, member counts, and a recent-activity timestamp. Admin-gated.
pub async fn rooms(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let sync = match cs_curl(
            "GET",
            "/_matrix/client/v3/sync?timeout=0",
            Some(&o.access_token),
            None,
        ) {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let mut out = vec![];
        let mut members = std::collections::HashSet::new();
        let mut active = std::collections::HashSet::new();
        let mut activity: Vec<Value> = vec![];
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if let Some(join) = sync.pointer("/rooms/join").and_then(|v| v.as_object()) {
            for (rid, room) in join {
                let name = room
                    .pointer("/state/events")
                    .and_then(|v| v.as_array())
                    .and_then(|evs| {
                        evs.iter()
                            .rev()
                            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("m.room.name"))
                            .and_then(|e| e.pointer("/content/name"))
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| rid.clone());
                let member_count = room
                    .pointer("/summary/m.joined_member_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                // unique community members across rooms (from join state events)
                if let Some(evs) = room.pointer("/state/events").and_then(|v| v.as_array()) {
                    for e in evs {
                        if e.get("type").and_then(|t| t.as_str()) == Some("m.room.member")
                            && e.pointer("/content/membership").and_then(|m| m.as_str()) == Some("join")
                        {
                            if let Some(sk) = e.get("state_key").and_then(|s| s.as_str()) {
                                members.insert(sk.to_string());
                            }
                        }
                    }
                }
                let mut last_sender = String::new();
                let mut last_body = String::new();
                let mut last_ts = 0i64;
                if let Some(evs) = room.pointer("/timeline/events").and_then(|v| v.as_array()) {
                    for e in evs {
                        if e.get("type").and_then(|t| t.as_str()) != Some("m.room.message") {
                            continue;
                        }
                        let ts = e.get("origin_server_ts").and_then(|t| t.as_i64()).unwrap_or(0);
                        let sender = e.get("sender").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        let body = e.pointer("/content/body").and_then(|b| b.as_str()).unwrap_or("").to_string();
                        // "active now" = posted in the last hour (presence over Tor
                        // federation is unreliable, so we proxy it with activity).
                        if now - ts < 3_600_000 && !sender.is_empty() {
                            active.insert(sender.clone());
                        }
                        activity.push(json!({"room": name, "sender": sender, "body": body, "ts": ts}));
                        last_sender = sender;
                        last_body = body;
                        last_ts = ts;
                    }
                }
                out.push(json!({"room_id": rid, "name": name, "last_ts": last_ts, "members": member_count, "last_sender": last_sender, "last_body": last_body}));
            }
        }
        activity.sort_by(|a, b| b["ts"].as_i64().unwrap_or(0).cmp(&a["ts"].as_i64().unwrap_or(0)));
        activity.truncate(15);
        let rooms_count = out.len();
        json!({"ok": true, "rooms": out, "stats": {"rooms": rooms_count, "members": members.len(), "active": active.len()}, "activity": activity})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "rooms task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct SendReq {
    pub body: String,
}

/// POST /api/orbnet/rooms/:id/send — send a plain text message as the owner.
/// Admin-gated.
pub async fn send(State(_s): State<AppState>, AxPath(room_id): AxPath<String>, Json(req): Json<SendReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let txn = rand_hex(8);
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            urlencode(&room_id),
            txn
        );
        match cs_curl(
            "PUT",
            &path,
            Some(&o.access_token),
            Some(&json!({"msgtype": "m.text", "body": req.body}).to_string()),
        ) {
            Ok(r) if r.get("event_id").is_some() => json!({"ok": true, "event_id": r["event_id"]}),
            Ok(r) => json!({"ok": false, "err": r}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "send task failed"}));
    Json(v)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ── community + groups + DMs ─────────────────────────────────────────────────

/// High-level interest rooms every Orb seeds locally. Public + unencrypted so
/// they federate cleanly and the dashboard can render activity.
const INTEREST_ROOMS: &[(&str, &str)] = &[
    ("general", "OrbNet · General"),
    ("tech", "OrbNet · Technology"),
    ("ai", "OrbNet · AI & Agents"),
    ("makers", "OrbNet · Makers & Hardware"),
    ("privacy", "OrbNet · Privacy & Security"),
    ("random", "OrbNet · Random"),
];

/// Idempotently create the OrbNet interest rooms on this homeserver and ensure
/// the owner is joined. Re-enable resolves the existing alias instead of
/// duplicating. Best-effort per room.
fn setup_community(owner: &Owner) -> Value {
    let token = owner.access_token.as_str();
    let onion = run_script(&["info"]).unwrap_or_default();
    let mut rooms = vec![];
    for (slug, name) in INTEREST_ROOMS {
        let body = json!({
            "name": name,
            "room_alias_name": format!("orbnet-{slug}"),
            "preset": "public_chat",
            "visibility": "public",
            "topic": format!("OrbNet community · {slug}"),
        });
        let create = cs_curl("POST", "/_matrix/client/v3/createRoom", Some(token), Some(&body.to_string()));
        let rid: Option<String> = match &create {
            Ok(v) if v.get("room_id").is_some() => v["room_id"].as_str().map(String::from),
            _ => {
                let alias = format!("#orbnet-{slug}:{onion}");
                cs_curl(
                    "GET",
                    &format!("/_matrix/client/v3/directory/room/{}", urlencode(&alias)),
                    Some(token),
                    None,
                )
                .ok()
                .and_then(|v| v.get("room_id").and_then(|x| x.as_str()).map(String::from))
            }
        };
        if let Some(rid) = rid {
            let _ = cs_curl(
                "POST",
                &format!("/_matrix/client/v3/rooms/{}/join", urlencode(&rid)),
                Some(token),
                Some("{}"),
            );
            rooms.push(json!({"slug": slug, "room_id": rid, "name": name}));
        }
    }
    json!({"rooms": rooms})
}

#[derive(Deserialize)]
pub struct GroupReq {
    pub name: String,
    /// Matrix ids to invite, e.g. ["@nova:abc…onion"].
    #[serde(default)]
    pub invite: Vec<String>,
    /// Private groups can opt into E2EE (dashboard activity then shows metadata
    /// only — that's the privacy trade).
    #[serde(default)]
    pub encrypted: bool,
}

/// POST /api/orbnet/group — create a private group room + invite members.
pub async fn create_group(State(_s): State<AppState>, Json(req): Json<GroupReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let mut body = json!({"name": req.name, "preset": "private_chat", "invite": req.invite});
        if req.encrypted {
            body["initial_state"] = json!([{
                "type": "m.room.encryption", "state_key": "",
                "content": {"algorithm": "m.megolm.v1.aes-sha2"}
            }]);
        }
        match cs_curl("POST", "/_matrix/client/v3/createRoom", Some(&o.access_token), Some(&body.to_string())) {
            Ok(r) if r.get("room_id").is_some() => json!({"ok": true, "room_id": r["room_id"]}),
            Ok(r) => json!({"ok": false, "err": r}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "group task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct DmReq {
    pub user_id: String,
}

/// POST /api/orbnet/dm — start an E2EE direct message with another Orb user.
pub async fn create_dm(State(_s): State<AppState>, Json(req): Json<DmReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let body = json!({
            "preset": "trusted_private_chat",
            "invite": [req.user_id],
            "is_direct": true,
            "initial_state": [{
                "type": "m.room.encryption", "state_key": "",
                "content": {"algorithm": "m.megolm.v1.aes-sha2"}
            }],
        });
        match cs_curl("POST", "/_matrix/client/v3/createRoom", Some(&o.access_token), Some(&body.to_string())) {
            Ok(r) if r.get("room_id").is_some() => json!({"ok": true, "room_id": r["room_id"]}),
            Ok(r) => json!({"ok": false, "err": r}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "dm task failed"}));
    Json(v)
}

/// GET /api/orbnet/rooms/:id/messages — recent messages (newest first), digested
/// to sender/body/ts. The dashboard applies the owner's keyword filter
/// client-side. Admin-gated.
pub async fn room_messages(State(_s): State<AppState>, AxPath(room_id): AxPath<String>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let path = format!(
            "/_matrix/client/v3/rooms/{}/messages?dir=b&limit=50",
            urlencode(&room_id)
        );
        match cs_curl("GET", &path, Some(&o.access_token), None) {
            Ok(r) => {
                let msgs: Vec<Value> = r
                    .get("chunk")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("m.room.message"))
                            .map(|e| {
                                json!({
                                    "sender": e.get("sender"),
                                    "body": e.pointer("/content/body"),
                                    "ts": e.get("origin_server_ts"),
                                    "event_id": e.get("event_id"),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({"ok": true, "messages": msgs})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "messages task failed"}));
    Json(v)
}

// ── mesh / directory ─────────────────────────────────────────────────────────

/// Join a peer Orb's interest rooms by alias (federates over Tor). Best-effort;
/// returns how many rooms were joined. This is the mesh: peering with another
/// Orb's onion pulls its community rooms into the shared federation.
fn peer_seed(owner: &Owner, onion: &str) -> usize {
    let mut n = 0;
    for (slug, _name) in INTEREST_ROOMS {
        let alias = format!("#orbnet-{slug}:{onion}");
        let r = cs_curl(
            "POST",
            &format!("/_matrix/client/v3/join/{}", urlencode(&alias)),
            Some(&owner.access_token),
            Some("{}"),
        );
        if r.map(|v| v.get("room_id").is_some()).unwrap_or(false) {
            n += 1;
        }
    }
    n
}

#[derive(Deserialize)]
pub struct PeerReq {
    pub onion: String,
}

/// POST /api/orbnet/peer — federate with another Orb: join its community rooms
/// over Tor + remember it as a directory seed for future enables. Admin-gated.
pub async fn peer(State(_s): State<AppState>, Json(req): Json<PeerReq>) -> Json<Value> {
    let onion = req
        .onion
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string();
    let v = tokio::task::spawn_blocking(move || -> Value {
        if !onion.ends_with(".onion") {
            return json!({"ok": false, "err": "expected a .onion address"});
        }
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let joined = peer_seed(&o, &onion);
        let mut cfg = read_config();
        if !cfg.directory_seeds.contains(&onion) {
            cfg.directory_seeds.push(onion.clone());
            let _ = write_config(&cfg);
        }
        json!({"ok": true, "onion": onion, "joined_rooms": joined})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "peer task failed"}));
    Json(v)
}

// ── personas (human-placed LLM bots) ─────────────────────────────────────────
//
// A persona is an LLM-backed Matrix bot with its own account on this homeserver.
// It is ONLY ever added to a room by an explicit human action (the admin-gated
// endpoint below) — never automatically. A background responder syncs each
// persona's rooms and replies to non-persona messages via the configured LLM.

const PERSONAS_JSON: &str = "/var/lib/aeon/orbnet/personas.json";
const SINCE_DIR: &str = "/var/lib/aeon/orbnet/persona-since";

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Persona {
    name: String,
    handle: String,
    user_id: String,
    access_token: String,
    #[serde(default)]
    system_prompt: String,
    #[serde(default)]
    llm_url: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    rooms: Vec<String>,
}

fn read_personas() -> Vec<Persona> {
    std::fs::read_to_string(PERSONAS_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}
fn write_personas(p: &[Persona]) -> std::io::Result<()> {
    std::fs::create_dir_all(RUNTIME_DIR)?;
    let tmp = format!("{PERSONAS_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(p).unwrap_or_default())?;
    std::fs::rename(tmp, PERSONAS_JSON)
}
fn read_since(handle: &str) -> String {
    std::fs::read_to_string(format!("{SINCE_DIR}/{handle}")).unwrap_or_default().trim().to_string()
}
fn write_since(handle: &str, since: &str) {
    let _ = std::fs::create_dir_all(SINCE_DIR);
    let _ = std::fs::write(format!("{SINCE_DIR}/{handle}"), since);
}

/// Register a fresh Matrix account on the local homeserver (UIA token flow).
fn register_account(handle: &str, password: &str) -> Result<(String, String), String> {
    let reg_token = run_script(&["reg-token"])?;
    if reg_token.is_empty() {
        return Err("no registration token".into());
    }
    let path = "/_matrix/client/v3/register?kind=user";
    let r1 = cs_curl("POST", path, None, Some(&json!({"username": handle, "password": password}).to_string()))?;
    if let Some(t) = r1.get("access_token").and_then(|v| v.as_str()) {
        return Ok((r1["user_id"].as_str().unwrap_or_default().to_string(), t.to_string()));
    }
    let session = r1.get("session").and_then(|v| v.as_str()).ok_or_else(|| format!("register: {r1}"))?;
    let auth = json!({"type": "m.login.registration_token", "token": reg_token, "session": session});
    let r2 = cs_curl("POST", path, None, Some(&json!({"username": handle, "password": password, "auth": auth}).to_string()))?;
    let t = r2.get("access_token").and_then(|v| v.as_str()).ok_or_else(|| format!("register2: {r2}"))?;
    Ok((r2["user_id"].as_str().unwrap_or_default().to_string(), t.to_string()))
}

/// Ask the persona's LLM (OpenAI-compatible chat completions) for a reply.
fn llm_reply(p: &Persona, context: &[(String, String)]) -> Option<String> {
    if p.llm_url.is_empty() {
        return None;
    }
    let mut messages = vec![json!({"role": "system", "content": p.system_prompt})];
    for (sender, body) in context {
        let role = if sender == &p.user_id { "assistant" } else { "user" };
        messages.push(json!({"role": role, "content": body}));
    }
    let payload = json!({"model": p.model, "messages": messages, "max_tokens": 400, "temperature": 0.8});
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "--max-time", "90", "-X", "POST", &p.llm_url, "-H", "Content-Type: application/json"]);
    if !p.api_key.is_empty() {
        cmd.arg("-H").arg(format!("Authorization: Bearer {}", p.api_key));
    }
    cmd.arg("-d").arg(payload.to_string());
    let out = cmd.output().ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).map(|s| s.trim().to_string())
}

/// One sync+reply pass for a persona. First sync records position (no replies to
/// history); later syncs reply to new non-persona messages.
fn respond_for_persona(p: &Persona, persona_ids: &[String]) {
    let since = read_since(&p.handle);
    let path = if since.is_empty() {
        "/_matrix/client/v3/sync?timeout=0".to_string()
    } else {
        format!("/_matrix/client/v3/sync?since={}&timeout=0", urlencode(&since))
    };
    let Ok(sync) = cs_curl("GET", &path, Some(&p.access_token), None) else {
        return;
    };
    if let Some(nb) = sync.get("next_batch").and_then(|v| v.as_str()) {
        write_since(&p.handle, nb);
    }
    if since.is_empty() {
        return; // first pass: just set the cursor
    }
    let Some(join) = sync.pointer("/rooms/join").and_then(|v| v.as_object()) else {
        return;
    };
    for (rid, room) in join {
        let Some(events) = room.pointer("/timeline/events").and_then(|v| v.as_array()) else {
            continue;
        };
        let mut context: Vec<(String, String)> = vec![];
        let mut has_user_msg = false;
        for e in events {
            if e.get("type").and_then(|t| t.as_str()) != Some("m.room.message") {
                continue;
            }
            let sender = e.get("sender").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let body = e.pointer("/content/body").and_then(|b| b.as_str()).unwrap_or("").to_string();
            if body.is_empty() {
                continue;
            }
            if sender != p.user_id && !persona_ids.contains(&sender) {
                has_user_msg = true;
            }
            context.push((sender, body));
        }
        if !has_user_msg {
            continue;
        }
        let ctx: Vec<(String, String)> = context.into_iter().rev().take(8).rev().collect();
        if let Some(reply) = llm_reply(p, &ctx) {
            if !reply.is_empty() {
                let txn = rand_hex(8);
                let _ = cs_curl(
                    "PUT",
                    &format!("/_matrix/client/v3/rooms/{}/send/m.room.message/{}", urlencode(rid), txn),
                    Some(&p.access_token),
                    Some(&json!({"msgtype": "m.text", "body": reply}).to_string()),
                );
            }
        }
    }
}

/// Background loop: every few seconds, give each placed persona a sync+reply pass.
/// Spawned once from main().
pub async fn persona_responder_loop() {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
        let personas = tokio::task::spawn_blocking(read_personas).await.unwrap_or_default();
        if personas.is_empty() {
            continue;
        }
        let ids: Vec<String> = personas.iter().map(|p| p.user_id.clone()).collect();
        for p in personas {
            let ids = ids.clone();
            let _ = tokio::task::spawn_blocking(move || respond_for_persona(&p, &ids)).await;
        }
    }
}

#[derive(Deserialize)]
pub struct PersonaReq {
    pub name: String,
    pub room_id: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub llm_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

/// POST /api/orbnet/persona — HUMAN-DIRECTED: place a persona bot into a room.
/// Registers (or re-uses) the bot account, invites + joins it, records its
/// config for the responder. Admin-gated — never automatic. Admin-gated.
pub async fn place_persona(State(_s): State<AppState>, Json(req): Json<PersonaReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(owner) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let handle = format!("persona-{}", sanitize_handle(&req.name));
        let mut personas = read_personas();
        // re-use an existing persona account with the same handle, else register
        let (user_id, token) = if let Some(ex) = personas.iter().find(|p| p.handle == handle) {
            (ex.user_id.clone(), ex.access_token.clone())
        } else {
            match register_account(&handle, &rand_hex(16)) {
                Ok(pair) => {
                    let _ = cs_curl(
                        "PUT",
                        &format!("/_matrix/client/v3/profile/{}/displayname", urlencode(&pair.0)),
                        Some(&pair.1),
                        Some(&json!({"displayname": format!("🎭 {}", req.name)}).to_string()),
                    );
                    pair
                }
                Err(e) => return json!({"ok": false, "err": format!("register persona: {e}")}),
            }
        };
        // owner invites the bot, bot joins
        let _ = cs_curl(
            "POST",
            &format!("/_matrix/client/v3/rooms/{}/invite", urlencode(&req.room_id)),
            Some(&owner.access_token),
            Some(&json!({"user_id": user_id}).to_string()),
        );
        let join = cs_curl(
            "POST",
            &format!("/_matrix/client/v3/join/{}", urlencode(&req.room_id)),
            Some(&token),
            Some("{}"),
        );
        if join.map(|v| v.get("room_id").is_none()).unwrap_or(true) {
            return json!({"ok": false, "err": "persona could not join the room"});
        }
        // record / update
        if let Some(p) = personas.iter_mut().find(|p| p.handle == handle) {
            if !p.rooms.contains(&req.room_id) {
                p.rooms.push(req.room_id.clone());
            }
            if !req.system_prompt.is_empty() { p.system_prompt = req.system_prompt.clone(); }
            if !req.llm_url.is_empty() { p.llm_url = req.llm_url.clone(); }
            if !req.model.is_empty() { p.model = req.model.clone(); }
            if !req.api_key.is_empty() { p.api_key = req.api_key.clone(); }
        } else {
            personas.push(Persona {
                name: req.name.clone(),
                handle: handle.clone(),
                user_id: user_id.clone(),
                access_token: token,
                system_prompt: req.system_prompt,
                llm_url: req.llm_url,
                model: req.model,
                api_key: req.api_key,
                rooms: vec![req.room_id.clone()],
            });
        }
        let _ = write_personas(&personas);
        json!({"ok": true, "user_id": user_id, "room_id": req.room_id})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "persona task failed"}));
    Json(v)
}

/// GET /api/orbnet/personas — list placed personas (name + rooms, no secrets).
pub async fn personas(State(_s): State<AppState>) -> Json<Value> {
    let list: Vec<Value> = read_personas()
        .iter()
        .map(|p| json!({"name": p.name, "user_id": p.user_id, "rooms": p.rooms, "has_llm": !p.llm_url.is_empty()}))
        .collect();
    Json(json!({"ok": true, "personas": list}))
}

// ── membership + federation management ────────────────────────────────────────

/// POST /api/orbnet/rooms/:id/leave — leave (and forget) a room, group, or DM.
pub async fn leave_room(State(_s): State<AppState>, AxPath(id): AxPath<String>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        match cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/leave", urlencode(&id)), Some(&o.access_token), Some("{}")) {
            Ok(v) if v.get("errcode").is_none() => {
                let _ = cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/forget", urlencode(&id)), Some(&o.access_token), Some("{}"));
                json!({"ok": true})
            }
            Ok(v) => json!({"ok": false, "err": v}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "leave task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct KickReq {
    pub user_id: String,
    #[serde(default)]
    pub reason: String,
    /// true = ban (also blocks rejoin); false = kick.
    #[serde(default)]
    pub ban: bool,
}

/// POST /api/orbnet/rooms/:id/kick — remove (kick) or ban a member from a room
/// the owner moderates. Requires the owner to hold the needed power level (true
/// for rooms/groups it created). Admin-gated.
pub async fn kick_member(State(_s): State<AppState>, AxPath(id): AxPath<String>, Json(req): Json<KickReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(o) = read_owner() else {
            return json!({"ok": false, "err": "owner not provisioned"});
        };
        let action = if req.ban { "ban" } else { "kick" };
        let body = json!({"user_id": req.user_id, "reason": req.reason});
        match cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/{}", urlencode(&id), action), Some(&o.access_token), Some(&body.to_string())) {
            Ok(v) if v.get("errcode").is_none() => json!({"ok": true, "action": action}),
            Ok(v) => json!({"ok": false, "err": v}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "kick task failed"}));
    Json(v)
}

/// GET /api/orbnet/peers — the onions this Orb federates with (its directory
/// seeds). Admin-gated.
pub async fn peers(State(_s): State<AppState>) -> Json<Value> {
    let cfg = read_config();
    Json(json!({"ok": true, "peers": cfg.directory_seeds}))
}

/// POST /api/orbnet/peer/remove — stop federating with an onion: drop it from
/// the seed list (so it isn't re-peered on enable) and leave its rooms the owner
/// is currently in. Admin-gated.
pub async fn unpeer(State(_s): State<AppState>, Json(req): Json<PeerReq>) -> Json<Value> {
    let onion = req.onion.trim().trim_end_matches('/').to_string();
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut cfg = read_config();
        let before = cfg.directory_seeds.len();
        cfg.directory_seeds.retain(|s| s != &onion);
        let removed = cfg.directory_seeds.len() != before;
        let _ = write_config(&cfg);
        // Leave any currently-joined rooms hosted on that onion (room_id ends
        // with ":<onion>"). Uses a local sync — no flaky remote alias lookups.
        let mut left = 0;
        if let Some(o) = read_owner() {
            if let Ok(sync) = cs_curl("GET", "/_matrix/client/v3/sync?timeout=0", Some(&o.access_token), None) {
                if let Some(join) = sync.pointer("/rooms/join").and_then(|v| v.as_object()) {
                    let suffix = format!(":{onion}");
                    for rid in join.keys().filter(|r| r.ends_with(&suffix)) {
                        let _ = cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/leave", urlencode(rid)), Some(&o.access_token), Some("{}"));
                        let _ = cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/forget", urlencode(rid)), Some(&o.access_token), Some("{}"));
                        left += 1;
                    }
                }
            }
        }
        json!({"ok": true, "removed": removed, "left_rooms": left})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unpeer task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct PersonaRemoveReq {
    pub user_id: String,
}

/// POST /api/orbnet/persona/remove — retire a persona: stop the responder for it,
/// have its bot account leave every room, and drop its record + sync cursor.
/// Admin-gated.
pub async fn remove_persona(State(_s): State<AppState>, Json(req): Json<PersonaRemoveReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut personas = read_personas();
        let Some(idx) = personas.iter().position(|p| p.user_id == req.user_id) else {
            return json!({"ok": false, "err": "no such persona"});
        };
        let p = personas.remove(idx);
        for rid in &p.rooms {
            let _ = cs_curl("POST", &format!("/_matrix/client/v3/rooms/{}/leave", urlencode(rid)), Some(&p.access_token), Some("{}"));
        }
        let _ = write_personas(&personas);
        let _ = std::fs::remove_file(format!("{SINCE_DIR}/{}", p.handle));
        json!({"ok": true, "removed": p.name})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "remove persona task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct PersonaUpdateReq {
    pub user_id: String,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub llm_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

/// POST /api/orbnet/persona/update — edit a placed persona's brain config after
/// the fact: model + endpoint, soul (system_prompt), optional display name. Any
/// omitted field is left unchanged. Admin-gated.
pub async fn update_persona(State(_s): State<AppState>, Json(req): Json<PersonaUpdateReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut personas = read_personas();
        let Some(idx) = personas.iter().position(|p| p.user_id == req.user_id) else {
            return json!({"ok": false, "err": "no such persona"});
        };
        if let Some(s) = req.system_prompt { personas[idx].system_prompt = s; }
        if let Some(u) = req.llm_url { personas[idx].llm_url = u; }
        if let Some(m) = req.model { personas[idx].model = m; }
        if let Some(k) = req.api_key { personas[idx].api_key = k; }
        let token = personas[idx].access_token.clone();
        let uid = personas[idx].user_id.clone();
        if let Err(e) = write_personas(&personas) {
            return json!({"ok": false, "err": format!("save: {e}")});
        }
        if let Some(dn) = req.display_name {
            if !dn.is_empty() {
                let _ = cs_curl(
                    "PUT",
                    &format!("/_matrix/client/v3/profile/{}/displayname", urlencode(&uid)),
                    Some(&token),
                    Some(&json!({"displayname": dn}).to_string()),
                );
            }
        }
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "update persona task failed"}));
    Json(v)
}
