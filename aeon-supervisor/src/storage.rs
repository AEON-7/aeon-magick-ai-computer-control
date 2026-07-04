//! /api/storage — manage uploaded ISOs and which one is the active
//! USB-CDROM (presented to the connected host as a bootable disk).
//!
//! Storage layout:
//!
//!   /var/lib/aeon/iso/<slug>.iso     — the uploaded ISO bytes
//!   /var/lib/aeon/iso/<slug>.meta    — companion JSON: display name,
//!                                       size, sha256, upload time
//!   /etc/aeon/storage.toml           — active slug (read by aeon-hid)
//!
//! Slugs are derived from the uploaded filename: lowercased, spaces +
//! special chars → hyphens, deduplicated against existing slugs.

use crate::api::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::SystemTime;

const ISO_DIR: &str = "/var/lib/aeon/iso";
const STORAGE_TOML: &str = "/etc/aeon/storage.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StorageToml {
    #[serde(default)]
    mass_storage: MassStorageSection,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MassStorageSection {
    #[serde(default)]
    active: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IsoMeta {
    slug: String,
    display: String,
    size_bytes: u64,
    sha256: Option<String>,
    uploaded_at_ms: i64,
}

fn read_storage_toml() -> StorageToml {
    std::fs::read_to_string(STORAGE_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_storage_toml(s: &StorageToml) -> std::io::Result<()> {
    let text = toml::to_string_pretty(s)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(parent) = std::path::Path::new(STORAGE_TOML).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(STORAGE_TOML, text)?;
    Ok(())
}

/// Scan /var/lib/aeon/iso for *.iso files + their .meta sidecars.
fn list_isos() -> Vec<IsoMeta> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(ISO_DIR) else { return out; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("iso") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let meta_path = path.with_extension("meta");
        let meta: Option<IsoMeta> = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
        out.push(match meta {
            Some(m) => m,
            None => IsoMeta {
                slug: stem.clone(),
                display: stem,
                size_bytes,
                sha256: None,
                uploaded_at_ms: 0,
            },
        });
    }
    // Newest first
    out.sort_by(|a, b| b.uploaded_at_ms.cmp(&a.uploaded_at_ms));
    out
}

fn write_meta(slug: &str, meta: &IsoMeta) -> std::io::Result<()> {
    let path = PathBuf::from(ISO_DIR).join(format!("{slug}.meta"));
    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| std::io::Error::other(format!("json: {e}")))?;
    std::fs::write(path, json)
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn slugify(name: &str) -> String {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    stem.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn dedupe_slug(base: &str) -> String {
    if !std::path::Path::new(ISO_DIR).join(format!("{base}.iso")).exists() {
        return base.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !std::path::Path::new(ISO_DIR).join(format!("{candidate}.iso")).exists() {
            return candidate;
        }
    }
    format!("{base}-{}", epoch_ms())
}

fn free_space_bytes() -> u64 {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let path = std::ffi::CString::new(ISO_DIR).unwrap_or_else(|_| std::ffi::CString::new("/var").unwrap());
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return 0;
    }
    (stat.f_bavail as u64) * (stat.f_frsize as u64)
}

/// GET /api/storage — list all ISOs + active slug + free disk space.
pub async fn list(State(_state): State<AppState>) -> Json<Value> {
    let cfg = read_storage_toml();
    let isos = list_isos();
    Json(json!({
        "ok": true,
        "active": cfg.mass_storage.active,
        "isos": isos,
        "free_bytes": free_space_bytes(),
        "iso_dir": ISO_DIR,
    }))
}

#[derive(Deserialize)]
pub struct ActivateReq {
    /// Slug from the library. Empty string = eject (detach CDROM).
    pub slug: String,
}

/// PUT /api/storage/active — set or clear the active ISO. Triggers
/// aeon-hid restart so the gadget composite picks up the new state.
pub async fn put_active(
    State(_state): State<AppState>,
    Json(req): Json<ActivateReq>,
) -> impl IntoResponse {
    let slug = req.slug.trim();
    // Validate: empty (eject) or matches an existing slug.
    if !slug.is_empty() {
        // Sanity-check the slug doesn't contain path traversal.
        if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "invalid slug"})),
            )
                .into_response();
        }
        let iso_path = PathBuf::from(ISO_DIR).join(format!("{slug}.iso"));
        if !iso_path.exists() {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"ok": false, "err": format!("no ISO with slug '{slug}'")})),
            )
                .into_response();
        }
    }

    let mut cfg = read_storage_toml();
    cfg.mass_storage.active = slug.to_string();
    if let Err(e) = write_storage_toml(&cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }

    // Restart aeon-hid so the new gadget composite is built. The host
    // sees a brief USB disconnect + reconnect.
    let _ = std::process::Command::new("systemctl")
        .args(["restart", "aeon-hid.service"])
        .status();

    Json(json!({
        "ok": true,
        "active": cfg.mass_storage.active,
        "note": "aeon-hid restarted; host will see a brief USB re-enumeration",
    }))
    .into_response()
}

/// DELETE /api/storage/:slug — delete an uploaded ISO. Refuses if the
/// slug is currently active.
pub async fn delete(
    State(_state): State<AppState>,
    Path(slug): Path<String>,
) -> impl IntoResponse {
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "invalid slug"})),
        )
            .into_response();
    }
    let cfg = read_storage_toml();
    if cfg.mass_storage.active == slug {
        return (
            StatusCode::CONFLICT,
            Json(json!({"ok": false, "err": "cannot delete active ISO — eject first by PUTting active=''"})),
        )
            .into_response();
    }
    let iso = PathBuf::from(ISO_DIR).join(format!("{slug}.iso"));
    let meta = PathBuf::from(ISO_DIR).join(format!("{slug}.meta"));
    let _ = std::fs::remove_file(&iso);
    let _ = std::fs::remove_file(&meta);
    Json(json!({"ok": true})).into_response()
}

/// POST /api/storage/upload — stream a multipart-encoded ISO into the
/// library. The Content-Disposition `filename` becomes the slug
/// source. Returns the slug + metadata once writing completes.
///
/// NOTE: this is a STREAMING upload — the body is written to disk as
/// it arrives so we don't need to buffer multi-GB ISOs in RAM. The
/// supervisor's max-body axum limit needs to be raised, which we
/// handle in api.rs via DefaultBodyLimit::disable() for this route.
pub async fn upload(
    State(_state): State<AppState>,
    request: axum::extract::Request<Body>,
) -> Response {
    use axum::http::header;
    use futures::StreamExt;

    // Extract filename hint from Content-Disposition. If absent, fall
    // back to a timestamp-based name.
    let filename = request
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_cd_filename)
        .unwrap_or_else(|| format!("upload-{}.iso", epoch_ms()));

    let base_slug = slugify(&filename);
    if base_slug.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "filename produces empty slug"})),
        )
            .into_response();
    }
    let slug = dedupe_slug(&base_slug);

    // Ensure the iso dir exists
    if let Err(e) = std::fs::create_dir_all(ISO_DIR) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("mkdir {ISO_DIR}: {e}")})),
        )
            .into_response();
    }

    let iso_path = PathBuf::from(ISO_DIR).join(format!("{slug}.iso"));

    // Stream the request body to the file. We use a temp suffix + atomic
    // rename so a half-uploaded file never appears as a complete ISO in
    // the library listing.
    let tmp_path = iso_path.with_extension("iso.partial");
    let mut file = match tokio::fs::File::create(&tmp_path).await {
        Ok(f) => f,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("create tmp: {e}")})),
            )
                .into_response();
        }
    };

    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let mut hasher = Sha256::new();
    let mut bytes_written: u64 = 0;
    let mut stream = request.into_body().into_data_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "err": format!("stream read: {e}")})),
                )
                    .into_response();
            }
        };
        hasher.update(&bytes);
        if let Err(e) = file.write_all(&bytes).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "err": format!("write: {e}")})),
            )
                .into_response();
        }
        bytes_written += bytes.len() as u64;
    }
    if let Err(e) = file.flush().await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("flush: {e}")})),
        )
            .into_response();
    }
    drop(file);

    // Atomic rename
    if let Err(e) = tokio::fs::rename(&tmp_path, &iso_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rename: {e}")})),
        )
            .into_response();
    }

    let sha256_hex = hex::encode(hasher.finalize());
    let meta = IsoMeta {
        slug: slug.clone(),
        display: filename.clone(),
        size_bytes: bytes_written,
        sha256: Some(sha256_hex.clone()),
        uploaded_at_ms: epoch_ms(),
    };
    let _ = write_meta(&slug, &meta);

    Json(json!({
        "ok": true,
        "slug": slug,
        "display": filename,
        "size_bytes": bytes_written,
        "sha256": sha256_hex,
    }))
    .into_response()
}

type Response = axum::response::Response;

/// Parse a Content-Disposition `filename="..."` parameter. Returns
/// None if the header doesn't include one. (Shared with the IPFS model
/// upload, which uses the same raw-body + Content-Disposition contract.)
pub(crate) fn parse_cd_filename(cd: &str) -> Option<String> {
    for part in cd.split(';') {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("filename=") {
            let v = rest.trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
        // RFC 5987 encoded form
        if let Some(rest) = p.strip_prefix("filename*=UTF-8''") {
            if let Ok(decoded) = urlencoding::decode(rest) {
                let v = decoded.to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}
