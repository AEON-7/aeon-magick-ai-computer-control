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
/// Merged view of every Orb's catalog heard over pubsub, written by the
/// `aeon-modelshare` gossip daemon. The fleet-free global index.
const REGISTRY_JSON: &str = "/var/lib/aeon/ipfs-models/registry.json";
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
        // Auto-enroll: IPFS is ON by default so an Orb joins the Model Share
        // network the moment it boots (the aeon-ipfs-boot oneshot brings the
        // node up unless this is explicitly set false / the opt-out flag is
        // present). Historically this defaulted false; flipped in v112.
        Self { enabled: true, storage_max: default_storage() }
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

/// Like `run_script` but returns raw stdout bytes (for binary content such as
/// a card image — `String::from_utf8_lossy` would corrupt it).
fn run_script_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{SCRIPT} {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// Like `run_script` but pipes `input` to the child's stdin (for `mfs-write`,
/// which writes stdin into an MFS file).
fn run_script_stdin(args: &[&str], input: &[u8]) -> Result<String, String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(SCRIPT)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "no stdin".to_string())?
        .write_all(input)
        .map_err(|e| format!("write stdin: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    if !out.status.success() {
        return Err(format!("{SCRIPT} {args:?}: {}", String::from_utf8_lossy(&out.stderr).trim()));
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

// ── Model Share: AI model publishing over IPFS ──────────────────────────────
//
// A shared model is an IPFS *directory* (so one CID carries both the weights
// and a human/agent-readable model card): `ipfs add -r` a temp dir holding the
// model file plus `model-card.json`, and the resulting dir CID resolves at
// <gateway>/ipfs/<cid>/<file> and <gateway>/ipfs/<cid>/model-card.json.
//
// Discovery is fleet-FREE: the `aeon-modelshare` daemon gossips this Orb's
// catalog over the libp2p pubsub topic `aeon-model-share/v1` and merges every
// other Orb's announcements into registry.json. Any Orb on the IPFS network
// converges on the same global index with no shared token — the fleet
// heartbeat still carries the catalog too (a fast LAN path) but is not
// required.

/// The model card — rich metadata written to `model-card.json` inside the
/// shared directory AND kept in the catalog entry so the index is browsable
/// without fetching each card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelCard {
    #[serde(default)]
    pub kind: String, // llm | vlm | vision | stt | tts | embedding | other
    #[serde(default)]
    pub base_model: String,
    #[serde(default)]
    pub params: String, // "8B", "74M", …
    #[serde(default)]
    pub quant: String, // "int4", "fp16", "gguf-q4_k_m", …
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub intended_use: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub format: String, // gguf | safetensors | onnx | hef | …
    /// Card image filename inside the shared dir (e.g. "card-image.png"), or
    /// empty. Rendered from <gateway>/ipfs/<cid>/<image>.
    #[serde(default)]
    pub image: String,
    /// True when a README.md is present in the shared dir.
    #[serde(default)]
    pub readme: bool,
}

/// One shared model = one IPFS directory CID. `origin_*` is the identity of the
/// Orb that FIRST shared it; it travels with the entry when peers mirror it, so
/// provenance survives replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub cid: String, // directory CID
    pub name: String, // display name
    #[serde(default)]
    pub file: String, // model filename inside the dir (for the download link)
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub card: ModelCard,
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

/// POST /api/ipfs/models/upload?name=&card=<urlencoded json> — streaming model
/// upload. Body = raw file bytes (Content-Disposition carries the filename),
/// same pattern as the ISO upload (body limit disabled in api.rs). The bytes
/// stream to a per-share staging directory; a `model-card.json` is written
/// beside the file, then `ipfs add -r` adds the whole DIRECTORY (pinning it)
/// so one CID carries the weights + the card. The staging dir is deleted after
/// (content lives in the blockstore). Poll /models until the task clears.
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
    // The model filename inside the shared dir — sanitized so the gateway path
    // is clean; the display name lives in the card/entry.
    let safe_file: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let display = q.get("name").cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| filename.clone());
    let card: ModelCard = q
        .get("card")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // Per-share staging directory: <MODELS_DIR>/staging/<epoch>/<file> + card.
    let share_dir = std::path::Path::new(MODELS_DIR).join("staging").join(epoch_ms().to_string());
    if let Err(e) = std::fs::create_dir_all(&share_dir) {
        return Json(json!({"ok": false, "err": format!("mkdir staging: {e}")}));
    }
    let file_path = share_dir.join(&safe_file);
    let tmp_path = share_dir.join(format!("{safe_file}.partial"));

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
                let _ = tokio::fs::remove_dir_all(&share_dir).await;
                return Json(json!({"ok": false, "err": format!("stream read: {e}")}));
            }
        };
        hasher.update(&bytes);
        if let Err(e) = file.write_all(&bytes).await {
            let _ = tokio::fs::remove_dir_all(&share_dir).await;
            return Json(json!({"ok": false, "err": format!("write: {e}")}));
        }
        bytes_written += bytes.len() as u64;
    }
    if let Err(e) = file.flush().await {
        let _ = tokio::fs::remove_dir_all(&share_dir).await;
        return Json(json!({"ok": false, "err": format!("flush: {e}")}));
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&tmp_path, &file_path).await {
        let _ = tokio::fs::remove_dir_all(&share_dir).await;
        return Json(json!({"ok": false, "err": format!("rename: {e}")}));
    }

    let sha256_hex = hex::encode(hasher.finalize());
    let (origin_id, origin_label) = crate::fleet::identity();

    // Write the model-card.json into the dir (goes into IPFS with the model).
    let card_doc = json!({
        "schema": "aeon-model-card/1",
        "name": display,
        "file": safe_file,
        "size_bytes": bytes_written,
        "sha256": sha256_hex,
        "card": card,
        "shared_by": { "id": origin_id, "label": origin_label },
        "created_ms": epoch_ms(),
    });
    let card_path = share_dir.join("model-card.json");
    if let Err(e) = tokio::fs::write(&card_path, serde_json::to_vec_pretty(&card_doc).unwrap_or_default()).await {
        let _ = tokio::fs::remove_dir_all(&share_dir).await;
        return Json(json!({"ok": false, "err": format!("write card: {e}")}));
    }

    let entry_seed = ModelEntry {
        cid: String::new(), // filled after `ipfs add -r`
        name: display.clone(),
        file: safe_file,
        size_bytes: bytes_written,
        sha256: sha256_hex.clone(),
        card,
        added_at_ms: epoch_ms(),
        origin_id,
        origin_label,
    };

    task_set(&display, "adding to IPFS…");
    let dir_str = share_dir.to_string_lossy().to_string();
    let key = display.clone();
    tokio::task::spawn_blocking(move || {
        // `ipfs add -rQ <dir>` → the directory's root CID (weights + card).
        match run_script(&["add", &dir_str]) {
            Ok(cid) if !cid.is_empty() => {
                let mut entry = entry_seed;
                entry.cid = cid;
                catalog_add(entry);
                let _ = std::fs::remove_dir_all(&dir_str); // bytes now in the blockstore
                task_clear(&key);
                announce(); // gossip the updated catalog immediately
            }
            Ok(_) => task_set(&key, "error: add produced no CID"),
            Err(e) => task_set(&key, &format!("error: {e}")),
        }
    });

    Json(json!({"ok": true, "name": display, "size_bytes": bytes_written, "processing": true}))
}

/// Nudge the gossip daemon to re-announce this Orb's catalog now (so a fresh
/// share/unshare shows up on peers within seconds instead of the next tick).
/// Best-effort: the daemon also announces on a timer.
fn announce() {
    let _ = std::fs::write("/run/aeon/modelshare-announce", "");
}

#[derive(Deserialize)]
pub struct FetchReq {
    #[serde(flatten)]
    pub entry: ModelEntry,
    /// Source Orb hints for a direct swarm connection (addresses + kubo PeerID
    /// from the registry) — best-effort; the DHT is the fallback.
    #[serde(default)]
    pub addrs: Vec<String>,
    #[serde(default)]
    pub peer_id: String,
}

/// POST /api/ipfs/models/fetch — download+host a model shared by ANY Orb (no
/// fleet needed). Swarm-connects to a hosting Orb when hints are given, then
/// `ipfs pin add` fetches the whole directory (weights + card). Progress via
/// /models tasks. Pinned = hosted, so this Orb joins the model's host set.
pub async fn fetch_model(State(_s): State<AppState>, Json(req): Json<FetchReq>) -> Json<Value> {
    if !valid_cid(&req.entry.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let cid = req.entry.cid.clone();
    task_set(&cid, "downloading from IPFS…");
    tokio::task::spawn_blocking(move || {
        if !req.peer_id.is_empty() {
            for addr in req.addrs.iter().take(4) {
                if addr.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':') {
                    let ma = format!("/ip4/{addr}/tcp/4001/p2p/{}", req.peer_id);
                    let _ = run_script(&["connect", &ma]);
                }
            }
        }
        match run_script(&["pin", &req.entry.cid]) {
            Ok(_) => {
                catalog_add(req.entry);
                task_clear(&cid);
                announce();
            }
            Err(e) => task_set(&cid, &format!("error: {e}")),
        }
    });
    Json(json!({"ok": true, "fetching": true}))
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
        announce();
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "remove task failed"}));
    Json(v)
}

/// One Orb's gossiped presence in registry.json (written by aeon-modelshare).
#[derive(Debug, Clone, Default, Deserialize)]
struct RegPeer {
    #[serde(default)]
    peer_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    addrs: Vec<String>,
    #[serde(default)]
    updated_ms: i64,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

fn read_registry() -> HashMap<String, RegPeer> {
    std::fs::read_to_string(REGISTRY_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("peers").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default()
}

fn self_peer_id() -> String {
    std::fs::read_to_string(IPFS_REPO_CONFIG)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("Identity").and_then(|i| i.get("PeerID")).and_then(|p| p.as_str()).map(String::from))
        .unwrap_or_default()
}

/// GET /api/ipfs/models/registry — the FLEET-FREE global index. Unions this
/// Orb's local catalog (pinned = hosted here) with every peer the gossip
/// daemon has heard on the pubsub topic, grouped by CID so a model mirrored on
/// several Orbs is one row with multiple hosts. Also returns in-flight tasks.
pub async fn models_registry(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let me = self_peer_id();
        let local = read_catalog();
        let peers = read_registry();
        let tasks: HashMap<String, String> = tasks().lock().map(|g| g.clone()).unwrap_or_default();

        // cid -> (entry, hosts[])
        let mut rows: HashMap<String, (ModelEntry, Vec<Value>)> = HashMap::new();
        let mut push_host = |cid: &str, entry: &ModelEntry, host: Value| {
            let e = rows.entry(cid.to_string()).or_insert_with(|| (entry.clone(), Vec::new()));
            e.1.push(host);
        };
        for e in &local {
            push_host(&e.cid, e, json!({"label": "this orb", "is_self": true, "online": true, "peer_id": me}));
        }
        for (_pid, p) in &peers {
            for e in &p.models {
                let host = json!({
                    "label": if p.label.is_empty() { p.peer_id.chars().take(12).collect::<String>() } else { p.label.clone() },
                    "is_self": p.peer_id == me,
                    "online": true,
                    "peer_id": p.peer_id,
                    "addrs": p.addrs,
                    "updated_ms": p.updated_ms,
                });
                push_host(&e.cid, e, host);
            }
        }
        let local_cids: std::collections::HashSet<String> = local.iter().map(|e| e.cid.clone()).collect();
        let mut out: Vec<Value> = rows
            .into_iter()
            .map(|(cid, (entry, hosts))| {
                json!({ "entry": entry, "hosts": hosts, "local": local_cids.contains(&cid) })
            })
            .collect();
        // newest first by the entry's added_at_ms
        out.sort_by(|a, b| {
            let ai = a["entry"]["added_at_ms"].as_i64().unwrap_or(0);
            let bi = b["entry"]["added_at_ms"].as_i64().unwrap_or(0);
            bi.cmp(&ai)
        });
        json!({"ok": true, "self_peer_id": me, "models": out, "tasks": tasks, "peer_count": peers.len()})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "registry task failed"}));
    Json(v)
}

/// GET /api/ipfs/models/card?cid=<dirCID> — fetch a shared model's card JSON
/// through the local node (`ipfs cat <cid>/model-card.json`), so the browser
/// never needs cross-origin access to the gateway.
pub async fn model_card(
    State(_s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Json<Value> {
    let cid = q.get("cid").cloned().unwrap_or_default();
    if !valid_cid(&cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["cat", &format!("{cid}/model-card.json")]) {
            Ok(s) => serde_json::from_str::<Value>(&s)
                .map(|card| json!({"ok": true, "card": card}))
                .unwrap_or_else(|_| json!({"ok": false, "err": "card not valid JSON"})),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "card task failed"}));
    Json(v)
}

/// GET /api/ipfs/models/image?cid=<dir>&name=card-image.png — stream a shared
/// model's card image through the supervisor (same-origin HTTPS) so it isn't
/// blocked as mixed content the way the plain-HTTP :8080 gateway would be on
/// the HTTPS console. Whitelisted to `card-image.<ext>` only.
pub async fn model_image(
    State(_s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    let cid = q.get("cid").cloned().unwrap_or_default();
    let name = q.get("name").cloned().unwrap_or_default();
    // card-image.<ext> only — no path traversal, no arbitrary files.
    let ok_name = name.strip_prefix("card-image.").map(|e| {
        e.chars().all(|c| c.is_ascii_alphanumeric())
    }).unwrap_or(false);
    if !valid_cid(&cid) || !ok_name {
        return (StatusCode::BAD_REQUEST, "bad request").into_response();
    }
    let mime = match name.rsplit('.').next().unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    };
    let bytes = tokio::task::spawn_blocking(move || run_script_bytes(&["cat", &format!("{cid}/{name}")]))
        .await
        .unwrap_or_else(|_| Err("image task failed".into()));
    match bytes {
        Ok(b) => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
            ],
            b,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// GET /api/ipfs/models/file?cid=<dir>&name=README.md — fetch a text file from
/// a shared model dir through the local node (`ipfs cat`), whitelisted to the
/// known sidecar files so this can't be used to read arbitrary paths.
pub async fn model_file(
    State(_s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Json<Value> {
    let cid = q.get("cid").cloned().unwrap_or_default();
    let name = q.get("name").cloned().unwrap_or_default();
    if !valid_cid(&cid) || !matches!(name.as_str(), "README.md" | "model-card.json") {
        return Json(json!({"ok": false, "err": "invalid request"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        match run_script(&["cat", &format!("{cid}/{name}")]) {
            Ok(s) => json!({"ok": true, "text": s}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "file task failed"}));
    Json(v)
}

// ── Rich share (weights + card + README + image) & metadata EDIT ─────────────

static DRAFT_CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn new_draft_id() -> String {
    let n = DRAFT_CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", epoch_ms())
}

fn staging_root() -> std::path::PathBuf {
    std::path::Path::new(MODELS_DIR).join("staging")
}

/// Keep image extensions to a safe known set; default png.
fn safe_img_ext(ext: &str) -> String {
    match ext.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "jpg".into(),
        "webp" => "webp".into(),
        "gif" => "gif".into(),
        "svg" => "svg".into(),
        _ => "png".into(),
    }
}

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Decode a base64 card image, enforcing the size cap. Returns (bytes, ext).
fn decode_image(image_b64: &str, image_ext: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    let b64 = image_b64.trim();
    if b64.is_empty() {
        return Ok(None);
    }
    // Accept a data: URL or bare base64.
    let raw = b64.rsplit(',').next().unwrap_or(b64);
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| format!("bad image base64: {e}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("image too large ({} KB, max {} KB)", bytes.len() / 1024, MAX_IMAGE_BYTES / 1024));
    }
    Ok(Some((bytes, safe_img_ext(image_ext))))
}

/// POST /api/ipfs/models/upload-weights — phase 1 of a rich share. Streams the
/// weights (Content-Disposition filename, raw body, body-limit disabled) to a
/// per-draft staging dir and returns a draft_id. The console then finalizes
/// with /models/publish, carrying the card + README + image as JSON — so
/// arbitrary-size metadata and a binary image don't have to ride a query
/// string. sha256 is computed in-stream.
pub async fn upload_weights(
    State(_s): State<AppState>,
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
    let safe_file: String = filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();

    let draft_id = new_draft_id();
    let dir = staging_root().join(&draft_id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Json(json!({"ok": false, "err": format!("mkdir draft: {e}")}));
    }
    let file_path = dir.join(&safe_file);
    let tmp_path = dir.join(format!("{safe_file}.partial"));
    let mut file = match tokio::fs::File::create(&tmp_path).await {
        Ok(f) => f,
        Err(e) => return Json(json!({"ok": false, "err": format!("create draft: {e}")})),
    };
    let mut hasher = Sha256::new();
    let mut n: u64 = 0;
    let mut stream = request.into_body().into_data_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Json(json!({"ok": false, "err": format!("stream: {e}")}));
            }
        };
        hasher.update(&bytes);
        if let Err(e) = file.write_all(&bytes).await {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Json(json!({"ok": false, "err": format!("write: {e}")}));
        }
        n += bytes.len() as u64;
    }
    let _ = file.flush().await;
    drop(file);
    if let Err(e) = tokio::fs::rename(&tmp_path, &file_path).await {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Json(json!({"ok": false, "err": format!("rename: {e}")}));
    }
    // Sidecar (OUTSIDE the dir so it isn't added to IPFS) recording the weights
    // filename for the publish step.
    let sha = hex::encode(hasher.finalize());
    let meta = json!({"file": safe_file, "size_bytes": n, "sha256": sha, "created_ms": epoch_ms()});
    let _ = std::fs::write(staging_root().join(format!("{draft_id}.meta.json")), meta.to_string());

    Json(json!({"ok": true, "draft_id": draft_id, "file": safe_file, "size_bytes": n, "sha256": sha}))
}

/// Prune draft dirs + sidecars older than ~2h (uploads never finalized).
fn prune_stale_drafts() {
    let root = staging_root();
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.modified().map(|m| m < cutoff).unwrap_or(false) {
                    let p = e.path();
                    if md.is_dir() { let _ = std::fs::remove_dir_all(&p); }
                    else { let _ = std::fs::remove_file(&p); }
                }
            }
        }
    }
}

#[derive(Deserialize)]
pub struct PublishReq {
    pub draft_id: String,
    pub name: String,
    #[serde(default)]
    pub card: ModelCard,
    #[serde(default)]
    pub readme: String,
    #[serde(default)]
    pub image_b64: String,
    #[serde(default)]
    pub image_ext: String,
}

/// POST /api/ipfs/models/publish — phase 2: finalize a draft into a shared
/// model dir. Writes model-card.json + optional README.md + optional
/// card-image into the draft dir, `ipfs add -r` (pins) the whole directory so
/// one CID carries weights + card + readme + image, catalogs it, cleans up, and
/// announces to the network.
pub async fn publish_model(State(_s): State<AppState>, Json(req): Json<PublishReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        prune_stale_drafts();
        // draft_id is a server-minted "<ms>-<n>" — reject anything else so this
        // can't be steered outside the staging root.
        if req.draft_id.is_empty() || !req.draft_id.chars().all(|c| c.is_ascii_digit() || c == '-') {
            return json!({"ok": false, "err": "bad draft id"});
        }
        let dir = staging_root().join(&req.draft_id);
        let meta_path = staging_root().join(format!("{}.meta.json", req.draft_id));
        let meta: Value = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| json!({}));
        let file = meta.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if file.is_empty() || !dir.join(&file).exists() {
            return json!({"ok": false, "err": "draft not found (expired?)"});
        }
        let size_bytes = meta.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let sha256 = meta.get("sha256").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Decode image → write into the dir; set card.image.
        let mut card = req.card;
        match decode_image(&req.image_b64, &req.image_ext) {
            Ok(Some((bytes, ext))) => {
                let img_name = format!("card-image.{ext}");
                if std::fs::write(dir.join(&img_name), &bytes).is_ok() {
                    card.image = img_name;
                }
            }
            Ok(None) => {}
            Err(e) => return json!({"ok": false, "err": e}),
        }
        card.readme = !req.readme.trim().is_empty();
        if card.readme {
            if std::fs::write(dir.join("README.md"), &req.readme).is_err() {
                card.readme = false;
            }
        }

        let (origin_id, origin_label) = crate::fleet::identity();
        let card_doc = json!({
            "schema": "aeon-model-card/1", "name": req.name, "file": file,
            "size_bytes": size_bytes, "sha256": sha256, "card": card,
            "shared_by": {"id": origin_id, "label": origin_label}, "created_ms": epoch_ms(),
        });
        if std::fs::write(dir.join("model-card.json"), serde_json::to_vec_pretty(&card_doc).unwrap_or_default()).is_err() {
            return json!({"ok": false, "err": "write card failed"});
        }

        let dir_str = dir.to_string_lossy().to_string();
        match run_script(&["add", &dir_str]) {
            Ok(cid) if !cid.is_empty() => {
                catalog_add(ModelEntry {
                    cid: cid.clone(), name: req.name, file, size_bytes, sha256,
                    card, added_at_ms: epoch_ms(), origin_id, origin_label,
                });
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&meta_path);
                announce();
                json!({"ok": true, "cid": cid})
            }
            Ok(_) => json!({"ok": false, "err": "add produced no CID"}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "publish task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct EditReq {
    pub cid: String, // the model dir CID being edited
    pub name: String,
    #[serde(default)]
    pub card: ModelCard,
    /// None = keep the existing README; Some("") = remove it; Some(text) = set.
    #[serde(default)]
    pub readme: Option<String>,
    #[serde(default)]
    pub image_b64: String,
    #[serde(default)]
    pub image_ext: String,
    #[serde(default)]
    pub remove_image: bool,
}

/// POST /api/ipfs/models/edit — edit a model this Orb hosts WITHOUT re-uploading
/// the weights. Copies the existing dir into MFS by CID reference (weights +
/// any kept image are shared, not re-transferred), rewrites model-card.json,
/// sets/keeps/clears README.md, replaces/keeps/removes the card image, then
/// pins the new dir CID, unpins + de-catalogs the old one, and re-announces.
/// Provenance (origin, first-shared time) is preserved.
pub async fn edit_model(State(_s): State<AppState>, Json(req): Json<EditReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        // Must be a model this Orb hosts (in the local catalog) to edit it.
        let old = match read_catalog().into_iter().find(|e| e.cid == req.cid) {
            Some(e) => e,
            None => return json!({"ok": false, "err": "not a locally-hosted model"}),
        };
        let img = match decode_image(&req.image_b64, &req.image_ext) {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "err": e}),
        };

        // Flat top-level MFS path: `files cp` needs the destination's parent to
        // exist, and `/` always does (a nested /aeon-build/<id> would need an
        // explicit mkdir of the parent first).
        let base = format!("/aeon-build-{}", new_draft_id());
        let _ = run_script(&["mfs-rm", &base]);
        // Seed from the existing dir — weights + current files reused by ref.
        if let Err(e) = run_script(&["mfs-cp", &format!("/ipfs/{}", req.cid), &base]) {
            return json!({"ok": false, "err": format!("mfs seed: {e}")});
        }

        let mut card = req.card;
        // README: keep / set / clear.
        let readme_present = match &req.readme {
            None => old.card.readme, // keep existing
            Some(md) if md.trim().is_empty() => {
                let _ = run_script(&["mfs-rm", &format!("{base}/README.md")]);
                false
            }
            Some(md) => {
                if let Err(e) = run_script_stdin(&["mfs-write", &format!("{base}/README.md")], md.as_bytes()) {
                    let _ = run_script(&["mfs-rm", &base]);
                    return json!({"ok": false, "err": format!("write readme: {e}")});
                }
                true
            }
        };
        card.readme = readme_present;

        // Image: replace / remove / keep.
        let old_img = old.card.image.clone();
        if let Some((bytes, ext)) = img {
            if !old_img.is_empty() {
                let _ = run_script(&["mfs-rm", &format!("{base}/{old_img}")]);
            }
            let tmp = staging_root().join(format!("img-{}.{ext}", new_draft_id()));
            let _ = std::fs::create_dir_all(staging_root());
            let img_name = format!("card-image.{ext}");
            if std::fs::write(&tmp, &bytes).is_ok() {
                match run_script(&["add", &tmp.to_string_lossy()]) {
                    Ok(icid) if !icid.is_empty() => {
                        let _ = run_script(&["mfs-cp", &format!("/ipfs/{icid}"), &format!("{base}/{img_name}")]);
                        card.image = img_name;
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_file(&tmp);
        } else if req.remove_image {
            if !old_img.is_empty() {
                let _ = run_script(&["mfs-rm", &format!("{base}/{old_img}")]);
            }
            card.image = String::new();
        } else {
            card.image = old_img; // keep
        }

        // Rewrite model-card.json.
        let card_doc = json!({
            "schema": "aeon-model-card/1", "name": req.name, "file": old.file,
            "size_bytes": old.size_bytes, "sha256": old.sha256, "card": card,
            "shared_by": {"id": old.origin_id, "label": old.origin_label},
            "created_ms": old.added_at_ms,
        });
        let card_bytes = serde_json::to_vec_pretty(&card_doc).unwrap_or_default();
        let _ = run_script(&["mfs-rm", &format!("{base}/model-card.json")]);
        if let Err(e) = run_script_stdin(&["mfs-write", &format!("{base}/model-card.json")], &card_bytes) {
            let _ = run_script(&["mfs-rm", &base]);
            return json!({"ok": false, "err": format!("write card: {e}")});
        }

        let new_cid = match run_script(&["mfs-hash", &base]) {
            Ok(c) if !c.is_empty() => c,
            _ => { let _ = run_script(&["mfs-rm", &base]); return json!({"ok": false, "err": "no new CID"}); }
        };
        let _ = run_script(&["mfs-rm", &base]);

        if new_cid == req.cid {
            return json!({"ok": true, "cid": new_cid, "unchanged": true});
        }
        if let Err(e) = run_script(&["pin", &new_cid]) {
            return json!({"ok": false, "err": format!("pin new: {e}")});
        }
        // Swap catalog entry (preserve provenance + first-shared time).
        let mut entries: Vec<ModelEntry> = read_catalog().into_iter().filter(|e| e.cid != req.cid).collect();
        entries.push(ModelEntry {
            cid: new_cid.clone(), name: req.name, file: old.file, size_bytes: old.size_bytes,
            sha256: old.sha256, card, added_at_ms: old.added_at_ms,
            origin_id: old.origin_id, origin_label: old.origin_label,
        });
        let _ = write_catalog(&entries);
        let _ = run_script(&["unpin", &req.cid]);
        announce();
        json!({"ok": true, "cid": new_cid})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "edit task failed"}));
    Json(v)
}
