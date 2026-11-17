//! Authentication, session management, and API tokens for the supervisor.
//!
//! The portal can be in one of two modes:
//!   - `Open`   — first boot, no real password set yet. The setup wizard
//!                is the only writable endpoint; everything else returns
//!                403. Web UI shows the setup page.
//!   - `Locked` — normal operation. Requests must present one of:
//!                  * a valid `aeon_session=<sig>.<payload>` cookie
//!                    (issued by POST /api/login, HMAC-signed with the
//!                    per-device session key)
//!                  * an `Authorization: Bearer aeon_tok_<...>` header
//!                    (an API token issued via /api/auth/tokens)
//!                  * an `Authorization: Basic <base64>` header (admin
//!                    username + password against the argon2 hash)
//!
//! API tokens have a scope (Admin > Full > Macros > Read). The router
//! applies the scope check in `scope_allows`.

use crate::api::AppState;
use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Persisted forms ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthState {
    Open,
    Locked,
}

impl Default for AuthState {
    fn default() -> Self {
        Self::Open
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AuthFile {
    #[serde(default)]
    pub state: AuthState,
    pub username: String,
    /// PHC string from argon2 — never a plain password.
    pub password_hash: String,
    /// HMAC key (base64) for session cookies. Random per-deployment.
    pub session_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TokenScope {
    /// Can do everything including token management and password changes.
    Admin,
    /// Every HID / macro / snapshot op, but no token mgmt or password change.
    Full,
    /// Run pre-stored macros + read state/snapshots. No raw HID.
    Macros,
    /// State + snapshot + list macros/prompts. No mutations of any kind.
    Read,
}

impl TokenScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Full => "full",
            Self::Macros => "macros",
            Self::Read => "read",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenRecord {
    /// Public 8-char hex id. Listed by `GET /api/auth/tokens`.
    pub id: String,
    /// Human label set by the operator.
    pub name: String,
    /// argon2(plaintext) — the plaintext is only shown once, at creation.
    pub hash: String,
    pub scope: TokenScope,
    pub created_at_ms: u64,
    #[serde(default)]
    pub last_used_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TokensFile {
    #[serde(default)]
    pub tokens: Vec<TokenRecord>,
}

// ── In-memory store ────────────────────────────────────────────────────

pub struct AuthStore {
    auth_path: PathBuf,
    tokens_path: PathBuf,
    auth: RwLock<AuthFile>,
    tokens: RwLock<TokensFile>,
    /// sha256(token) → token id, for tokens that have already passed the full
    /// argon2 check. Cleared whenever a token is revoked.
    verified: RwLock<std::collections::HashMap<[u8; 32], String>>,
}

/// Write a secret file so it is never half-written and never world-readable:
/// created 0600 beside the target, synced, then renamed over it. A plain
/// `fs::write` truncates first — a power cut at that moment left an empty
/// auth.toml, which the device read as "not set up yet".
fn write_private_atomic(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// How often a token's "last used" time is written back. Persisting it on
/// every request rewrote tokens.toml on the SD card for each API call an
/// agent made.
const LAST_USED_PERSIST_MS: u64 = 5 * 60 * 1000;

impl AuthStore {
    pub fn load(auth_path: &Path, tokens_path: &Path) -> Self {
        let auth = std::fs::read_to_string(auth_path)
            .ok()
            .and_then(|t| toml::from_str::<AuthFile>(&t).ok())
            .unwrap_or_default();
        let tokens = std::fs::read_to_string(tokens_path)
            .ok()
            .and_then(|t| toml::from_str::<TokensFile>(&t).ok())
            .unwrap_or_default();
        if auth.password_hash.is_empty() {
            let unreadable = std::fs::metadata(auth_path).map(|m| m.len() > 0).unwrap_or(false);
            tracing::warn!(
                path=%auth_path.display(),
                unreadable,
                "auth.toml missing, empty or unreadable — device is in OPEN state; the setup wizard will ask for the setup code"
            );
        }
        Self {
            auth_path: auth_path.to_path_buf(),
            tokens_path: tokens_path.to_path_buf(),
            auth: RwLock::new(auth),
            tokens: RwLock::new(tokens),
            verified: RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// "Open" = no password has been set yet (fresh device, setup wizard
    /// needed). Authoritative signal is `password_hash` being empty. The
    /// `state` field in the TOML is informational; on upgrade from an
    /// older binary the field may be missing entirely and serde will
    /// default it to Open — that doesn't matter as long as a real password
    /// is already hashed in.
    pub fn is_open(&self) -> bool {
        self.auth.read().password_hash.is_empty()
    }

    #[allow(dead_code)]
    pub fn admin_username(&self) -> String {
        self.auth.read().username.clone()
    }

    pub fn verify_password(&self, user: &str, password: &str) -> bool {
        let f = self.auth.read();
        if f.username != user || f.password_hash.is_empty() {
            return false;
        }
        let Ok(hash) = PasswordHash::new(&f.password_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    }

    /// Sign a session payload with HMAC-SHA256(session_key). Cookie format:
    /// `aeon_session=<base64url(payload)>.<base64url(sig)>`
    pub fn issue_session(&self, user: &str) -> String {
        let payload = format!(
            "{}|{}|{}",
            user,
            now_ms(),
            now_ms() + 24 * 60 * 60 * 1000  // 24h expiry
        );
        let key = self.auth.read().session_key.clone();
        let sig = hmac_sha256(&key, payload.as_bytes());
        format!("{}.{}", B64URL.encode(&payload), B64URL.encode(&sig))
    }

    /// Verify a session cookie. Returns Some(user) if valid and not expired.
    pub fn verify_session(&self, cookie: &str) -> Option<String> {
        let (b64_payload, b64_sig) = cookie.split_once('.')?;
        let payload = B64URL.decode(b64_payload).ok()?;
        let sig = B64URL.decode(b64_sig).ok()?;
        let key = self.auth.read().session_key.clone();
        let expected = hmac_sha256(&key, &payload);
        if !constant_time_eq(&sig, &expected) {
            return None;
        }
        let payload_str = std::str::from_utf8(&payload).ok()?;
        let mut parts = payload_str.split('|');
        let user = parts.next()?.to_string();
        let _issued = parts.next()?;
        let expires: u64 = parts.next()?.parse().ok()?;
        if now_ms() > expires {
            return None;
        }
        Some(user)
    }

    /// Verify an API token. Returns the matching scope on success.
    pub fn verify_token(&self, plain: &str) -> Option<TokenScope> {
        if !plain.starts_with("aeon_tok_") {
            return None;
        }
        // Fast path. The full check is argon2 against EVERY stored token —
        // tens of milliseconds each on a Pi, under a lock, on every request an
        // agent makes. A token that already passed it is recognised by its
        // SHA-256 instead (tokens are long random strings, so a fast hash is
        // safe as a lookup key; the cache lives only in memory).
        let key: [u8; 32] = {
            use sha2::{Digest, Sha256};
            Sha256::digest(plain.as_bytes()).into()
        };
        let cached = self.verified.read().get(&key).cloned();
        let id = match cached {
            Some(id) => id,
            None => {
                // Slow path, without holding the lock across the hashing.
                let candidates: Vec<(String, String)> =
                    self.tokens.read().tokens.iter().map(|r| (r.id.clone(), r.hash.clone())).collect();
                let id = candidates.into_iter().find_map(|(id, stored)| {
                    let hash = PasswordHash::new(&stored).ok()?;
                    Argon2::default().verify_password(plain.as_bytes(), &hash).ok().map(|_| id)
                })?;
                let mut cache = self.verified.write();
                if cache.len() > 1024 {
                    cache.clear();
                }
                cache.insert(key, id.clone());
                id
            }
        };
        // The record must still exist — a revoked token is gone from here.
        let now = now_ms();
        let mut tokens = self.tokens.write();
        let Some(rec) = tokens.tokens.iter_mut().find(|r| r.id == id) else {
            drop(tokens);
            self.verified.write().remove(&key);
            return None;
        };
        let scope = rec.scope;
        let stale = rec.last_used_at_ms.map(|t| now.saturating_sub(t) >= LAST_USED_PERSIST_MS).unwrap_or(true);
        if stale {
            rec.last_used_at_ms = Some(now);
            let snap = tokens.clone();
            drop(tokens);
            let _ = self.save_tokens(&snap);
        }
        Some(scope)
    }

    pub fn list_tokens(&self) -> Vec<Value> {
        use serde_json::Value;
        self.tokens
            .read()
            .tokens
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "scope": t.scope.as_str(),
                    "created_at_ms": t.created_at_ms,
                    "last_used_at_ms": t.last_used_at_ms,
                })
            })
            .collect::<Vec<Value>>()
    }

    /// Create a new token. Returns (id, plaintext_token).
    /// Plaintext is shown ONCE — only the argon2 hash persists.
    pub fn create_token(&self, name: &str, scope: TokenScope) -> Result<(String, String)> {
        use argon2::password_hash::SaltString;
        use argon2::PasswordHasher;

        let id = random_hex(8);
        let secret = random_alphanumeric(28);
        let plain = format!("aeon_tok_{secret}");

        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        let hash = Argon2::default()
            .hash_password(plain.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
            .to_string();

        let rec = TokenRecord {
            id: id.clone(),
            name: name.to_string(),
            hash,
            scope,
            created_at_ms: now_ms(),
            last_used_at_ms: None,
        };

        {
            let mut tokens = self.tokens.write();
            tokens.tokens.push(rec);
            let snap = tokens.clone();
            drop(tokens);
            self.save_tokens(&snap)?;
        }
        Ok((id, plain))
    }

    pub fn revoke_token(&self, id: &str) -> Result<bool> {
        self.verified.write().clear();
        let mut tokens = self.tokens.write();
        let before = tokens.tokens.len();
        tokens.tokens.retain(|t| t.id != id);
        let removed = tokens.tokens.len() != before;
        if removed {
            let snap = tokens.clone();
            drop(tokens);
            self.save_tokens(&snap)?;
        }
        Ok(removed)
    }

    /// First-boot or setup-wizard call: set the admin password and transition
    /// to Locked. Also ensures a session_key exists.
    pub fn set_password(&self, username: &str, password: &str) -> Result<()> {
        use argon2::password_hash::SaltString;
        use argon2::PasswordHasher;

        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
            .to_string();

        let session_key = {
            let cur = self.auth.read().session_key.clone();
            if cur.is_empty() {
                let mut bytes = [0u8; 32];
                rand::rngs::OsRng.fill(&mut bytes);
                B64.encode(bytes)
            } else {
                cur
            }
        };

        let snap = {
            let mut f = self.auth.write();
            f.state = AuthState::Locked;
            f.username = username.to_string();
            f.password_hash = hash;
            f.session_key = session_key;
            f.clone()
        };
        self.save_auth(&snap)?;
        Ok(())
    }

    fn save_auth(&self, f: &AuthFile) -> Result<()> {
        let text = toml::to_string_pretty(f)?;
        if let Some(p) = self.auth_path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        write_private_atomic(&self.auth_path, &text)
    }

    fn save_tokens(&self, f: &TokensFile) -> Result<()> {
        let text = toml::to_string_pretty(f)?;
        if let Some(p) = self.tokens_path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        write_private_atomic(&self.tokens_path, &text)
    }
}

// ── Identity resolved from request headers ─────────────────────────────

#[derive(Debug, Clone)]
pub struct Identity {
    pub user: String,
    pub scope: TokenScope,
}

pub fn identify(store: &AuthStore, headers: &HeaderMap) -> Option<Identity> {
    // 1. Session cookie
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for entry in cookie.split(';') {
            let entry = entry.trim();
            if let Some(val) = entry.strip_prefix("aeon_session=") {
                if let Some(user) = store.verify_session(val) {
                    return Some(Identity {
                        user,
                        scope: TokenScope::Admin,
                    });
                }
            }
        }
    }
    // 2. Authorization header — Bearer aeon_tok_... OR Basic user:pass
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if let Some(scope) = store.verify_token(token.trim()) {
                return Some(Identity {
                    user: format!("token:{}", &token[..token.len().min(17)]), // "token:aeon_tok_xx"
                    scope,
                });
            }
        }
        if let Some(b64) = auth.strip_prefix("Basic ") {
            if let Ok(raw) = B64.decode(b64.trim()) {
                if let Ok(s) = std::str::from_utf8(&raw) {
                    if let Some((u, p)) = s.split_once(':') {
                        if store.verify_password(u, p) {
                            return Some(Identity {
                                user: u.to_string(),
                                scope: TokenScope::Admin,
                            });
                        }
                    }
                }
            }
        }
    }
    // 3. X-Aeon-Token convenience header (matches MCP clients that prefer this)
    if let Some(token) = headers.get("x-aeon-token").and_then(|v| v.to_str().ok()) {
        if let Some(scope) = store.verify_token(token.trim()) {
            return Some(Identity {
                user: "token".into(),
                scope,
            });
        }
    }
    None
}

/// Decide whether `identity` is allowed to perform a given request.
/// Token management endpoints require Admin scope. Mutation HID endpoints
/// require Full+ scope. Macro-run requires Macros+ scope. Read endpoints
/// require Read+ scope.
pub fn scope_allows(identity: &Identity, method: &Method, path: &str) -> bool {
    // Admin can do anything.
    if matches!(identity.scope, TokenScope::Admin) {
        return true;
    }
    // LOCKDOWN / per-category exposure: refuse non-admin callers when the
    // killswitch is on, or when the path's category has been disabled. The admin
    // session (above) is never affected — the human keeps control + the KVM.
    if crate::lockdown::is_blocked(path) {
        return false;
    }
    // Token management + Pi-side reboot/poweroff require Admin.
    // /system/info is a read-only health endpoint and is whitelisted
    // lower in the match. Target power controls are also Admin-only:
    // a non-admin token shouldn't be able to forcibly power-cycle the
    // attached client machine, that's a privileged operation.
    if path.starts_with("/api/auth/tokens")
        || path.starts_with("/api/setup/")
        || path == "/api/system/reboot"
        || path == "/api/system/poweroff"
        || path == "/api/system/pi-reboot"
        || path.starts_with("/api/system/config") // config backup/restore (sensitive)
        || path == "/api/system/pi-poweroff"
        || path == "/api/target/power-tap"
        || path == "/api/target/power-hold"
        || path == "/api/target/wake"
        || path == "/api/target/reboot"
        || path == "/api/target/config"
        // SSH access management (authorized_keys) is a HUMAN-ADMIN-ONLY action.
        // The web UI uses the admin session (Admin scope, allowed at the top of
        // this fn); an agent's API token (Full/Macros/Read) is denied here, and
        // there is no MCP tool for it. Agents never manage SSH.
        || path.starts_with("/api/ssh/")
        // The whole Agent Dash (connected systems, agent provisioning incl.
        // per-agent SSH users + sudo, metrics) is a human-admin console. Agents
        // operate via the main HID/vision/etc. API with their issued token; they
        // never reach /api/agent/* — gate it all to the admin session.
        || path.starts_with("/api/agent/")
        // OrbNet (federation enable/disable, owner account, send, personas,
        // moderation, lockdown) is a human-admin console like the Agent Dash.
        || path.starts_with("/api/orbnet/")
        // Hailo AI accelerator (driver install + model deploy/unload) is a
        // human-admin console over REST — admin-gated exactly like OrbNet, no
        // allow-list bypass. Agents never install drivers or reflash the NNC.
        || path.starts_with("/api/hailo/")
        // Hidden-services hosting (mint/retire onions) is a human-admin console
        // over REST; agents reach it only via the gated hidden_service_* MCP tools.
        || path.starts_with("/api/onions/")
        // IPFS node/gateway/pin management is a human-admin console over REST;
        // agents pin/add via the gated ipfs_* MCP tools.
        || path.starts_with("/api/ipfs/")
        // Mysterium node management (earnings + wallet-adjacent) is human-admin only.
        || path.starts_with("/api/mysterium/")
        // Lockdown + exposure controls are a human-admin failsafe.
        || path.starts_with("/api/lockdown")
        // Publisher identities are the keys models are signed with: creating,
        // unlocking, signing out and revealing a seed phrase are the operator's.
        || path.starts_with("/api/publisher/")
        // Bench actions that change a connected machine (deploy/stop/update a
        // pod, or add an SSH key to it) are the operator's, like /api/agent/*.
        // Read-only bench endpoints stay available to agent tokens.
        || matches!(path, "/api/bench/authorize" | "/api/bench/deploy" | "/api/bench/update" | "/api/bench/stop" | "/api/bench/run_verified")
        // Erasing the audit trail is not something an agent token gets to do.
        || (path == "/api/audit" && *method == Method::DELETE)
    {
        return false;
    }
    match identity.scope {
        TokenScope::Full => {
            // Everything except admin-only endpoints (already filtered above).
            true
        }
        TokenScope::Macros => {
            // Reads OK. Macro run OK. No raw HID. No PUT/DELETE on macros/prompts.
            if method == Method::GET {
                return true;
            }
            // POST /api/macros/:name/run is allowed.
            if method == Method::POST
                && path.starts_with("/api/macros/")
                && path.ends_with("/run")
            {
                return true;
            }
            // POST /mcp is allowed but the MCP handler also enforces per-tool
            // (e.g. `set_persona` shouldn't be runnable by a macros-scope token
            // even via MCP). For now we accept this as a known gap and
            // recommend Full scope for MCP tokens.
            if path == "/api/mcp" {
                return true;
            }
            false
        }
        TokenScope::Read => {
            // GETs are fair game; MCP is also allowed — the MCP handler's
            // per-tool scope gate then restricts a read token to read-tier tools
            // (state, snapshot, list_*, *_status, …). Without this a read-scope
            // agent token couldn't use MCP at all (POST /api/mcp).
            if method == Method::GET || path == "/api/mcp" {
                return true;
            }
            false
        }
        TokenScope::Admin => true,
    }
}

// ── HTTP handlers ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    if !state.auth.verify_password(&req.username, &req.password) {
        // Audit failed login — actor is the *attempted* username. Useful
        // for spotting brute-force attempts from a particular client.
        crate::audit::log(
            &format!("{} (attempt)", req.username),
            "login_fail",
            "bad credentials",
            "fail",
            Some("invalid username or password"),
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "err": "bad credentials"})),
        )
            .into_response();
    }
    let session = state.auth.issue_session(&req.username);
    crate::audit::log(
        &format!("{} (session)", req.username),
        "login_ok",
        "new session issued",
        "ok",
        None,
    );
    let cookie = format!(
        "aeon_session={session}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=86400"
    );
    let mut resp = Json(json!({"ok": true, "user": req.username})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Best-effort: capture WHO is logging out by identifying first.
    let who = identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    crate::audit::log(&who, "logout", "session ended", "ok", None);
    let cookie = "aeon_session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0";
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(cookie).unwrap());
    resp
}

/// GET /api/auth/me — who am I? Used by the SPA to decide whether to show
/// the login screen, setup wizard, or the main UI.
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let server = crate::api::server_mode();
    if state.auth.is_open() {
        return Json(json!({
            "ok": true,
            "state": "open",
            "needs_setup": true,
            "setup_code_required": !server && !in_claim_window(),
            "admin_username_default": "admin",
            "server": server,
        }))
        .into_response();
    }
    match identify(&state.auth, &headers) {
        Some(id) => Json(json!({
            "ok": true,
            "state": "locked",
            "authenticated": true,
            "user": id.user,
            "scope": id.scope.as_str(),
            "server": server,
        }))
        .into_response(),
        None => Json(json!({
            "ok": true,
            "state": "locked",
            "authenticated": false,
            "server": server,
        }))
        .into_response(),
    }
}

#[derive(Deserialize)]
pub struct SetupReq {
    #[serde(default = "default_admin")]
    pub username: String,
    pub password: String,
    /// The per-device setup code from aeon-credentials.txt on the SD card.
    /// Needed once the first-boot window has passed.
    #[serde(default)]
    pub code: String,
}

// ── Who may complete first-boot setup ──────────────────────────────────
//
// Completing setup sets the admin password, and that password is mirrored to
// the Unix `admin` account (SSH, passwordless sudo) — so it is a grant of
// root. It used to be open to anyone who could reach the device for as long
// as it stayed un-configured: a neighbour on the LAN, anyone in range of the
// setup hotspot (whose passphrase is the same on every Orb), and equally an
// Orb whose auth file had been lost or truncated.
//
// Now it is open only during the FIRST BOOT, for 30 minutes — the moment the
// owner is standing next to a device they just plugged in. After that, or
// after any restart, it takes the per-device setup code that first boot
// wrote to the SD card, i.e. proof of having the device in hand.

const SETUP_CODE_FILE: &str = "/etc/aeon/setup-code";
const FIRSTBOOT_MARK: &str = "/var/lib/aeon/firstboot-done";
const CLAIM_WINDOW_SECS: f64 = 30.0 * 60.0;

/// The decision, separated from where its inputs come from so it can be tested.
fn claim_window_open(boot_id: &str, firstboot_mark: &str, uptime_secs: f64) -> bool {
    let boot_id = boot_id.trim();
    !boot_id.is_empty() && firstboot_mark.trim() == boot_id && (0.0..CLAIM_WINDOW_SECS).contains(&uptime_secs)
}

fn in_claim_window() -> bool {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let uptime = read("/proc/uptime").split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(f64::MAX);
    claim_window_open(&read("/proc/sys/kernel/random/boot_id"), &read(FIRSTBOOT_MARK), uptime)
}

/// Codes are compared without regard to case, spaces or dashes ("ab12-cd34").
fn normalize_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

fn setup_code_matches(expected_file_text: &str, supplied: &str) -> bool {
    let (want, got) = (normalize_code(expected_file_text), normalize_code(supplied));
    // No code on the device means nothing can match — never "anything goes".
    if want.len() < 6 || want.len() != got.len() {
        return false;
    }
    // Constant time over the length.
    want.bytes().zip(got.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// A device that never got a setup code (an image from before this existed,
/// still un-configured) is given one now, recorded where the owner can read
/// it: the boot partition of the SD card.
pub fn ensure_setup_code() {
    if crate::api::server_mode() || std::fs::metadata(SETUP_CODE_FILE).map(|m| m.len() > 0).unwrap_or(false) {
        return;
    }
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rngs::OsRng;
    let code: String = (0..8).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect();
    if write_private_atomic(Path::new(SETUP_CODE_FILE), &format!("{code}\n")).is_err() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open("/boot/firmware/aeon-credentials.txt") {
        let _ = writeln!(
            f,
            "\n# Setup code: {}-{}\n#     The setup wizard asks for this once the device has been on for more\n#     than 30 minutes, or has been restarted, without setup being completed.",
            &code[..4], &code[4..]
        );
    }
}

fn default_admin() -> String {
    "admin".to_string()
}

/// POST /api/setup/password — first-boot setup wizard target.
/// Only callable while the auth store is in Open state. Transitions to Locked.
pub async fn setup_password(
    State(state): State<AppState>,
    Json(req): Json<SetupReq>,
) -> impl IntoResponse {
    if !state.auth.is_open() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "err": "device already configured"})),
        )
            .into_response();
    }
    // The headless server container has no SD card to read a code from; its
    // claim flow is unchanged here.
    if !crate::api::server_mode() && !in_claim_window() {
        let expected = std::fs::read_to_string(SETUP_CODE_FILE).unwrap_or_default();
        if !setup_code_matches(&expected, &req.code) {
            // Slow down guessing; 32^8 codes is far out of reach at this rate.
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
            crate::audit::log("anonymous (setup)", "setup_refused", "missing or wrong setup code", "denied", None);
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "ok": false,
                    "code_required": true,
                    "err": "Enter this device's setup code. It is in aeon-credentials.txt on the SD card's boot partition.",
                })),
            )
                .into_response();
        }
    }
    if req.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "password must be 8+ chars"})),
        )
            .into_response();
    }
    if let Err(e) = state.auth.set_password(&req.username, &req.password) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": e.to_string()})),
        )
            .into_response();
    }
    // Mirror onto the Unix/SSH account so first-boot setup also replaces the
    // shipped default SSH password (console + SSH share one credential).
    sync_unix_password(&req.username, &req.password);
    crate::audit::log(
        &format!("{} (setup)", req.username),
        "password_set",
        "initial admin password configured",
        "ok",
        None,
    );
    // Auto-login: issue a session cookie so the wizard flows straight into the
    // main UI without forcing a second prompt.
    let session = state.auth.issue_session(&req.username);
    let cookie = format!(
        "aeon_session={session}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=86400"
    );
    let mut resp = Json(json!({"ok": true, "user": req.username})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordReq>,
) -> impl IntoResponse {
    let Some(id) = identify(&state.auth, &headers) else {
        return (StatusCode::UNAUTHORIZED, "auth required").into_response();
    };
    if !matches!(id.scope, TokenScope::Admin) {
        return (StatusCode::FORBIDDEN, "admin scope required").into_response();
    }
    if !state.auth.verify_password(&id.user, &req.current_password) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"ok": false, "err": "current password wrong"})),
        )
            .into_response();
    }
    if req.new_password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "password must be 8+ chars"})),
        )
            .into_response();
    }
    if let Err(e) = state.auth.set_password(&id.user, &req.new_password) {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    // Keep the Unix/SSH password in lockstep with the console password.
    sync_unix_password(&id.user, &req.new_password);
    crate::audit::log(
        &crate::audit::actor_for(&id),
        "password_change",
        "admin password updated",
        "ok",
        None,
    );
    Json(json!({"ok": true})).into_response()
}

/// Mirror the console password onto the same-named Unix login account so the
/// web console and SSH share one credential — setting the password at first
/// boot therefore also replaces the shipped default SSH password. Best-effort:
/// a failure is logged, not fatal (the console password is still set). The
/// supervisor runs as root, so `chpasswd` works directly. Gated to the shipped
/// `admin` account so a stray console username can't touch a system user.
fn sync_unix_password(user: &str, plaintext: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    if user != "admin" {
        return;
    }
    let res = (|| -> std::io::Result<std::process::ExitStatus> {
        let mut child = Command::new("chpasswd")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(format!("{user}:{plaintext}\n").as_bytes())?;
        }
        child.wait()
    })();
    match res {
        Ok(s) if s.success() => {
            tracing::info!(user, "console password mirrored to the Unix/SSH account")
        }
        Ok(s) => tracing::warn!(user, code = ?s.code(), "chpasswd non-zero — SSH password not synced"),
        Err(e) => tracing::warn!(user, %e, "chpasswd failed — SSH password not synced"),
    }
}

// ── Token CRUD ──────────────────────────────────────────────────────────

pub async fn list_tokens(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"tokens": state.auth.list_tokens()}))
}

#[derive(Deserialize)]
pub struct CreateTokenReq {
    pub name: String,
    #[serde(default = "default_scope")]
    pub scope: TokenScope,
}

fn default_scope() -> TokenScope {
    TokenScope::Full
}

pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateTokenReq>,
) -> impl IntoResponse {
    let actor = identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    match state.auth.create_token(&req.name, req.scope) {
        Ok((id, plain)) => {
            crate::audit::log(
                &actor,
                "token_create",
                &format!("name={} scope={} id={}", req.name, req.scope.as_str(), id),
                "ok",
                None,
            );
            Json(json!({
                "ok": true,
                "id": id,
                "name": req.name,
                "scope": req.scope.as_str(),
                // SHOWN ONCE — caller must store this. Server keeps only the argon2 hash.
                "token": plain,
            }))
            .into_response()
        }
        Err(e) => {
            crate::audit::log(
                &actor,
                "token_create",
                &format!("name={}", req.name),
                "fail",
                Some(&e.to_string()),
            );
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub async fn revoke_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let actor = identify(&state.auth, &headers)
        .map(|id| crate::audit::actor_for(&id))
        .unwrap_or_else(|| "anonymous".into());
    match state.auth.revoke_token(&id) {
        Ok(true) => {
            crate::audit::log(
                &actor,
                "token_revoke",
                &format!("id={}", id),
                "ok",
                None,
            );
            Json(json!({"ok": true, "revoked": id})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "no such token").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Crypto helpers ─────────────────────────────────────────────────────

fn hmac_sha256(key_b64: &str, msg: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};

    let key = B64.decode(key_b64).unwrap_or_default();
    // RFC 2104 inline (no separate hmac crate dep)
    let block_size = 64;
    let mut k = if key.len() > block_size {
        let mut h = Sha256::new();
        h.update(&key);
        h.finalize().to_vec()
    } else {
        key
    };
    k.resize(block_size, 0);
    let mut o_key = vec![0x5c; block_size];
    let mut i_key = vec![0x36; block_size];
    for i in 0..block_size {
        o_key[i] ^= k[i];
        i_key[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(&i_key);
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(&o_key);
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Secrets below come straight from the kernel generator (`OsRng`), not from a
// userspace generator seeded from it. Both are cryptographically sound; for
// values that live for months — API tokens, the session-signing key — there is
// simply no reason to put anything between the kernel and the secret.
fn random_alphanumeric(n: usize) -> String {
    use rand::distributions::Alphanumeric;
    rand::rngs::OsRng
        .sample_iter(&Alphanumeric)
        .take(n)
        .map(char::from)
        .collect()
}

fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; (n + 1) / 2];
    rand::rngs::OsRng.fill(&mut bytes[..]);
    bytes
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
        .chars()
        .take(n)
        .collect()
}

/// First-boot helper. Generates session_key + leaves the device in OPEN state
/// (no password set yet) so the web UI's setup wizard prompts the user.
/// Older firstboot scripts that called this also set a generated password —
/// we no longer do that; the setup wizard owns first-time credentials.
pub fn generate_default(path: &Path) -> Result<String> {
    let mut session_key_bytes = [0u8; 32];
    rand::rngs::OsRng.fill(&mut session_key_bytes);
    let session_key = B64.encode(session_key_bytes);

    let f = AuthFile {
        state: AuthState::Open,
        username: "admin".to_string(),
        password_hash: String::new(),
        session_key,
    };
    let text = toml::to_string_pretty(&f)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    // Return empty plaintext — firstboot script knows OPEN state means
    // "no password, user must visit /setup".
    Ok(String::new())
}

// Re-export Value type used by list_tokens (avoids leaking serde_json::Value).
pub use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_is_open_without_a_code_only_during_the_first_boot() {
        let boot = "3f2a-boot-id\n";
        // Same boot that first-boot ran in, a few minutes up: open.
        assert!(claim_window_open(boot, "3f2a-boot-id\n", 120.0));
        // …but not once 30 minutes have passed,
        assert!(!claim_window_open(boot, "3f2a-boot-id", 30.0 * 60.0));
        // not after a restart (a different boot id),
        assert!(!claim_window_open("another-boot", "3f2a-boot-id", 60.0));
        // not when first-boot never recorded a boot (older image: empty marker),
        assert!(!claim_window_open(boot, "", 60.0));
        // and not when the running boot id cannot be read.
        assert!(!claim_window_open("", "", 60.0));
        assert!(!claim_window_open(boot, "3f2a-boot-id", f64::MAX));
    }

    #[test]
    fn the_setup_code_must_actually_match() {
        assert!(setup_code_matches("ABCD2345\n", "ABCD2345"));
        assert!(setup_code_matches("ABCD2345\n", "abcd-2345"));
        assert!(setup_code_matches("ABCD2345", " abcd 2345 "));
        assert!(!setup_code_matches("ABCD2345", "ABCD2346"));
        assert!(!setup_code_matches("ABCD2345", "ABCD234"));
        assert!(!setup_code_matches("ABCD2345", ""));
        // No code stored on the device must never mean "any code works".
        assert!(!setup_code_matches("", ""));
        assert!(!setup_code_matches("\n", ""));
        assert!(!setup_code_matches("", "ABCD2345"));
    }

    fn store(tag: &str) -> (AuthStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("aeon-auth-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (AuthStore::load(&dir.join("auth.toml"), &dir.join("tokens.toml")), dir)
    }

    #[test]
    fn tokens_verify_from_cache_and_stop_working_once_revoked() {
        let (s, dir) = store("tok");
        let (id, plain) = s.create_token("agent", TokenScope::Read).unwrap();
        assert!(matches!(s.verify_token(&plain), Some(TokenScope::Read)));
        assert_eq!(s.verified.read().len(), 1);
        // Second use is served from the cache and does not rewrite the file.
        let before = std::fs::metadata(dir.join("tokens.toml")).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(matches!(s.verify_token(&plain), Some(TokenScope::Read)));
        assert_eq!(std::fs::metadata(dir.join("tokens.toml")).unwrap().modified().unwrap(), before);
        // A wrong token is refused and not cached.
        assert!(s.verify_token("aeon_tok_definitely-not-a-real-token").is_none());
        assert_eq!(s.verified.read().len(), 1);
        // Revoked → refused, even though it had been cached.
        assert!(s.revoke_token(&id).unwrap());
        assert!(s.verify_token(&plain).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn auth_files_are_written_private_and_whole() {
        use std::os::unix::fs::PermissionsExt;
        let (s, dir) = store("perm");
        s.set_password("admin", "correct horse battery").unwrap();
        let meta = std::fs::metadata(dir.join("auth.toml")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(!dir.join("auth.tmp").exists()); // renamed into place, nothing left behind
        // Reloading sees a configured device, not an open one.
        assert!(!AuthStore::load(&dir.join("auth.toml"), &dir.join("tokens.toml")).is_open());
        let _ = std::fs::remove_dir_all(dir);
    }
}

