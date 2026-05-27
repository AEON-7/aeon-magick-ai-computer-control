//! Manage trusted SSH public keys in the admin user's authorized_keys.
//!
//! Routes:
//!   GET    /api/ssh/keys           — list keys (with fingerprints)
//!   POST   /api/ssh/keys           — add a key (body: {"key": "<openssh-format>"})
//!   DELETE /api/ssh/keys/:id       — remove one
//!
//! We never accept private keys. We validate the OpenSSH public-key
//! format (`<type> <base64> [comment]`) before writing. Fingerprints
//! are SHA256-of-base64-decoded-blob, matching `ssh-keygen -lf`.

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// authorized_keys path. The `admin` user is the SSH-accessible account
/// (created by pi-gen's FIRST_USER_NAME). We never touch root's keys.
fn auth_keys_path() -> PathBuf {
    PathBuf::from("/home/admin/.ssh/authorized_keys")
}

const VALID_KEY_TYPES: &[&str] = &[
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// Parsed key line.
struct ParsedKey {
    type_: String,
    blob: String,           // base64
    comment: String,
}

fn parse_key_line(line: &str) -> Result<ParsedKey, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Err("empty / comment".into());
    }
    let parts: Vec<&str> = line.splitn(3, char::is_whitespace).collect();
    if parts.len() < 2 {
        return Err("must be: <type> <base64> [comment]".into());
    }
    let type_ = parts[0].to_string();
    let blob = parts[1].to_string();
    if !VALID_KEY_TYPES.contains(&type_.as_str()) {
        return Err(format!("unsupported key type '{}'", type_));
    }
    // Reject private-key markers — a confused user pasting a private
    // key into this field would be a disaster.
    if blob.contains("BEGIN")
        || blob.contains("PRIVATE")
        || line.contains("BEGIN OPENSSH")
    {
        return Err("looks like a private key — paste the PUBLIC key only".into());
    }
    // Base64 validation.
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&blob)
        .map_err(|_| "key blob is not valid base64".to_string())?;
    if decoded.is_empty() || decoded.len() < 16 {
        return Err("key blob too short".into());
    }
    let comment = parts.get(2).copied().unwrap_or("").to_string();
    Ok(ParsedKey {
        type_,
        blob,
        comment,
    })
}

fn fingerprint_for(blob_b64: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let b64 =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{}", b64)
}

/// Stable per-key id = first 16 hex of SHA256(blob). Survives re-adds.
fn id_for(blob_b64: &str) -> String {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob_b64)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

/// GET /api/ssh/keys
pub async fn list_keys(State(_state): State<AppState>) -> Json<Value> {
    let text = std::fs::read_to_string(auth_keys_path()).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Ok(p) = parse_key_line(line) else { continue };
        out.push(json!({
            "id": id_for(&p.blob),
            "type": p.type_,
            "comment": p.comment,
            "fingerprint": fingerprint_for(&p.blob),
            // Approximate "added at" via file mtime — not perfect (one
            // mtime for all keys), but good enough for the UI's sort.
            "added_at_ms": std::fs::metadata(auth_keys_path())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }));
    }
    Json(json!({"ok": true, "keys": out}))
}

#[derive(Deserialize)]
pub struct AddKeyReq {
    pub key: String,
}

/// POST /api/ssh/keys
pub async fn add_key(
    State(_state): State<AppState>,
    Json(req): Json<AddKeyReq>,
) -> impl IntoResponse {
    let parsed = match parse_key_line(&req.key) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": e})),
            )
                .into_response();
        }
    };
    let id = id_for(&parsed.blob);

    // Make sure ~/.ssh exists with the right perms (sshd refuses to read
    // a world-readable authorized_keys).
    let path = auth_keys_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::set_permissions(
            parent,
            std::fs::Permissions::from_mode(0o700),
        );
    }

    // Read existing → de-dup by id → append → atomic-rename.
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    let already = existing.lines().any(|l| {
        parse_key_line(l)
            .map(|p| id_for(&p.blob) == id)
            .unwrap_or(false)
    });
    if !already {
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str(&format!(
            "{} {} {}\n",
            parsed.type_, parsed.blob, parsed.comment
        ));
    }
    // Atomic write via tmpfile + rename.
    let tmp = path.with_extension("authorized_keys.tmp");
    {
        let mut f = match std::fs::File::create(&tmp) {
            Ok(f) => f,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "err": format!("write: {e}")})),
                )
                    .into_response();
            }
        };
        let _ = f.write_all(existing.as_bytes());
        let _ = f.sync_all();
    }
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    if let Err(e) = std::fs::rename(&tmp, &path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rename: {e}")})),
        )
            .into_response();
    }
    // Chown to admin:admin since the supervisor runs as root.
    let _ = std::process::Command::new("chown")
        .args(["admin:admin", &path.to_string_lossy()])
        .status();
    Json(json!({"ok": true, "id": id})).into_response()
}

/// DELETE /api/ssh/keys/:id
pub async fn remove_key(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let path = auth_keys_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept = String::new();
    let mut removed = false;
    for line in text.lines() {
        let is_target = parse_key_line(line)
            .map(|p| id_for(&p.blob) == id)
            .unwrap_or(false);
        if is_target {
            removed = true;
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if !removed {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "err": "unknown key id"})),
        )
            .into_response();
    }
    if let Err(e) = std::fs::write(&path, kept) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("write: {e}")})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}
