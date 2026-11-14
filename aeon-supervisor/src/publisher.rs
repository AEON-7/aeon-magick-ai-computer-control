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

// ── Orb key + delegation: signing that works out of the box ─────────────────
//
// A user identity is only in memory while unlocked, so before this existed a
// reboot silently turned every later share into an UNSIGNED one. Each Orb now
// owns a device key, generated on first use, and that key signs everything the
// Orb originates. Unlocking a user identity issues a DELEGATION — the user key
// signs the Orb key — which is stored on disk, so the Orb keeps signing on the
// user's behalf across reboots without the user's secret staying resident.
// "Lock" revokes the delegation on this Orb.

const ORB_KEY_FILE: &str = "/etc/aeon/orb-publisher.key";
const DELEGATION_FILE: &str = "/etc/aeon/orb-publisher-delegation.json";

/// "The holder of `user_key` lets `orb_key` publish in its name."
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    pub user_key: String,  // hex(32) — the identity reputation accrues to
    #[serde(default)]
    pub user_name: String, // decorative petname
    pub orb_key: String,   // hex(32) — the device key being authorized
    #[serde(default)]
    pub issued_ms: i64,
    pub sig: String,       // base64 ed25519 by user_key over delegation_digest
}

/// Canonical bytes a user key signs to authorize an Orb key.
pub fn delegation_digest(user_key: &str, orb_key: &str, issued_ms: i64) -> Vec<u8> {
    format!("aeon-delegate/1\n{user_key}\n{orb_key}\n{issued_ms}").into_bytes()
}

/// True when the delegation's signature really is the user key's.
pub fn verify_delegation(d: &Delegation) -> bool {
    is_pubkey_hex(&d.user_key)
        && is_pubkey_hex(&d.orb_key)
        && verify(&d.user_key, &delegation_digest(&d.user_key, &d.orb_key, d.issued_ms), &d.sig)
}

fn is_pubkey_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn issue_delegation(user: &SigningKey, user_name: &str, orb_key: &str, issued_ms: i64) -> Delegation {
    let user_key = hex::encode(user.verifying_key().to_bytes());
    let sig = B64.encode(user.sign(&delegation_digest(&user_key, orb_key, issued_ms)).to_bytes());
    Delegation { user_key, user_name: user_name.trim().to_string(), orb_key: orb_key.to_string(), issued_ms, sig }
}

/// The identity a signature should be CREDITED to: the delegating user key
/// when a valid delegation names this signer, else the signer itself.
pub fn identity_of(signer_key: &str, delegation: Option<&Delegation>) -> String {
    match delegation {
        Some(d) if d.orb_key == signer_key && verify_delegation(d) => d.user_key.clone(),
        _ => signer_key.to_string(),
    }
}

/// Write a secret file that is never world-readable, even for an instant
/// (create with 0600 rather than chmod after the fact).
fn write_private(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = format!("{path}.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(tmp, path)
}

/// This Orb's device signing key — loaded, or generated on first use.
fn orb_key() -> Result<SigningKey, String> {
    #[cfg(test)]
    {
        return Ok(SigningKey::from_bytes(&[42u8; 32]));
    }
    #[cfg(not(test))]
    orb_key_from_disk()
}

#[cfg(not(test))]
fn orb_key_from_disk() -> Result<SigningKey, String> {
    static K: OnceLock<Mutex<Option<SigningKey>>> = OnceLock::new();
    let mut g = K.get_or_init(|| Mutex::new(None)).lock().map_err(|_| "orb key lock")?;
    if let Some(k) = g.as_ref() {
        return Ok(k.clone());
    }
    let parse = |t: &str| -> Option<SigningKey> {
        let raw: [u8; 32] = hex::decode(t.trim()).ok()?.try_into().ok()?;
        Some(SigningKey::from_bytes(&raw))
    };
    let key = match std::fs::read_to_string(ORB_KEY_FILE).ok().as_deref().and_then(parse) {
        Some(k) => k,
        None => {
            // Never overwrite an existing-but-unreadable key: that would
            // silently change this Orb's identity and orphan its reputation.
            if std::path::Path::new(ORB_KEY_FILE).exists() {
                return Err(format!("{ORB_KEY_FILE} exists but is not a valid key"));
            }
            let mut secret = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            write_private(ORB_KEY_FILE, hex::encode(secret).as_bytes())
                .map_err(|e| format!("write orb key: {e}"))?;
            SigningKey::from_bytes(&secret)
        }
    };
    *g = Some(key.clone());
    Ok(key)
}

/// This Orb's device public key (hex), or "" if it cannot be created.
pub fn orb_pubkey() -> String {
    orb_key().map(|k| hex::encode(k.verifying_key().to_bytes())).unwrap_or_default()
}

/// The delegation stored on this Orb, if it is valid AND for this Orb's key.
pub fn current_delegation() -> Option<Delegation> {
    let d: Delegation = serde_json::from_str(&std::fs::read_to_string(DELEGATION_FILE).ok()?).ok()?;
    (d.orb_key == orb_pubkey() && verify_delegation(&d)).then_some(d)
}

/// A signature made by this Orb, plus who it should be credited to.
#[derive(Debug, Clone)]
pub struct Signed {
    pub pubkey: String, // the Orb key that made `sig`
    pub sig: String,    // base64
    pub name: String,   // decorative: the delegating user's petname, if any
    pub delegation: Option<Delegation>,
}

/// Sign as this Orb. Always available (no unlock needed); carries the user
/// delegation when one is active so verifiers credit the user identity.
pub fn sign_as_orb(msg: &[u8]) -> Result<Signed, String> {
    let k = orb_key()?;
    let delegation = current_delegation();
    Ok(Signed {
        pubkey: hex::encode(k.verifying_key().to_bytes()),
        sig: B64.encode(k.sign(msg).to_bytes()),
        name: delegation.as_ref().map(|d| d.user_name.clone()).unwrap_or_default(),
        delegation,
    })
}

/// Canonical bytes an origin signs INSIDE the shared directory's
/// model-card.json. It names every file by hash, so it is independent of the
/// transport: the same statement verifies whether the bytes arrived over
/// IPFS, BitTorrent, or a USB stick. `files` must be sorted by path.
pub fn origin_digest(name: &str, source: &str, verified: bool, signed_ms: i64, files: &[(String, u64, String)]) -> Vec<u8> {
    let mut s = format!("aeon-origin/1\n{name}\n{source}\n{}\n{signed_ms}\n", u8::from(verified));
    for (path, size, sha256) in files {
        s.push_str(&format!("{sha256} {size} {path}\n"));
    }
    s.into_bytes()
}

/// Static proof that the holder of an Orb key runs a given libp2p peer, so a
/// gossiped host can be recognized as a model's ORIGIN seeder.
pub fn peer_binding_digest(orb_key: &str, peer_id: &str) -> Vec<u8> {
    format!("aeon-peer/1\n{orb_key}\n{peer_id}").into_bytes()
}

// ── public API used by the rest of the supervisor ───────────────────────────

/// Canonical bytes signed for a model publication — binds the identity to the
/// exact content (CID) + metadata. Any Orb recomputes and verifies this.
pub fn publication_digest(cid: &str, name: &str, sha256: &str, created_ms: i64) -> Vec<u8> {
    format!("aeon-publish/1\n{cid}\n{name}\n{sha256}\n{created_ms}").into_bytes()
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

/// True when `pubkey` is one of this Orb's own signing keys: its device key or
/// a user identity stored here.
pub fn is_local_key(pubkey: &str) -> bool {
    !pubkey.is_empty() && (pubkey == orb_pubkey() || load().accounts.iter().any(|a| a.pubkey == pubkey))
}

/// The active identity as (pubkey, username), if one is set.
pub fn active() -> Option<(String, String)> {
    let s = load();
    s.accounts.iter().find(|a| a.pubkey == s.active).map(|a| (a.pubkey.clone(), a.username.clone()))
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
    // "unlocked" = the user identity shares are credited to right now. That is
    // the delegating identity when one is stored (it survives a reboot), else
    // whatever is unlocked in memory.
    let delegated = current_delegation().map(|d| d.user_key).unwrap_or_default();
    let unlocked_pk = if delegated.is_empty() {
        unlocked().lock().ok().and_then(|g| g.as_ref().map(|(p, _)| p.clone())).unwrap_or_default()
    } else {
        delegated.clone()
    };
    let orb = orb_pubkey();
    Json(json!({
        "orb_key": orb,
        "orb_fingerprint": orb.get(..12).unwrap_or(&orb),
        "delegated": delegated,
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
                // Authorize this Orb's device key to publish as this identity, and
                // persist that — signing then survives a reboot without the user's
                // secret having to stay unlocked in memory.
                let orb = orb_pubkey();
                let delegated = !orb.is_empty() && {
                    let d = issue_delegation(&sk, &acct.username, &orb, epoch_ms());
                    serde_json::to_vec_pretty(&d)
                        .ok()
                        .map(|b| write_private(DELEGATION_FILE, &b).is_ok())
                        .unwrap_or(false)
                };
                if let Ok(mut g) = unlocked().lock() {
                    *g = Some((acct.pubkey.clone(), sk));
                }
                s.active = acct.pubkey.clone();
                let _ = save(&s);
                json!({"ok": true, "active": acct.pubkey, "username": acct.username, "delegated": delegated})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unlock task failed"}));
    Json(v)
}

/// POST /api/publisher/lock — forget the in-memory key and revoke the stored
/// delegation.
pub async fn lock_account(State(_s): State<AppState>) -> Json<Value> {
    if let Ok(mut g) = unlocked().lock() {
        *g = None;
    }
    // Signing out also withdraws this Orb's right to publish as the user.
    // Shares keep being signed, but by the Orb key alone.
    let _ = std::fs::remove_file(DELEGATION_FILE);
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

    fn key(seed: u8) -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let pk = hex::encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    #[test]
    fn delegation_credits_the_user_only_when_genuine() {
        let (user, user_pk) = key(1);
        let (_orb, orb_pk) = key(2);
        let (mallory, _) = key(3);

        let d = issue_delegation(&user, " alice ", &orb_pk, 1000);
        assert_eq!(d.user_name, "alice");
        assert!(verify_delegation(&d));
        assert_eq!(identity_of(&orb_pk, Some(&d)), user_pk);

        // A delegation for a DIFFERENT orb key does not transfer credit.
        let (_other, other_pk) = key(4);
        assert_eq!(identity_of(&other_pk, Some(&d)), other_pk);

        // Forged: mallory signs a delegation that claims to be from alice.
        let mut forged = issue_delegation(&mallory, "alice", &orb_pk, 1000);
        forged.user_key = user_pk.clone();
        assert!(!verify_delegation(&forged));
        assert_eq!(identity_of(&orb_pk, Some(&forged)), orb_pk);

        // Tampering with any signed field breaks it.
        let mut t = d.clone();
        t.issued_ms += 1;
        assert!(!verify_delegation(&t));
        let mut t = d.clone();
        t.orb_key = other_pk;
        assert!(!verify_delegation(&t));

        // No delegation → credited to the signer itself.
        assert_eq!(identity_of(&orb_pk, None), orb_pk);
    }

    #[test]
    fn origin_digest_binds_every_file_and_field() {
        let files = vec![
            ("README.md".to_string(), 12u64, "aa".repeat(32)),
            ("model.gguf".to_string(), 4096u64, "bb".repeat(32)),
        ];
        let base = origin_digest("m", "https://x", true, 5, &files);
        assert_eq!(
            String::from_utf8(base.clone()).unwrap(),
            format!("aeon-origin/1\nm\nhttps://x\n1\n5\n{} 12 README.md\n{} 4096 model.gguf\n", "aa".repeat(32), "bb".repeat(32))
        );
        let mut swapped = files.clone();
        swapped[1].2 = "cc".repeat(32);
        assert_ne!(base, origin_digest("m", "https://x", true, 5, &swapped));
        assert_ne!(base, origin_digest("m", "https://x", false, 5, &files));
        assert_ne!(base, origin_digest("m2", "https://x", true, 5, &files));
        assert_ne!(base, origin_digest("m", "https://x", true, 5, &files[..1]));

        let (sk, pk) = key(9);
        let sig = B64.encode(sk.sign(&base).to_bytes());
        assert!(verify(&pk, &base, &sig));
        assert!(!verify(&pk, &origin_digest("m", "https://x", true, 5, &swapped), &sig));
    }

    /// The published test vectors are the contract other implementations
    /// (FrostByte's bridge) verify against. This rebuilds every signature from
    /// the fixed keys and fails if the file and the code ever disagree.
    /// Regenerate with AEON_WRITE_VECTORS=1 after an INTENTIONAL format change.
    #[test]
    fn published_vectors_match_this_implementation() {
        let (orb, orb_pk) = key(0x2a);
        let (user, user_pk) = key(0x01);
        let sign = |k: &SigningKey, m: &[u8]| B64.encode(k.sign(m).to_bytes());
        let text = |m: &[u8]| String::from_utf8(m.to_vec()).unwrap();

        let (cid, name, sha, added) = ("bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", "Example 1B Q4", "ab".repeat(32), 1_760_000_000_000_i64);
        let publication = publication_digest(cid, name, &sha, added);

        let delegation = issue_delegation(&user, "alice", &orb_pk, 1_760_000_000_001);
        let delegation_bytes = delegation_digest(&user_pk, &orb_pk, delegation.issued_ms);

        let files = vec![
            ("README.md".to_string(), 7u64, "11".repeat(32)),
            ("example-1b-q4.gguf".to_string(), 1_048_576u64, "ab".repeat(32)),
            ("sub/tokenizer.json".to_string(), 2u64, "22".repeat(32)),
        ];
        let origin = origin_digest(name, "https://huggingface.co/example/example-1b", true, 1_760_000_000_002, &files);

        let peer = "12D3KooWExamplePeerExamplePeerExamplePeerExamplePeer1";
        let binding = peer_binding_digest(&orb_pk, peer);

        let v = json!({
            "about": "Model Share signing test vectors. Signatures are Ed25519 (RFC 8032, deterministic) over the exact UTF-8 bytes of `signed_text`; keys are 32-byte hex, signatures standard base64. Secrets here are public test values.",
            "keys": {
                "orb_secret": hex::encode([0x2au8; 32]), "orb_key": orb_pk,
                "user_secret": hex::encode([0x01u8; 32]), "user_key": user_pk,
            },
            "publication": {
                "domain": "aeon-publish/1",
                "cid": cid, "name": name, "sha256": sha, "added_at_ms": added,
                "signed_text": text(&publication), "signer": orb_pk, "sig": sign(&orb, &publication),
            },
            "delegation": {
                "domain": "aeon-delegate/1",
                "user_key": user_pk, "user_name": "alice", "orb_key": orb_pk, "issued_ms": delegation.issued_ms,
                "signed_text": text(&delegation_bytes), "signer": user_pk, "sig": delegation.sig,
            },
            "origin": {
                "domain": "aeon-origin/1",
                "name": name, "source": "https://huggingface.co/example/example-1b", "verified": true,
                "signed_ms": 1_760_000_000_002_i64,
                "files": files.iter().map(|(p, s, h)| json!({"path": p, "size": s, "sha256": h})).collect::<Vec<_>>(),
                "signed_text": text(&origin), "signer": orb_pk, "sig": sign(&orb, &origin),
            },
            "peer_binding": {
                "domain": "aeon-peer/1",
                "orb_key": orb_pk, "peer_id": peer,
                "signed_text": text(&binding), "signer": orb_pk, "sig": sign(&orb, &binding),
            },
        });

        // Self-consistency: everything in the vector verifies.
        for section in ["publication", "delegation", "origin", "peer_binding"] {
            let s = &v[section];
            assert!(
                verify(s["signer"].as_str().unwrap(), s["signed_text"].as_str().unwrap().as_bytes(), s["sig"].as_str().unwrap()),
                "{section} does not verify"
            );
        }
        assert!(verify_delegation(&delegation));
        assert_eq!(identity_of(&orb_pk, Some(&delegation)), user_pk);

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/model-share-vectors.json");
        if std::env::var("AEON_WRITE_VECTORS").is_ok() {
            std::fs::write(path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
        }
        let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("docs/model-share-vectors.json")).unwrap();
        assert_eq!(on_disk, v, "docs/model-share-vectors.json is out of date");
    }

    #[test]
    fn peer_binding_is_specific_to_one_peer() {
        let (sk, pk) = key(5);
        let sig = B64.encode(sk.sign(&peer_binding_digest(&pk, "12D3KooWA")).to_bytes());
        assert!(verify(&pk, &peer_binding_digest(&pk, "12D3KooWA"), &sig));
        // Replayed by another peer, the same proof does not verify.
        assert!(!verify(&pk, &peer_binding_digest(&pk, "12D3KooWB"), &sig));
    }
}
