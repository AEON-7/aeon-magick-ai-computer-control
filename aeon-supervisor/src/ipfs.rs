//! IPFS — a local kubo node + HTTP gateway (reachable from any device, LAN or
//! Tailscale) with an adjustable storage cap. Host content on the decentralized
//! web. Drives the `aeon-ipfs` infra script. OFF by default; admin-gated over
//! REST. Agents pin/add content via MCP (ipfs_pin / ipfs_add).
//!
//! AI-model sharing (v111): upload a model file → it streams to a staging
//! area, gets `ipfs add`ed (content moves into the blockstore, pinned), and a
//! catalog entry (name/size/sha256/kind/…/origin) is recorded in
//! /var/lib/aeon/ipfs-models/catalog.json. The catalog rides the fleet
//! heartbeat (fleet.rs self_status → "ipfs_models"), so every Orb sees a
//! federated index of models shared by every fleet peer — and pinning a
//! peer's model (models/fetch) both fetches it over the IPFS swarm and
//! re-shares it from this Orb (pinned = hosted).

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/ipfs.toml";
const SCRIPT: &str = "/usr/local/bin/aeon-ipfs";
const MODELS_DIR: &str = "/var/lib/aeon/ipfs-models";
const CATALOG_JSON: &str = "/var/lib/aeon/ipfs-models/catalog.json";
/// kubo's own repo config — read directly (fast, no shell) for the PeerID the
/// fleet heartbeat advertises so peers can swarm-connect before fetching.
const IPFS_REPO_CONFIG: &str = "/var/lib/aeon/ipfs/config";

fn default_storage() -> String {
    "10GB".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IpfsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_storage")]
    pub storage_max: String,
}

impl Default for IpfsConfig {
    fn default() -> Self {
        Self { enabled: false, storage_max: default_storage() }
    }
}

fn read_config() -> IpfsConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &IpfsConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

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
        .unwrap_or_else(|| json!({"installed": false, "daemon": "inactive"}))
}

/// GET /api/ipfs/status — feature + node/gateway state. Admin-gated.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let st = script_status();
        let get = |k: &str, d: Value| st.get(k).cloned().unwrap_or(d);
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "installed": get("installed", json!(false)),
            "daemon": get("daemon", json!("inactive")),
            "version": get("version", json!("")),
            "peer_id": get("peer_id", json!("")),
            "peers": get("peers", json!(0)),
            "repo_bytes": get("repo_bytes", json!(0)),
            "storage_max": get("storage_max", json!(cfg.storage_max)),
            "gateway_port": get("gateway_port", json!(8080)),
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/ipfs/enable — install kubo (first time downloads ~30 MB) + start the
/// node. Non-blocking: persists config + brings the node up in the background;
/// the dashboard polls /status until the daemon is active. Admin-gated.
pub async fn enable(State(_s): State<AppState>) -> Json<Value> {
    let saved = tokio::task::spawn_blocking(|| -> bool {
        let mut cfg = read_config();
        cfg.enabled = true;
        write_config(&cfg).is_ok()
    })
    .await
    .unwrap_or(false);
    if saved {
        tokio::task::spawn_blocking(|| {
            let _ = run_script(&["up"]);
        });
        Json(json!({"ok": true, "starting": true}))
    } else {
        Json(json!({"ok": false, "err": "write config failed"}))
    }
}

/// POST /api/ipfs/disable — stop the node. Repo + pins persist. Admin-gated.
pub async fn disable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let mut cfg = read_config();
        cfg.enabled = false;
        let _ = write_config(&cfg);
        let _ = run_script(&["down"]);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "disable task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct StorageReq {
    pub size: String,
}

/// POST /api/ipfs/storage — set the on-disk storage cap (e.g. "20GB"). Admin-gated.
pub async fn set_storage(State(_s): State<AppState>, Json(req): Json<StorageReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["storage", &req.size]) {
            Ok(s) => {
                let mut cfg = read_config();
                cfg.storage_max = if s.is_empty() { req.size.clone() } else { s.clone() };
                let _ = write_config(&cfg);
                json!({"ok": true, "storage_max": cfg.storage_max})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "storage task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct CidReq {
    pub cid: String,
}

/// POST /api/ipfs/pin — pin a CID so this node hosts + keeps it. Admin over REST;
/// also the ipfs_pin MCP tool.
pub async fn pin(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["pin", &req.cid]) {
            Ok(o) => json!({"ok": true, "out": o}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "pin task failed"}));
    Json(v)
}

/// POST /api/ipfs/unpin — drop a pinned CID. Admin-gated.
pub async fn unpin(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let _ = run_script(&["unpin", &req.cid]);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unpin task failed"}));
    Json(v)
}

/// GET /api/ipfs/pins — list pinned CIDs. Admin-gated.
pub async fn pins(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let out = run_script(&["pins"]).unwrap_or_default();
        let cids: Vec<String> = out.lines().filter(|l| !l.is_empty()).map(String::from).collect();
        json!({"ok": true, "pins": cids})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "pins task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct AddReq {
    pub path: String,
}

/// POST /api/ipfs/add — add a local file/dir to IPFS, returning its root CID (so
/// it's reachable at <gateway>/ipfs/<cid> and on any IPFS gateway). Admin over
/// REST; also the ipfs_add MCP tool so an agent can publish a site/app it built.
pub async fn add_path(State(_s): State<AppState>, Json(req): Json<AddReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["add", &req.path]) {
            Ok(cid) if !cid.is_empty() => json!({"ok": true, "cid": cid}),
            Ok(_) => json!({"ok": false, "err": "add produced no CID"}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "add task failed"}));
    Json(v)
}

// ── AI model sharing ────────────────────────────────────────────────────────

/// One shared model. `origin_*` is the fleet identity of the Orb that FIRST
/// shared it and travels with the entry when peers mirror it, so provenance
/// survives replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub cid: String,
    pub name: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub kind: String, // llm | vlm | vision | stt | tts | other (free text)
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub added_at_ms: i64,
    #[serde(default)]
    pub origin_id: String,
    #[serde(default)]
    pub origin_label: String,
}

fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn read_catalog() -> Vec<ModelEntry> {
    std::fs::read_to_string(CATALOG_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_catalog(entries: &[ModelEntry]) -> std::io::Result<()> {
    std::fs::create_dir_all(MODELS_DIR)?;
    let tmp = format!("{CATALOG_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(entries).unwrap_or_default())?;
    std::fs::rename(tmp, CATALOG_JSON)
}

/// Append an entry unless the CID is already cataloged (idempotent mirror).
fn catalog_add(entry: ModelEntry) {
    let mut entries = read_catalog();
    if !entries.iter().any(|e| e.cid == entry.cid) {
        entries.push(entry);
        let _ = write_catalog(&entries);
    }
}

/// In-flight model work: name-or-cid → phase string ("adding to IPFS…",
/// "fetching from swarm…", "error: …"). Errors stay until the next attempt
/// for the same key so the UI can surface them.
static TASKS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn tasks() -> &'static Mutex<HashMap<String, String>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn task_set(key: &str, phase: &str) {
    if let Ok(mut t) = tasks().lock() {
        t.insert(key.to_string(), phase.to_string());
    }
}

fn task_clear(key: &str) {
    if let Ok(mut t) = tasks().lock() {
        t.remove(key);
    }
}

/// CIDv0 (Qm…) / CIDv1 (baf…) shape check. Args reach kubo via an exec arg
/// vector (never a shell string), so this is belt-and-braces, not the only
/// injection defense.
fn valid_cid(cid: &str) -> bool {
    cid.len() >= 10
        && cid.len() <= 128
        && cid.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Fleet-facing summary: what this Orb shares, plus the kubo PeerID so peers
/// can swarm-connect straight to us before pinning. Pure fs reads — called
/// synchronously from fleet::self_status on every heartbeat.
pub fn shared_models() -> Value {
    let cfg = read_config();
    if !cfg.enabled {
        return json!({"enabled": false, "models": []});
    }
    let peer_id = std::fs::read_to_string(IPFS_REPO_CONFIG)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("Identity")
                .and_then(|i| i.get("PeerID"))
                .and_then(|p| p.as_str())
                .map(String::from)
        })
        .unwrap_or_default();
    json!({
        "enabled": true,
        "peer_id": peer_id,
        "models": read_catalog(),
    })
}

/// GET /api/ipfs/models — local catalog + in-flight upload/fetch phases.
pub async fn models(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let t: HashMap<String, String> =
            tasks().lock().map(|g| g.clone()).unwrap_or_default();
        json!({"ok": true, "models": read_catalog(), "tasks": t})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "models task failed"}));
    Json(v)
}

/// POST /api/ipfs/models/upload?kind=&desc=&license= — streaming model upload.
/// Body = raw file bytes (Content-Disposition carries the filename), same
/// pattern as the ISO upload (body limit disabled in api.rs). The bytes stream
/// to a staging file; `ipfs add` (which pins) then moves the content into the
/// blockstore in the background and the staging copy is deleted — models are
/// stored once, in IPFS. Poll /models until the task for this name clears.
pub async fn upload_model(
    State(_s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
    request: axum::extract::Request<axum::body::Body>,
) -> Json<Value> {
    use axum::http::header;
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;

    let filename = request
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(crate::storage::parse_cd_filename)
        .unwrap_or_else(|| format!("model-{}", epoch_ms()));
    // Keep the display name; sanitize only the staging path component.
    let safe: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();

    let staging = std::path::Path::new(MODELS_DIR).join("staging");
    if let Err(e) = std::fs::create_dir_all(&staging) {
        return Json(json!({"ok": false, "err": format!("mkdir staging: {e}")}));
    }
    let tmp_path = staging.join(format!("{safe}.partial"));
    let final_path = staging.join(&safe);

    let mut file = match tokio::fs::File::create(&tmp_path).await {
        Ok(f) => f,
        Err(e) => return Json(json!({"ok": false, "err": format!("create staging: {e}")})),
    };
    let mut hasher = Sha256::new();
    let mut bytes_written: u64 = 0;
    let mut stream = request.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Json(json!({"ok": false, "err": format!("stream read: {e}")}));
            }
        };
        hasher.update(&bytes);
        if let Err(e) = file.write_all(&bytes).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Json(json!({"ok": false, "err": format!("write: {e}")}));
        }
        bytes_written += bytes.len() as u64;
    }
    if let Err(e) = file.flush().await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Json(json!({"ok": false, "err": format!("flush: {e}")}));
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Json(json!({"ok": false, "err": format!("rename: {e}")}));
    }

    let sha256_hex = hex::encode(hasher.finalize());
    let (origin_id, origin_label) = crate::fleet::identity();
    let entry_seed = ModelEntry {
        cid: String::new(), // filled after `ipfs add`
        name: filename.clone(),
        size_bytes: bytes_written,
        sha256: sha256_hex.clone(),
        kind: q.get("kind").cloned().unwrap_or_default(),
        desc: q.get("desc").cloned().unwrap_or_default(),
        license: q.get("license").cloned().unwrap_or_default(),
        added_at_ms: epoch_ms(),
        origin_id,
        origin_label,
    };

    task_set(&filename, "adding to IPFS…");
    let path_str = final_path.to_string_lossy().to_string();
    let key = filename.clone();
    tokio::task::spawn_blocking(move || {
        match run_script(&["add", &path_str]) {
            Ok(cid) if !cid.is_empty() => {
                let mut entry = entry_seed;
                entry.cid = cid;
                catalog_add(entry);
                let _ = std::fs::remove_file(&path_str); // bytes now live in the blockstore
                task_clear(&key);
            }
            Ok(_) => task_set(&key, "error: add produced no CID"),
            Err(e) => task_set(&key, &format!("error: {e}")),
        }
    });

    Json(json!({
        "ok": true,
        "name": filename,
        "size_bytes": bytes_written,
        "sha256": sha256_hex,
        "processing": true,
    }))
}

#[derive(Deserialize)]
pub struct FetchReq {
    pub cid: String,
    pub name: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub origin_id: String,
    #[serde(default)]
    pub origin_label: String,
    /// Source Orb hints for a direct swarm connection (LAN/tailnet addresses
    /// + kubo PeerID from the fleet index) — best-effort, the DHT is the
    /// fallback.
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub peer_id: String,
}

/// POST /api/ipfs/models/fetch — pin a fleet peer's model on this Orb
/// (mirror it). Swarm-connects to the source Orb when hints are provided,
/// then `ipfs pin add` fetches the content. Progress via /models tasks.
pub async fn fetch_model(State(_s): State<AppState>, Json(req): Json<FetchReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let cid = req.cid.clone();
    task_set(&cid, "fetching from IPFS swarm…");
    tokio::task::spawn_blocking(move || {
        // Best-effort direct connection to the Orb that hosts it — makes
        // LAN/tailnet fetches immediate instead of waiting on DHT routing.
        if !req.peer_id.is_empty() {
            for addr in req.addrs.iter().take(4) {
                if addr.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':') {
                    let ma = format!("/ip4/{addr}/tcp/4001/p2p/{}", req.peer_id);
                    let _ = run_script(&["connect", &ma]);
                }
            }
        }
        match run_script(&["pin", &req.cid]) {
            Ok(_) => {
                catalog_add(ModelEntry {
                    cid: req.cid.clone(),
                    name: req.name,
                    size_bytes: req.size_bytes,
                    sha256: req.sha256,
                    kind: req.kind,
                    desc: req.desc,
                    license: req.license,
                    added_at_ms: epoch_ms(),
                    origin_id: req.origin_id,
                    origin_label: req.origin_label,
                });
                task_clear(&req.cid);
            }
            Err(e) => task_set(&req.cid, &format!("error: {e}")),
        }
    });
    Json(json!({"ok": true, "fetching": cid}))
}

/// POST /api/ipfs/models/remove — unshare: unpin the CID + drop the catalog
/// entry. Blocks are reclaimed by kubo GC as the storage cap demands.
pub async fn remove_model(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let _ = run_script(&["unpin", &req.cid]);
        let entries: Vec<ModelEntry> =
            read_catalog().into_iter().filter(|e| e.cid != req.cid).collect();
        let _ = write_catalog(&entries);
        task_clear(&req.cid);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "remove task failed"}));
    Json(v)
}
