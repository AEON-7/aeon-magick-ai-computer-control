//! Publisher identity — self-sovereign accounts for Model Share.
//!
//! There is no central server, so an "account" IS a cryptographic keypair (like
//! Nostr / PGP): the ed25519 PUBLIC key is the pseudonymous identity, the
//! username is a display petname, and the PRIVATE key is derived from a BIP39
//! seed phrase (the recovery secret) and stored encrypted at rest with the
//! user's password. Model publications are SIGNED with the key, so authorship is
//! provable and reputation accrues to the key over time — no PII, portable
//! across Orbs via the seed phrase.

use axum::extract::State;
use axum::Json;
use base64::Engine;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

use crate::api::AppState;

const STORE: &str = "/etc/aeon/publisher-accounts.json";
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Account {
    pubkey: String,     // hex(32) — the canonical identity
    username: String,   // display petname (not globally unique)
    created_ms: i64,
    kdf_salt: String,   // base64(16)
    nonce: String,      // base64(12)
    enc_secret: String, // base64(chacha20poly1305(32-byte ed25519 secret))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Store {
    #[serde(default)]
    accounts: Vec<Account>,
    #[serde(default)]
    active: String, // active pubkey (the one used to sign)
}

fn load() -> Store {
    std::fs::read_to_string(STORE)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(s: &Store) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(STORE).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = format!("{STORE}.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap_or_default())?;
    // 0600 — the encrypted keys live here.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    std::fs::rename(tmp, STORE)
}

/// The signing key decrypted into memory after `unlock`, held for the session.
fn unlocked() -> &'static Mutex<Option<(String, SigningKey)>> {
    static U: OnceLock<Mutex<Option<(String, SigningKey)>>> = OnceLock::new();
    U.get_or_init(|| Mutex::new(None))
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Derive a 32-byte symmetric key from a password + salt (argon2id).
fn kdf(password: &[u8], salt: &[u8]) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password, salt, &mut out)
        .map_err(|e| format!("kdf: {e}"))?;
    Ok(out)
}

/// Build an encrypted Account from a raw 32-byte ed25519 secret (= BIP39 entropy).
fn from_secret(username: &str, password: &str, secret: &[u8; 32]) -> Result<Account, String> {
    let sk = SigningKey::from_bytes(secret);
    let pubkey = hex::encode(sk.verifying_key().to_bytes());
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let key = kdf(password.as_bytes(), &salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), secret.as_ref())
        .map_err(|_| "encrypt failed".to_string())?;
    Ok(Account {
        pubkey,
        username: username.trim().to_string(),
        created_ms: epoch_ms(),
        kdf_salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        enc_secret: B64.encode(ct),
    })
}

fn decrypt_key(acct: &Account, password: &str) -> Result<SigningKey, String> {
    let salt = B64.decode(&acct.kdf_salt).map_err(|_| "bad salt")?;
    let nonce = B64.decode(&acct.nonce).map_err(|_| "bad nonce")?;
    let ct = B64.decode(&acct.enc_secret).map_err(|_| "bad ciphertext")?;
    let key = kdf(password.as_bytes(), &salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let pt = cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
        .map_err(|_| "wrong password".to_string())?;
    if pt.len() != 32 {
        return Err("corrupt key material".into());
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&pt);
    Ok(SigningKey::from_bytes(&secret))
}

fn upsert(mut s: Store, acct: Account) -> Result<String, String> {
    let pubkey = acct.pubkey.clone();
    s.accounts.retain(|a| a.pubkey != acct.pubkey);
    s.accounts.push(acct);
    if s.active.is_empty() {
        s.active = pubkey.clone();
    }
    save(&s).map_err(|e| e.to_string())?;
    Ok(pubkey)
}

// ── public API used by the rest of the supervisor ───────────────────────────

/// Canonical bytes signed for a model publication — binds the identity to the
/// exact content (CID) + metadata. Any Orb recomputes and verifies this.
pub fn publication_digest(cid: &str, name: &str, sha256: &str, created_ms: i64) -> Vec<u8> {
    format!("aeon-publish/1\n{cid}\n{name}\n{sha256}\n{created_ms}").into_bytes()
}

/// Sign a message with the UNLOCKED active key → (pubkey_hex, base64 signature).
pub fn sign(msg: &[u8]) -> Result<(String, String), String> {
    let g = unlocked().lock().map_err(|_| "lock")?;
    let (pk, sk) = g.as_ref().ok_or("no publisher identity unlocked — unlock it first")?;
    Ok((pk.clone(), B64.encode(sk.sign(msg).to_bytes())))
}

/// Verify a publication signature (pubkey hex, message, base64 signature).
pub fn verify(pubkey_hex: &str, msg: &[u8], sig_b64: &str) -> bool {
    let Ok(pk) = hex::decode(pubkey_hex) else { return false };
    let Ok(pk): Result<[u8; 32], _> = pk.try_into() else { return false };
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    let Ok(sig) = B64.decode(sig_b64) else { return false };
    let Ok(sig): Result<[u8; 64], _> = sig.try_into() else { return false };
    vk.verify(msg, &Signature::from_bytes(&sig)).is_ok()
}

/// The active identity as (pubkey, username), if one is set.
pub fn active() -> Option<(String, String)> {
    let s = load();
    s.accounts.iter().find(|a| a.pubkey == s.active).map(|a| (a.pubkey.clone(), a.username.clone()))
}

/// Display username for a pubkey we've seen locally (else "").
pub fn username_for(pubkey: &str) -> String {
    load().accounts.iter().find(|a| a.pubkey == pubkey).map(|a| a.username.clone()).unwrap_or_default()
}

// ── REST handlers (mounted under /api/publisher/*) ──────────────────────────

#[derive(Deserialize)]
pub struct CreateReq {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub seed_phrase: String, // present → recover/import instead of create
}

/// POST /api/publisher/accounts — create a NEW identity (returns the one-time
/// seed phrase to back up), or recover one from a `seed_phrase`.
pub async fn create_account(State(_s): State<AppState>, Json(req): Json<CreateReq>) -> Json<Value> {
    if req.username.trim().is_empty() || req.password.len() < 6 {
        return Json(json!({"ok": false, "err": "username required and password ≥ 6 chars"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        if !req.seed_phrase.trim().is_empty() {
            // Recover / import from a seed phrase.
            let m = match bip39::Mnemonic::parse(req.seed_phrase.trim()) {
                Ok(m) => m,
                Err(_) => return json!({"ok": false, "err": "invalid seed phrase"}),
            };
            let ent = m.to_entropy();
            if ent.len() != 32 {
                return json!({"ok": false, "err": "seed phrase must be 24 words"});
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&ent);
            match from_secret(&req.username, &req.password, &secret).and_then(|a| upsert(load(), a)) {
                Ok(pk) => json!({"ok": true, "pubkey": pk, "recovered": true}),
                Err(e) => json!({"ok": false, "err": e}),
            }
        } else {
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            let phrase = match bip39::Mnemonic::from_entropy(&secret) {
                Ok(m) => m.to_string(),
                Err(e) => return json!({"ok": false, "err": e.to_string()}),
            };
            match from_secret(&req.username, &req.password, &secret).and_then(|a| upsert(load(), a)) {
                // seed_phrase is returned ONCE — the UI must prompt the user to write it down.
                Ok(pk) => json!({"ok": true, "pubkey": pk, "seed_phrase": phrase}),
                Err(e) => json!({"ok": false, "err": e}),
            }
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "account task failed"}));
    Json(v)
}

/// GET /api/publisher/accounts — list local identities (no secrets) + which is
/// active + which (if any) is currently unlocked for signing.
pub async fn list_accounts(State(_s): State<AppState>) -> Json<Value> {
    let s = load();
    let unlocked_pk = unlocked().lock().ok().and_then(|g| g.as_ref().map(|(p, _)| p.clone())).unwrap_or_default();
    Json(json!({
        "ok": true,
        "accounts": s.accounts.iter().map(|a| json!({
            "pubkey": a.pubkey,
            "username": a.username,
            "created_ms": a.created_ms,
            "fingerprint": a.pubkey.get(..12).unwrap_or(&a.pubkey),
        })).collect::<Vec<_>>(),
        "active": s.active,
        "unlocked": unlocked_pk,
    }))
}

#[derive(Deserialize)]
pub struct PubkeyPwReq {
    pub pubkey: String,
    #[serde(default)]
    pub password: String,
}

/// POST /api/publisher/unlock — decrypt an identity's key into memory + make it
/// active, so publications sign with it this session.
pub async fn unlock_account(State(_s): State<AppState>, Json(req): Json<PubkeyPwReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut s = load();
        let Some(acct) = s.accounts.iter().find(|a| a.pubkey == req.pubkey).cloned() else {
            return json!({"ok": false, "err": "no such account"});
        };
        match decrypt_key(&acct, &req.password) {
            Ok(sk) => {
                if let Ok(mut g) = unlocked().lock() {
                    *g = Some((acct.pubkey.clone(), sk));
                }
                s.active = acct.pubkey.clone();
                let _ = save(&s);
                json!({"ok": true, "active": acct.pubkey, "username": acct.username})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unlock task failed"}));
    Json(v)
}

/// POST /api/publisher/lock — forget the in-memory key.
pub async fn lock_account(State(_s): State<AppState>) -> Json<Value> {
    if let Ok(mut g) = unlocked().lock() {
        *g = None;
    }
    Json(json!({"ok": true}))
}

/// POST /api/publisher/seed — reveal an identity's seed phrase (needs the
/// password) so it can be re-backed-up.
pub async fn export_seed(State(_s): State<AppState>, Json(req): Json<PubkeyPwReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let s = load();
        let Some(acct) = s.accounts.iter().find(|a| a.pubkey == req.pubkey) else {
            return json!({"ok": false, "err": "no such account"});
        };
        match decrypt_key(acct, &req.password) {
            Ok(sk) => match bip39::Mnemonic::from_entropy(&sk.to_bytes()) {
                Ok(m) => json!({"ok": true, "seed_phrase": m.to_string()}),
                Err(e) => json!({"ok": false, "err": e.to_string()}),
            },
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "seed task failed"}));
    Json(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystore_roundtrip() {
        let secret = [7u8; 32];
        let acct = from_secret("alice", "hunter2!", &secret).unwrap();

        // right password decrypts to the same key; wrong password fails.
        assert_eq!(decrypt_key(&acct, "hunter2!").unwrap().to_bytes(), secret);
        assert!(decrypt_key(&acct, "wrong").is_err());

        // sign with the key + verify against the account pubkey; tamper fails.
        let msg = publication_digest("QmCid", "model", "shahash", 123);
        let sig = B64.encode(SigningKey::from_bytes(&secret).sign(&msg).to_bytes());
        assert!(verify(&acct.pubkey, &msg, &sig));
        assert!(!verify(&acct.pubkey, b"tampered", &sig));

        // BIP39 seed phrase roundtrips the exact secret (recovery works).
        let phrase = bip39::Mnemonic::from_entropy(&secret).unwrap().to_string();
        let back = bip39::Mnemonic::parse(&phrase).unwrap().to_entropy();
        assert_eq!(back.as_slice(), &secret);
        assert_eq!(phrase.split_whitespace().count(), 24);
    }
}
