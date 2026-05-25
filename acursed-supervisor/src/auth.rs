//! Simple username + argon2-hashed-password auth. Sessions are signed
//! cookies. First boot writes a randomly generated password to
//! `/etc/acursed/auth.toml` so we never ship default creds.

use crate::api::AppState;
use anyhow::{Context, Result};
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AuthFile {
    pub username: String,
    /// PHC string from argon2 — never a plain password.
    pub password_hash: String,
    /// HMAC key for session cookies. Random per-deployment.
    pub session_key: String,
}

pub struct AuthStore(RwLock<AuthFile>);

impl AuthStore {
    pub fn load(path: &Path) -> Self {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(f) = toml::from_str::<AuthFile>(&text) {
                return Self(RwLock::new(f));
            }
        }
        tracing::warn!("auth file missing, instantiating empty (login will fail)");
        Self(RwLock::new(AuthFile::default()))
    }

    pub fn verify(&self, user: &str, password: &str) -> bool {
        let f = self.0.read();
        if f.username != user {
            return false;
        }
        let Ok(hash) = PasswordHash::new(&f.password_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    }
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> impl IntoResponse {
    if !state.auth.verify(&req.username, &req.password) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "err": "bad credentials"})))
            .into_response();
    }

    // Issue an HttpOnly + Secure session cookie.
    let token = random_token();
    let cookie = format!(
        "acursed_session={}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=86400",
        token
    );
    let mut resp =
        Json(json!({"ok": true, "user": req.username})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

pub async fn logout() -> impl IntoResponse {
    let cookie =
        "acursed_session=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0";
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(cookie).unwrap());
    resp
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes)
}

/// First-boot helper: generate a random password, write hashed form to
/// /etc/acursed/auth.toml, print the cleartext to stdout (which a setup
/// script captures into /boot/firmware/aeon-credentials.txt for the user
/// to find on their SD card).
pub fn generate_default(path: &Path) -> Result<String> {
    use argon2::password_hash::SaltString;
    use argon2::PasswordHasher;
    use rand::distributions::Alphanumeric;

    let password: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect();
    let mut session_key_bytes = [0u8; 32];
    rand::thread_rng().fill(&mut session_key_bytes);
    let session_key = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &session_key_bytes,
    );

    let salt = SaltString::generate(&mut rand::thread_rng());
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;

    let f = AuthFile {
        username: "admin".to_string(),
        password_hash: hash.to_string(),
        session_key,
    };
    let text = toml::to_string(&f)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).ok();
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(password)
}
