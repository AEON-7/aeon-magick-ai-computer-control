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
/// Where models pulled DOWN to this Orb are materialized as plain files (weights
/// + card), one `<slug>/` per model. This is the re-pushable library that
/// `agent_connect::push_model` rsyncs to connected systems — distinct from the
/// kubo block repo. Lives under /var/lib/aeon so it follows a relocated data
/// root (USB storage) with everything else.
pub const MODEL_LIBRARY_DIR: &str = "/var/lib/aeon/model-library";
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
            "disk_free_bytes": get("disk_free_bytes", json!(0)),
            "disk_total_bytes": get("disk_total_bytes", json!(0)),
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
    /// Mature/adult content (e.g. Civitai NSFW) — hidden unless the viewer has
    /// opted into mature content (with an 18+ attestation).
    #[serde(default)]
    pub nsfw: bool,
    /// Example/gallery image filenames inside the shared dir (gallery-1.<ext>…),
    /// e.g. sample generations for a diffusion model. Served same-origin.
    #[serde(default)]
    pub gallery: Vec<String>,
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
    /// True when every LFS weight file's sha256 matched the hash HuggingFace
    /// published for it (imported models only) — an integrity signature.
    #[serde(default)]
    pub verified: bool,
    /// Provenance URL for imported models (e.g. the HuggingFace repo).
    #[serde(default)]
    pub source: String,
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

// ── Per-source auth tokens (optional; for gated / mature-content pulls) ──────
// A user can register a token per model host — HuggingFace (gated repos),
// Civitai (gated + mature "red" content), Ollama — so the importer authenticates
// the download. Secrets: stored 0600, NEVER put in a model card or gossiped, and
// masked (not returned) over the API.
const TOKENS_JSON: &str = "/etc/aeon/model-tokens.json";

fn read_tokens() -> HashMap<String, String> {
    std::fs::read_to_string(TOKENS_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// The registered token for a source ("huggingface" | "civitai" | "ollama"), if any.
fn source_token(source: &str) -> Option<String> {
    read_tokens().get(source).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[derive(Deserialize)]
pub struct SetTokenReq {
    pub source: String,
    pub token: String, // empty string clears it
}

/// POST /api/ipfs/models/tokens — set (or clear) a source's auth token.
pub async fn set_source_token(State(_s): State<AppState>, Json(req): Json<SetTokenReq>) -> Json<Value> {
    let source = req.source.trim().to_ascii_lowercase();
    if !["huggingface", "civitai", "ollama"].contains(&source.as_str()) {
        return Json(json!({"ok": false, "err": "unknown source"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut t = read_tokens();
        let tok = req.token.trim();
        if tok.is_empty() {
            t.remove(&source);
        } else {
            t.insert(source.clone(), tok.to_string());
        }
        if let Some(dir) = std::path::Path::new(TOKENS_JSON).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = format!("{TOKENS_JSON}.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&t).unwrap_or_default()).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::rename(&tmp, TOKENS_JSON);
        }
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "token task failed"}));
    Json(v)
}

/// GET /api/ipfs/models/tokens — which sources have a token set (values MASKED).
pub async fn get_source_tokens(State(_s): State<AppState>) -> Json<Value> {
    let t = read_tokens();
    let mask = |k: &str| t.get(k).map(|v| !v.trim().is_empty()).unwrap_or(false);
    Json(json!({
        "ok": true,
        "huggingface": mask("huggingface"),
        "civitai": mask("civitai"),
        "ollama": mask("ollama"),
    }))
}

// ── Mature-content viewing (opt-in, with an 18+ attestation) ────────────────
const VIEW_JSON: &str = "/etc/aeon/modelshare-view.json";

fn read_view() -> Value {
    std::fs::read_to_string(VIEW_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({"mature_ok": false, "attested_ms": 0}))
}

/// GET /api/ipfs/models/view — the mature-content opt-in state.
pub async fn get_view_settings(State(_s): State<AppState>) -> Json<Value> {
    let v = read_view();
    Json(json!({
        "ok": true,
        "mature_ok": v.get("mature_ok").and_then(|b| b.as_bool()).unwrap_or(false),
        "attested_ms": v.get("attested_ms").and_then(|n| n.as_i64()).unwrap_or(0),
    }))
}

#[derive(Deserialize)]
pub struct ViewReq {
    pub mature_ok: bool,
    #[serde(default)]
    pub attest_18: bool, // must be true to ENABLE mature content
}

/// POST /api/ipfs/models/view — opt into (or out of) mature content. Enabling
/// REQUIRES an 18+ attestation; we record the timestamp of the attestation.
pub async fn set_view_settings(State(_s): State<AppState>, Json(req): Json<ViewReq>) -> Json<Value> {
    if req.mature_ok && !req.attest_18 {
        return Json(json!({"ok": false, "err": "an 18+ attestation is required to view mature content"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let doc = json!({
            "mature_ok": req.mature_ok,
            "attested_ms": if req.mature_ok { epoch_ms() } else { 0 },
        });
        if let Some(dir) = std::path::Path::new(VIEW_JSON).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(VIEW_JSON, serde_json::to_vec_pretty(&doc).unwrap_or_default());
        json!({"ok": true, "mature_ok": req.mature_ok})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "view task failed"}));
    Json(v)
}

/// A unit of in-flight model work, surfaced to the UI. `phase` is the human
/// label ("adding to IPFS…", "downloading to quarantine…", an "error: …" prefix
/// flags failure). During a download where the total size is known, `pct` +
/// `done_bytes`/`total_bytes` drive a real progress bar; otherwise `pct` is null
/// and the UI falls back to a spinner + bytes-pulled-so-far.
#[derive(Clone, Serialize)]
struct TaskState {
    phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pct: Option<u8>,
    #[serde(skip_serializing_if = "is_zero_u64")]
    done_bytes: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    total_bytes: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// In-flight model work: name-or-cid → progress state. Errors stay until the
/// next attempt for the same key so the UI can surface them.
static TASKS: OnceLock<Mutex<HashMap<String, TaskState>>> = OnceLock::new();

fn tasks() -> &'static Mutex<HashMap<String, TaskState>> {
    TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn task_set(key: &str, phase: &str) {
    if let Ok(mut t) = tasks().lock() {
        t.insert(
            key.to_string(),
            TaskState { phase: phase.to_string(), pct: None, done_bytes: 0, total_bytes: 0 },
        );
    }
}

/// Update a download's byte progress. `total == 0` means the size is unknown, so
/// no percentage is reported (UI shows the spinner + bytes pulled instead). The
/// percentage is clamped to 99 until the phase advances, so the bar never claims
/// 100% while bytes are still landing.
fn task_progress(key: &str, phase: &str, done: u64, total: u64) {
    let pct = (total > 0).then(|| ((done.saturating_mul(100) / total).min(99)) as u8);
    if let Ok(mut t) = tasks().lock() {
        t.insert(
            key.to_string(),
            TaskState { phase: phase.to_string(), pct, done_bytes: done, total_bytes: total },
        );
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
        let t: HashMap<String, TaskState> =
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

    // Format gate: a SHARED weight must be a real, safe model format. Reject
    // executables/scripts, pickle/zip weights (code-execution on load), and
    // files whose magic bytes don't match a claimed weight extension — so the
    // network never carries something disguised as a model.
    {
        let mut head = [0u8; 16];
        let n = { use std::io::Read; std::fs::File::open(&file_path).and_then(|mut f| f.read(&mut head)).unwrap_or(0) };
        let kind = sniff_kind(&head[..n]);
        let low = safe_file.to_ascii_lowercase();
        let bad = match kind {
            "elf" | "pe" | "macho" | "script" => Some("an executable/script".to_string()),
            "pickle" | "zip" => Some("a pickle/zip weight (can run code when loaded) — export it to safetensors or gguf first".to_string()),
            _ if low.ends_with(".gguf") && kind != "gguf" => Some(format!("claiming .gguf but its content is {kind}")),
            _ if low.ends_with(".safetensors") && kind != "safetensors" => Some(format!("claiming .safetensors but its content is {kind}")),
            _ => None,
        };
        if let Some(why) = bad {
            let _ = tokio::fs::remove_dir_all(&share_dir).await;
            return Json(json!({"ok": false, "err": format!("won't share “{safe_file}”: {why}")}));
        }
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
        verified: false,
        source: String::new(),
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
/// Classify a file by leading magic bytes — so we can catch fake extensions and
/// files that have no business in a model directory (executables, scripts).
fn sniff_kind(head: &[u8]) -> &'static str {
    if head.starts_with(b"\x7fELF") { return "elf"; }
    if head.starts_with(b"MZ") { return "pe"; }
    if head.starts_with(&[0xfe, 0xed, 0xfa, 0xce]) || head.starts_with(&[0xcf, 0xfa, 0xed, 0xfe])
        || head.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) { return "macho"; }
    if head.starts_with(b"#!") { return "script"; }
    if head.starts_with(b"GGUF") { return "gguf"; }
    if head.starts_with(b"PK\x03\x04") { return "zip"; }               // torch .pt is a zip
    if head.first() == Some(&0x80) && head.get(1).is_some_and(|b| *b <= 5) { return "pickle"; }
    if head.len() >= 9 && head[8] == b'{' { return "safetensors"; }    // 8-byte LE len + JSON
    if head.starts_with(b"\x89PNG") || head.starts_with(&[0xff, 0xd8, 0xff])
        || head.starts_with(b"GIF8") || (head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP") { return "image"; }
    if head.starts_with(b"{") || head.iter().take(64).all(|b| *b == b'\n' || *b == b'\r' || *b == b'\t' || (0x20..0x7f).contains(b) || *b >= 0x80) { return "text"; }
    "unknown"
}

/// Recursively inspect a materialized model directory: reject anything that is
/// executable/scriptable, a pickle/zip weight (code-execution on load), or a
/// weight file whose magic bytes don't match its claimed extension.
fn inspect_model_dir(dir: &std::path::Path) -> Result<(), String> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| format!("read quarantine: {e}"))?;
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() { stack.push(p); continue; }
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            let mut head = [0u8; 16];
            let n = {
                use std::io::Read;
                std::fs::File::open(&p).and_then(|mut f| f.read(&mut head)).unwrap_or(0)
            };
            let kind = sniff_kind(&head[..n]);
            match kind {
                "elf" | "pe" | "macho" | "script" =>
                    return Err(format!("rejected: “{name}” is an executable/script — not a model file")),
                "pickle" | "zip" =>
                    return Err(format!("rejected: “{name}” is a pickle/zip weight (can run code when loaded); only safetensors/gguf/onnx are accepted")),
                _ => {}
            }
            if name.ends_with(".gguf") && kind != "gguf" {
                return Err(format!("rejected: “{name}” claims .gguf but its content is {kind} (fake extension)"));
            }
            if name.ends_with(".safetensors") && kind != "safetensors" {
                return Err(format!("rejected: “{name}” claims .safetensors but its content is {kind} (fake extension)"));
            }
        }
    }
    Ok(())
}

/// On-demand ClamAV scan of a directory. Ok(true)=scanned clean, Ok(false)=scan
/// unavailable (clamav not installed / no signature DB yet) so we don't block,
/// Err=infected. Big weight files need the size caps lifted.
fn virus_scan(dir: &std::path::Path) -> Result<bool, String> {
    let out = std::process::Command::new("clamscan")
        .args(["-r", "-i", "--no-summary", "--max-filesize=16000M", "--max-scansize=16000M"])
        .arg(dir)
        .output();
    match out {
        Ok(o) => match o.status.code() {
            Some(0) => Ok(true),
            Some(1) => Err(format!(
                "infected — {}",
                String::from_utf8_lossy(&o.stdout).lines().take(3).collect::<Vec<_>>().join("; ")
            )),
            _ => Ok(false), // scan error (e.g. DB not fetched yet) — don't block
        },
        Err(_) => Ok(false), // clamscan not present — format check still ran
    }
}

/// POST /api/ipfs/models/fetch — download a peer's model through a QUARANTINE
/// sandbox: fetch to staging → validate every file's magic bytes (fake
/// extensions / stowaway executables / pickle weights) → ClamAV scan → and only
/// on a clean pass pin it + add to the catalog. Progress phases surface in the
/// task map. Rejected content never reaches the live library.
pub async fn fetch_model(State(_s): State<AppState>, Json(req): Json<FetchReq>) -> Json<Value> {
    if !valid_cid(&req.entry.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let cid = req.entry.cid.clone();
    task_set(&cid, "checking disk…");
    tokio::task::spawn_blocking(move || {
        let fail = |msg: &str| task_set(&cid, &format!("error: {msg}"));

        // Connect to the sharer's hinted addrs for a fast direct fetch.
        if !req.peer_id.is_empty() {
            for addr in req.addrs.iter().take(4) {
                if addr.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':') {
                    let _ = run_script(&["connect", &format!("/ip4/{addr}/tcp/4001/p2p/{}", req.peer_id)]);
                }
            }
        }

        // Disk guard: materializing quarantines a full copy alongside the
        // blockstore blocks (~2× peak), so require room for that + headroom.
        let sz = req.entry.size_bytes;
        let free = free_bytes(MODELS_DIR);
        if sz > 0 && sz.saturating_mul(2) + 512 * 1024 * 1024 > free {
            return fail(&format!("not enough disk: model is {} but only {} free", human(sz), human(free)));
        }

        let q = staging_root().join(format!("quarantine-{}", new_draft_id()));
        task_progress(&cid, "downloading to quarantine…", 0, sz);

        // `ipfs get` blocks until the whole directory has materialized, so a
        // sibling thread polls the quarantine dir as it fills to drive a real
        // byte-level progress bar. The download bytes land on disk here, so the
        // dir size is an honest gauge against the entry's known total.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let monitor = {
            let (stop, qm, cidm) = (stop.clone(), q.clone(), cid.clone());
            std::thread::spawn(move || {
                use std::sync::atomic::Ordering;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    task_progress(&cidm, "downloading to quarantine…", dir_size(&qm), sz);
                }
            })
        };
        let got = run_script(&["get", &cid, &q.to_string_lossy()]);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = monitor.join();
        if let Err(e) = got {
            let _ = std::fs::remove_dir_all(&q);
            return fail(&e);
        }

        task_set(&cid, "validating weights (file types)…");
        if let Err(e) = inspect_model_dir(&q) {
            let _ = std::fs::remove_dir_all(&q);
            return fail(&e);
        }

        task_set(&cid, "scanning for viruses…");
        match virus_scan(&q) {
            Err(e) => { let _ = std::fs::remove_dir_all(&q); return fail(&format!("virus scan: {e}")); }
            Ok(_) => {}
        }

        // Clean → keep it (pin the CID) and index it.
        task_set(&cid, "pinning…");
        match run_script(&["pin", &req.entry.cid]) {
            Ok(_) => {
                catalog_add(req.entry);
                let _ = std::fs::remove_dir_all(&q);
                task_clear(&cid);
                announce();
            }
            Err(e) => { let _ = std::fs::remove_dir_all(&q); fail(&e); }
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
    /// CIDs this peer has starred — gossiped so every Orb can tally a
    /// community star count per model (fleet-free reputation).
    #[serde(default)]
    starred: Vec<String>,
}

const STARS_JSON: &str = "/var/lib/aeon/ipfs-models/stars.json";

/// This Orb's starred model CIDs (a plain JSON array). Gossiped in our
/// announcement so peers can aggregate a network-wide star count.
fn read_stars() -> Vec<String> {
    std::fs::read_to_string(STARS_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .unwrap_or_default()
}

fn write_stars(cids: &[String]) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(STARS_JSON).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = format!("{STARS_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&cids).unwrap_or_default())?;
    std::fs::rename(tmp, STARS_JSON)
}

/// Model Karma — this Orb's bitswap ledger: bytes SERVED to the network vs bytes
/// DOWNLOADED. A personal contribution gauge (how much you give back vs take).
/// Read from `ipfs bitswap stat` (since the daemon started).
fn karma() -> Value {
    let stat = run_script(&["bitswap-stat"]).unwrap_or_default();
    // Parse "data sent: N" / "data received: N" (bytes) from the text output.
    let field = |needle: &str| -> u64 {
        stat.lines()
            .find(|l| l.trim_start().starts_with(needle))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let served = field("data sent");
    let downloaded = field("data received");
    let ratio = if downloaded > 0 { served as f64 / downloaded as f64 } else { 0.0 };
    json!({ "served_bytes": served, "downloaded_bytes": downloaded, "ratio": ratio })
}

#[derive(Deserialize)]
pub struct StarReq {
    pub cid: String,
}

/// POST /api/ipfs/models/star — toggle a star on a model (by dir CID). Persists
/// locally and re-announces so the star count propagates across the network.
pub async fn star_model(State(_s): State<AppState>, Json(req): Json<StarReq>) -> Json<Value> {
    let cid = req.cid.trim().to_string();
    if cid.is_empty() || cid.chars().any(|c| !c.is_ascii_alphanumeric()) {
        return Json(json!({"ok": false, "err": "bad cid"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut stars = read_stars();
        let starred = if let Some(i) = stars.iter().position(|c| c == &cid) {
            stars.remove(i);
            false
        } else {
            stars.push(cid.clone());
            true
        };
        let _ = write_stars(&stars);
        json!({"ok": true, "starred": starred, "count": stars.len()})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "star task failed"}));
    // Re-announce so the updated star set gossips out immediately.
    announce();
    Json(v)
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
        let tasks: HashMap<String, TaskState> = tasks().lock().map(|g| g.clone()).unwrap_or_default();

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
            // Skip our OWN gossiped announcement — with floodsub self-delivery the
            // daemon hears its own catalog back, and the local catalog above
            // already represents "this orb". Without this, every locally-hosted
            // model shows "this orb" twice.
            if p.peer_id == me {
                continue;
            }
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
        // Community stars: a model's star count = the number of DISTINCT Orbs
        // that starred its CID (self + every gossiped peer). Keyed by peer_id so
        // a stale self-entry in the registry can't double-count.
        let my_stars: std::collections::HashSet<String> = read_stars().into_iter().collect();
        let mut starrers: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for cid in &my_stars {
            starrers.entry(cid.clone()).or_default().insert(me.clone());
        }
        for (_pid, p) in &peers {
            for cid in &p.starred {
                starrers.entry(cid.clone()).or_default().insert(p.peer_id.clone());
            }
        }

        let local_cids: std::collections::HashSet<String> = local.iter().map(|e| e.cid.clone()).collect();
        let mut out: Vec<Value> = rows
            .into_iter()
            .map(|(cid, (entry, hosts))| {
                let host_count = hosts.len();
                let star_count = starrers.get(&cid).map(|s| s.len()).unwrap_or(0);
                json!({
                    "entry": entry, "hosts": hosts, "local": local_cids.contains(&cid),
                    "host_count": host_count, "star_count": star_count,
                    "starred": my_stars.contains(&cid),
                })
            })
            .collect();
        // Rank: most-hosted + most-starred first, then newest.
        out.sort_by(|a, b| {
            let score = |m: &Value| {
                m["star_count"].as_i64().unwrap_or(0) * 3 + m["host_count"].as_i64().unwrap_or(0)
            };
            score(b).cmp(&score(a)).then_with(|| {
                let ai = a["entry"]["added_at_ms"].as_i64().unwrap_or(0);
                let bi = b["entry"]["added_at_ms"].as_i64().unwrap_or(0);
                bi.cmp(&ai)
            })
        });
        json!({"ok": true, "self_peer_id": me, "models": out, "tasks": tasks,
               "peer_count": peers.len(), "my_star_count": my_stars.len()})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "registry task failed"}));
    Json(v)
}

/// GET /api/ipfs/models/karma — this Orb's Model Karma (bytes served vs
/// downloaded, from the bitswap ledger). A personal contribution gauge; kept
/// out of /registry so the frequent poll doesn't shell out each time.
pub async fn models_karma(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| json!({"ok": true, "karma": karma()}))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "karma task failed"}));
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
    // Whitelist card-image.<ext> and gallery-<n>.<ext> only — no path traversal,
    // no arbitrary files.
    let ext_ok = |e: &str| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_name = name.strip_prefix("card-image.").map(&ext_ok).unwrap_or(false)
        || name.strip_prefix("gallery-").is_some_and(|rest| {
            rest.split_once('.').is_some_and(|(n, e)| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() && ext_ok(e))
        });
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
                    verified: false, source: String::new(),
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
            verified: old.verified, source: old.source,
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

// ── HuggingFace import ──────────────────────────────────────────────────────

fn hf_agent() -> ureq::Agent {
    ureq::builder()
        .redirects(10)
        .timeout_connect(std::time::Duration::from_secs(20))
        .user_agent("aeon-magick-orb/modelshare")
        .build()
}

/// GET a HuggingFace URL as JSON using the stored HF token (if any). Public so
/// sibling modules (e.g. `bench`, to preview a model's serve recipe) can read
/// model metadata without duplicating the token/agent plumbing.
pub(crate) fn hf_get_json(url: &str) -> Result<Value, String> {
    http_get_json(url, source_token("huggingface").as_deref())
}

/// GET a URL as JSON, optionally with a Bearer auth token (for gated content).
fn http_get_json(url: &str, auth: Option<&str>) -> Result<Value, String> {
    let mut req = hf_agent().get(url);
    if let Some(t) = auth {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let resp = req.call().map_err(|e| match e {
        ureq::Error::Status(401, _) | ureq::Error::Status(403, _) => {
            "gated/private — add an auth token for this source (Settings) and retry".to_string()
        }
        ureq::Error::Status(404, _) => "not found".to_string(),
        other => format!("request failed: {other}"),
    })?;
    resp.into_json::<Value>().map_err(|e| format!("bad json: {e}"))
}

/// Parse a HuggingFace URL / id into (repo_id, optional specific file path).
/// Accepts: https://huggingface.co/org/model[/tree/main][/blob|resolve/main/<file>]
/// or a bare "org/model".
fn parse_hf(input: &str) -> Result<(String, Option<String>), String> {
    let s = input.trim();
    let rest = s
        .strip_prefix("https://huggingface.co/")
        .or_else(|| s.strip_prefix("http://huggingface.co/"))
        .or_else(|| s.strip_prefix("huggingface.co/"))
        .unwrap_or(s);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 2 {
        return Err("expected a huggingface.co/<org>/<model> URL".into());
    }
    // Reject anything that isn't a plausible repo segment.
    let ok = |p: &str| p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !ok(parts[0]) || !ok(parts[1]) {
        return Err("invalid repo id".into());
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    // /blob/main/<file> or /resolve/main/<file> → a specific file.
    if parts.len() >= 5 && (parts[2] == "blob" || parts[2] == "resolve") {
        let file = parts[4..].join("/");
        if file.contains("..") {
            return Err("invalid file path".into());
        }
        return Ok((repo, Some(file)));
    }
    Ok((repo, None))
}

fn is_weight(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".safetensors", ".gguf", ".bin", ".onnx", ".pt", ".pth", ".ot", ".gduf"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// Free bytes on the filesystem holding `path` (via `df`, avoiding a libc dep).
fn free_bytes(path: &str) -> u64 {
    std::process::Command::new("df")
        .args(["-Pk", path])
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            s.lines().nth(1).and_then(|l| l.split_whitespace().nth(3).map(String::from))
        })
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(u64::MAX)
}

/// Pick a token like "8B" / "0.5B" / "70B" out of a model name, for the card.
fn parse_params(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'B' || bytes[i] == b'b' || bytes[i] == b'M' || bytes[i] == b'm') {
                // require it to be a size suffix, not part of a longer word
                let next_ok = bytes.get(i + 1).map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true);
                if next_ok {
                    return format!("{}{}", &name[start..i], (bytes[i] as char).to_ascii_uppercase());
                }
            }
        } else {
            i += 1;
        }
    }
    String::new()
}

fn kind_from_pipeline(pt: &str) -> &'static str {
    match pt {
        "text-generation" | "text2text-generation" => "llm",
        "image-text-to-text" | "visual-question-answering" | "image-to-text" | "video-text-to-text" => "vlm",
        "automatic-speech-recognition" | "audio-classification" => "stt",
        "text-to-speech" | "text-to-audio" => "tts",
        "feature-extraction" | "sentence-similarity" => "embedding",
        "image-classification" | "object-detection" | "image-segmentation" | "depth-estimation"
        | "zero-shot-image-classification" => "vision",
        // Generative / diffusion (ComfyUI · Stable Diffusion · Flux · video).
        "text-to-image" | "image-to-image" | "unconditional-image-generation" => "checkpoint",
        "text-to-video" | "image-to-video" => "checkpoint",
        _ => "other",
    }
}

/// Map a Civitai model type to our card `kind` (generative model classes).
fn kind_from_civitai(t: &str) -> &'static str {
    match t.to_ascii_lowercase().as_str() {
        "checkpoint" => "checkpoint",
        "lora" | "locon" | "dora" => "lora",
        "vae" => "vae",
        "controlnet" => "controlnet",
        "textualinversion" => "embedding",
        "upscaler" => "upscaler",
        "motionmodule" => "checkpoint",
        _ => "other",
    }
}

/// Stream a URL to `dest`, hashing as we go; drives overall progress via the
/// task phase string. Returns (sha256_hex, bytes_written).
fn hf_download(
    url: &str,
    dest: &std::path::Path,
    key: &str,
    label: &str,
    grand_total: u64,
    prior_done: u64,
    auth: Option<&str>,
) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let mut rq = hf_agent().get(url);
    if let Some(t) = auth {
        rq = rq.set("Authorization", &format!("Bearer {t}"));
    }
    let resp = rq.call().map_err(|e| format!("download {label}: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| format!("create {label}: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    let mut last_pct: i64 = -1;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read {label}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("write {label}: {e}"))?;
        done += n as u64;
        if grand_total > 0 {
            let pct = ((prior_done + done) * 100 / grand_total) as i64;
            if pct != last_pct {
                last_pct = pct;
                task_set(key, &format!("downloading {label} — {pct}%"));
            }
        }
    }
    file.flush().ok();
    Ok((hex::encode(hasher.finalize()), done))
}

#[derive(Deserialize)]
pub struct ImportHfReq {
    pub url: String,
}

/// POST /api/ipfs/models/import-hf — import a model straight from HuggingFace:
/// pull the weight file(s), the README (model card) and the author avatar,
/// verify each weight's sha256 against the hash HuggingFace publishes (LFS
/// oid), wrap it all in an IPFS directory, pin + catalog + announce it. Runs in
/// the background; progress shows in /models tasks.
pub async fn import_hf(State(_s): State<AppState>, Json(req): Json<ImportHfReq>) -> Json<Value> {
    if !read_config().enabled {
        return Json(json!({"ok": false, "err": "IPFS is off — enable it first"}));
    }
    let (repo, only_file) = match parse_hf(&req.url) {
        Ok(v) => v,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let model_name = repo.split('/').next_back().unwrap_or(&repo).to_string();
    let key = model_name.clone();
    let importing = key.clone();
    task_set(&key, "fetching model info…");
    tokio::task::spawn_blocking(move || {
        if let Err(e) = do_import_hf(&repo, only_file, &model_name, &key) {
            task_set(&key, &format!("error: {e}"));
        } else {
            task_clear(&key);
            announce();
        }
    });
    Json(json!({"ok": true, "importing": importing}))
}

fn do_import_hf(repo: &str, only_file: Option<String>, model_name: &str, key: &str) -> Result<(), String> {
    let meta = hf_get_json(&format!("https://huggingface.co/api/models/{repo}"))?;
    let tree = hf_get_json(&format!("https://huggingface.co/api/models/{repo}/tree/main"))?;
    let files = tree.as_array().cloned().unwrap_or_default();

    // (path, size, lfs_sha256)
    let mut weights: Vec<(String, u64, Option<String>)> = Vec::new();
    let get = |f: &Value, k: &str| f.get(k).and_then(|v| v.as_str()).map(String::from);
    let lfs_oid = |f: &Value| f.get("lfs").and_then(|l| l.get("oid")).and_then(|v| v.as_str()).map(String::from);
    let size_of = |f: &Value| f.get("size").and_then(|v| v.as_u64()).unwrap_or(0);

    if let Some(one) = &only_file {
        let f = files.iter().find(|f| get(f, "path").as_deref() == Some(one.as_str()));
        let (sz, oid) = f.map(|f| (size_of(f), lfs_oid(f))).unwrap_or((0, None));
        weights.push((one.clone(), sz, oid));
    } else {
        let paths: Vec<String> = files.iter().filter_map(|f| get(f, "path")).collect();
        let has = |ext: &str| paths.iter().any(|p| p.to_ascii_lowercase().ends_with(ext));
        if has(".safetensors") {
            for f in &files {
                if let Some(p) = get(f, "path") {
                    let pl = p.to_ascii_lowercase();
                    if pl.ends_with(".safetensors") || p == "config.json" || p == "generation_config.json" {
                        weights.push((p, size_of(f), lfs_oid(f)));
                    }
                }
            }
        } else if has(".gguf") {
            // one GGUF — prefer a common balanced quant, else the smallest.
            let prefs = ["q4_k_m", "q4_0", "q5_k_m", "q8_0", "q6_k", "q3_k_m"];
            let ggufs: Vec<&Value> = files.iter().filter(|f| get(f, "path").map(|p| p.to_ascii_lowercase().ends_with(".gguf")).unwrap_or(false)).collect();
            let pick = prefs.iter().find_map(|q| ggufs.iter().find(|f| get(f, "path").map(|p| p.to_ascii_lowercase().contains(q)).unwrap_or(false)).copied())
                .or_else(|| ggufs.iter().min_by_key(|f| size_of(f)).copied());
            if let Some(f) = pick {
                if let Some(p) = get(f, "path") {
                    weights.push((p, size_of(f), lfs_oid(f)));
                }
            }
        } else {
            for f in &files {
                if let Some(p) = get(f, "path") {
                    if is_weight(&p) || p == "config.json" {
                        weights.push((p, size_of(f), lfs_oid(f)));
                    }
                }
            }
        }
    }
    if weights.is_empty() {
        return Err("no weight files found in the repo".into());
    }
    let total: u64 = weights.iter().map(|(_, s, _)| *s).sum();
    let dir = staging_root().join(format!("hf-{}", new_draft_id()));
    let free = free_bytes(MODELS_DIR);
    if total > 0 && total + 512 * 1024 * 1024 > free {
        return Err(format!(
            "not enough disk: model is {} but only {} free",
            human(total), human(free)
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;

    // Download weights, verifying each against HF's published sha256.
    let mut prior = 0u64;
    let mut all_verified = true;
    let mut any_verifiable = false;
    let mut primary_sha = String::new();
    let mut primary_file = String::new();
    for (path, size, oid) in &weights {
        let leaf = path.rsplit('/').next().unwrap_or(path).to_string();
        let url = format!("https://huggingface.co/{repo}/resolve/main/{path}");
        let (sha, n) = hf_download(&url, &dir.join(&leaf), key, &leaf, total, prior, source_token("huggingface").as_deref())?;
        prior += n;
        if primary_file.is_empty() && is_weight(path) {
            primary_file = leaf.clone();
            primary_sha = sha.clone();
        }
        if let Some(expected) = oid {
            any_verifiable = true;
            if !expected.eq_ignore_ascii_case(&sha) {
                all_verified = false;
            }
        }
        let _ = size; // (size was only for the disk/total estimate)
    }
    let verified = any_verifiable && all_verified;

    // README (the model card) + author avatar — best-effort.
    task_set(key, "fetching README + image…");
    if let Ok(resp) = hf_agent().get(&format!("https://huggingface.co/{repo}/resolve/main/README.md")).call() {
        if let Ok(text) = resp.into_string() {
            let _ = std::fs::write(dir.join("README.md"), text);
        }
    }
    let mut image_name = String::new();
    if let Ok(resp) = hf_agent().get(&format!("https://huggingface.co/{repo}")).call() {
        if let Ok(html) = resp.into_string() {
            if let Some(url) = find_avatar_url(&html) {
                let ext = url.rsplit('.').next().filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("jpg");
                let name = format!("card-image.{}", safe_img_ext(ext));
                if let Ok(r) = hf_agent().get(&url).call() {
                    let mut bytes = Vec::new();
                    use std::io::Read;
                    if r.into_reader().take(MAX_IMAGE_BYTES as u64).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                        if std::fs::write(dir.join(&name), &bytes).is_ok() {
                            image_name = name;
                        }
                    }
                }
            }
        }
    }

    // Build the card from HF metadata.
    let card_data = meta.get("cardData").cloned().unwrap_or_else(|| json!({}));
    let pipeline = meta.get("pipeline_tag").and_then(|v| v.as_str())
        .or_else(|| card_data.get("pipeline_tag").and_then(|v| v.as_str())).unwrap_or("");
    let license = card_data.get("license").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let base_model = card_data.get("base_model").and_then(|v| v.as_str()).map(String::from)
        .or_else(|| card_data.get("base_model").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let tags: Vec<String> = meta.get("tags").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).filter(|t| !t.contains(':') && t.len() < 24).take(8).collect())
        .unwrap_or_default();
    let format = if primary_file.to_ascii_lowercase().ends_with(".gguf") { "gguf" }
        else if primary_file.to_ascii_lowercase().ends_with(".safetensors") { "safetensors" }
        else if primary_file.to_ascii_lowercase().ends_with(".onnx") { "onnx" }
        else { "" };
    let quant = if format == "gguf" {
        // e.g. "qwen2.5-0.5b-instruct-q4_k_m.gguf" → "q4_k_m": the last dash/dot
        // token starting with q<digit>.
        let low = primary_file.to_ascii_lowercase();
        let stem = low.strip_suffix(".gguf").unwrap_or(&low);
        stem.split(|c| c == '-' || c == '.')
            .rev()
            .find(|p| {
                let b = p.as_bytes();
                b.first() == Some(&b'q') && b.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false)
            })
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    let card = ModelCard {
        kind: kind_from_pipeline(pipeline).to_string(),
        base_model,
        params: parse_params(model_name),
        quant,
        license,
        description: format!("Imported from HuggingFace: {repo}"),
        intended_use: String::new(),
        tags,
        format: format.to_string(),
        image: image_name,
        readme: dir.join("README.md").exists(),
        ..Default::default()
    };
    let (origin_id, origin_label) = crate::fleet::identity();
    let source = format!("https://huggingface.co/{repo}");
    let card_doc = json!({
        "schema": "aeon-model-card/1", "name": model_name, "file": primary_file,
        "size_bytes": total, "sha256": primary_sha, "card": card,
        "verified": verified, "source": source,
        "shared_by": {"id": origin_id, "label": origin_label}, "created_ms": epoch_ms(),
    });
    std::fs::write(dir.join("model-card.json"), serde_json::to_vec_pretty(&card_doc).unwrap_or_default())
        .map_err(|e| format!("write card: {e}"))?;

    task_set(key, "adding to IPFS…");
    let cid = run_script(&["add", &dir.to_string_lossy()])?;
    if cid.is_empty() {
        return Err("ipfs add produced no CID".into());
    }
    catalog_add(ModelEntry {
        cid, name: model_name.to_string(), file: primary_file, size_bytes: total,
        sha256: primary_sha, card, added_at_ms: epoch_ms(), origin_id, origin_label,
        verified, source,
    });
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

// ── Ollama import ───────────────────────────────────────────────────────────
// Ollama distributes models through an OCI-style registry (registry.ollama.ai):
// a manifest lists layers; the GGUF weights are the layer with mediaType
// application/vnd.ollama.image.model, addressed by a sha256 digest we can pull
// directly and verify — no running ollama needed. Same registry protocol backs
// hailo-ollama's zoo, so this path is reused for Hailo LLM imports too.

/// Parse an Ollama model reference into (namespace, model, tag). Accepts
/// "llama3.2:3b", "library/llama3.2", "user/model:tag", or a full
/// "ollama.com/library/llama3.2:3b" / "registry.ollama.ai/…" URL.
fn parse_ollama_ref(input: &str) -> Result<(String, String, String), String> {
    let s = input.trim();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let rest = rest
        .strip_prefix("registry.ollama.ai/")
        .or_else(|| rest.strip_prefix("ollama.com/"))
        .or_else(|| rest.strip_prefix("ollama.ai/"))
        .unwrap_or(rest);
    let rest = rest.trim_matches('/');
    // Split the tag off the LAST segment only (namespaces have no colon).
    let (path, tag) = match rest.rsplit_once(':') {
        Some((p, t)) if !t.contains('/') => (p, t.to_string()),
        _ => (rest, "latest".to_string()),
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (namespace, model) = match parts.as_slice() {
        [m] => ("library".to_string(), m.to_string()),
        [ns, m] => (ns.to_string(), m.to_string()),
        _ => return Err("expected an ollama ref like llama3.2:3b or user/model:tag".into()),
    };
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !ok(&namespace) || !ok(&model) || !ok(&tag) {
        return Err("invalid ollama reference".into());
    }
    Ok((namespace, model, tag))
}

/// Fetch a model's Ollama-registry manifest. `host` lets the same code hit
/// hailo-ollama's registry for Hailo imports.
fn ollama_manifest(host: &str, ns: &str, model: &str, tag: &str) -> Result<Value, String> {
    let url = format!("https://{host}/v2/{ns}/{model}/manifests/{tag}");
    let resp = hf_agent()
        .get(&url)
        .set("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => format!("model not found on the registry: {ns}/{model}:{tag}"),
            other => format!("registry request failed: {other}"),
        })?;
    resp.into_json::<Value>().map_err(|e| format!("bad manifest json: {e}"))
}

fn do_import_ollama(reference: &str, model_name: &str, key: &str) -> Result<(), String> {
    let (ns, model, tag) = parse_ollama_ref(reference)?;
    let host = "registry.ollama.ai";
    task_set(key, "resolving Ollama manifest…");
    let manifest = ollama_manifest(host, &ns, &model, &tag)?;
    let layers = manifest.get("layers").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let find_layer = |mt: &str| layers.iter().find(|l| l.get("mediaType").and_then(|v| v.as_str()) == Some(mt)).cloned();

    let model_layer = find_layer("application/vnd.ollama.image.model")
        .ok_or("no model layer in the Ollama manifest (unsupported model type)")?;
    let digest = model_layer.get("digest").and_then(|v| v.as_str()).ok_or("model layer missing digest")?.to_string();
    let size = model_layer.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    let expected_sha = digest.strip_prefix("sha256:").unwrap_or(&digest).to_string();

    let free = free_bytes(MODELS_DIR);
    if size > 0 && size + 512 * 1024 * 1024 > free {
        return Err(format!("not enough disk: model is {} but only {} free", human(size), human(free)));
    }
    let dir = staging_root().join(format!("ollama-{}", new_draft_id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;

    let leaf = format!("{model}-{tag}.gguf");
    let blob_url = format!("https://{host}/v2/{ns}/{model}/blobs/{digest}");
    let (sha, _n) = hf_download(&blob_url, &dir.join(&leaf), key, &leaf, size, 0, source_token("ollama").as_deref())?;
    let verified = !expected_sha.is_empty() && expected_sha.eq_ignore_ascii_case(&sha);

    // Small text layers → license + a README from the system prompt.
    let fetch_text = |mt: &str| -> Option<String> {
        let l = find_layer(mt)?;
        let d = l.get("digest").and_then(|v| v.as_str())?;
        hf_agent().get(&format!("https://{host}/v2/{ns}/{model}/blobs/{d}")).call().ok()?.into_string().ok()
    };
    let license = fetch_text("application/vnd.ollama.image.license").unwrap_or_default();
    let license_short: String = license.lines().next().unwrap_or("").chars().take(64).collect();
    let mut readme = format!("# {model_name}\n\nImported from Ollama: `{ns}/{model}:{tag}`\n");
    if let Some(sys) = fetch_text("application/vnd.ollama.image.system") {
        readme.push_str(&format!("\n## System prompt\n\n{sys}\n"));
    }
    let _ = std::fs::write(dir.join("README.md"), &readme);

    let quant = tag.to_ascii_lowercase().split(|c| c == '-' || c == '.').rev()
        .find(|p| { let b = p.as_bytes(); b.first() == Some(&b'q') && b.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false) })
        .unwrap_or("").to_string();

    let card = ModelCard {
        kind: "llm".into(),
        base_model: format!("{ns}/{model}"),
        params: parse_params(&format!("{model} {tag}")),
        quant,
        license: license_short,
        description: format!("Imported from Ollama: {ns}/{model}:{tag}"),
        intended_use: String::new(),
        tags: vec!["ollama".into(), "gguf".into()],
        format: "gguf".into(),
        image: String::new(),
        readme: true,
        ..Default::default()
    };
    let (origin_id, origin_label) = crate::fleet::identity();
    let ns_seg = if ns == "library" { String::new() } else { format!("{ns}/") };
    let source = format!("https://ollama.com/{ns_seg}{model}:{tag}");
    let card_doc = json!({
        "schema": "aeon-model-card/1", "name": model_name, "file": leaf,
        "size_bytes": size, "sha256": sha, "card": card,
        "verified": verified, "source": source,
        "shared_by": {"id": origin_id, "label": origin_label}, "created_ms": epoch_ms(),
    });
    std::fs::write(dir.join("model-card.json"), serde_json::to_vec_pretty(&card_doc).unwrap_or_default())
        .map_err(|e| format!("write card: {e}"))?;

    task_set(key, "adding to IPFS…");
    let cid = run_script(&["add", &dir.to_string_lossy()])?;
    if cid.is_empty() {
        return Err("ipfs add produced no CID".into());
    }
    catalog_add(ModelEntry {
        cid, name: model_name.to_string(), file: leaf, size_bytes: size,
        sha256: sha, card, added_at_ms: epoch_ms(), origin_id, origin_label,
        verified, source,
    });
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[derive(Deserialize)]
pub struct ImportOllamaReq {
    pub reference: String,
}

/// POST /api/ipfs/models/import-ollama — import a model from the Ollama registry:
/// pull the GGUF weights (verified against the layer digest) + the license/system
/// metadata, package them as a shared IPFS model. Non-blocking; poll /models.
pub async fn import_ollama(State(_s): State<AppState>, Json(req): Json<ImportOllamaReq>) -> Json<Value> {
    if !read_config().enabled {
        return Json(json!({"ok": false, "err": "IPFS is off — enable it first"}));
    }
    let reference = req.reference.trim().to_string();
    let (_ns, model, tag) = match parse_ollama_ref(&reference) {
        Ok(v) => v,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let model_name = if tag == "latest" { model } else { format!("{model}-{tag}") };
    let key = model_name.clone();
    let importing = key.clone();
    task_set(&key, "fetching model info…");
    tokio::task::spawn_blocking(move || {
        if let Err(e) = do_import_ollama(&reference, &model_name, &key) {
            task_set(&key, &format!("error: {e}"));
        } else {
            task_clear(&key);
            announce();
        }
    });
    Json(json!({"ok": true, "importing": importing}))
}

// ── Civitai import (generative models: checkpoints, LoRAs, VAEs, ControlNets) ─
// Civitai's API resolves a model version to a downloadable file + a published
// SHA-256; gated / mature ("red") content needs the account's token. We prefer
// the SafeTensor file (never a pickle) so the shared network stays code-safe.

/// Parse a Civitai reference into (model_id, optional version_id).
fn parse_civitai_ref(input: &str) -> Result<(Option<u64>, Option<u64>), String> {
    let s = input.trim();
    // /api/download/models/<vid>  or  ?modelVersionId=<vid>
    if let Some(i) = s.find("/api/download/models/") {
        let tail: String = s[i + 21..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = tail.parse() { return Ok((None, Some(v))); }
    }
    let vid = s.find("modelVersionId=").and_then(|i| {
        s[i + 15..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    });
    // /models/<id>
    let mid = if let Some(i) = s.find("/models/") {
        s[i + 8..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    } else {
        s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
    };
    if mid.is_none() && vid.is_none() {
        return Err("expected a civitai.com/models/<id> URL or a model/version id".into());
    }
    Ok((mid, vid))
}

fn do_import_civitai(reference: &str, model_name_hint: &str, key: &str) -> Result<(), String> {
    let (model_id, version_id) = parse_civitai_ref(reference)?;
    let auth = source_token("civitai");
    task_set(key, "resolving Civitai model…");

    // Resolve to a specific version + its parent model summary.
    let (version, model_meta) = if let Some(vid) = version_id {
        let v = http_get_json(&format!("https://civitai.com/api/v1/model-versions/{vid}"), auth.as_deref())?;
        let m = v.get("model").cloned().unwrap_or_else(|| json!({}));
        (v, m)
    } else {
        let mid = model_id.ok_or("no model id")?;
        let m = http_get_json(&format!("https://civitai.com/api/v1/models/{mid}"), auth.as_deref())?;
        let v = m.get("modelVersions").and_then(|a| a.as_array()).and_then(|a| a.first()).cloned()
            .ok_or("model has no downloadable versions")?;
        (v, m)
    };

    let files = version.get("files").and_then(|f| f.as_array()).cloned().unwrap_or_default();
    // Prefer a SafeTensor primary-model file; never a pickle.
    let pick = files.iter()
        .find(|f| f.get("type").and_then(|v| v.as_str()) == Some("Model")
            && f.get("metadata").and_then(|m| m.get("format")).and_then(|v| v.as_str()) == Some("SafeTensor"))
        .or_else(|| files.iter().find(|f| f.get("metadata").and_then(|m| m.get("format")).and_then(|v| v.as_str()) == Some("SafeTensor")))
        .cloned();
    let file = match pick {
        Some(f) => f,
        None => return Err("no SafeTensor file on this model — pickle/.ckpt-only models aren't accepted (code-execution risk); pick one with a safetensors version".into()),
    };

    let leaf = {
        let n = file.get("name").and_then(|v| v.as_str()).unwrap_or("model.safetensors");
        n.rsplit('/').next().unwrap_or(n).to_string()
    };
    let size = (file.get("sizeKB").and_then(|v| v.as_f64()).unwrap_or(0.0) * 1024.0) as u64;
    let expected_sha = file.get("hashes").and_then(|h| h.get("SHA256")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let vid = version.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    // Download URL — token as a query param survives Civitai's CDN redirect.
    let mut dl = file.get("downloadUrl").and_then(|v| v.as_str()).map(String::from)
        .unwrap_or_else(|| format!("https://civitai.com/api/download/models/{vid}"));
    if let Some(t) = &auth {
        dl.push_str(if dl.contains('?') { "&" } else { "?" });
        dl.push_str(&format!("token={t}"));
    }

    let free = free_bytes(MODELS_DIR);
    if size > 0 && size + 512 * 1024 * 1024 > free {
        return Err(format!("not enough disk: model is {} but only {} free", human(size), human(free)));
    }
    let dir = staging_root().join(format!("civitai-{}", new_draft_id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let (sha, _n) = hf_download(&dl, &dir.join(&leaf), key, &leaf, size, 0, None)?;
    let verified = !expected_sha.is_empty() && expected_sha.eq_ignore_ascii_case(&sha);

    // Reject anything that isn't actually a safetensors (fake/mislabelled).
    {
        let mut head = [0u8; 16];
        let hn = { use std::io::Read; std::fs::File::open(dir.join(&leaf)).and_then(|mut f| f.read(&mut head)).unwrap_or(0) };
        if !matches!(sniff_kind(&head[..hn]), "safetensors") {
            let _ = std::fs::remove_dir_all(&dir);
            return Err("downloaded file is not a valid safetensors".into());
        }
    }

    // Example GALLERY (up to 6) → card thumbnail + gallery-N.<ext> (best-effort;
    // may be mature for "red" models — the viewer's mature opt-in gates display).
    let mut image_name = String::new();
    let mut gallery: Vec<String> = Vec::new();
    if let Some(imgs) = version.get("images").and_then(|a| a.as_array()) {
        for (i, url) in imgs.iter().filter_map(|im| im.get("url").and_then(|v| v.as_str())).take(6).enumerate() {
            let ext = url.rsplit('.').next().filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("jpg");
            let name = format!("gallery-{}.{}", i + 1, safe_img_ext(ext));
            if let Ok(r) = hf_agent().get(url).call() {
                let mut bytes = Vec::new();
                use std::io::Read;
                if r.into_reader().take(MAX_IMAGE_BYTES as u64).read_to_end(&mut bytes).is_ok() && !bytes.is_empty()
                    && std::fs::write(dir.join(&name), &bytes).is_ok()
                {
                    if image_name.is_empty() {
                        image_name = name.clone();
                    }
                    gallery.push(name);
                }
            }
        }
    }

    let m_type = model_meta.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let m_name = model_meta.get("name").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty()).unwrap_or(model_name_hint).to_string();
    let nsfw = model_meta.get("nsfw").and_then(|v| v.as_bool()).unwrap_or(false)
        || version.get("model").and_then(|m| m.get("nsfw")).and_then(|v| v.as_bool()).unwrap_or(false);
    let base_model = version.get("baseModel").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut tags: Vec<String> = model_meta.get("tags").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).take(6).collect()).unwrap_or_default();
    if nsfw { tags.insert(0, "nsfw".into()); }
    let readme = String::from_utf8(strip_html(model_meta.get("description").and_then(|v| v.as_str()).unwrap_or("")).into_bytes()).unwrap_or_default();
    let _ = std::fs::write(dir.join("README.md"), format!("# {m_name}\n\nImported from Civitai: {reference}\n\n{readme}\n"));

    let card = ModelCard {
        kind: kind_from_civitai(m_type).to_string(),
        base_model,
        params: String::new(),
        quant: String::new(),
        license: String::new(),
        description: format!("Imported from Civitai{}", if nsfw { " (mature content)" } else { "" }),
        intended_use: String::new(),
        tags,
        format: "safetensors".into(),
        image: image_name,
        readme: true,
        nsfw,
        gallery,
    };
    let (origin_id, origin_label) = crate::fleet::identity();
    let source = format!("https://civitai.com/models/{}", model_meta.get("id").and_then(|v| v.as_u64()).unwrap_or(vid));
    let card_doc = json!({
        "schema": "aeon-model-card/1", "name": m_name, "file": leaf,
        "size_bytes": size, "sha256": sha, "card": card,
        "verified": verified, "source": source,
        "shared_by": {"id": origin_id, "label": origin_label}, "created_ms": epoch_ms(),
    });
    std::fs::write(dir.join("model-card.json"), serde_json::to_vec_pretty(&card_doc).unwrap_or_default())
        .map_err(|e| format!("write card: {e}"))?;

    task_set(key, "adding to IPFS…");
    let cid = run_script(&["add", &dir.to_string_lossy()])?;
    if cid.is_empty() {
        return Err("ipfs add produced no CID".into());
    }
    catalog_add(ModelEntry {
        cid, name: m_name, file: leaf, size_bytes: size,
        sha256: sha, card, added_at_ms: epoch_ms(), origin_id, origin_label,
        verified, source,
    });
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Strip HTML tags → plain text (Civitai descriptions are HTML).
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").chars().take(4000).collect()
}

#[derive(Deserialize)]
pub struct ImportCivitaiReq {
    pub reference: String,
}

/// POST /api/ipfs/models/import-civitai — import a generative model from Civitai
/// (checkpoint / LoRA / VAE / …). Uses the stored Civitai token for gated +
/// mature content. Non-blocking; poll /models.
pub async fn import_civitai(State(_s): State<AppState>, Json(req): Json<ImportCivitaiReq>) -> Json<Value> {
    if !read_config().enabled {
        return Json(json!({"ok": false, "err": "IPFS is off — enable it first"}));
    }
    let reference = req.reference.trim().to_string();
    if parse_civitai_ref(&reference).is_err() {
        return Json(json!({"ok": false, "err": "not a valid Civitai model reference"}));
    }
    let key = format!("civitai-{}", new_draft_id());
    let importing = key.clone();
    task_set(&key, "fetching model info…");
    tokio::task::spawn_blocking(move || {
        if let Err(e) = do_import_civitai(&reference, "Civitai model", &key) {
            task_set(&key, &format!("error: {e}"));
        } else {
            task_clear(&key);
            announce();
        }
    });
    Json(json!({"ok": true, "importing": importing}))
}

/// First `cdn-avatars.huggingface.co/...` URL in the page HTML — the author's
/// avatar (the circular image HF shows on the model card). HF embeds this
/// inside JSON in the HTML, so slashes may be escaped (`\/` or `/`);
/// unescape a window first, then cut at the first real delimiter.
fn find_avatar_url(html: &str) -> Option<String> {
    let needle = "cdn-avatars.huggingface.co/";
    let idx = html.find(needle)?;
    let window = &html[idx..(idx + 400).min(html.len())];
    let unescaped = window.replace("\\/", "/").replace("\\u002F", "/").replace("\\u002f", "/");
    // `&` terminates the URL when it's in an HTML-entity-encoded attribute
    // (`…jpeg&quot;`); `?` drops any query string.
    let end = unescaped
        .find(|c: char| matches!(c, '"' | '\'' | ' ' | '<' | '>' | ')' | '\\' | '\n' | '?' | '&'))
        .unwrap_or(unescaped.len());
    let path = &unescaped[..end];
    if path.len() < needle.len() + 4 {
        return None;
    }
    Some(format!("https://{path}"))
}

fn human(b: u64) -> String {
    let u = ["B", "KB", "MB", "GB", "TB"];
    let mut x = b as f64;
    let mut i = 0;
    while x >= 1024.0 && i < u.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    format!("{x:.1} {}", u[i])
}

// ── Model library: pull DOWN to the Orb, ready to push to systems ────────────
//
// A model shared over IPFS is a directory CID (weights + model-card.json + …).
// "Pulling it down" materializes that directory as plain files under
// MODEL_LIBRARY_DIR/<slug>/ so the Orb holds a re-pushable copy that
// agent_connect::push_model rsyncs to a connected system. With a big data disk
// (the user's 1 TB card, or relocated USB storage) this library can hold many
// models and re-push each instantly without re-fetching from the network.

/// Turn a model name into a filesystem-safe slug for its library subdir.
pub fn model_slug(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "model".to_string() } else { s.chars().take(80).collect() }
}

/// Look up a cataloged model by CID (exact) or by name (case-insensitive),
/// searching this Orb's local catalog first, then the gossiped global registry.
/// Lets callers push a model they only know by name/CID without it being local.
pub fn find_model(id_or_cid: &str) -> Option<ModelEntry> {
    let q = id_or_cid.trim();
    let ql = q.to_lowercase();
    let hit = |e: &ModelEntry| e.cid == q || e.name.to_lowercase() == ql;
    if let Some(e) = read_catalog().into_iter().find(|e| hit(e)) {
        return Some(e);
    }
    std::fs::read_to_string(REGISTRY_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<ModelEntry>>(&t).ok())
        .and_then(|v| v.into_iter().find(|e| hit(e)))
}

/// Absolute path of a model's materialized directory in the library (may not
/// exist yet). `pub` so agent_connect can rsync straight from it.
pub fn library_path(slug: &str) -> std::path::PathBuf {
    std::path::Path::new(MODEL_LIBRARY_DIR).join(slug)
}

/// True once a model's files are materialized locally (dir exists + non-empty).
pub fn library_has(slug: &str) -> bool {
    std::fs::read_dir(library_path(slug)).map(|mut d| d.next().is_some()).unwrap_or(false)
}

/// Materialize a model directory CID into the library (idempotent — a no-op if
/// already present). Fetches from the IPFS network on demand. Returns the
/// on-disk byte size. `pub` so push_model can auto-pull before rsync.
pub fn ensure_in_library(cid: &str, slug: &str) -> Result<u64, String> {
    let dir = library_path(slug);
    if library_has(slug) {
        return Ok(dir_size(&dir));
    }
    let dest = dir.to_string_lossy().to_string();
    run_script(&["get", cid, &dest]).map(|s| s.trim().parse().unwrap_or_else(|_| dir_size(&dir)))
}

fn dir_size(p: &std::path::Path) -> u64 {
    fn walk(p: &std::path::Path) -> u64 {
        let Ok(rd) = std::fs::read_dir(p) else { return 0 };
        rd.flatten()
            .map(|e| {
                let path = e.path();
                match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&path),
                    Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => 0,
                }
            })
            .sum()
    }
    walk(p)
}

/// One library entry for the API: slug, matching catalog name/cid if known, size.
fn library_entries() -> Vec<Value> {
    let cat = read_catalog();
    std::fs::read_dir(MODEL_LIBRARY_DIR)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let slug = e.file_name().to_string_lossy().to_string();
            // Best-effort re-associate with a catalog entry (slug ← name).
            let hit = cat.iter().find(|c| model_slug(&c.name) == slug);
            json!({
                "slug": slug,
                "name": hit.map(|c| c.name.clone()).unwrap_or_else(|| slug.clone()),
                "cid": hit.map(|c| c.cid.clone()).unwrap_or_default(),
                "size_bytes": dir_size(&e.path()),
            })
        })
        .collect()
}

#[derive(Deserialize)]
pub struct PullReq {
    /// Directory CID to pull. Optional if `name` resolves via the catalog.
    #[serde(default)]
    pub cid: String,
    /// Display name — also used (slugified) as the library subdir.
    #[serde(default)]
    pub name: String,
}

/// Everything an MCP agent needs to choose a model to pull/push: the shared
/// catalog (this Orb's own + gossiped peers, each with cid/name/size) and which
/// models are already materialized in the local library.
pub fn model_list_value() -> Value {
    let catalog: Vec<Value> = read_catalog()
        .into_iter()
        .map(|e| json!({"cid": e.cid, "name": e.name, "size_bytes": e.size_bytes, "size": human(e.size_bytes), "in_library": library_has(&model_slug(&e.name))}))
        .collect();
    let registry: Vec<Value> = std::fs::read_to_string(REGISTRY_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<ModelEntry>>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|e| json!({"cid": e.cid, "name": e.name, "size_bytes": e.size_bytes, "size": human(e.size_bytes)}))
        .collect();
    json!({
        "ok": true,
        "catalog": catalog,        // shareable on this Orb
        "network": registry,       // discovered on the network (gossip)
        "library": library_entries(),  // pulled down, ready to push
    })
}

/// GET /api/ipfs/models/library — what's materialized locally + total size.
pub async fn models_library(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| {
        let items = library_entries();
        let total: u64 = items.iter().filter_map(|i| i.get("size_bytes").and_then(|x| x.as_u64())).sum();
        json!({"ok": true, "library": items, "total_bytes": total, "dir": MODEL_LIBRARY_DIR})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "library task failed"}));
    Json(v)
}

/// POST /api/ipfs/models/pull — materialize a model to the Orb's library so it
/// can be pushed to connected systems. Resolves CID/name via the catalog when
/// one is omitted. Synchronous but fast when the blocks are already local.
pub async fn pull_model(State(_s): State<AppState>, Json(req): Json<PullReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        // Resolve to (cid, name): prefer explicit fields, fall back to catalog.
        let (cid, name) = if !req.cid.is_empty() && !req.name.is_empty() {
            (req.cid.clone(), req.name.clone())
        } else if let Some(e) = find_model(if !req.cid.is_empty() { &req.cid } else { &req.name }) {
            (
                if req.cid.is_empty() { e.cid } else { req.cid.clone() },
                if req.name.is_empty() { e.name } else { req.name.clone() },
            )
        } else {
            return json!({"ok": false, "err": "unknown model — pass both cid and name, or a cataloged cid/name"});
        };
        if cid.is_empty() {
            return json!({"ok": false, "err": "no CID to pull"});
        }
        let slug = model_slug(&name);
        task_set(&format!("pull:{slug}"), "materializing from IPFS…");
        let out = ensure_in_library(&cid, &slug);
        task_clear(&format!("pull:{slug}"));
        match out {
            Ok(sz) => json!({"ok": true, "slug": slug, "name": name, "cid": cid, "size_bytes": sz, "size": human(sz)}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "pull task failed"}));
    Json(v)
}
