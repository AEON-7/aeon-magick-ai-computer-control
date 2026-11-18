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

pub mod directory;
pub mod upstream;

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
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
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
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
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

/// The only places `/api/ipfs/add` (and the ipfs_add MCP tool) will publish from.
const ADD_ROOTS: [&str; 3] = ["/var/lib/aeon/files", "/var/lib/aeon/model-library", "/srv"];

/// POST /api/ipfs/add — add a local file/dir to IPFS, returning its root CID (so
/// it's reachable at <gateway>/ipfs/<cid> and on any IPFS gateway). Admin over
/// REST; also the ipfs_add MCP tool so an agent can publish a site/app it built.
pub async fn add_path(State(_s): State<AppState>, Json(req): Json<AddReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        // Adding to IPFS is PUBLISHING — the bytes become fetchable by anyone,
        // permanently. An agent token can reach this through the ipfs_add MCP
        // tool, so it must not be a way to push an arbitrary readable file off
        // the Orb. Resolve links first, then require a publishing directory.
        let real = match std::fs::canonicalize(&req.path) {
            Ok(p) => p,
            Err(e) => return json!({"ok": false, "err": format!("no such path: {e}")}),
        };
        if !ADD_ROOTS.iter().any(|root| real.starts_with(root)) {
            return json!({"ok": false, "err": format!(
                "refusing to publish {} — put what you want to share under one of: {}",
                real.display(), ADD_ROOTS.join(", "))});
        }
        match run_script(&["add", &real.to_string_lossy()]) {
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
// Discovery is fleet-FREE: `aeon-modelshare` finds other Orbs via Amino DHT
// `findprovs` of a well-known rendezvous CID (WAN, no LAN IPs), swarm-connects
// (direct / hole-punch / circuit-relay), then gossips this Orb's catalog over
// the libp2p pubsub topic `aeon-model-share/v1` and merges every other Orb's
// announcements into registry.json. Any Orb on the IPFS network converges on
// the same global index with no shared token — the fleet heartbeat still
// carries the catalog too (a fast LAN path) but is not required.

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Publisher provenance (Phase 1b) — set when an unlocked publisher identity
    /// signs this model at share/import time. `pub_key` = the ed25519 signer
    /// pubkey (hex, the canonical identity), `pub_name` = the signer's local
    /// display petname, `pub_sig` = base64 signature over
    /// publisher::publication_digest(cid, name, sha256, added_at_ms). Empty =
    /// unsigned. Gossiped with the entry and re-verified on receipt — the name
    /// is decorative, the pubkey/fingerprint is what's cryptographically real.
    #[serde(default)]
    pub pub_key: String,
    #[serde(default)]
    pub pub_name: String,
    #[serde(default)]
    pub pub_sig: String,
    /// Present when `pub_key` is an Orb's device key publishing on behalf of a
    /// user identity: the user key's signed authorization of that Orb key.
    /// Verifiers credit the entry to `delegation.user_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<crate::publisher::Delegation>,
    /// BitTorrent v1 infohash of a torrent holding exactly these files. Set
    /// when this Orb pulled the model from a directory (it then seeds that
    /// torrent too), or by another node that says it seeds one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub btih: String,
    /// Set on a byte-identical mirror of a vendor's artifact (e.g.
    /// "hailo-model-zoo"). The Orb announcing it is a SEEDER of the vendor's
    /// file, not its publisher — the index must not present it as the origin
    /// or let the vendor's work build the Orb's reputation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upstream: String,
    /// For a model imported from a provider: the revision that was pinned and,
    /// per file, the SHA-256 the provider published (or, where it published
    /// none, the hash computed at import — marked as such). See upstream.rs.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "upstream::lenient_record")]
    pub upstream_record: Option<upstream::UpstreamRecord>,
    /// Signed statements over `upstream_record`: who saw the provider publish
    /// those hashes, and when each of them says it did.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "upstream::lenient_witnesses")]
    pub witnesses: Vec<upstream::Witness>,
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
    // The catalog is announced as it is written. An entry over the gossip
    // size limit would be dropped whole by every peer, so one that carries
    // more witnesses or a larger upstream record than fits gives those up.
    let mut entries = entries.to_vec();
    entries.iter_mut().for_each(upstream::fit_for_gossip);
    let tmp = format!("{CATALOG_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&entries).unwrap_or_default())?;
    std::fs::rename(tmp, CATALOG_JSON)
}

/// Append an entry unless the CID is already cataloged (idempotent mirror).
/// Write an entry to the catalog verbatim — no signing. Used ONLY on the fetch
/// path, where the entry is a PEER's (its publisher signature, if any, must be
/// preserved, never overwritten with ours).
fn catalog_add_raw(entry: ModelEntry) {
    let mut entries = read_catalog();
    if !entries.iter().any(|e| e.cid == entry.cid) {
        entries.push(entry);
        let _ = write_catalog(&entries);
    }
}

/// Sign a model this Orb ORIGINATES (share / publish / import) unless it is
/// already signed. The Orb's device key signs, so this works from first boot
/// with nothing to unlock; when a user identity has delegated to this Orb the
/// delegation rides along and the entry is credited to that user. The name is
/// a decorative petname; the key/signature is the cryptographic provenance.
fn sign_entry(entry: &mut ModelEntry) {
    sign_entry_with(entry, true)
}

/// `sign_entry`, choosing whether the signed-in publisher identity is attached.
fn sign_entry_with(entry: &mut ModelEntry, identity: bool) {
    if !entry.pub_sig.is_empty() {
        return;
    }
    let digest =
        crate::publisher::publication_digest(&entry.cid, &entry.name, &entry.sha256, entry.added_at_ms);
    if let Ok(s) = crate::publisher::sign_as_orb_with(&digest, identity) {
        entry.pub_name = if !s.name.is_empty() {
            s.name
        } else if !entry.origin_label.is_empty() {
            entry.origin_label.clone()
        } else {
            // No user identity and no fleet label: the hostname beats "anon".
            std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_default()
        };
        entry.pub_key = s.pubkey;
        entry.pub_sig = s.sig;
        entry.delegation = s.delegation;
    }
}

/// True when `entry.pub_key` really signed this entry's cid/name/sha256/time.
/// Cached: the registry view re-checks every gossiped entry on each poll, and
/// an ed25519 verify per entry per poll adds up on a Pi.
fn entry_sig_ok(entry: &ModelEntry) -> bool {
    if entry.pub_sig.is_empty() || entry.pub_key.is_empty() {
        return false;
    }
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let digest =
        crate::publisher::publication_digest(&entry.cid, &entry.name, &entry.sha256, entry.added_at_ms);
    let key = format!("{}|{}|{}", entry.pub_key, entry.pub_sig, String::from_utf8_lossy(&digest));
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return hit;
    }
    let ok = crate::publisher::verify(&entry.pub_key, &digest, &entry.pub_sig);
    if let Ok(mut c) = cache.lock() {
        if c.len() > 20_000 {
            c.clear();
        }
        c.insert(key, ok);
    }
    ok
}

/// The identity an entry is credited to: the delegating user key when the
/// signer carries a valid delegation, else the signer. "" when unsigned or the
/// signature does not verify.
fn entry_identity(entry: &ModelEntry) -> String {
    if entry_sig_ok(entry) {
        crate::publisher::identity_of(&entry.pub_key, entry.delegation.as_ref())
    } else {
        String::new()
    }
}

// ── Signed manifest + origin attestation ────────────────────────────────────
//
// A model's ORIGIN is whoever first packaged it, and that has to stay
// checkable no matter how many Orbs later mirror it. So the origin signs a
// manifest — every file's path, size and SHA-256 — and the signature lives in
// model-card.json INSIDE the shared directory. Mirrors carry it unchanged (it
// is part of the content the CID names), nobody can swap it without producing
// a different CID, and because it names plain file hashes it verifies the
// same over IPFS, BitTorrent or a copied folder.

const CARD_FILE: &str = "model-card.json";
const VERIFY_JSON: &str = "/var/lib/aeon/ipfs-models/verify.json";
const TRUST_JSON: &str = "/var/lib/aeon/ipfs-models/trust.json";
const BINDING_JSON: &str = "/var/lib/aeon/ipfs-models/orb-binding.json";

/// One file of a shared model, as named in the signed manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRec {
    pub path: String, // relative to the model dir, '/'-separated
    pub size: u64,
    pub sha256: String, // lowercase hex
}

/// The origin statement stored in model-card.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OriginBlock {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String, // signer (an Orb key), hex
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub signed_ms: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String, // base64 ed25519 over publisher::origin_digest
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<crate::publisher::Delegation>,
    /// Set INSTEAD of a signature on a byte-identical mirror of a vendor's
    /// artifact (e.g. "hailo-model-zoo"). Such a card holds nothing specific
    /// to one Orb, so every Orb mirroring the same file produces the same CID
    /// and they all seed one swarm.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upstream: String,
}

/// The parts of model-card.json that verification reads.
#[derive(Debug, Default, Deserialize)]
struct CardDoc {
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    files: Option<Vec<FileRec>>,
    #[serde(default)]
    origin: Option<OriginBlock>,
    #[serde(default, deserialize_with = "upstream::lenient_record")]
    upstream_record: Option<upstream::UpstreamRecord>,
    #[serde(default, deserialize_with = "upstream::lenient_witnesses")]
    witnesses: Vec<upstream::Witness>,
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Every regular file under `dir` except the card, sorted by path. Refuses
/// symbolic links and special files outright: a link materialized from a
/// peer's directory could point anywhere on this Orb and would later be
/// scanned, hashed or rsynced as if it were model data. `known` supplies
/// hashes already computed while the bytes streamed in, to skip a re-read.
fn build_manifest(dir: &std::path::Path, known: &HashMap<String, String>) -> Result<Vec<FileRec>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = std::fs::read_dir(&d).map_err(|e| format!("read model dir: {e}"))?;
        for ent in rd {
            let ent = ent.map_err(|e| format!("read model dir: {e}"))?;
            let p = ent.path();
            let rel = p
                .strip_prefix(dir)
                .map_err(|_| "path escapes the model dir".to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            // DirEntry::file_type does NOT follow symlinks.
            let ft = ent.file_type().map_err(|e| format!("stat “{rel}”: {e}"))?;
            if ft.is_symlink() {
                return Err(format!("rejected: “{rel}” is a symbolic link — a model may only contain regular files"));
            }
            if ft.is_dir() {
                stack.push(p);
                continue;
            }
            if !ft.is_file() {
                return Err(format!("rejected: “{rel}” is not a regular file"));
            }
            if rel.chars().any(|c| c.is_control()) {
                return Err("rejected: a file name contains control characters".into());
            }
            if rel == CARD_FILE {
                continue;
            }
            let size = ent.metadata().map(|m| m.len()).map_err(|e| format!("stat “{rel}”: {e}"))?;
            let sha256 = match known.get(&rel) {
                Some(h) => h.to_ascii_lowercase(),
                None => sha256_file(&p).map_err(|e| format!("hash “{rel}”: {e}"))?,
            };
            out.push(FileRec { path: rel, size, sha256 });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn manifest_tuples(files: &[FileRec]) -> Vec<(String, u64, String)> {
    files.iter().map(|f| (f.path.clone(), f.size, f.sha256.clone())).collect()
}

/// What a share needs beyond the files already sitting in its directory.
struct ShareSpec {
    name: String,
    /// Primary weights file (path inside the dir). "" = the largest file.
    file: String,
    card: ModelCard,
    verified: bool,
    source: String,
    /// Non-empty = byte-identical vendor mirror; see `OriginBlock::upstream`.
    upstream: String,
    /// Credit the share to the signed-in publisher identity (the default).
    /// False = signed by this Orb's key alone, with no name attached.
    identity: bool,
    /// For an import from a provider: what the provider published for these
    /// files, and when this Orb read it. None for everything else.
    provider_record: Option<(upstream::UpstreamRecord, i64)>,
}

/// Write model-card.json (manifest + origin statement) into `dir` and return
/// the catalog entry for it, minus the CID. Pure filesystem + signing, so it
/// is unit-testable without an IPFS node.
fn prepare_share(dir: &std::path::Path, spec: ShareSpec, known: &HashMap<String, String>) -> Result<ModelEntry, String> {
    let files = build_manifest(dir, known)?;
    let primary = files
        .iter()
        .find(|f| f.path == spec.file)
        .or_else(|| files.iter().max_by_key(|f| f.size))
        .cloned()
        .ok_or("nothing to share — the model directory is empty")?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    let vendor_mirror = !spec.upstream.is_empty();
    // A vendor mirror must hash identically on every Orb: no clock, no Orb id.
    let now = if vendor_mirror { 0 } else { epoch_ms() };
    let (origin_id, origin_label) = crate::fleet::identity();

    let origin = if vendor_mirror {
        OriginBlock { upstream: spec.upstream.clone(), ..Default::default() }
    } else {
        let digest =
            crate::publisher::origin_digest(&spec.name, &spec.source, spec.verified, now, &manifest_tuples(&files));
        match crate::publisher::sign_as_orb_with(&digest, spec.identity) {
            Ok(s) => OriginBlock {
                key: s.pubkey,
                name: s.name,
                signed_ms: now,
                sig: s.sig,
                delegation: s.delegation,
                upstream: String::new(),
            },
            Err(_) => OriginBlock { signed_ms: now, ..Default::default() },
        }
    };

    // What the provider published, and this Orb's signed statement that it
    // saw it. The record may only speak about files that are in this share.
    let (upstream_record, witnesses) = match spec.provider_record {
        Some((rec, seen_ms)) if !vendor_mirror => {
            if !rec.within(&files) {
                return Err("the upstream record does not match the files being shared".into());
            }
            let witness = upstream::sign_witness(&rec, seen_ms, spec.identity);
            (Some(rec), witness.into_iter().collect::<Vec<_>>())
        }
        _ => (None, Vec::new()),
    };

    let mut doc = json!({
        "schema": "aeon-model-card/2",
        "name": spec.name,
        "file": primary.path,
        "size_bytes": total,
        "sha256": primary.sha256,
        "card": spec.card,
        "verified": spec.verified,
        "source": spec.source,
        "files": files,
        "origin": origin,
        "created_ms": now,
    });
    if !vendor_mirror {
        doc["shared_by"] = json!({"id": origin_id, "label": origin_label});
    }
    if let Some(rec) = &upstream_record {
        doc["upstream_record"] = json!(rec);
        if !witnesses.is_empty() {
            doc["witnesses"] = json!(witnesses);
        }
    }
    std::fs::write(dir.join(CARD_FILE), serde_json::to_vec_pretty(&doc).unwrap_or_default())
        .map_err(|e| format!("write card: {e}"))?;

    Ok(ModelEntry {
        cid: String::new(),
        name: spec.name,
        file: primary.path,
        size_bytes: total,
        sha256: primary.sha256,
        card: spec.card,
        added_at_ms: epoch_ms(),
        origin_id,
        origin_label,
        verified: spec.verified,
        source: spec.source,
        upstream: spec.upstream,
        upstream_record,
        witnesses,
        ..Default::default()
    })
}

/// `prepare_share`, then add the directory to IPFS (pinned) and sign the entry.
/// Every origination path ends here, so every model is shared the same way.
fn finalize_share(dir: &std::path::Path, spec: ShareSpec, known: &HashMap<String, String>) -> Result<ModelEntry, String> {
    let vendor_mirror = !spec.upstream.is_empty();
    let identity = spec.identity;
    let mut entry = prepare_share(dir, spec, known)?;
    let d = dir.to_string_lossy().to_string();
    // Vendor mirrors pin the import parameters so the CID is the same on every
    // Orb regardless of kubo version or config defaults.
    let cid = if vendor_mirror {
        run_script(&["add-fixed", &d]).or_else(|_| run_script(&["add", &d]))?
    } else {
        run_script(&["add", &d])?
    };
    if !valid_cid(&cid) {
        return Err("ipfs add produced no CID".into());
    }
    entry.cid = cid;
    sign_entry_with(&mut entry, identity);
    // We built it from local files, so it is verified by construction.
    record_check(&entry.cid, &ContentCheck {
        manifest: true,
        files_ok: true,
        origin_key: entry.pub_key.clone(),
        origin_identity: entry_identity(&entry),
        origin_ok: entry_sig_ok(&entry),
        upstream: if vendor_mirror { "local".into() } else { String::new() },
        checked_ms: epoch_ms(),
        ..Default::default()
    });
    // This Orb's own memory of what the provider published: one line in the
    // append-only history.
    upstream::note_recorded(&entry);
    Ok(entry)
}

/// The outcome of checking a downloaded model directory against its card.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentCheck {
    /// The card carried a file manifest (schema /2).
    #[serde(default)]
    pub manifest: bool,
    /// Every file matched its recorded size + SHA-256 and nothing was extra.
    #[serde(default)]
    pub files_ok: bool,
    /// The origin signature verified over that manifest.
    #[serde(default)]
    pub origin_ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin_key: String,
    /// Who the origin is credited to (a user key via delegation, or the signer).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin_identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub upstream: String,
    /// What the (signed) card says about an upstream hash check.
    #[serde(default)]
    pub claims_verified: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default)]
    pub checked_ms: i64,
    /// The card's upstream record, when every file it names is in the checked
    /// manifest with the same size and hash, and the witnesses on the card
    /// whose signatures hold for it. Passed to the caller, not stored here.
    #[serde(skip)]
    pub upstream_record: Option<upstream::UpstreamRecord>,
    #[serde(skip)]
    pub witnesses: Vec<upstream::Witness>,
}

/// Check a materialized model directory before it is trusted: no links, every
/// file present with the size and hash the manifest promises, nothing extra,
/// and the origin signature valid over exactly that manifest. `Err` is a hard
/// rejection (the bytes are not what was signed or advertised). `Ok` with
/// `origin_ok == false` is honest-but-unsigned content — allowed, shown as
/// unverified.
fn verify_model_dir(dir: &std::path::Path, entry: &ModelEntry) -> Result<ContentCheck, String> {
    let actual = build_manifest(dir, &HashMap::new())?;
    let mut check = ContentCheck { checked_ms: epoch_ms(), ..Default::default() };

    let card_path = dir.join(CARD_FILE);
    let card: Option<CardDoc> = match std::fs::symlink_metadata(&card_path) {
        Ok(m) if m.is_file() && m.len() <= 1024 * 1024 => std::fs::read_to_string(&card_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok()),
        Ok(m) if m.is_file() => return Err("rejected: model-card.json is implausibly large".into()),
        _ => None,
    };

    if let Some(CardDoc { files: Some(mut files), name, source, verified, origin, upstream_record, witnesses }) = card {
        check.manifest = true;
        for f in &mut files {
            f.sha256 = f.sha256.to_ascii_lowercase();
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if files != actual {
            let odd = actual
                .iter()
                .find(|a| !files.contains(a))
                .map(|a| a.path.clone())
                .or_else(|| files.iter().find(|f| !actual.contains(f)).map(|f| f.path.clone()))
                .unwrap_or_default();
            return Err(format!("rejected: the files do not match the model's signed manifest (“{odd}”)"));
        }
        check.files_ok = true;
        // An upstream record is kept only when it describes these very files.
        if let Some(rec) = upstream_record.filter(|r| r.within(&files)) {
            check.witnesses = upstream::valid_witnesses(&rec, &witnesses);
            check.upstream_record = Some(rec);
        }
        if let Some(o) = origin {
            check.upstream = o.upstream.clone();
            if !o.sig.is_empty() {
                let digest =
                    crate::publisher::origin_digest(&name, &source, verified, o.signed_ms, &manifest_tuples(&files));
                if !crate::publisher::verify(&o.key, &digest, &o.sig) {
                    return Err("rejected: the origin signature does not match this model's contents".into());
                }
                check.origin_ok = true;
                check.origin_identity = crate::publisher::identity_of(&o.key, o.delegation.as_ref());
                check.origin_name = match &o.delegation {
                    Some(d) if check.origin_identity == d.user_key => d.user_name.clone(),
                    _ => o.name.clone(),
                };
                check.origin_key = o.key;
                check.claims_verified = verified;
                check.source = source;
            }
        }
        return Ok(check);
    }

    // Older card with no manifest: the one thing the entry promises is the
    // primary file's hash, so hold it to that.
    if !entry.sha256.is_empty() && !entry.file.is_empty() {
        match actual.iter().find(|f| f.path == entry.file) {
            Some(f) if f.sha256.eq_ignore_ascii_case(&entry.sha256) => check.files_ok = true,
            Some(_) => {
                return Err(format!("rejected: “{}” does not match the SHA-256 it was advertised with", entry.file))
            }
            None => return Err(format!("rejected: the advertised file “{}” is not in this model", entry.file)),
        }
    }
    Ok(check)
}

/// Guards read-modify-write of the small local JSON stores below.
fn store_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

fn write_json_file<T: Serialize>(path: &str, v: &T) {
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// cid → what this Orb found when it checked that model's bytes itself. Kept
/// out of catalog.json on purpose: the catalog is gossiped, and "I verified
/// it" is only worth anything to the Orb that did the verifying.
fn read_checks() -> HashMap<String, ContentCheck> {
    std::fs::read_to_string(VERIFY_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn record_check(cid: &str, check: &ContentCheck) {
    let _g = store_lock().lock();
    let mut all = read_checks();
    all.insert(cid.to_string(), check.clone());
    write_json_file(VERIFY_JSON, &all);
}

// ── Publisher trust + reputation (local-first) ──────────────────────────────
//
// There is no global authority to say a publisher is good, and anything
// gossiped can be faked by spinning up identities. So reputation is computed
// on each Orb, weighted by how hard the evidence is to forge: the operator's
// own decision outranks everything, then this Orb's own verified downloads,
// then mirrors (each costs a real Orb real disk), and last and least, stars.

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Publisher {
    /// The operator's own name for this key — the only name worth trusting.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    petname: String,
    #[serde(default)]
    trusted: bool,
    #[serde(default)]
    blocked: bool,
    #[serde(default)]
    first_seen_ms: i64,
    /// Downloads from this publisher that verified here / that failed here.
    #[serde(default)]
    ok: u32,
    #[serde(default)]
    bad: u32,
    /// The name the publisher last used for itself (self-asserted, decorative).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    last_name: String,
}

/// Which identity this Orb FIRST saw offering a given weights hash. A later,
/// different identity offering the same bytes is a re-publisher, not the origin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Claim {
    key: String,
    first_seen_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustStore {
    #[serde(default)]
    publishers: HashMap<String, Publisher>,
    #[serde(default)]
    claims: HashMap<String, Claim>,
}

fn read_trust() -> TrustStore {
    std::fs::read_to_string(TRUST_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn trust_update(f: impl FnOnce(&mut TrustStore)) {
    let _g = store_lock().lock();
    let mut t = read_trust();
    f(&mut t);
    write_json_file(TRUST_JSON, &t);
}

/// 0–100. `starrers` = distinct Orbs starring any of the publisher's models,
/// `mirrors` = distinct other Orbs hosting any of them.
fn reputation(p: Option<&Publisher>, signed: bool, starrers: usize, mirrors: usize, now_ms: i64) -> u8 {
    if !signed {
        return 0; // nobody to hold to account
    }
    let Some(p) = p else { return 20 };
    if p.blocked {
        return 0;
    }
    if p.trusted {
        return 100;
    }
    if p.bad > 0 {
        return 5; // served bytes that failed verification here
    }
    let mut s = 20.0_f64;
    s += f64::from(p.ok.min(3)) * 15.0;
    s += (5.0 * ((1 + starrers) as f64).log2()).min(15.0);
    s += (mirrors as f64 * 2.0).min(10.0);
    let age_days = (now_ms - p.first_seen_ms).max(0) / 86_400_000;
    s += if age_days >= 30 { 10.0 } else if age_days >= 7 { 5.0 } else { 0.0 };
    s.min(95.0) as u8 // 100 is reserved for the operator's explicit trust
}

/// GET /api/ipfs/models/trust — every publisher this Orb has seen, with the
/// operator's decisions and this Orb's own verification tally.
pub async fn get_trust(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let t = read_trust();
        let now = epoch_ms();
        let mut list: Vec<Value> = t
            .publishers
            .iter()
            .map(|(key, p)| {
                json!({
                    "key": key, "fp": key.get(..12).unwrap_or(key),
                    "name": if p.petname.is_empty() { &p.last_name } else { &p.petname },
                    "petname": p.petname, "trusted": p.trusted, "blocked": p.blocked,
                    "first_seen_ms": p.first_seen_ms, "verified_ok": p.ok, "verified_bad": p.bad,
                    "reputation": reputation(Some(p), true, 0, 0, now),
                })
            })
            .collect();
        list.sort_by(|a, b| b["reputation"].as_u64().cmp(&a["reputation"].as_u64()));
        json!({"ok": true, "publishers": list, "this_orb": crate::publisher::orb_pubkey()})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "trust task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct TrustReq {
    pub key: String,
    #[serde(default)]
    pub trusted: Option<bool>,
    #[serde(default)]
    pub blocked: Option<bool>,
    #[serde(default)]
    pub petname: Option<String>,
}

/// POST /api/ipfs/models/trust — trust or block a publisher key, or give it a
/// local name. Trusting and blocking are mutually exclusive.
pub async fn set_trust(State(_s): State<AppState>, Json(req): Json<TrustReq>) -> Json<Value> {
    let key = req.key.trim().to_ascii_lowercase();
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Json(json!({"ok": false, "err": "not a publisher key"}));
    }
    tokio::task::spawn_blocking(move || {
        trust_update(|t| {
            let p = t.publishers.entry(key).or_insert_with(|| Publisher { first_seen_ms: epoch_ms(), ..Default::default() });
            if let Some(v) = req.trusted {
                p.trusted = v;
                if v {
                    p.blocked = false;
                }
            }
            if let Some(v) = req.blocked {
                p.blocked = v;
                if v {
                    p.trusted = false;
                }
            }
            if let Some(n) = req.petname {
                p.petname = n.trim().chars().filter(|c| !c.is_control()).take(40).collect();
            }
        });
    })
    .await
    .ok();
    Json(json!({"ok": true}))
}

/// Keep `orb-binding.json` current: this Orb's key vouching for its own
/// libp2p peer id. The gossip daemon copies it into every announcement, which
/// is what lets another Orb tell that a host IS a model's origin seeder.
fn ensure_binding() {
    let peer_id = self_peer_id();
    let orb_key = crate::publisher::orb_pubkey();
    if peer_id.is_empty() || orb_key.is_empty() {
        return;
    }
    let delegation = crate::publisher::current_delegation();
    let current: Value = std::fs::read_to_string(BINDING_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let want_user = delegation.as_ref().map(|d| d.user_key.as_str()).unwrap_or("");
    let have_user = current.get("delegation").and_then(|d| d.get("user_key")).and_then(|v| v.as_str()).unwrap_or("");
    if current.get("peer_id").and_then(|v| v.as_str()) == Some(peer_id.as_str())
        && current.get("orb_key").and_then(|v| v.as_str()) == Some(orb_key.as_str())
        && want_user == have_user
    {
        return;
    }
    let Ok(s) = crate::publisher::sign_as_orb(&crate::publisher::peer_binding_digest(&orb_key, &peer_id)) else {
        return;
    };
    let mut doc = json!({"orb_key": orb_key, "peer_id": peer_id, "orb_sig": s.sig});
    if let Some(d) = delegation {
        doc["delegation"] = json!(d);
    }
    write_json_file(BINDING_JSON, &doc);
}

/// One-time upgrade at startup: sign the models THIS Orb originated that were
/// shared while no identity was unlocked. Before the Orb key existed those went
/// out unsigned (on the Orb this was first run against, 2 of 6). Entries that
/// came from other Orbs are never touched — their signature, or lack of one,
/// is theirs.
pub fn upgrade_catalog_signatures() -> usize {
    // Also the moment this Orb's key and peer binding come into existence, so
    // the first announcement after an upgrade already carries them.
    ensure_binding();
    let (my_id, _) = crate::fleet::identity();
    if my_id.is_empty() {
        return 0;
    }
    let mut entries = read_catalog();
    let mut signed = 0;
    for e in entries.iter_mut() {
        if e.pub_sig.is_empty() && e.origin_id == my_id && valid_cid(&e.cid) {
            sign_entry(e);
            if !e.pub_sig.is_empty() {
                signed += 1;
            }
        }
    }
    if signed > 0 {
        let _ = write_catalog(&entries);
        announce();
    }
    signed
}

/// Whether a catalog entry is this Orb's own publication. Decided by KEY where
/// there is a signature — the fleet id in `origin_id` is derived from
/// /etc/machine-id and changes when the SD card is re-imaged or cloned, at
/// which point an Orb would stop recognising models it shared itself.
fn is_mine(e: &ModelEntry) -> bool {
    if entry_sig_ok(e) {
        return crate::publisher::is_local_key(&e.pub_key)
            || e.delegation.as_ref().is_some_and(|d| crate::publisher::is_local_key(&d.user_key));
    }
    let (my_id, _) = crate::fleet::identity();
    !my_id.is_empty() && e.origin_id == my_id
}

/// POST /api/ipfs/models/claim — the operator states that an UNSIGNED model in
/// this Orb's catalog is one this Orb originated, and signs it. This is the
/// way to put a key behind models shared before signing was automatic when the
/// Orb's fleet id has since changed (so the startup upgrade cannot tell they
/// are its own). Refused for anything that already carries a signature.
pub async fn claim_model(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut entries = read_catalog();
        let Some(e) = entries.iter_mut().find(|e| e.cid == req.cid) else {
            return json!({"ok": false, "err": "not in this Orb's catalog"});
        };
        if !e.pub_sig.is_empty() {
            return json!({"ok": false, "err": "that model is already signed by its publisher"});
        }
        let (id, label) = crate::fleet::identity();
        e.origin_id = id;
        e.origin_label = label;
        sign_entry(e);
        if e.pub_sig.is_empty() {
            return json!({"ok": false, "err": "this Orb has no signing key available"});
        }
        let fp = e.pub_key.get(..12).unwrap_or(&e.pub_key).to_string();
        let _ = write_catalog(&entries);
        announce();
        json!({"ok": true, "signed_by": fp})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "claim task failed"}));
    Json(v)
}

/// Add a model this Orb originates — signs it (if an identity is unlocked) then
/// catalogs it. Every origination path (share/publish/import) flows through here;
/// only the fetch path bypasses it via `catalog_add_raw`.
fn catalog_add(mut entry: ModelEntry) {
    sign_entry(&mut entry);
    catalog_add_raw(entry);
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
        // Created 0600 from the start — write-then-chmod leaves the tokens
        // readable by every local user for a moment.
        let tmp = format!("{TOKENS_JSON}.tmp");
        let _ = std::fs::remove_file(&tmp);
        let wrote = {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
                .and_then(|mut f| f.write_all(&serde_json::to_vec_pretty(&t).unwrap_or_default()))
                .and_then(|_| std::fs::rename(&tmp, TOKENS_JSON))
        };
        match wrote {
            Ok(()) => json!({"ok": true}),
            Err(e) => json!({"ok": false, "err": format!("could not save the token: {e}")}),
        }
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
/// Seeding is ON unless the operator opts out. A download, import, or pull
/// then stays pinned and is announced on the beacon. Missing file = seed.
const SEED_JSON: &str = "/etc/aeon/modelshare-seed.json";

fn seed_enabled() -> bool {
    std::fs::read_to_string(SEED_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("seed").and_then(|b| b.as_bool()))
        .unwrap_or(true)
}

fn write_seed(on: bool) {
    if let Some(dir) = std::path::Path::new(SEED_JSON).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(SEED_JSON, serde_json::to_vec_pretty(&json!({"seed": on})).unwrap_or_default());
}

/// Drop a just-imported CID from the public catalog and keep a private copy.
fn retain_private(cid: &str, name: &str) {
    let _ = ensure_in_library(cid, &model_slug(name));
    let entries: Vec<ModelEntry> = read_catalog().into_iter().filter(|e| e.cid != cid).collect();
    let _ = write_catalog(&entries);
    let _ = run_script(&["unpin", cid]);
}

/// GET /api/ipfs/models/seed — whether downloads are re-pinned onto the swarm.
pub async fn get_seed(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "seed": seed_enabled()}))
}

#[derive(Deserialize)]
pub struct SeedReq {
    pub seed: bool,
}

/// POST /api/ipfs/models/seed — opt out of (or back into) automatic seeding.
pub async fn set_seed(State(_s): State<AppState>, Json(req): Json<SeedReq>) -> Json<Value> {
    tokio::task::spawn_blocking(move || {
        write_seed(req.seed);
        // One switch for both networks.
        directory::torrents_seed(req.seed);
    })
    .await
    .ok();
    Json(json!({"ok": true, "seed": req.seed}))
}

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

/// Another Orb's `shared_models()` as it arrived in a fleet heartbeat, rebuilt
/// with the types the console expects. The roster hands a peer's heartbeat to
/// the browser as it came, and the page that unions these lists stops at the
/// first entry it cannot read — so one malformed entry from one fleet peer
/// would blank the whole fleet index. Entries are read one at a time, as for
/// the gossip registry; anything that is not a usable value reads as off,
/// empty, or no entry.
pub fn fleet_models(raw: &Value) -> Value {
    json!({
        "enabled": raw.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        "peer_id": raw.get("peer_id").and_then(Value::as_str).unwrap_or(""),
        "models": raw.get("models").and_then(Value::as_array).map(|a| tolerant_entry_list(a)).unwrap_or_default(),
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
    let spec = ShareSpec {
        name: display.clone(),
        file: safe_file.clone(),
        card,
        verified: false,
        source: String::new(),
        upstream: String::new(), identity: true, provider_record: None,
    };
    let known: HashMap<String, String> = [(safe_file, sha256_hex)].into_iter().collect();

    task_set(&display, "adding to IPFS…");
    let key = display.clone();
    tokio::task::spawn_blocking(move || {
        // Card (manifest + origin signature) written beside the weights, then
        // the whole directory is added so one CID carries both.
        match finalize_share(&share_dir, spec, &known) {
            Ok(entry) => {
                catalog_add(entry);
                task_clear(&key);
                announce(); // gossip the updated catalog immediately
            }
            Err(e) => task_set(&key, &format!("error: {e}")),
        }
        let _ = std::fs::remove_dir_all(&share_dir); // bytes now in the blockstore
    });

    Json(json!({"ok": true, "name": display, "size_bytes": bytes_written, "processing": true}))
}

/// Nudge the gossip daemon to re-announce this Orb's catalog now (so a fresh
/// share/unshare shows up on peers within seconds instead of the next tick).
/// Best-effort: the daemon also announces on a timer.
fn announce() {
    ensure_binding();
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
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            // file_type() does not follow links; `p.is_dir()` did, so a link to
            // "/" used to send this walk across the whole filesystem.
            let Ok(ft) = ent.file_type() else {
                return Err(format!("rejected: cannot inspect “{name}”"));
            };
            if ft.is_symlink() {
                return Err(format!("rejected: “{name}” is a symbolic link — not a model file"));
            }
            // Nothing in a model is hidden. A dot-name is how a directory
            // smuggles `.ssh/authorized_keys` or `.bashrc` onto whatever
            // machine the model is later pushed to.
            if name.starts_with('.') {
                return Err(format!("rejected: “{name}” is a hidden file or folder — not a model file"));
            }
            if ft.is_dir() { stack.push(p); continue; }
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

/// Parse a kubo storage-size string ("10GB", "512MB", "2TB") to bytes. kubo uses
/// SI multipliers (GB = 10^9), so match that. 0 = unparseable/unset.
fn parse_size(s: &str) -> u64 {
    let up = s.trim().to_ascii_uppercase();
    let (num, mult): (&str, u64) =
        if let Some(n) = up.strip_suffix("TB").or_else(|| up.strip_suffix('T')) { (n, 1_000_000_000_000) }
        else if let Some(n) = up.strip_suffix("GB").or_else(|| up.strip_suffix('G')) { (n, 1_000_000_000) }
        else if let Some(n) = up.strip_suffix("MB").or_else(|| up.strip_suffix('M')) { (n, 1_000_000) }
        else if let Some(n) = up.strip_suffix("KB").or_else(|| up.strip_suffix('K')) { (n, 1_000) }
        else if let Some(n) = up.strip_suffix('B') { (n, 1) }
        else { (up.as_str(), 1) };
    num.trim().parse::<f64>().ok().map(|f| (f * mult as f64) as u64).unwrap_or(0)
}

/// (repo_bytes, storage_max_bytes) from `aeon-ipfs status`. Either may be 0 when
/// unknown (status unavailable / repo-stat slow) — callers treat 0 as "can't tell".
fn ipfs_repo_usage() -> (u64, u64) {
    let v = run_script(&["status"])
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let repo = v.get("repo_bytes").and_then(|x| x.as_u64()).unwrap_or(0);
    let smax = v.get("storage_max").and_then(|x| x.as_str()).map(parse_size).unwrap_or(0);
    (repo, smax)
}

/// Remove orphaned `quarantine-*` dirs left by a fetch whose process was killed
/// (e.g. a supervisor restart) — only ones older than `min_age`, so a concurrent
/// in-flight fetch's quarantine is never touched. Best-effort.
fn sweep_stale_quarantine(min_age: std::time::Duration) {
    let now = std::time::SystemTime::now();
    if let Ok(rd) = std::fs::read_dir(staging_root()) {
        for ent in rd.flatten() {
            if !ent.file_name().to_string_lossy().starts_with("quarantine-") {
                continue;
            }
            let old = ent.metadata().ok()
                .and_then(|m| m.modified().ok())
                .and_then(|mt| now.duration_since(mt).ok())
                .map(|age| age >= min_age)
                .unwrap_or(false);
            if old {
                let _ = std::fs::remove_dir_all(ent.path());
            }
        }
    }
}

/// Append actionable guidance to a fetch error — the download is resumable, and
/// storage-cap failures are the common cause.
fn storage_hint(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("storage") || l.contains("disk") || l.contains("space") || l.contains("datastore") {
        format!("{e} — raise the IPFS storage limit (Storage settings), then retry: the download resumes from what's already fetched.")
    } else {
        format!("{e} — retry to resume from what's already downloaded, or purge to reclaim it and start fresh.")
    }
}

/// Stop hosting content that failed quarantine. Called AFTER the reason has
/// been reported, so the operator sees why immediately.
///
/// This only UNPINS. It used to run `purge` — a whole-repository garbage
/// collection — inline, before the error was even shown: on an Orb holding
/// ~120 GB of models that took minutes per rejection, drove the load average
/// past 30, and made every other download wait behind kubo's GC lock. (Deleting
/// just the rejected blocks is no cheaper: kubo has to walk every pinned model
/// to prove a block is unpinned.) Unpinned, the content is no longer part of
/// anything this Orb shares; its blocks are reclaimed by the next GC the
/// operator runs or the storage limit triggers.
fn discard_rejected(cid: &str) {
    let _ = run_script(&["unpin", cid]);
}

/// The body of a fetch request, or the answer to send for one that cannot be
/// used. The entry in the body is a row of the index, so its fields began on
/// another node: a body with a field of the wrong type gets the same
/// `{ok: false}` answer as any other refusal, which the console can show,
/// instead of the framework's plain-text rejection.
fn fetch_request(req: Result<Json<FetchReq>, axum::extract::rejection::JsonRejection>) -> Result<FetchReq, Value> {
    match req {
        Ok(Json(req)) if valid_cid(&req.entry.cid) => Ok(req),
        Ok(_) => Err(json!({"ok": false, "err": "invalid CID"})),
        Err(e) => Err(json!({"ok": false, "err": format!("not a fetch request: {}", e.body_text())})),
    }
}

/// Disk a fetch of `sz` bytes needs at its peak: the pinned blocks, a full
/// quarantine copy for scanning, and headroom. `sz` is whatever the entry
/// claims, so nothing here may overflow: a plain `+` panics in a debug build
/// and in a release build wraps to a small number that passes the disk guard.
fn fetch_peak_bytes(sz: u64) -> u64 {
    sz.saturating_mul(2).saturating_add(512 * 1024 * 1024)
}

/// POST /api/ipfs/models/fetch — download a peer's model through a QUARANTINE
/// sandbox: fetch to staging → validate every file's magic bytes (fake
/// extensions / stowaway executables / pickle weights) → ClamAV scan → and only
/// on a clean pass pin it + add to the catalog. Progress phases surface in the
/// task map. Rejected content never reaches the live library.
pub async fn fetch_model(
    State(_s): State<AppState>,
    req: Result<Json<FetchReq>, axum::extract::rejection::JsonRejection>,
) -> Json<Value> {
    let req = match fetch_request(req) {
        Ok(req) => req,
        Err(refusal) => return Json(refusal),
    };
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

        // Clean up any quarantine dirs orphaned by an earlier killed fetch (only
        // ones >30 min old, so a concurrent fetch's quarantine is left alone).
        sweep_stale_quarantine(std::time::Duration::from_secs(1800));

        let sz = req.entry.size_bytes;

        // Disk guard 1 — filesystem: pinned blocks + a full quarantine copy for
        // scanning (~2× peak) + headroom.
        let free = free_bytes(MODELS_DIR);
        if sz > 0 && fetch_peak_bytes(sz) > free {
            return fail(&format!("not enough disk: model is {} but only {} free", human(sz), human(free)));
        }
        // Disk guard 2 — IPFS storage cap: check UP FRONT so we fail with clear
        // guidance instead of the old mid-stream failure (auto-GC evicting the
        // in-flight blocks the moment the repo crossed StorageMax).
        let (repo, smax) = ipfs_repo_usage();
        if sz > 0 && smax > 0 && repo.saturating_add(sz) > smax {
            return fail(&format!(
                "won't fit under the IPFS storage limit — this model needs ~{}, the limit is {} and {} is already used. Raise the IPFS storage limit (Storage settings) to at least {}, then retry.",
                human(sz), human(smax), human(repo),
                human(repo.saturating_add(sz).saturating_add(512 * 1024 * 1024))
            ));
        }

        // Fetch via `pin add`, NOT `ipfs get`: pin holds kubo's pin lock for the
        // whole fetch, so `--enable-gc` can't evict the in-flight blocks when the
        // repo crosses StorageMax (the reported bug), AND re-running after any
        // interruption RESUMES from the blocks already cached. Progress is gauged
        // from bytes landing on disk (cheap statvfs delta — no repo-stat lock).
        task_progress(&cid, "downloading…", 0, sz);
        let free0 = free_bytes(MODELS_DIR);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let monitor = {
            let (stop, cidm) = (stop.clone(), cid.clone());
            std::thread::spawn(move || {
                use std::sync::atomic::Ordering;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let done = free0.saturating_sub(free_bytes(MODELS_DIR));
                    task_progress(&cidm, "downloading…", done, sz);
                }
            })
        };
        let pinned = run_script(&["pin", &cid]);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = monitor.join();
        if let Err(e) = pinned {
            // Partial blocks stay cached (unpinned) so a retry resumes; the errored
            // task is purge-able from the UI (GCs them + starts fresh).
            return fail(&storage_hint(&e));
        }

        // Now fully local + pinned. Materialize a copy to quarantine for scanning
        // (a LOCAL read — fast, no network, no eviction risk). On any rejection,
        // purge (unpin + GC) so the bad content never lingers pinned/hosted.
        let q = staging_root().join(format!("quarantine-{}", new_draft_id()));
        task_set(&cid, "validating weights (file types)…");
        if let Err(e) = run_script(&["get", &cid, &q.to_string_lossy()]) {
            let _ = std::fs::remove_dir_all(&q);
            fail(&e);
            return discard_rejected(&cid);
        }
        // Hold the bytes to what was signed and advertised BEFORE anything else
        // reads them. The entry in this request is whatever the caller sent, so
        // it proves nothing by itself; the manifest inside the directory does.
        task_set(&cid, "verifying against the signed manifest…");
        let credited = entry_identity(&req.entry);
        let check = match verify_model_dir(&q, &req.entry) {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&q);
                fail(&e);
                // A publisher whose bytes fail verification here loses standing here.
                if !credited.is_empty() {
                    trust_update(|t| {
                        let p = t.publishers.entry(credited.clone()).or_insert_with(|| Publisher { first_seen_ms: epoch_ms(), ..Default::default() });
                        p.bad += 1;
                    });
                }
                return discard_rejected(&cid);
            }
        };
        task_set(&cid, "validating weights (file types)…");
        if let Err(e) = inspect_model_dir(&q) {
            let _ = std::fs::remove_dir_all(&q);
            fail(&e);
            return discard_rejected(&cid);
        }

        task_set(&cid, "scanning for viruses…");
        if let Err(e) = virus_scan(&q) {
            let _ = std::fs::remove_dir_all(&q);
            fail(&format!("virus scan: {e}"));
            return discard_rejected(&cid);
        }

        // Clean. Seeding is the default: stay pinned and announce so the next
        // stranger can fetch it from us. Opting out keeps a private library
        // copy and drops the pin so we do not provide it.
        record_check(&cid, &check);
        // Verifying our own models says nothing about a publisher's record.
        if check.origin_ok && !check.origin_identity.is_empty() && !crate::publisher::is_local_key(&check.origin_identity) {
            let who = check.origin_identity.clone();
            let name = check.origin_name.clone();
            trust_update(|t| {
                let p = t.publishers.entry(who).or_insert_with(|| Publisher { first_seen_ms: epoch_ms(), ..Default::default() });
                p.ok += 1;
                if !name.is_empty() {
                    p.last_name = name;
                }
            });
        }
        // "Verified against the upstream hash" is a claim. Keep it only when it
        // is part of what the origin signed; otherwise it is just a flag a peer
        // set, and re-announcing it from here would launder it.
        let mut entry = req.entry;
        entry.verified = check.origin_ok && check.claims_verified;
        if check.origin_ok {
            entry.source = check.source.clone();
        }
        // The upstream record comes from the card inside the directory that
        // was just checked, never from the request. Witnesses the caller
        // passed along are other publishers' statements: kept when their
        // signatures hold for that same record.
        let heard = if entry.upstream_record == check.upstream_record { std::mem::take(&mut entry.witnesses) } else { Vec::new() };
        entry.upstream_record = check.upstream_record.clone();
        entry.witnesses = match &entry.upstream_record {
            Some(rec) => upstream::valid_witnesses(rec, &[check.witnesses.clone(), heard].concat()),
            None => Vec::new(),
        };
        if seed_enabled() {
            catalog_add_raw(entry);
            announce();
        } else {
            let slug = model_slug(&entry.name);
            let _ = ensure_in_library(&cid, &slug);
            let _ = run_script(&["unpin", &cid]);
        }
        let _ = std::fs::remove_dir_all(&q);
        task_clear(&cid);
    });
    Json(json!({"ok": true, "fetching": true}))
}

/// POST /api/ipfs/models/remove — unshare: unpin the CID + drop the catalog
/// entry. Blocks are reclaimed by kubo GC as the storage cap demands.
pub async fn remove_model(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let _ = run_script(&["unpin", &req.cid]);
        let all = read_catalog();
        // Unsharing covers both networks: stop seeding its torrent too.
        if let Some(gone) = all.iter().find(|e| e.cid == req.cid && !e.btih.is_empty()) {
            directory::torrent_forget(&gone.btih);
        }
        let entries: Vec<ModelEntry> = all.into_iter().filter(|e| e.cid != req.cid).collect();
        let _ = write_catalog(&entries);
        task_clear(&req.cid);
        announce();
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "remove task failed"}));
    Json(v)
}

/// POST /api/ipfs/models/purge — recover from a FAILED or stuck download: clear
/// its error, kill any hung fetch of the CID, unpin any partial data, and GC the
/// orphaned blocks so the space is reclaimed. Then the user can retry from
/// scratch. Refuses a CID that's a fully-shared model (use /remove to unshare).
pub async fn purge_download(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    // Don't let purge unshare a model that actually finished + is in the catalog —
    // that's what /remove is for. (Small file read; fast.)
    if read_catalog().iter().any(|e| e.cid == req.cid) {
        return Json(json!({"ok": false, "err": "that model is fully downloaded and shared — use Remove to unshare it, not purge"}));
    }
    // Clear the error task NOW so the UI resets instantly. The heavy part — killing
    // a hung fetch, unpinning, and `ipfs repo gc` to reclaim the orphaned blocks —
    // can take minutes on a large datastore, so run it DETACHED and return right
    // away rather than holding the request open.
    task_clear(&req.cid);
    let cid = req.cid.clone();
    tokio::task::spawn_blocking(move || {
        let _ = run_script(&["purge", &cid]); // pkill + pin rm + repo gc
    });
    Json(json!({
        "ok": true,
        "message": "Failed download cleared; reclaiming its disk space in the background."
    }))
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
    #[serde(default, deserialize_with = "tolerant_entries")]
    models: Vec<ModelEntry>,
    /// CIDs this peer has starred — gossiped so every Orb can tally a
    /// community star count per model (fleet-free reputation).
    #[serde(default)]
    starred: Vec<String>,
    /// This peer's Orb key and its signature over (orb_key, peer_id): proof the
    /// key holder runs this libp2p peer. A host whose bound key signed a model
    /// is that model's origin seeder.
    #[serde(default)]
    orb_key: String,
    #[serde(default)]
    orb_sig: String,
    /// The user identity that authorised this peer's Orb key, if it shows one.
    #[serde(default)]
    delegation: Option<crate::publisher::Delegation>,
}

/// Model entries that came from another node, read one at a time. An entry
/// that is not an object, or has a known field of the wrong type, is left out
/// and the rest are kept: what one peer wrote into one entry must not decide
/// whether anyone else's models are listed.
fn tolerant_entry_list(list: &[Value]) -> Vec<ModelEntry> {
    list.iter().filter(|m| m.is_object()).filter_map(|m| ModelEntry::deserialize(m).ok()).collect()
}

/// `tolerant_entry_list` for a struct field. A value that is not a list at
/// all is an error, which fails the one record it sits in.
fn tolerant_entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ModelEntry>, D::Error> {
    Ok(tolerant_entry_list(&Vec::<Value>::deserialize(d)?))
}

/// The peers in a parsed registry.json (`{"peers": {<peer id>: {…}}}`), read
/// one peer at a time and, inside a peer, one model entry at a time. A peer
/// record that cannot be used is left out alone, and so is a model entry.
///
/// This used to be a single typed parse of the whole map, so one mistyped
/// field in one entry of one peer (`"size_bytes": "x"`) emptied the index —
/// and anything on the gossip topic could send that.
fn registry_peers(doc: &Value) -> HashMap<String, RegPeer> {
    let Some(peers) = doc.get("peers").and_then(Value::as_object) else {
        return HashMap::new();
    };
    peers
        .iter()
        .filter(|(_, rec)| rec.is_object())
        .filter_map(|(id, rec)| Some((id.clone(), RegPeer::deserialize(rec).ok()?)))
        .collect()
}

/// Every model every gossiped peer currently announces, flattened.
fn registry_models() -> Vec<ModelEntry> {
    read_registry().into_values().flat_map(|p| p.models).filter(|e| valid_cid(&e.cid)).collect()
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
        .map(|doc| registry_peers(&doc))
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
        ensure_binding();
        let me = self_peer_id();
        let local = read_catalog();
        let peers = read_registry();
        let tasks: HashMap<String, TaskState> = tasks().lock().map(|g| g.clone()).unwrap_or_default();
        let checks = read_checks();
        let now = epoch_ms();

        // One row per CID. Several Orbs may announce the same CID with their own
        // copy of the metadata, so WHICH copy is shown must not be an accident of
        // map iteration order (it used to be — a peer could win the coin toss and
        // have its name / flags displayed for someone else's model). Lowest rank
        // wins: this Orb's own copy, then the origin seeder's, then any validly
        // signed copy, then the rest; ties break on the earliest share time.
        struct Acc {
            entry: ModelEntry,
            rank: (u8, i64, String),
            hosts: Vec<Value>,
        }
        let mut rows: HashMap<String, Acc> = HashMap::new();
        {
            let mut offer = |e: &ModelEntry, class: u8, peer: &str, host: Value| {
                let rank = (class, e.added_at_ms, peer.to_string());
                match rows.get_mut(&e.cid) {
                    Some(a) => {
                        if rank < a.rank {
                            a.entry = e.clone();
                            a.rank = rank;
                        }
                        a.hosts.push(host);
                    }
                    None => {
                        rows.insert(e.cid.clone(), Acc { entry: e.clone(), rank, hosts: vec![host] });
                    }
                }
            };
            for e in &local {
                // Ours if one of this Orb's keys signed it — the Orb key, or a
                // user identity that lives here (models signed before Orb keys
                // existed carry the user key directly).
                let is_origin = e.upstream.is_empty() && entry_sig_ok(e) && is_mine(e);
                offer(e, 0, "", json!({"label": "this orb", "is_self": true, "online": true, "peer_id": me, "is_origin": is_origin}));
            }
            for p in peers.values() {
                // Skip our OWN gossiped announcement — with floodsub self-delivery
                // the daemon hears its own catalog back, and the local catalog
                // above already represents "this orb".
                if p.peer_id.is_empty() || p.peer_id == me {
                    continue;
                }
                let bound = !p.orb_key.is_empty()
                    && crate::publisher::verify(
                        &p.orb_key,
                        &crate::publisher::peer_binding_digest(&p.orb_key, &p.peer_id),
                        &p.orb_sig,
                    );
                let label = if p.label.is_empty() { p.peer_id.chars().take(12).collect::<String>() } else { p.label.clone() };
                // Whose Orb this is: the user identity that delegated to its
                // key, when the peer shows a valid delegation; else its own key.
                let peer_identity = if bound {
                    crate::publisher::identity_of(&p.orb_key, p.delegation.as_ref())
                } else {
                    String::new()
                };
                for e in p.models.iter().filter(|e| valid_cid(&e.cid)) {
                    let signed = entry_sig_ok(e);
                    let is_origin = e.upstream.is_empty()
                        && bound
                        && signed
                        && (e.pub_key == p.orb_key || entry_identity(e) == peer_identity);
                    let class = if is_origin { 1 } else if signed { 2 } else { 3 };
                    offer(e, class, &p.peer_id, json!({
                        "label": label, "is_self": false, "online": true, "peer_id": p.peer_id,
                        "addrs": p.addrs, "updated_ms": p.updated_ms, "is_origin": is_origin,
                    }));
                }
            }
        }

        // Community stars: a model's star count = the number of DISTINCT Orbs
        // that starred its CID (self + every gossiped peer).
        let my_stars: std::collections::HashSet<String> = read_stars().into_iter().collect();
        let mut starrers: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for cid in &my_stars {
            starrers.entry(cid.clone()).or_default().insert(me.clone());
        }
        for p in peers.values() {
            for cid in &p.starred {
                starrers.entry(cid.clone()).or_default().insert(p.peer_id.clone());
            }
        }

        // Who each row is credited to, and the network-side evidence about each
        // publisher (summed over all of that publisher's models).
        let credited: HashMap<String, String> =
            rows.iter().map(|(cid, a)| (cid.clone(), entry_identity(&a.entry))).collect();
        let mut pub_starrers: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        let mut pub_mirrors: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        for (cid, a) in &rows {
            let id = &credited[cid];
            if id.is_empty() || !a.entry.upstream.is_empty() {
                continue;
            }
            if let Some(s) = starrers.get(cid) {
                pub_starrers.entry(id.clone()).or_default().extend(s.iter().cloned());
            }
            for h in &a.hosts {
                if !h["is_origin"].as_bool().unwrap_or(false) {
                    if let Some(pid) = h["peer_id"].as_str() {
                        pub_mirrors.entry(id.clone()).or_default().insert(pid.to_string());
                    }
                }
            }
        }

        // Remember when each publisher, and each weights hash, was first seen
        // here. Oldest-claimed first so two identities met in the same pass are
        // ordered the same way on every run.
        let mut trust = read_trust();
        {
            let mut order: Vec<&Acc> = rows.values().collect();
            order.sort_by(|a, b| (a.entry.added_at_ms, &a.entry.cid).cmp(&(b.entry.added_at_ms, &b.entry.cid)));
            let mut fresh_pubs: Vec<(String, String)> = Vec::new();
            let mut fresh_claims: Vec<(String, String)> = Vec::new();
            for a in order {
                let id = &credited[&a.entry.cid];
                if id.is_empty() {
                    continue;
                }
                if !trust.publishers.contains_key(id) && !fresh_pubs.iter().any(|(k, _)| k == id) {
                    fresh_pubs.push((id.clone(), a.entry.pub_name.clone()));
                }
                let sha = &a.entry.sha256;
                if !sha.is_empty() && !trust.claims.contains_key(sha) && !fresh_claims.iter().any(|(k, _)| k == sha) {
                    fresh_claims.push((sha.clone(), id.clone()));
                }
            }
            if !fresh_pubs.is_empty() || !fresh_claims.is_empty() {
                trust_update(|t| {
                    for (id, name) in &fresh_pubs {
                        t.publishers.entry(id.clone()).or_insert_with(|| Publisher {
                            first_seen_ms: now,
                            last_name: name.chars().filter(|c| !c.is_control()).take(40).collect(),
                            ..Default::default()
                        });
                    }
                    for (sha, id) in &fresh_claims {
                        t.claims.entry(sha.clone()).or_insert_with(|| Claim { key: id.clone(), first_seen_ms: now });
                    }
                });
                trust = read_trust();
            }
        }

        // Every signed witness in sight — this Orb's and every announcing
        // peer's, across all CIDs — so each row can say how many separate
        // publishers recorded the same provider hashes. Signatures that do not
        // hold are dropped here; the gossip daemon has already dropped them once.
        let mut witnessed = upstream::WitnessIndex::new();
        for e in local.iter().chain(peers.values().filter(|p| p.peer_id != me).flat_map(|p| p.models.iter())) {
            witnessed.add(e);
        }

        let local_cids: std::collections::HashSet<String> = local.iter().map(|e| e.cid.clone()).collect();
        let mut out: Vec<Value> = rows
            .into_iter()
            .map(|(cid, Acc { mut entry, hosts, .. })| {
                upstream::scrub(&mut entry);
                let witnesses = entry.upstream_record.as_ref().map(|rec| witnessed.tally(rec));
                let host_count = hosts.len();
                let star_count = starrers.get(&cid).map(|s| s.len()).unwrap_or(0);
                let id = credited.get(&cid).cloned().unwrap_or_default();
                let known = trust.publishers.get(&id);
                let rep = reputation(
                    known,
                    !id.is_empty(),
                    pub_starrers.get(&id).map(|s| s.len()).unwrap_or(0),
                    pub_mirrors.get(&id).map(|s| s.len()).unwrap_or(0),
                    now,
                );
                let delegated = !id.is_empty() && id != entry.pub_key;
                // Name precedence: the operator's own petname, then the name in a
                // verified delegation, then whatever the signer calls itself.
                let display_name = known
                    .map(|p| p.petname.clone())
                    .filter(|n| !n.is_empty())
                    .or_else(|| entry.delegation.as_ref().filter(|_| delegated).map(|d| d.user_name.clone()))
                    .unwrap_or_else(|| entry.pub_name.clone());
                let blocked = known.map(|p| p.blocked).unwrap_or(false);
                let standing = if !entry.upstream.is_empty() {
                    "mirror"
                } else if id.is_empty() {
                    "unsigned"
                } else if blocked {
                    "blocked"
                } else if known.map(|p| p.trusted).unwrap_or(false) {
                    "trusted"
                } else if known.map(|p| p.bad > 0).unwrap_or(false) {
                    "failed"
                } else if known.map(|p| p.ok > 0).unwrap_or(false) {
                    "proven"
                } else {
                    "new"
                };
                let seeder_online = hosts.iter().any(|h| h["is_origin"].as_bool().unwrap_or(false));
                // Same weights first seen here under a DIFFERENT identity → this
                // row is a re-publication, not the origin.
                let earlier = trust
                    .claims
                    .get(&entry.sha256)
                    .filter(|c| !id.is_empty() && c.key != id)
                    .map(|c| json!({"key": c.key, "fp": c.key.get(..12).unwrap_or(&c.key), "first_seen_ms": c.first_seen_ms}));
                let origin = json!({
                    "key": id,
                    "fp": id.get(..12).unwrap_or(&id),
                    "name": display_name,
                    "signed": !id.is_empty(),
                    "delegated": delegated,
                    "standing": standing,
                    "reputation": rep,
                    "seeder_online": seeder_online,
                    "upstream": entry.upstream,
                    "content": checks.get(&cid),
                    "earlier_claim": earlier,
                });
                // Kept for older consoles: the bare signature verdict.
                let signature = if entry.pub_sig.is_empty() {
                    Value::Null
                } else {
                    json!({
                        "ok": entry_sig_ok(&entry),
                        "name": entry.pub_name,
                        "fp": entry.pub_key.get(..12).unwrap_or(&entry.pub_key),
                    })
                };
                json!({
                    "entry": entry, "hosts": hosts, "local": local_cids.contains(&cid),
                    "host_count": host_count, "star_count": star_count,
                    "starred": my_stars.contains(&cid), "signature": signature,
                    "origin": origin, "blocked": blocked,
                    // Who signed that they saw the provider publish these hashes.
                    "witnessed": witnesses,
                })
            })
            .collect();
        // Rank on who published it first and foremost: the origin's standing
        // outweighs popularity, because stars and host counts are cheap for an
        // attacker to inflate and a verified track record is not. Blocked
        // publishers sink to the bottom.
        out.sort_by(|a, b| {
            let score = |m: &Value| -> i64 {
                if m["blocked"].as_bool().unwrap_or(false) {
                    return i64::MIN;
                }
                m["origin"]["reputation"].as_i64().unwrap_or(0) * 2
                    + m["star_count"].as_i64().unwrap_or(0).min(50) * 3
                    + m["host_count"].as_i64().unwrap_or(0).min(50)
                    + if m["origin"]["seeder_online"].as_bool().unwrap_or(false) { 10 } else { 0 }
            };
            score(b).cmp(&score(a)).then_with(|| {
                let ai = a["entry"]["added_at_ms"].as_i64().unwrap_or(0);
                let bi = b["entry"]["added_at_ms"].as_i64().unwrap_or(0);
                bi.cmp(&ai).then_with(|| a["entry"]["cid"].as_str().cmp(&b["entry"]["cid"].as_str()))
            })
        });
        let now = epoch_ms();
        let fresh = 15 * 60 * 1000;
        let gossip_others = peers
            .values()
            .filter(|p| p.peer_id != me && now.saturating_sub(p.updated_ms) < fresh)
            .count();
        let mesh: Value = std::fs::read_to_string("/var/lib/aeon/ipfs-models/mesh.json")
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(json!({}));
        let beacon_orbs = mesh.get("orbs_on_beacon").and_then(|n| n.as_u64()).unwrap_or(0);
        let orbs_online = (1 + gossip_others as u64).max(beacon_orbs);
        let mut seen_cids: HashMap<String, u64> = HashMap::new();
        for e in &local {
            seen_cids.insert(e.cid.clone(), e.size_bytes);
        }
        for p in peers.values() {
            if p.peer_id == me {
                continue;
            }
            for e in &p.models {
                seen_cids.entry(e.cid.clone()).or_insert(e.size_bytes);
            }
        }
        let shared_bytes: u64 = seen_cids.values().copied().sum();
        let local_bytes: u64 = local.iter().map(|e| e.size_bytes).sum();
        // One catalog row is one pinned directory. The primary weight file is
        // the file we can count without walking the blockstore on every poll.
        let our_files = local.len();
        json!({
            "ok": true, "self_peer_id": me, "models": out, "tasks": tasks,
            "peer_count": orbs_online, "my_star_count": my_stars.len(),
            "network": {
                "orbs_online": orbs_online,
                "orbs_gossiping": 1 + gossip_others,
                "orbs_on_beacon": beacon_orbs,
                "models": seen_cids.len(),
                "shared_bytes": shared_bytes,
                "local_models": local.len(),
                "local_bytes": local_bytes,
                "seeding": seed_enabled(),
                "beacon": mesh.get("beacon").cloned().unwrap_or(Value::Null),
                "our_pin": {
                    "beacon": mesh.get("beacon").cloned().unwrap_or(Value::Null),
                    "models": local.len(),
                    "bytes": local_bytes,
                    "on_beacon": beacon_orbs > 0 || !me.is_empty(),
                },
                "our_files": {
                    "count": our_files,
                    "bytes": local_bytes,
                },
            }
        })
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
        match cat_capped(&format!("{cid}/model-card.json"), MAX_TEXT_BYTES) {
            Ok(b) => serde_json::from_slice::<Value>(&b)
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
        // No SVG: it is a document that can carry script, this endpoint serves
        // bytes chosen by strangers, and it does so from the console's own
        // origin. Opened directly, a hostile SVG would run as the admin.
        _ => return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported image type").into_response(),
    };
    let bytes = tokio::task::spawn_blocking(move || cat_capped(&format!("{cid}/{name}"), MAX_IMAGE_BYTES))
        .await
        .unwrap_or_else(|_| Err("image task failed".into()));
    match bytes {
        // Only bytes that really are a raster image leave here as one.
        Ok(b) if sniff_kind(&b[..b.len().min(16)]) == "image" => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                // The response sits behind the console login, so it is not
                // "public"; content-addressed, so it never changes.
                (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox".to_string()),
            ],
            b,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
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
        match cat_capped(&format!("{cid}/{name}"), MAX_TEXT_BYTES) {
            Ok(b) => json!({"ok": true, "text": String::from_utf8_lossy(&b)}),
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
        _ => "png".into(),
    }
}

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// Cap for a card / README pulled through the node for display.
const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// `ipfs cat` with a hard size limit. These paths name content published by
/// strangers and are fetched just by browsing the index, so an unbounded read
/// let any peer make the supervisor buffer gigabytes (or wait forever). Asks
/// for one byte past the limit so "too big" is told apart from "exactly at
/// the limit"; the script also time-boxes the call.
fn cat_capped(path: &str, max: usize) -> Result<Vec<u8>, String> {
    let bytes = run_script_bytes(&["cat", path, &(max + 1).to_string()])?;
    if bytes.len() > max {
        return Err(format!("larger than the {} KB limit", max / 1024));
    }
    Ok(bytes)
}

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
    if sniff_kind(&bytes[..bytes.len().min(16)]) != "image" {
        return Err("that file is not a PNG, JPEG, WebP or GIF image".into());
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
    /// Sign with the signed-in publisher identity. Defaults to true; false
    /// publishes under this Orb's key alone.
    #[serde(default = "yes")]
    pub sign_identity: bool,
}

fn yes() -> bool {
    true
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

        let _ = size_bytes; // the manifest re-derives sizes from the files themselves
        let known: HashMap<String, String> = [(file.clone(), sha256)].into_iter().collect();
        let spec = ShareSpec {
            name: req.name, file, card, verified: false, source: String::new(), upstream: String::new(), identity: req.sign_identity,
            provider_record: None,
        };
        match finalize_share(&dir, spec, &known) {
            Ok(entry) => {
                let cid = entry.cid.clone();
                catalog_add(entry);
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(&meta_path);
                announce();
                json!({"ok": true, "cid": cid})
            }
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
        // Editing rebuilds the directory and re-signs it as ours. Doing that to
        // a model another Orb originated would erase its origin and put our
        // name on it, so it is only allowed for what this Orb shared itself.
        if !is_mine(&old) {
            return json!({"ok": false, "err": "only its publisher can edit a model — this one is a mirror of someone else's. (If it is yours but unsigned, sign it first.)"});
        }
        // The current card's manifest, if it has one, so the edited card can
        // carry a correct one too (weights hashes reused, sidecars re-hashed).
        let old_files: Option<Vec<FileRec>> = cat_capped(&format!("{}/{CARD_FILE}", req.cid), MAX_TEXT_BYTES)
            .ok()
            .and_then(|b| serde_json::from_slice::<CardDoc>(&b).ok())
            .and_then(|c| c.files);
        let mut new_files = old_files.clone().unwrap_or_default();
        let rec = |path: &str, bytes: &[u8]| -> FileRec {
            use sha2::{Digest, Sha256};
            FileRec { path: path.to_string(), size: bytes.len() as u64, sha256: hex::encode(Sha256::digest(bytes)) }
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
                new_files.retain(|f| f.path != "README.md");
                false
            }
            Some(md) => {
                if let Err(e) = run_script_stdin(&["mfs-write", &format!("{base}/README.md")], md.as_bytes()) {
                    let _ = run_script(&["mfs-rm", &base]);
                    return json!({"ok": false, "err": format!("write readme: {e}")});
                }
                new_files.retain(|f| f.path != "README.md");
                new_files.push(rec("README.md", md.as_bytes()));
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
                        new_files.retain(|f| f.path != old_img && f.path != img_name);
                        new_files.push(rec(&img_name, &bytes));
                        card.image = img_name;
                    }
                    _ => {}
                }
            }
            let _ = std::fs::remove_file(&tmp);
        } else if req.remove_image {
            if !old_img.is_empty() {
                let _ = run_script(&["mfs-rm", &format!("{base}/{old_img}")]);
                new_files.retain(|f| f.path != old_img);
            }
            card.image = String::new();
        } else {
            card.image = old_img; // keep
        }

        // Rewrite model-card.json. A card that had a signed manifest gets a
        // fresh one over the edited file set; an older card without one stays
        // without (we cannot vouch for hashes of files we never listed).
        let mut card_doc = json!({
            "schema": "aeon-model-card/1", "name": req.name, "file": old.file,
            "size_bytes": old.size_bytes, "sha256": old.sha256, "card": card,
            "verified": old.verified, "source": old.source,
            "shared_by": {"id": old.origin_id, "label": old.origin_label},
            "created_ms": old.added_at_ms,
        });
        if old_files.is_some() {
            new_files.sort_by(|a, b| a.path.cmp(&b.path));
            let signed_ms = epoch_ms();
            let digest = crate::publisher::origin_digest(
                &req.name, &old.source, old.verified, signed_ms, &manifest_tuples(&new_files));
            let origin = match crate::publisher::sign_as_orb(&digest) {
                Ok(s) => OriginBlock { key: s.pubkey, name: s.name, signed_ms, sig: s.sig, delegation: s.delegation, upstream: String::new() },
                Err(_) => OriginBlock { signed_ms, ..Default::default() },
            };
            card_doc["schema"] = json!("aeon-model-card/2");
            card_doc["files"] = json!(new_files);
            card_doc["origin"] = json!(origin);
        }
        // An edit changes the card, the README and the picture, never the
        // weights: what the provider published for them still holds, as long
        // as every recorded file is still in the edited directory.
        let kept_record = old.upstream_record.clone().filter(|rec| old_files.is_some() && rec.within(&new_files));
        let kept_witnesses = match &kept_record {
            Some(rec) => upstream::valid_witnesses(rec, &old.witnesses),
            None => Vec::new(),
        };
        if let Some(rec) = &kept_record {
            card_doc["upstream_record"] = json!(rec);
            if !kept_witnesses.is_empty() {
                card_doc["witnesses"] = json!(kept_witnesses);
            }
        }
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
        let mut edited = ModelEntry {
            cid: new_cid.clone(), name: req.name, file: old.file, size_bytes: old.size_bytes,
            sha256: old.sha256, card, added_at_ms: old.added_at_ms,
            origin_id: old.origin_id, origin_label: old.origin_label,
            verified: old.verified, source: old.source,
            upstream_record: kept_record, witnesses: kept_witnesses,
            ..Default::default()
        };
        sign_entry(&mut edited); // re-signed: editing rebuilds the model (new CID)
        entries.push(edited);
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
        // Per-read, not whole-request: a multi-GB download may take hours, but
        // two minutes of total silence means the connection is dead. Without
        // this a stalled socket held the import (and its task row) forever.
        .timeout_read(std::time::Duration::from_secs(120))
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

/// A staging directory that removes itself when dropped, so an import that
/// fails part-way (network error, hash mismatch, full disk) cannot strand
/// gigabytes under staging/ the way a bare `?` return used to.
struct StagingDir(std::path::PathBuf);

impl StagingDir {
    fn new(prefix: &str) -> Result<Self, String> {
        let p = staging_root().join(format!("{prefix}-{}", new_draft_id()));
        std::fs::create_dir_all(&p).map_err(|e| format!("mkdir: {e}"))?;
        Ok(Self(p))
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A repo-relative path is safe to recreate under a staging dir when every
/// segment is a plain name — no absolute paths, no `..`, no odd characters.
fn safe_rel_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && p.split('/').all(|seg| {
            !seg.is_empty() && seg != "." && seg != ".." && !seg.chars().any(|c| c.is_control() || c == '\\')
        })
}

/// Small non-weight files a model needs in order to actually LOAD: tokenizer,
/// configs, chat template, vocab. Leaving them out (as the importer once did,
/// taking only config.json) shares weights nobody can run.
fn is_sidecar(path: &str, size: u64) -> bool {
    const MAX: u64 = 64 * 1024 * 1024;
    let leaf = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    size <= MAX
        && !leaf.starts_with('.')
        && leaf != "readme.md"
        && [".json", ".txt", ".model", ".jinja", ".yaml", ".yml", ".vocab", ".spm"].iter().any(|e| leaf.ends_with(e))
}

/// Replace a secret with dots wherever it appears in a message bound for the UI.
fn redact(msg: &str, secret: Option<&str>) -> String {
    match secret {
        Some(s) if !s.is_empty() => msg.replace(s, "•••"),
        _ => msg.to_string(),
    }
}

fn do_import_hf(repo: &str, only_file: Option<String>, model_name: &str, key: &str) -> Result<(), String> {
    let meta = hf_get_json(&format!("https://huggingface.co/api/models/{repo}"))?;
    // Pin the exact commit the default branch is at right now. The file list,
    // every download and the README are all read at this one revision, so a
    // push that lands mid-import cannot mix two versions, and the upstream
    // record can say precisely which revision these hashes belong to.
    let commit = meta
        .get("sha")
        .and_then(|v| v.as_str())
        .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .ok_or("HuggingFace did not say which revision this repository is at, so the import cannot be pinned — nothing was imported")?
        .to_string();
    let observed_ms = epoch_ms();
    // Recursive: weights and configs often sit in subfolders (unet/, vae/, …).
    let tree = hf_get_json(&format!("https://huggingface.co/api/models/{repo}/tree/{commit}?recursive=true"))?;
    let get = |f: &Value, k: &str| f.get(k).and_then(|v| v.as_str()).map(String::from);
    // (path, size, lfs sha256)
    let entries: Vec<(String, u64, Option<String>)> = tree
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|f| f.get("type").and_then(|v| v.as_str()) != Some("directory"))
        .filter_map(|f| {
            let path = get(f, "path")?;
            let size = f.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            let oid = f.get("lfs").and_then(|l| l.get("oid")).and_then(|v| v.as_str()).map(String::from);
            Some((path, size, oid))
        })
        .filter(|(p, _, _)| safe_rel_path(p))
        .collect();

    let ends = |p: &str, ext: &str| p.to_ascii_lowercase().ends_with(ext);
    let mut picks: Vec<(String, u64, Option<String>)> = Vec::new();
    if let Some(one) = &only_file {
        if !safe_rel_path(one) {
            return Err("invalid file path".into());
        }
        picks.push(entries.iter().find(|(p, _, _)| p == one).cloned().unwrap_or((one.clone(), 0, None)));
    } else if entries.iter().any(|(p, _, _)| ends(p, ".safetensors")) {
        picks.extend(entries.iter().filter(|(p, s, _)| ends(p, ".safetensors") || is_sidecar(p, *s)).cloned());
    } else if entries.iter().any(|(p, _, _)| ends(p, ".gguf")) {
        // A GGUF is self-contained: take one — a common balanced quant if the
        // repo has it, else the smallest.
        let prefs = ["q4_k_m", "q4_0", "q5_k_m", "q8_0", "q6_k", "q3_k_m"];
        let ggufs: Vec<&(String, u64, Option<String>)> = entries.iter().filter(|(p, _, _)| ends(p, ".gguf")).collect();
        let pick = prefs
            .iter()
            .find_map(|q| ggufs.iter().find(|(p, _, _)| p.to_ascii_lowercase().contains(q)).copied())
            .or_else(|| ggufs.iter().min_by_key(|(_, s, _)| *s).copied());
        picks.extend(pick.cloned());
    } else {
        picks.extend(entries.iter().filter(|(p, s, _)| is_weight(p) || is_sidecar(p, *s)).cloned());
    }
    if !picks.iter().any(|(p, _, _)| is_weight(p)) {
        return Err("no weight files found in the repo".into());
    }
    let total: u64 = picks.iter().map(|(_, s, _)| *s).sum();
    let free = free_bytes(MODELS_DIR);
    if total > 0 && total + 512 * 1024 * 1024 > free {
        return Err(format!(
            "not enough disk: model is {} but only {} free",
            human(total), human(free)
        ));
    }
    let staging = StagingDir::new("hf")?;
    let dir = staging.path();

    // Download, holding every LFS file to the SHA-256 HuggingFace publishes.
    let token = source_token("huggingface");
    let mut prior = 0u64;
    let mut any_verifiable = false;
    let mut known: HashMap<String, String> = HashMap::new();
    // (path, bytes received, sha256, did HuggingFace publish that sha256)
    let mut recorded: Vec<(String, u64, String, bool)> = Vec::new();
    for (path, _size, oid) in &picks {
        let dest = dir.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let url = format!("https://huggingface.co/{repo}/resolve/{commit}/{path}");
        let (sha, n) = hf_download(&url, &dest, key, path, total, prior, token.as_deref())?;
        prior += n;
        // HuggingFace publishes a SHA-256 only for files kept in LFS (the LFS
        // object id). For a file kept in git itself there is none: its hash is
        // the one computed here, and is recorded as computed.
        recorded.push((path.clone(), n, sha.clone(), oid.is_some()));
        if let Some(expected) = oid {
            any_verifiable = true;
            if !expected.eq_ignore_ascii_case(&sha) {
                // This used to be recorded as "unverified" and shared anyway.
                // A mismatch means the bytes are not the model: stop.
                return Err(format!(
                    "“{path}” does not match the SHA-256 HuggingFace publishes for it — the download was corrupted or altered, so nothing was shared. Retry the import."
                ));
            }
        }
        known.insert(path.clone(), sha);
    }
    let verified = any_verifiable;
    // Prefer a top-level weight as the headline file (its name is what the
    // gateway download link uses).
    let primary_file = picks
        .iter()
        .map(|(p, _, _)| p)
        .filter(|p| is_weight(p))
        .min_by_key(|p| p.matches('/').count())
        .cloned()
        .unwrap_or_default();

    // README (the model card) + author avatar — best-effort.
    task_set(key, "fetching README + image…");
    if let Ok(resp) = hf_agent().get(&format!("https://huggingface.co/{repo}/resolve/{commit}/README.md")).call() {
        let mut text = String::new();
        use std::io::Read;
        if resp.into_reader().take(MAX_TEXT_BYTES as u64).read_to_string(&mut text).is_ok() && !text.is_empty() {
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
                    if r.into_reader().take(MAX_IMAGE_BYTES as u64).read_to_end(&mut bytes).is_ok()
                        && sniff_kind(&bytes[..bytes.len().min(16)]) == "image"
                        && std::fs::write(dir.join(&name), &bytes).is_ok()
                    {
                        image_name = name;
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
    let format = if ends(&primary_file, ".gguf") { "gguf" }
        else if ends(&primary_file, ".safetensors") { "safetensors" }
        else if ends(&primary_file, ".onnx") { "onnx" }
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

    // Hold our own imports to the gate every peer applies on download — no
    // point sharing something each of them will quarantine and reject.
    inspect_model_dir(dir)?;
    task_set(key, "adding to IPFS…");
    let spec = ShareSpec {
        name: model_name.to_string(),
        file: primary_file,
        card,
        verified,
        source: format!("https://huggingface.co/{repo}"),
        upstream: String::new(), identity: true,
        provider_record: upstream::record_from_import("huggingface", repo, &commit, recorded).map(|r| (r, observed_ms)),
    };
    let entry = finalize_share(dir, spec, &known)?;
    // Gated/private weights stay on this Orb only. Seeding off does the same
    // for a public repo: a local library copy, no pin, no announcement.
    let gated = hf_is_gated(&meta);
    if gated || !seed_enabled() {
        let _ = ensure_in_library(&entry.cid, &model_slug(model_name));
        let _ = run_script(&["unpin", &entry.cid]);
        if gated {
            return Err("this HuggingFace repo is gated or private — kept locally, not seeded".into());
        }
        return Ok(());
    }
    catalog_add(entry);
    Ok(())
}

fn hf_is_gated(meta: &Value) -> bool {
    if meta.get("private").and_then(|v| v.as_bool()).unwrap_or(false) {
        return true;
    }
    match meta.get("gated") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            let s = s.trim();
            !s.is_empty() && s != "false"
        }
        _ => false,
    }
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

/// Fetch a model's Ollama-registry manifest and its digest: the SHA-256 of the
/// manifest exactly as the registry served it, which is what pins every layer
/// it lists. `host` lets the same code hit hailo-ollama's registry for Hailo
/// imports.
fn ollama_manifest(host: &str, ns: &str, model: &str, tag: &str) -> Result<(Value, String), String> {
    let url = format!("https://{host}/v2/{ns}/{model}/manifests/{tag}");
    let resp = hf_agent()
        .get(&url)
        .set("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => format!("model not found on the registry: {ns}/{model}:{tag}"),
            other => format!("registry request failed: {other}"),
        })?;
    let mut body = Vec::new();
    {
        use std::io::Read;
        resp.into_reader()
            .take(MAX_TEXT_BYTES as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| format!("read manifest: {e}"))?;
    }
    if body.len() > MAX_TEXT_BYTES {
        return Err("the registry's manifest is implausibly large".into());
    }
    let manifest = serde_json::from_slice::<Value>(&body).map_err(|e| format!("bad manifest json: {e}"))?;
    let digest = {
        use sha2::{Digest, Sha256};
        format!("sha256:{}", hex::encode(Sha256::digest(&body)))
    };
    Ok((manifest, digest))
}

fn do_import_ollama(reference: &str, model_name: &str, key: &str) -> Result<(), String> {
    let (ns, model, tag) = parse_ollama_ref(reference)?;
    let host = "registry.ollama.ai";
    task_set(key, "resolving Ollama manifest…");
    let (manifest, manifest_digest) = ollama_manifest(host, &ns, &model, &tag)?;
    let observed_ms = epoch_ms();
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
    let staging = StagingDir::new("ollama")?;
    let dir = staging.path();

    let leaf = format!("{model}-{tag}.gguf");
    let blob_url = format!("https://{host}/v2/{ns}/{model}/blobs/{digest}");
    let (sha, received) = hf_download(&blob_url, &dir.join(&leaf), key, &leaf, size, 0, source_token("ollama").as_deref())?;
    // The layer digest IS the blob's SHA-256; a mismatch means these are not
    // the model's bytes, so stop rather than share them marked "unverified".
    if !expected_sha.is_empty() && !expected_sha.eq_ignore_ascii_case(&sha) {
        return Err("the downloaded weights do not match the registry's layer digest — corrupted or altered, nothing was shared".into());
    }
    let verified = !expected_sha.is_empty();

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
    let ns_seg = if ns == "library" { String::new() } else { format!("{ns}/") };
    let source = format!("https://ollama.com/{ns_seg}{model}:{tag}");
    inspect_model_dir(dir)?;
    task_set(key, "adding to IPFS…");
    // The registry publishes the weights' SHA-256 as the layer digest; the
    // manifest digest pins that layer together with every other one.
    let provider_record = upstream::record_from_import(
        "ollama",
        &format!("{ns}/{model}:{tag}"),
        &manifest_digest,
        [(leaf.clone(), received, sha.clone(), verified)],
    )
    .map(|r| (r, observed_ms));
    let known: HashMap<String, String> = [(leaf.clone(), sha)].into_iter().collect();
    let spec = ShareSpec {
        name: model_name.to_string(), file: leaf, card, verified, source, upstream: String::new(), identity: true,
        provider_record,
    };
    let entry = finalize_share(dir, spec, &known)?;
    let cid = entry.cid.clone();
    catalog_add(entry);
    if !seed_enabled() {
        retain_private(&cid, model_name);
    }
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

/// The plain file name a Civitai file is saved under. The name is chosen by an
/// uploader, so it is reduced to letters, digits, '.', '-' and '_'. The
/// re-check uses the same rule to find the file again.
pub(super) fn civitai_leaf(name: &str) -> String {
    let n: String = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let n = n.trim_start_matches('.').to_string();
    if n.is_empty() { "model.safetensors".to_string() } else { n }
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

    // A name chosen by an uploader: keep it to a plain file name.
    let leaf: String = civitai_leaf(file.get("name").and_then(|v| v.as_str()).unwrap_or("model.safetensors"));
    let observed_ms = epoch_ms();
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
    let staging = StagingDir::new("civitai")?;
    let dir = staging.path();
    // The token rides in the URL, and a failed request's error text quotes the
    // URL — which then showed up verbatim in the task list. Strip it.
    let (sha, received) = hf_download(&dl, &dir.join(&leaf), key, &leaf, size, 0, None)
        .map_err(|e| redact(&e, auth.as_deref()))?;
    if !expected_sha.is_empty() && !expected_sha.eq_ignore_ascii_case(&sha) {
        return Err("the downloaded file does not match the SHA-256 Civitai publishes for it — corrupted or altered, nothing was shared".into());
    }
    let verified = !expected_sha.is_empty();

    // Reject anything that isn't actually a safetensors (fake/mislabelled).
    {
        let mut head = [0u8; 16];
        let hn = { use std::io::Read; std::fs::File::open(dir.join(&leaf)).and_then(|mut f| f.read(&mut head)).unwrap_or(0) };
        if !matches!(sniff_kind(&head[..hn]), "safetensors") {
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
                if r.into_reader().take(MAX_IMAGE_BYTES as u64).read_to_end(&mut bytes).is_ok()
                    && sniff_kind(&bytes[..bytes.len().min(16)]) == "image"
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
    let source = format!("https://civitai.com/models/{}", model_meta.get("id").and_then(|v| v.as_u64()).unwrap_or(vid));
    inspect_model_dir(dir)?;
    task_set(key, "adding to IPFS…");
    // Pinned to the model-version id. The model id comes from the version
    // itself when the import was started from a version link. Without both
    // ids there is nothing to re-check against, so no record is made.
    let civitai_model = model_meta.get("id").and_then(|v| v.as_u64()).or_else(|| version.get("modelId").and_then(|v| v.as_u64()));
    let provider_record = civitai_model
        .filter(|_| vid > 0)
        .and_then(|mid| {
            upstream::record_from_import("civitai", &mid.to_string(), &vid.to_string(), [(leaf.clone(), received, sha.clone(), verified)])
        })
        .map(|r| (r, observed_ms));
    let known: HashMap<String, String> = [(leaf.clone(), sha)].into_iter().collect();
    let spec = ShareSpec {
        name: m_name.clone(), file: leaf, card, verified, source, upstream: String::new(), identity: true,
        provider_record,
    };
    let entry = finalize_share(dir, spec, &known)?;
    let cid = entry.cid.clone();
    catalog_add(entry);
    if !seed_enabled() {
        retain_private(&cid, &m_name);
    }
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
    // 400 is a byte count, so on a page with non-ASCII text it can land inside
    // a character, and slicing there panics (which aborts a release build).
    // Step back to a boundary; `idx` is one, so this stops there at the latest.
    let mut window_end = (idx + 400).min(html.len());
    while !html.is_char_boundary(window_end) {
        window_end -= 1;
    }
    let window = &html[idx..window_end];
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
    // Trim dots too: a name of "." or ".." must not become a path component
    // that points at the library itself or its parent.
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
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
    // registry.json is {"peers": {<id>: {models: [...]}}}. This used to parse
    // it as a bare list, which never matched, so nothing on the network could
    // be found by name and `model_list` always reported an empty network.
    registry_models().into_iter().find(|e| hit(e))
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
    let size = run_script(&["get", cid, &dest]).map(|s| s.trim().parse().unwrap_or_else(|_| dir_size(&dir)))?;
    // The library is what gets rsynced to connected machines, and a pull can
    // name any CID on the network — so it passes the same gate a download
    // does before it is allowed to stay. Content that already went through
    // quarantine (it is in our catalog) skips the slow virus scan.
    let gate = inspect_model_dir(&dir).and_then(|_| {
        if read_catalog().iter().any(|e| e.cid == cid) {
            Ok(())
        } else {
            virus_scan(&dir).map(|_| ()).map_err(|e| format!("virus scan: {e}"))
        }
    });
    if let Err(e) = gate {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(size)
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

/// source URL → CID for everything this Orb shares that came from a URL. Lets
/// the Hailo page show which of its models are also seeded on Model Share.
pub fn vendor_cids() -> HashMap<String, String> {
    read_catalog().into_iter().filter(|e| !e.source.is_empty()).map(|e| (e.source, e.cid)).collect()
}

/// Mirror a model file that already lives on this Orb — e.g. a .hef the Hailo
/// page downloaded — into Model Share, so anything fetched for local inference
/// is also seeded to the network. The card it gets holds nothing specific to
/// this Orb, so every Orb mirroring the same vendor file arrives at the same
/// CID: one row in the index, one swarm, more seeders with every Orb that
/// installs the model. Returns the CID. Idempotent.
pub fn share_vendor_file(
    src: &std::path::Path,
    file_name: &str,
    name: &str,
    card: ModelCard,
    source: &str,
    upstream: &str,
) -> Result<String, String> {
    if !read_config().enabled {
        return Err("IPFS is off".into());
    }
    if !seed_enabled() {
        return Err("seeding is off on this Orb".into());
    }
    let size = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?.len();
    if size == 0 {
        return Err("the model file is empty".into());
    }
    // Already mirrored at this size → done, without re-hashing gigabytes on
    // every boot.
    if let Some(e) = read_catalog().into_iter().find(|e| e.source == source && e.file == file_name && e.size_bytes == size) {
        if e.upstream != upstream {
            let mut all = read_catalog();
            for x in all.iter_mut().filter(|x| x.cid == e.cid) {
                x.upstream = upstream.to_string();
            }
            let _ = write_catalog(&all);
            announce();
        }
        return Ok(e.cid);
    }
    if size.saturating_add(512 * 1024 * 1024) > free_bytes(MODELS_DIR) {
        return Err(format!("not enough disk to seed it ({} needed)", human(size)));
    }
    let (repo, smax) = ipfs_repo_usage();
    if smax > 0 && repo.saturating_add(size) > smax {
        return Err(format!(
            "it would not fit under the IPFS storage limit ({} used of {}) — raise the limit to seed it",
            human(repo), human(smax)
        ));
    }
    let staging = StagingDir::new("vendor")?;
    let dest = staging.path().join(file_name);
    // A hard link costs no disk; fall back to a copy across filesystems (e.g.
    // when the data root lives on an adopted USB drive).
    if std::fs::hard_link(src, &dest).is_err() {
        std::fs::copy(src, &dest).map_err(|e| format!("stage {file_name}: {e}"))?;
    }
    inspect_model_dir(staging.path())?;
    let spec = ShareSpec {
        name: name.to_string(),
        file: file_name.to_string(),
        card,
        verified: false,
        source: source.to_string(),
        upstream: upstream.to_string(),
        identity: true,
        provider_record: None,
    };
    let entry = finalize_share(staging.path(), spec, &HashMap::new())?;
    let cid = entry.cid.clone();
    // A re-download that changed the file leaves an older mirror behind; drop it.
    let stale: Vec<String> = read_catalog()
        .into_iter()
        .filter(|e| e.source == source && e.file == file_name && e.cid != cid)
        .map(|e| e.cid)
        .collect();
    if !stale.is_empty() {
        let kept: Vec<ModelEntry> = read_catalog().into_iter().filter(|e| !stale.contains(&e.cid)).collect();
        let _ = write_catalog(&kept);
        for c in &stale {
            let _ = run_script(&["unpin", c]);
        }
    }
    catalog_add(entry);
    announce();
    Ok(cid)
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
    let mut seen = std::collections::HashSet::new();
    let registry: Vec<Value> = registry_models()
        .into_iter()
        .filter(|e| seen.insert(e.cid.clone()))
        .map(|e| json!({
            "cid": e.cid, "name": e.name, "size_bytes": e.size_bytes, "size": human(e.size_bytes),
            "signed": entry_sig_ok(&e),
        }))
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
        if !valid_cid(&cid) {
            return json!({"ok": false, "err": "no valid CID to pull"});
        }
        let slug = model_slug(&name);
        task_set(&format!("pull:{slug}"), "materializing from IPFS…");
        let out = ensure_in_library(&cid, &slug);
        if out.is_ok() && seed_enabled() {
            let _ = run_script(&["pin", &cid]);
            if find_model(&cid).is_none() {
                catalog_add_raw(ModelEntry {
                    cid: cid.clone(),
                    name: name.clone(),
                    size_bytes: out.as_ref().copied().unwrap_or(0),
                    added_at_ms: epoch_ms(),
                    source: "pull".into(),
                    ..Default::default()
                });
            }
            announce();
        }
        task_clear(&format!("pull:{slug}"));
        match out {
            Ok(sz) => json!({"ok": true, "slug": slug, "name": name, "cid": cid, "size_bytes": sz, "size": human(sz), "seeded": seed_enabled()}),
            Err(e) => json!({"ok": false, "err": storage_hint(&e)}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "pull task failed"}));
    Json(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A scratch model directory that cleans up after itself.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("aeon-ipfs-test-{tag}-{}-{}", std::process::id(), new_draft_id()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn put(&self, rel: &str, bytes: &[u8]) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(name: &str, file: &str) -> ShareSpec {
        ShareSpec {
            name: name.into(),
            file: file.into(),
            card: ModelCard { kind: "llm".into(), format: "gguf".into(), ..Default::default() },
            verified: true,
            source: "https://example.test/m".into(),
            upstream: String::new(), identity: true, provider_record: None,
        }
    }

    /// A shared model as an honest origin would publish it.
    fn shared(tag: &str) -> (Tmp, ModelEntry) {
        let t = Tmp::new(tag);
        t.put("model.gguf", b"GGUF-weights-go-here");
        t.put("README.md", b"# hello");
        t.put("sub/tokenizer.json", b"{}");
        let entry = prepare_share(t.path(), spec("m", "model.gguf"), &HashMap::new()).unwrap();
        (t, entry)
    }

    #[test]
    fn an_honest_share_verifies_and_is_credited_to_its_signer() {
        let (t, entry) = shared("ok");
        assert_eq!(entry.file, "model.gguf");
        assert_eq!(entry.size_bytes, 20 + 7 + 2); // every file, card excluded
        let c = verify_model_dir(t.path(), &entry).unwrap();
        assert!(c.manifest && c.files_ok && c.origin_ok);
        assert_eq!(c.origin_key, crate::publisher::orb_pubkey());
        assert_eq!(c.origin_identity, c.origin_key); // no delegation in tests
        assert!(c.claims_verified);
        assert_eq!(c.source, "https://example.test/m");
    }

    /// What an importer hands over: the provider published a SHA-256 for the
    /// weights and none for the tokenizer.
    fn provider_record(t: &Tmp) -> upstream::UpstreamRecord {
        let sha = |rel: &str| sha256_file(&t.path().join(rel)).unwrap();
        upstream::record_from_import(
            "huggingface",
            "org/m",
            "0123456789abcdef0123456789abcdef01234567",
            vec![
                ("model.gguf".to_string(), 20, sha("model.gguf"), true),
                ("sub/tokenizer.json".to_string(), 2, sha("sub/tokenizer.json"), false),
            ],
        )
        .unwrap()
    }

    fn imported(tag: &str) -> (Tmp, ModelEntry, upstream::UpstreamRecord) {
        let t = Tmp::new(tag);
        t.put("model.gguf", b"GGUF-weights-go-here");
        t.put("README.md", b"# hello");
        t.put("sub/tokenizer.json", b"{}");
        let rec = provider_record(&t);
        let mut s = spec("m", "model.gguf");
        s.provider_record = Some((rec.clone(), 1_760_000_000_000));
        let entry = prepare_share(t.path(), s, &HashMap::new()).unwrap();
        (t, entry, rec)
    }

    #[test]
    fn an_import_keeps_what_the_provider_published_on_the_entry_and_in_the_card() {
        let (t, entry, rec) = imported("rec");
        assert_eq!(entry.upstream_record.as_ref(), Some(&rec));
        assert_eq!(entry.witnesses.len(), 1);
        assert_eq!(entry.witnesses[0].key, crate::publisher::orb_pubkey());
        assert_eq!(entry.witnesses[0].observed_ms, 1_760_000_000_000);
        assert!(upstream::witness_ok(&rec, &entry.witnesses[0]));

        let doc: Value = serde_json::from_slice(&std::fs::read(t.path().join(CARD_FILE)).unwrap()).unwrap();
        assert_eq!(doc["upstream_record"]["revision"], "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(doc["upstream_record"]["files"][0]["hash_from"], "provider");
        assert_eq!(doc["upstream_record"]["files"][1]["hash_from"], "computed");
        assert_eq!(doc["witnesses"][0]["sig"], entry.witnesses[0].sig.as_str());

        // A mirror reads it back out of the directory it just verified.
        let c = verify_model_dir(t.path(), &entry).unwrap();
        assert_eq!(c.upstream_record.as_ref(), Some(&rec));
        assert_eq!(c.witnesses, entry.witnesses);
        // …and none of it ends up in the stored verification result.
        let stored = serde_json::to_value(&c).unwrap();
        assert!(stored.get("upstream_record").is_none() && stored.get("witnesses").is_none());
        // The entry survives gossip (a JSON round trip) with both intact.
        let back: ModelEntry = serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(back.upstream_record, entry.upstream_record);
        assert_eq!(back.witnesses, entry.witnesses);

        // A share that is not an import has neither, on the entry or the card.
        let (t, plain) = shared("norec");
        assert!(plain.upstream_record.is_none() && plain.witnesses.is_empty());
        let doc: Value = serde_json::from_slice(&std::fs::read(t.path().join(CARD_FILE)).unwrap()).unwrap();
        assert!(doc.get("upstream_record").is_none() && doc.get("witnesses").is_none());
    }

    #[test]
    fn an_upstream_record_may_only_describe_the_files_it_travels_with() {
        // At import: a record naming bytes that are not in the share is a bug
        // in the importer, and stops the share.
        let t = Tmp::new("recbad");
        t.put("model.gguf", b"GGUF-weights-go-here");
        t.put("sub/tokenizer.json", b"{}");
        let mut rec = provider_record(&t);
        rec.files[0].sha256 = "00".repeat(32);
        let mut s = spec("m", "model.gguf");
        s.provider_record = Some((rec, 5));
        assert!(prepare_share(t.path(), s, &HashMap::new()).unwrap_err().contains("upstream record"));

        // On download: a card whose record was rewritten to name another hash
        // is not believed. The files still verify; the record is dropped.
        let (t, entry, _) = imported("recswap");
        let card_path = t.path().join(CARD_FILE);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&card_path).unwrap()).unwrap();
        doc["upstream_record"]["files"][0]["sha256"] = json!("00".repeat(32));
        std::fs::write(&card_path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let c = verify_model_dir(t.path(), &entry).unwrap();
        assert!(c.files_ok && c.origin_ok);
        assert!(c.upstream_record.is_none() && c.witnesses.is_empty());

        // A record re-pointed at another repository or revision keeps its
        // files, but the witness that came with it no longer holds for it.
        let (t, entry, _) = imported("recmove");
        let card_path = t.path().join(CARD_FILE);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&card_path).unwrap()).unwrap();
        doc["upstream_record"]["repo"] = json!("famous-lab/m");
        std::fs::write(&card_path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let c = verify_model_dir(t.path(), &entry).unwrap();
        assert_eq!(c.upstream_record.as_ref().map(|r| r.repo.as_str()), Some("famous-lab/m"));
        assert!(c.witnesses.is_empty(), "nobody signed that they saw famous-lab publish these hashes");
    }

    #[test]
    fn changed_added_or_missing_files_are_rejected() {
        // Weights swapped for something else of the same length.
        let (t, entry) = shared("swap");
        t.put("model.gguf", b"GGUF-weights-go-HERE");
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("model.gguf"));

        // A stowaway the origin never signed.
        let (t, entry) = shared("extra");
        t.put("payload.bin", b"surprise");
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("payload.bin"));

        // A file the manifest promises is gone.
        let (t, entry) = shared("gone");
        std::fs::remove_file(t.path().join("sub/tokenizer.json")).unwrap();
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("tokenizer.json"));
    }

    #[test]
    fn a_card_rewritten_by_someone_else_fails_the_origin_signature() {
        let (t, entry) = shared("card");
        let card_path = t.path().join(CARD_FILE);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&card_path).unwrap()).unwrap();
        // Claim a different upstream while keeping the original signature.
        doc["source"] = json!("https://evil.test/m");
        std::fs::write(&card_path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("origin signature"));

        // Same for flipping the "verified upstream" claim.
        let (t, entry) = shared("flag");
        let card_path = t.path().join(CARD_FILE);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&card_path).unwrap()).unwrap();
        doc["verified"] = json!(false);
        std::fs::write(&card_path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("origin signature"));
    }

    #[test]
    fn symbolic_links_are_refused() {
        let (t, entry) = shared("link");
        std::os::unix::fs::symlink("/etc/passwd", t.path().join("weights2.gguf")).unwrap();
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("symbolic link"));
        // …including a link standing in for a directory.
        let (t, entry) = shared("dirlink");
        std::os::unix::fs::symlink("/", t.path().join("everything")).unwrap();
        assert!(verify_model_dir(t.path(), &entry).unwrap_err().contains("symbolic link"));
    }

    #[test]
    fn an_older_card_without_a_manifest_is_held_to_the_advertised_hash() {
        let t = Tmp::new("legacy");
        t.put("m.gguf", b"GGUF!");
        t.put(CARD_FILE, br#"{"schema":"aeon-model-card/1","name":"m"}"#);
        let good = ModelEntry { file: "m.gguf".into(), sha256: sha256_file(&t.path().join("m.gguf")).unwrap(), ..Default::default() };
        let c = verify_model_dir(t.path(), &good).unwrap();
        assert!(c.files_ok && !c.manifest && !c.origin_ok);

        let bad = ModelEntry { file: "m.gguf".into(), sha256: "00".repeat(32), ..Default::default() };
        assert!(verify_model_dir(t.path(), &bad).is_err());
        let missing = ModelEntry { file: "other.gguf".into(), sha256: "00".repeat(32), ..Default::default() };
        assert!(verify_model_dir(t.path(), &missing).is_err());
    }

    #[test]
    fn a_vendor_mirror_card_is_identical_wherever_it_is_built() {
        let build = |tag: &str| {
            let t = Tmp::new(tag);
            t.put("yolov8s.hef", b"\x01HEF-bytes");
            let mut s = spec("YOLOv8s", "yolov8s.hef");
            s.verified = false;
            s.upstream = "hailo-model-zoo".into();
            s.identity = false;
            prepare_share(t.path(), s, &HashMap::new()).unwrap();
            let card = std::fs::read(t.path().join(CARD_FILE)).unwrap();
            (t, card)
        };
        let (a, card_a) = build("va");
        std::thread::sleep(std::time::Duration::from_millis(5)); // a different clock reading
        let (_b, card_b) = build("vb");
        // Byte-identical cards → identical directory → identical CID on every Orb.
        assert_eq!(card_a, card_b);
        let doc: Value = serde_json::from_slice(&card_a).unwrap();
        assert_eq!(doc["origin"]["upstream"], "hailo-model-zoo");
        assert!(doc.get("shared_by").is_none());
        assert_eq!(doc["created_ms"], 0);
        // And it still verifies as content (unsigned, upstream-labelled).
        let c = verify_model_dir(a.path(), &ModelEntry::default()).unwrap();
        assert!(c.files_ok && !c.origin_ok);
        assert_eq!(c.upstream, "hailo-model-zoo");
    }

    #[test]
    fn entry_signatures_bind_cid_name_and_hash() {
        let mut e = ModelEntry { cid: "bafytestcid000".into(), name: "m".into(), sha256: "ab".repeat(32), added_at_ms: 7, ..Default::default() };
        assert!(!entry_sig_ok(&e));
        assert_eq!(entry_identity(&e), "");
        sign_entry(&mut e);
        assert!(entry_sig_ok(&e));
        assert_eq!(entry_identity(&e), crate::publisher::orb_pubkey());
        // Re-pointing a signed entry at different content invalidates it.
        let mut moved = e.clone();
        moved.cid = "bafyothercid000".into();
        assert!(!entry_sig_ok(&moved));
        let mut renamed = e.clone();
        renamed.name = "m (official)".into();
        assert!(!entry_sig_ok(&renamed));
        // An entry survives a JSON round trip (gossip) with its signature intact.
        let back: ModelEntry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert!(entry_sig_ok(&back));
    }

    #[test]
    fn reputation_weighs_hard_evidence_over_popularity() {
        let day = 86_400_000_i64;
        let now = 100 * day;
        let fresh = Publisher { first_seen_ms: now, ..Default::default() };
        // Unsigned: nobody to hold to account.
        assert_eq!(reputation(None, false, 999, 999, now), 0);
        // Signed but never seen before.
        assert_eq!(reputation(None, true, 0, 0, now), 20);
        // The operator's word is final, either way.
        assert_eq!(reputation(Some(&Publisher { trusted: true, ..fresh.clone() }), true, 0, 0, now), 100);
        assert_eq!(reputation(Some(&Publisher { blocked: true, ..fresh.clone() }), true, 999, 999, now), 0);
        // One failed verification here outweighs any number of stars.
        assert_eq!(reputation(Some(&Publisher { bad: 1, ok: 9, ..fresh.clone() }), true, 9999, 9999, now), 5);
        // A thousand starring Orbs (cheap to fake) is worth less than ONE
        // download this Orb verified itself.
        let stars_only = reputation(Some(&fresh), true, 1000, 0, now);
        let one_verified = reputation(Some(&Publisher { ok: 1, ..fresh.clone() }), true, 0, 0, now);
        assert!(stars_only <= one_verified, "{stars_only} vs {one_verified}");
        // Nothing but explicit trust reaches 100.
        let maxed = Publisher { ok: 50, first_seen_ms: 0, ..Default::default() };
        assert_eq!(reputation(Some(&maxed), true, 100_000, 100_000, now), 95);
    }

    #[test]
    fn names_and_paths_cannot_escape_their_directories() {
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "a/./b", "a\\b", "a/\n"] {
            assert!(!safe_rel_path(bad), "{bad:?} should be refused");
        }
        for ok in ["model.gguf", "unet/diffusion_pytorch_model.safetensors", "a/b/c.json"] {
            assert!(safe_rel_path(ok), "{ok:?} should be allowed");
        }
        assert_eq!(model_slug(".."), "model");
        assert_eq!(model_slug("."), "model");
        assert_eq!(model_slug("../../etc"), "etc");
        assert_eq!(model_slug("Qwen2.5 7B (Q4)"), "qwen2.5-7b--q4");
    }

    #[test]
    fn sidecars_cover_what_a_model_needs_to_load() {
        for f in ["tokenizer.json", "tokenizer_config.json", "special_tokens_map.json", "vocab.txt", "spiece.model", "chat_template.jinja", "unet/config.json"] {
            assert!(is_sidecar(f, 1000), "{f}");
        }
        assert!(!is_sidecar("README.md", 10));
        assert!(!is_sidecar(".gitattributes", 10));
        assert!(!is_sidecar("model.safetensors", 10));
        assert!(!is_sidecar("huge.json", 65 * 1024 * 1024));
    }

    #[test]
    fn secrets_are_redacted_from_messages() {
        let msg = "download x: https://civitai.com/api/download/models/1?token=s3cr3t: status code 401";
        assert!(!redact(msg, Some("s3cr3t")).contains("s3cr3t"));
        assert_eq!(redact(msg, None), msg);
        assert_eq!(redact(msg, Some("")), msg);
    }

    // ── Entries and peers that arrive malformed ─────────────────────────────

    /// A model entry as an honest Orb gossips it. `future_field` stands for
    /// something a newer Orb might add: unknown here, and no reason to drop it.
    fn gossiped(name: &str) -> Value {
        json!({
            "cid": "bafytestcid000", "name": name, "file": "model.gguf", "size_bytes": 20,
            "sha256": "ab".repeat(32),
            "card": {"kind": "llm", "tags": ["a"], "readme": true, "nsfw": false},
            "added_at_ms": 1_760_000_000_000_i64, "origin_id": "orb-1", "origin_label": "an orb",
            "verified": true, "source": "https://example.test/m",
            "pub_key": "", "pub_name": "", "pub_sig": "",
            "future_field": {"anything": [1, "two", null]},
        })
    }

    /// `gossiped`, with one field replaced.
    fn with(name: &str, field: &str, value: Value) -> Value {
        let mut e = gossiped(name);
        e[field] = value;
        e
    }

    /// One entry per way a field this Orb knows can have the wrong type, and
    /// the things that are not entries at all.
    fn mistyped_entries() -> Vec<Value> {
        vec![
            with("bad", "file", json!(5)), // text fields holding something else
            with("bad", "sha256", json!(null)),
            with("bad", "name", json!(["m"])),
            with("bad", "btih", json!(false)),
            with("bad", "upstream", json!({})),
            with("bad", "size_bytes", json!("x")), // number fields
            with("bad", "size_bytes", json!(-1)),
            with("bad", "size_bytes", json!(1.5)),
            with("bad", "added_at_ms", json!("yesterday")),
            with("bad", "verified", json!("yes")), // the flag
            with("bad", "verified", json!(1)),
            with("bad", "card", json!("a card")), // the card, and members of it
            with("bad", "card", json!({"tags": "llm"})),
            with("bad", "card", json!({"tags": ["a", 7]})),
            with("bad", "card", json!({"nsfw": "no"})),
            with("bad", "card", json!({"kind": 3})),
            with("bad", "delegation", json!("mine")),
            with("bad", "delegation", json!({"user_key": 1, "orb_key": "k", "sig": "s"})),
            json!({"name": "no cid"}),
            json!({"cid": "bafytestcid000"}),
            json!("not an entry"),
            json!(["bafytestcid000", "a list is not an entry"]),
            json!(null),
            json!(7),
        ]
    }

    fn names(entries: &[ModelEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn a_mistyped_entry_is_skipped_and_its_neighbours_are_kept() {
        for bad in mistyped_entries() {
            let doc = json!({"peers": {"alice": {"peer_id": "alice", "models": [gossiped("first"), bad, gossiped("last")]}}});
            assert_eq!(names(&registry_peers(&doc)["alice"].models), ["first", "last"], "with {}", doc["peers"]["alice"]["models"][1]);
        }
        // A field this Orb does not know is not a reason to drop an entry, and
        // neither is a malformed upstream record: that reads as "no record".
        let odd = with("odd", "upstream_record", json!("junk"));
        let doc = json!({"peers": {"alice": {"peer_id": "alice", "models": [with("odd", "witnesses", json!(5)), odd]}}});
        let got = registry_peers(&doc);
        assert_eq!(names(&got["alice"].models), ["odd", "odd"]);
        assert!(got["alice"].models.iter().all(|e| e.upstream_record.is_none() && e.witnesses.is_empty()));
    }

    #[test]
    fn one_bad_entry_and_one_bad_peer_leave_every_good_entry_listed() {
        let mut alice = vec![gossiped("a1")];
        alice.extend(mistyped_entries());
        alice.push(gossiped("a2"));
        let mut peers = serde_json::Map::new();
        peers.insert("alice".into(), json!({
            "peer_id": "alice", "label": "Alice", "addrs": ["203.0.113.7"], "updated_ms": 5, "sent_ms": 4,
            "models": alice, "starred": ["bafytestcid000"],
        }));
        peers.insert("bob".into(), json!({"peer_id": "bob", "models": [gossiped("b1")]}));
        // Peer records that cannot be used. Those that are objects each carry
        // a good entry, to show the record is skipped as a whole.
        let unusable = [
            json!("a peer"),
            json!(null),
            json!(3),
            json!([]),
            json!({"peer_id": 9, "models": [gossiped("never")]}),
            json!({"peer_id": "p", "label": ["x"], "models": [gossiped("never")]}),
            json!({"peer_id": "p", "updated_ms": "now", "models": [gossiped("never")]}),
            json!({"peer_id": "p", "addrs": ["203.0.113.9", 7], "models": [gossiped("never")]}),
            json!({"peer_id": "p", "starred": "all of them", "models": [gossiped("never")]}),
            json!({"peer_id": "p", "orb_key": {}, "models": [gossiped("never")]}),
            json!({"peer_id": "p", "delegation": "x", "models": [gossiped("never")]}),
            json!({"peer_id": "p", "models": "all of them"}),
            json!({"peer_id": "p", "models": {"0": gossiped("never")}}),
        ];
        for (i, bad) in unusable.iter().enumerate() {
            peers.insert(format!("bad{i}"), bad.clone());
        }
        let doc = json!({"peers": peers});
        // The single typed parse this replaces cannot read this file at all,
        // which is what used to empty the index.
        assert!(serde_json::from_value::<HashMap<String, RegPeer>>(doc["peers"].clone()).is_err());

        let got = registry_peers(&doc);
        let mut ids: Vec<&str> = got.keys().map(String::as_str).collect();
        ids.sort();
        assert_eq!(ids, ["alice", "bob"]);
        assert_eq!(names(&got["alice"].models), ["a1", "a2"]);
        assert_eq!(names(&got["bob"].models), ["b1"]);
        // What a kept peer says about itself is kept with it.
        assert_eq!((got["alice"].label.as_str(), got["alice"].updated_ms), ("Alice", 5));
        assert_eq!(got["alice"].addrs, ["203.0.113.7"]);
        assert_eq!(got["alice"].starred, ["bafytestcid000"]);
        // A good entry is read in full, card included.
        let a1 = &got["alice"].models[0];
        assert_eq!((a1.file.as_str(), a1.size_bytes, a1.verified), ("model.gguf", 20, true));
        assert_eq!((a1.card.kind.as_str(), a1.card.tags.as_slice(), a1.card.readme), ("llm", &["a".to_string()][..], true));

        // Something that is not a registry is an empty index, not an error.
        for not_a_registry in [json!(null), json!([]), json!({}), json!({"peers": []}), json!({"peers": "x"})] {
            assert!(registry_peers(&not_a_registry).is_empty(), "{not_a_registry}");
        }
    }

    #[test]
    fn a_fleet_peers_model_list_is_rebuilt_entry_by_entry() {
        let mut models = vec![gossiped("f1")];
        models.extend(mistyped_entries());
        models.push(gossiped("f2"));
        let got = fleet_models(&json!({"enabled": true, "peer_id": "12D3KooWtest", "models": models, "extra": 1}));
        assert_eq!((got["enabled"].as_bool(), got["peer_id"].as_str()), (Some(true), Some("12D3KooWtest")));
        let listed: Vec<&str> = got["models"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert_eq!(listed, ["f1", "f2"]);
        assert!(got.get("extra").is_none());
        // Whatever arrives, the console gets a flag, a string and a list.
        for raw in [json!(null), json!("x"), json!([]), json!({}), json!({"enabled": "yes", "peer_id": 5, "models": "all"}), json!({"enabled": 1, "models": {"length": 3}})] {
            assert_eq!(fleet_models(&raw), json!({"enabled": false, "peer_id": "", "models": []}), "{raw}");
        }
    }

    /// Run a request body through the same extractor the route uses.
    fn fetch_body(body: &str, content_type: &str) -> Result<FetchReq, Value> {
        use axum::extract::FromRequest;
        let req = axum::http::Request::builder()
            .method("POST")
            .header("content-type", content_type)
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        fetch_request(rt.block_on(Json::<FetchReq>::from_request(req, &())))
    }

    #[test]
    fn a_fetch_request_that_cannot_be_used_is_refused_in_json() {
        // What the console sends: a row of the index plus where to dial.
        let row = |mut e: Value| {
            e["addrs"] = json!(["203.0.113.7"]);
            e["peer_id"] = json!("12D3KooWtest");
            e["hosts"] = json!([{"label": "x"}]);
            e.to_string()
        };
        let Ok(req) = fetch_body(&row(gossiped("m")), "application/json") else {
            panic!("a well-formed row was refused");
        };
        assert_eq!((req.entry.name.as_str(), req.entry.size_bytes, req.peer_id.as_str()), ("m", 20, "12D3KooWtest"));
        assert_eq!(req.addrs, ["203.0.113.7"]);

        let refused = [
            row(with("m", "size_bytes", json!("x"))),
            row(with("m", "verified", json!("yes"))),
            row(with("m", "card", json!({"tags": 1}))),
            row(with("m", "cid", json!("../../etc"))),
            row(json!({"name": "no cid"})),
            "{".to_string(),
            "[]".to_string(),
            String::new(),
        ];
        for body in refused {
            let refusal = fetch_body(&body, "application/json").err().unwrap_or_else(|| panic!("{body} was accepted"));
            assert_eq!(refusal["ok"], false, "{body}");
            assert!(refusal["err"].as_str().is_some_and(|e| !e.is_empty()), "{body}");
        }
        // The body must still be sent as JSON, which a form on another site cannot do.
        assert!(fetch_body(&row(gossiped("m")), "text/plain").is_err());

        // The size is whatever the entry claims; the disk estimate saturates.
        assert_eq!(fetch_peak_bytes(1 << 30), (2 << 30) + (512 << 20));
        assert_eq!(fetch_peak_bytes(u64::MAX / 2), u64::MAX);
        assert_eq!(fetch_peak_bytes(u64::MAX), u64::MAX);
    }

    const AVATAR: &str = "cdn-avatars.huggingface.co/v1/production/uploads/6421/abcDEF_09.jpeg";

    #[test]
    fn an_avatar_url_is_found_when_the_window_ends_inside_a_character() {
        // The search window is 400 bytes from the match. Shift the text after
        // the URL one byte at a time so that offset lands on every byte of a
        // 2-, 3- and 4-byte character in turn; the release build aborts on a
        // panic, so slicing there would end the supervisor.
        for filler in ["é", "模", "🤗"] {
            let mut inside = 0;
            for pad in 0..filler.len() {
                let html = format!("<img src=\"https://{AVATAR}\"> {}{}", "x".repeat(pad), filler.repeat(200));
                let idx = html.find(AVATAR).unwrap();
                inside += usize::from(!html.is_char_boundary(idx + 400));
                assert_eq!(find_avatar_url(&html).as_deref(), Some(format!("https://{AVATAR}").as_str()), "{filler} {pad}");
            }
            assert_eq!(inside, filler.len() - 1, "{filler}: the offset never landed inside a character");
        }
        // With nothing to end the URL, the window ends at the last whole
        // character that fits in the 400 bytes: 31 bytes, then 184 of 'é'.
        let open = format!("cdn-avatars.huggingface.co/v1/x{}", "é".repeat(300));
        assert_eq!(find_avatar_url(&open), Some(format!("https://cdn-avatars.huggingface.co/v1/x{}", "é".repeat(184))));
    }

    #[test]
    fn an_avatar_url_on_an_ascii_page_is_unchanged() {
        let want = format!("https://{AVATAR}");
        let found = |html: String| find_avatar_url(&html);
        // An attribute, entity-encoded JSON, and a query string.
        assert_eq!(found(format!("<a><img src=\"https://{AVATAR}\"></a>")), Some(want.clone()));
        assert_eq!(found(format!("&quot;avatarUrl&quot;:&quot;https://{AVATAR}&quot;,&quot;x&quot;:1")), Some(want.clone()));
        assert_eq!(found(format!("\"https://{AVATAR}?w=200&h=200\"")), Some(want.clone()));
        // Slashes escaped the three ways JSON in a page escapes them.
        for slash in ["\\/", "\\u002F", "\\u002f"] {
            let escaped = AVATAR.replacen('/', "\u{0}", 1).replace('/', slash).replace('\u{0}', "/");
            assert_eq!(found(format!("\"https://{escaped}\"")), Some(want.clone()), "{slash}");
        }
        // Every terminator, and the end of the page.
        for end in ['"', '\'', ' ', '<', '>', ')', '\\', '\n', '?', '&'] {
            assert_eq!(found(format!("{AVATAR}{end}tail")), Some(want.clone()), "{end:?}");
        }
        assert_eq!(found(format!("see {AVATAR}")), Some(want.clone()));
        // The first one on the page wins.
        assert_eq!(found(format!("{AVATAR} cdn-avatars.huggingface.co/other.png")), Some(want));
        // Four characters after the host is the least that counts as a URL.
        assert_eq!(found("cdn-avatars.huggingface.co/abc\"".into()), None);
        assert_eq!(found("cdn-avatars.huggingface.co/abcd\"".into()).as_deref(), Some("https://cdn-avatars.huggingface.co/abcd"));
        assert_eq!(found("<html>no avatar here</html>".into()), None);
        // An unterminated run is still cut at 400 bytes from the match.
        let long = found(format!("x cdn-avatars.huggingface.co/{}", "a".repeat(1000))).unwrap();
        assert_eq!(long, format!("https://cdn-avatars.huggingface.co/{}", "a".repeat(400 - 27)));
    }

    #[test]
    fn the_other_offset_slices_take_non_ascii_text() {
        // parse_params slices from an ASCII digit to an ASCII suffix letter, and
        // parse_civitai_ref slices where an ASCII match ends. Both offsets are
        // character boundaries whatever text surrounds them.
        assert_eq!(parse_params("模型-7B-é"), "7B");
        assert_eq!(parse_params("é0.5b"), "0.5B");
        assert_eq!(parse_params("模7模 12é 3Bé"), "3B");
        assert_eq!(parse_params("é模🤗"), "");
        assert_eq!(parse_civitai_ref("https://civitai.com/models/123/模型?modelVersionId=456é"), Ok((Some(123), Some(456))));
        assert_eq!(parse_civitai_ref("é/api/download/models/789é"), Ok((None, Some(789))));
        assert!(parse_civitai_ref("/models/é").is_err());
        assert!(parse_civitai_ref("é/api/download/models/").is_err());
        assert_eq!(
            parse_hf("https://huggingface.co/org/model/blob/main/模型/é.gguf"),
            Ok(("org/model".to_string(), Some("模型/é.gguf".to_string())))
        );
        assert!(parse_hf("é/模").is_err());
    }
}

