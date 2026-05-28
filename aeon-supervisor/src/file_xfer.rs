//! Lean two-way file transfer between the Pi and any device on usb0
//! (or wlan/eth, depending on toggle).
//!
//! Architecture:
//!   • Files live in /var/lib/aeon/files/. Owned by root; the supervisor
//!     (running as root) reads + writes directly.
//!   • The /api/files/* surface is admin-authenticated like everything
//!     else. The web UI on the Pi is what humans use to upload.
//!   • The /files/* surface (no /api prefix) is the TARGET-FACING
//!     surface. It's served as plain HTTP on a configurable port (off
//!     by default; toggled on per network in /etc/aeon/file-xfer.toml)
//!     so the host on usb0 can `curl` or browser-download without
//!     needing the admin password.
//!
//! Disabled by default. Toggle on via /api/files/config, and pick
//! which interface(s) the no-auth target server binds to.

use crate::api::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

const FILES_DIR: &str = "/var/lib/aeon/files";
const CONFIG_TOML: &str = "/etc/aeon/file-xfer.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FileXferConfig {
    /// Off by default. When on, the target-facing server listens on
    /// usb0 (and any additional interfaces in `extra_ifaces`).
    #[serde(default)]
    pub enabled: bool,
    /// Port for the no-auth target-facing server. 80 conflicts with
    /// the captive portal; default 8080.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Allow the target host to upload files (POST). Off by default —
    /// most users only want one-way push from Pi → target.
    #[serde(default)]
    pub allow_upload: bool,
}
fn default_port() -> u16 { 8080 }

fn read_config() -> FileXferConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &FileXferConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(parent) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)?;
    Ok(())
}

/// Reject path traversal / hidden / control-char filenames. Slug must
/// be plain ascii + dash/underscore/dot. Length cap 200.
fn safe_slug(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.starts_with('/')
        && !name.contains("..")
        && name.chars().all(|c|
            c.is_ascii_alphanumeric()
            || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')' | '+' | '[' | ']' | '@'))
}

fn file_path(slug: &str) -> Option<PathBuf> {
    if !safe_slug(slug) { return None; }
    Some(PathBuf::from(FILES_DIR).join(slug))
}

fn ensure_dir() {
    let _ = std::fs::create_dir_all(FILES_DIR);
}

// ── Admin / config surface ──────────────────────────────────────────

/// GET /api/files/config
pub async fn get_config(State(_state): State<AppState>) -> Json<Value> {
    let c = read_config();
    Json(json!({
        "ok": true,
        "enabled": c.enabled,
        "port": c.port,
        "allow_upload": c.allow_upload,
    }))
}

#[derive(Deserialize)]
pub struct ConfigPut {
    #[serde(default)] pub enabled: Option<bool>,
    #[serde(default)] pub port: Option<u16>,
    #[serde(default)] pub allow_upload: Option<bool>,
}

/// PUT /api/files/config — toggle the target-facing server on/off
/// and tune its port + upload permission. The HTTP listener is
/// (re)spawned by main.rs reading this config on startup; flips
/// require an aeon-supervisor restart to take effect for now (UI
/// shows that hint).
pub async fn put_config(
    State(_state): State<AppState>,
    Json(req): Json<ConfigPut>,
) -> impl IntoResponse {
    let mut c = read_config();
    if let Some(v) = req.enabled { c.enabled = v; }
    if let Some(v) = req.port {
        if v < 1024 {
            return (StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": "port must be ≥ 1024"}))).into_response();
        }
        c.port = v;
    }
    if let Some(v) = req.allow_upload { c.allow_upload = v; }
    if let Err(e) = write_config(&c) {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")}))).into_response();
    }
    Json(json!({"ok": true, "needs_restart": true})).into_response()
}

// ── File CRUD (admin surface; same for both UIs) ────────────────────

/// GET /api/files — list files + metadata.
pub async fn list_files(State(_state): State<AppState>) -> Json<Value> {
    ensure_dir();
    let mut entries: Vec<Value> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(FILES_DIR) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !safe_slug(&name) { continue; }
            let meta = match e.metadata() { Ok(m) => m, _ => continue };
            if !meta.is_file() { continue; }
            entries.push(json!({
                "name": name,
                "size_bytes": meta.size(),
                "modified_ms": meta.mtime() * 1000,
            }));
        }
    }
    entries.sort_by(|a, b|
        b["modified_ms"].as_i64().unwrap_or(0)
            .cmp(&a["modified_ms"].as_i64().unwrap_or(0)));
    Json(json!({
        "ok": true,
        "files": entries,
        "dir": FILES_DIR,
    }))
}

/// GET /api/files/:name — stream a file back. Used by both the admin
/// UI and (if enabled) the target-facing http listener.
pub async fn download(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Response<Body> {
    let Some(path) = file_path(&name) else {
        return (StatusCode::BAD_REQUEST, "invalid filename").into_response();
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "no such file").into_response(),
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', "_"));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build").into_response())
}

/// DELETE /api/files/:name
pub async fn delete_file(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let Some(path) = file_path(&name) else {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "invalid filename"}))).into_response();
    };
    match std::fs::remove_file(&path) {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound =>
            (StatusCode::NOT_FOUND, Json(json!({"ok": false, "err": "no such file"})))
                .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("delete: {e}")}))).into_response(),
    }
}

/// POST /api/files/upload — streamed upload (Content-Disposition
/// filename header carries the name). Same pattern as the ISO upload
/// in storage.rs.
pub async fn upload(
    State(_state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> impl IntoResponse {
    ensure_dir();
    let filename = headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            // "attachment; filename=\"foo.txt\""
            s.split(';').find_map(|tok| {
                let t = tok.trim();
                if let Some(v) = t.strip_prefix("filename=") {
                    Some(v.trim_matches('"').to_string())
                } else { None }
            })
        })
        .unwrap_or_else(|| format!("upload-{}", crate::audit::actor_for(
            &crate::auth::Identity {
                user: "anon".into(),
                scope: crate::auth::TokenScope::Read,
            }
        )));
    if !safe_slug(&filename) {
        return (StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": "invalid filename"}))).into_response();
    }
    let path = PathBuf::from(FILES_DIR).join(&filename);
    let tmp = path.with_extension("part");
    // Stream the body to disk so multi-GB uploads don't blow up RAM.
    let mut writer = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("create: {e}")}))).into_response(),
    };
    use tokio::io::AsyncWriteExt;
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
        match chunk {
            Ok(b) => {
                if let Err(e) = writer.write_all(&b).await {
                    let _ = std::fs::remove_file(&tmp);
                    return (StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"ok": false, "err": format!("write: {e}")}))).into_response();
                }
                total += b.len() as u64;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return (StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"ok": false, "err": format!("read body: {e}")}))).into_response();
            }
        }
    }
    let _ = writer.sync_all().await;
    drop(writer);
    if let Err(e) = std::fs::rename(&tmp, &path) {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("rename: {e}")}))).into_response();
    }
    Json(json!({"ok": true, "name": filename, "size_bytes": total})).into_response()
}

// ── Target-facing public server ────────────────────────────────────

/// Spawn the no-auth target-facing HTTP listener if config.enabled.
/// Called from main.rs at startup.
pub async fn maybe_spawn_target_server() {
    let cfg = read_config();
    if !cfg.enabled {
        return;
    }
    // Bind on all interfaces but iptables INPUT rules (added by
    // aeon-usb-net.sh in isolation/restricted modes) gate which
    // clients can actually reach the port. We rely on those rules
    // rather than a bind-specific address because usb0's IP might
    // not exist yet at supervisor startup.
    let addr = format!("0.0.0.0:{}", cfg.port);
    let allow_upload = cfg.allow_upload;

    let mut router = axum::Router::new()
        .route("/", axum::routing::get(target_index))
        .route("/files/", axum::routing::get(target_index))
        .route("/files/:name", axum::routing::get(target_download))
        .layer(tower_http::trace::TraceLayer::new_for_http());
    if allow_upload {
        router = router.route("/files/upload",
            axum::routing::post(target_upload)
                .layer(axum::extract::DefaultBodyLimit::disable()));
    }
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("file_xfer target listener bind {addr} failed: {e}");
                return;
            }
        };
        tracing::info!("file_xfer target listener on {addr} (allow_upload={allow_upload})");
        let _ = axum::serve(listener, router).await;
    });
}

/// Plain HTML index for the target-facing endpoint. Tiny inline page
/// so it works on every browser without any JS framework.
async fn target_index() -> Response<Body> {
    ensure_dir();
    let cfg = read_config();
    let mut rows = String::new();
    if let Ok(dir) = std::fs::read_dir(FILES_DIR) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !safe_slug(&name) { continue; }
            let meta = match e.metadata() { Ok(m) => m, _ => continue };
            if !meta.is_file() { continue; }
            rows.push_str(&format!(
                r#"<tr>
                    <td><a href="/files/{name}">{name}</a></td>
                    <td>{size}</td>
                </tr>"#,
                name = html_escape(&name),
                size = fmt_bytes(meta.size()),
            ));
        }
    }
    let upload_form = if cfg.allow_upload {
        r#"<h2>Upload</h2>
           <form action="/files/upload" method="post" enctype="multipart/form-data">
             <input type="file" name="file" required>
             <button type="submit">Upload</button>
           </form>"#
    } else {
        ""
    };
    let html = format!(
        r#"<!DOCTYPE html><html><head>
<meta charset="utf-8"><title>aeon-magick files</title>
<style>
  body{{font-family:system-ui,sans-serif;background:#0a0a0a;color:#e4e4e7;padding:2em;max-width:760px;margin:0 auto}}
  h1{{font-size:1.2em;color:#d946ef;font-family:ui-monospace,monospace;letter-spacing:.1em}}
  h2{{font-size:1em;margin-top:2em;color:#a1a1aa}}
  table{{width:100%;border-collapse:collapse;margin-top:.5em}}
  td{{padding:.5em;border-bottom:1px solid #27272a;font-family:ui-monospace,monospace;font-size:.85em}}
  td:last-child{{text-align:right;color:#71717a}}
  a{{color:#d946ef;text-decoration:none}}a:hover{{text-decoration:underline}}
  form{{padding:1em;background:#18181b;border:1px solid #27272a;border-radius:.5em}}
  button{{background:#d946ef;color:white;border:0;padding:.5em 1em;border-radius:.25em;cursor:pointer;font-family:inherit}}
  button:hover{{background:#c026d3}}
  .empty{{color:#71717a;font-style:italic}}
</style></head><body>
<h1>aeon-magick — file transfer</h1>
<h2>Files on the Pi</h2>
{table_or_empty}
{upload_form}
<p style="color:#52525b;font-size:.8em;margin-top:3em">
Two-way transfer between this device and the Pi. Files persist on the Pi at <code>/var/lib/aeon/files/</code>.
</p>
</body></html>"#,
        table_or_empty = if rows.is_empty() {
            r#"<p class="empty">No files yet. The Pi operator uploads them via the admin UI.</p>"#.to_string()
        } else {
            format!("<table><tr><th></th><th></th></tr>{rows}</table>")
        },
        upload_form = upload_form,
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build").into_response())
}

async fn target_download(Path(name): Path<String>) -> Response<Body> {
    let Some(path) = file_path(&name) else {
        return (StatusCode::BAD_REQUEST, "invalid").into_response();
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "no such file").into_response(),
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = Body::from_stream(stream);
    let disposition = format!("attachment; filename=\"{}\"", name.replace('"', "_"));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(body)
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build").into_response())
}

async fn target_upload(body: axum::body::Body) -> Response<Body> {
    // Mirror of the admin upload but without the Content-Disposition
    // sniffing — the public form posts multipart/form-data. We accept
    // the first file part and use its filename. Keep it simple by
    // streaming directly to a tmp filename based on epoch ms, then
    // rename to the multipart-provided filename if we can parse one
    // out of the body. For now: name files "uploaded-<epoch>.bin" if
    // we can't parse a header — operators can rename in the admin UI.
    ensure_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = PathBuf::from(FILES_DIR).join(format!("uploaded-{ts}.bin"));
    let bytes = match body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR,
            format!("read body: {e}")).into_response(),
    };
    if let Err(e) = std::fs::write(&path, &bytes) {
        return (StatusCode::INTERNAL_SERVER_ERROR,
            format!("write: {e}")).into_response();
    }
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, "/")
        .body(Body::empty())
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build").into_response())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn fmt_bytes(b: u64) -> String {
    if b < 1024 { format!("{b} B") }
    else if b < 1024 * 1024 { format!("{:.1} kB", b as f64 / 1024.0) }
    else if b < 1024 * 1024 * 1024 { format!("{:.1} MB", b as f64 / 1024.0 / 1024.0) }
    else { format!("{:.2} GB", b as f64 / 1024.0 / 1024.0 / 1024.0) }
}
