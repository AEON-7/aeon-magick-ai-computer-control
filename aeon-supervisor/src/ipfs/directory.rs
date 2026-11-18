//! Model directories and the BitTorrent transport.
//!
//! A *directory* is any HTTPS service that answers the small search contract in
//! docs/MODEL_SHARE_PROTOCOL.md §9: it lists models with the exact files they
//! contain (path, size, SHA-256) and where to get them — a BitTorrent infohash,
//! an IPFS CID, or both. None is configured out of the box; the operator adds
//! the ones they trust.
//!
//! Pulling a listing over BitTorrent:
//!   1. the torrent's own file list must equal the listing's before a byte of
//!      content is downloaded;
//!   2. every file is copied out of the torrent folder through descriptors
//!      that never follow a link, hashed as it is copied, and held to the
//!      listing's size and SHA-256;
//!   3. the copy passes the same file-type gate and malware scan as an IPFS
//!      download;
//!   4. only then is it added to IPFS, announced, and left seeding on
//!      BitTorrent — both, or neither when seeding is switched off. A failed
//!      check removes the torrent and everything it downloaded.
//!
//! While a torrent downloads, the client trades pieces of that same torrent
//! with other peers; that is how BitTorrent works and it cannot be switched
//! off without stalling the download (an upload limit of zero also blocks the
//! request messages — observed on a Pi 5, transmission 4.1). Those pieces are
//! checked by the client against the torrent's own hashes.
//!
//! A directory is a claim about content, never a publisher identity: models
//! pulled this way are labelled as mirrors, not as signed origins.

use axum::extract::Query;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

const DIRS_JSON: &str = "/etc/aeon/model-directories.json";
const TORRENT_SCRIPT: &str = "/usr/local/bin/aeon-torrent";
/// Root-owned parent of the torrent service's folder: the first path component
/// the service's own user cannot replace.
const TORRENT_BASE: &str = "/var/lib/aeon/torrents";
const TORRENT_DATA: &str = "/var/lib/aeon/torrents/data";
const RPC_SECRET: &str = "/etc/aeon/torrent-rpc.secret";
const RPC_URL: &str = "http://127.0.0.1:9191/transmission/rpc";
const NETWORK_TOML: &str = "/etc/aeon/network.toml";

const MAX_DIRS: usize = 8;
const MAX_FILES: usize = 5000;
const MAX_RESPONSE: u64 = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 50;
/// Give up when no peer has supplied the torrent's metadata by then.
const FIND_SEEDERS: Duration = Duration::from_secs(10 * 60);
/// Give up when not one more byte has arrived for this long.
const STALL: Duration = Duration::from_secs(30 * 60);

// ── Directories ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Directory {
    pub id: String,
    pub name: String,
    pub url: String,
}

fn read_dirs() -> Vec<Directory> {
    std::fs::read_to_string(DIRS_JSON)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_dirs(dirs: &[Directory]) -> std::io::Result<()> {
    if let Some(p) = Path::new(DIRS_JSON).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{DIRS_JSON}.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(dirs).unwrap_or_default())?;
    std::fs::rename(tmp, DIRS_JSON)
}

/// Printable text, control characters dropped, cut to `max` characters.
fn clean_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// A directory address the supervisor is willing to call: HTTPS, or plain HTTP
/// to this machine only. No credentials, query or fragment — the address is
/// shown in the console and written to disk, so it must not carry a secret.
fn clean_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.len() > 512 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("that address is too long or contains spaces".into());
    }
    let (scheme, rest) = if let Some(r) = raw.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = raw.strip_prefix("http://") {
        ("http", r)
    } else {
        return Err("a directory address starts with https://".into());
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() || authority.contains('@') {
        return Err("the address must not contain a user name or password".into());
    }
    if !authority.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')) {
        return Err("the address has an invalid host name".into());
    }
    let host = if authority.starts_with('[') {
        authority.split(']').next().unwrap_or("").to_string() + "]"
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    if host.is_empty() || host == "[]" {
        return Err("the address has no host name".into());
    }
    if scheme == "http" && !is_loopback_host(&host) {
        return Err("plain http is only allowed for a directory on this Orb itself — use https".into());
    }
    if path.contains('?') || path.contains('#') || path.split('/').any(|s| s == "..") {
        return Err("the address must not contain a query, a fragment or “..”".into());
    }
    Ok(format!("{scheme}://{authority}{}", path.trim_end_matches('/')))
}

fn dir_id(url: &str) -> String {
    hex::encode(&Sha256::digest(url.as_bytes())[..6])
}

// ── Listings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// One model as a directory describes it, rebuilt field by field from the
/// response so nothing unexpected is carried along.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Listing {
    pub id: String,
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub license: String,
    pub source: String,
    pub files: Vec<ListFile>,
    pub btih: String,
    pub cid: String,
    pub trackers: Vec<String>,
    pub updated_ms: i64,
}

impl Listing {
    fn total(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_listing(v: &Value) -> Result<Listing, String> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let id = s("id");
    if id.is_empty()
        || id.len() > 128
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
    {
        return Err("listing has no usable id".into());
    }
    let name = clean_text(s("name"), 200);
    if name.is_empty() {
        return Err("listing has no name".into());
    }

    let raw_files = v.get("files").and_then(|f| f.as_array()).ok_or("listing has no file list")?;
    if raw_files.is_empty() || raw_files.len() > MAX_FILES {
        return Err(format!("a listing needs 1–{MAX_FILES} files"));
    }
    let mut files = Vec::with_capacity(raw_files.len());
    let mut seen: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    for f in raw_files {
        let path = f.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if path.len() > 1024 || !safe_rel_path(path) || path.contains(':') {
            return Err("a file path in the listing is not a plain relative path".into());
        }
        if path.split('/').any(|seg| seg.starts_with('.')) {
            return Err(format!("“{path}” is a hidden file — a model may not contain one"));
        }
        if !seen.insert(path.to_lowercase()) {
            return Err(format!("“{path}” is listed twice"));
        }
        let size = f.get("size").and_then(|x| x.as_u64()).ok_or("a file in the listing has no size")?;
        total = total.checked_add(size).ok_or("the listing is impossibly large")?;
        let sha = f.get("sha256").and_then(|x| x.as_str()).unwrap_or("");
        if !is_hex(sha, 64) {
            return Err(format!("“{path}” has no valid SHA-256 in the listing"));
        }
        files.push(ListFile { path: path.to_string(), size, sha256: sha.to_ascii_lowercase() });
    }
    // A name cannot be both a file and a folder.
    for key in &seen {
        let mut parts: Vec<&str> = key.split('/').collect();
        parts.pop();
        while !parts.is_empty() {
            if seen.contains(&parts.join("/")) {
                return Err("a file path and a folder path overlap in the listing".into());
            }
            parts.pop();
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let btih = s("btih").to_ascii_lowercase();
    if !btih.is_empty() && !is_hex(&btih, 40) {
        return Err("the listing's torrent hash is not a 40-character infohash".into());
    }
    let cid = s("cid").to_string();
    if !cid.is_empty() && !valid_cid(&cid) {
        return Err("the listing's CID is invalid".into());
    }
    if btih.is_empty() && cid.is_empty() {
        return Err("the listing names neither a torrent nor a CID".into());
    }

    let source = s("source");
    let source = if source.starts_with("https://")
        && source.len() <= 2048
        && !source.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '@' | '?' | '#'))
    {
        source.to_string()
    } else {
        String::new()
    };
    let trackers: Vec<String> = v
        .get("trackers")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str())
                .filter(|t| {
                    (t.starts_with("https://") || t.starts_with("http://") || t.starts_with("udp://"))
                        && t.len() <= 512
                        && !t.chars().any(|c| c.is_control() || c.is_whitespace() || c == '@')
                })
                .take(8)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    Ok(Listing {
        id: id.to_string(),
        name,
        description: clean_text(s("description"), 2000),
        publisher: clean_text(s("publisher"), 96),
        license: clean_text(s("license"), 100),
        source,
        files,
        btih,
        cid,
        trackers,
        updated_ms: v.get("updated_ms").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

fn magnet(l: &Listing) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{}&dn={}", l.btih, urlencoding::encode(&l.name));
    for t in &l.trackers {
        m.push_str("&tr=");
        m.push_str(&urlencoding::encode(t));
    }
    m
}

/// Does the torrent hold exactly the listed files? Returns the wrapper folder
/// every file sits under ("" when there is none), or None on any difference.
/// A multi-file torrent names each file "<torrent name>/<path>"; a single-file
/// torrent names its one file directly.
fn torrent_layout(listed: &[ListFile], torrent: &[(String, u64)]) -> Option<String> {
    let want: HashSet<(&str, u64)> = listed.iter().map(|f| (f.path.as_str(), f.size)).collect();
    if torrent.len() != want.len() {
        return None;
    }
    if torrent.iter().all(|(n, s)| want.contains(&(n.as_str(), *s))) {
        return Some(String::new());
    }
    let wrapper = torrent.first()?.0.split('/').next()?.to_string();
    if wrapper.is_empty() || wrapper == "." || wrapper == ".." || wrapper.chars().any(|c| c.is_control() || c == '\\') {
        return None;
    }
    let prefix = format!("{wrapper}/");
    let all = torrent.iter().all(|(n, s)| match n.strip_prefix(&prefix) {
        Some(rest) => want.contains(&(rest, *s)),
        None => false,
    });
    all.then_some(wrapper)
}

// ── Talking to a directory ───────────────────────────────────────────────────

fn dir_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout(Duration::from_secs(20))
        // A directory may not bounce the supervisor to some other address.
        .redirects(0)
        .user_agent("aeon-model-share/1")
        .build()
}

fn get_json(url: &str) -> Result<Value, String> {
    let resp = dir_agent().get(url).set("Accept", "application/json").call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the directory answered {code}"),
        other => format!("could not reach the directory: {other}"),
    })?;
    if resp.status() != 200 {
        return Err(format!("the directory answered {}", resp.status()));
    }
    use std::io::Read;
    let mut body = Vec::new();
    resp.into_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("reading the directory's answer: {e}"))?;
    if body.len() as u64 > MAX_RESPONSE {
        return Err("the directory's answer is too large".into());
    }
    serde_json::from_slice(&body).map_err(|_| "the directory did not answer with JSON".to_string())
}

static SEEN: OnceLock<Mutex<HashMap<String, Listing>>> = OnceLock::new();

fn remember(dir: &str, l: &Listing) {
    if let Ok(mut m) = SEEN.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        if m.len() > 400 {
            m.clear();
        }
        m.insert(format!("{dir}/{}", l.id), l.clone());
    }
}

fn recall(dir: &str, id: &str) -> Option<Listing> {
    SEEN.get()?.lock().ok()?.get(&format!("{dir}/{id}")).cloned()
}

/// What the console shows for one listing.
fn listing_view(l: &Listing, catalog: &[ModelEntry]) -> Value {
    let primary = l.files.iter().max_by_key(|f| f.size);
    let have = catalog.iter().any(|e| {
        (!l.btih.is_empty() && e.btih == l.btih) || primary.map(|p| p.sha256 == e.sha256).unwrap_or(false)
    });
    let blocked = if l.btih.is_empty() {
        "This listing has no torrent, so it cannot be pulled over BitTorrent."
    } else if l.files.iter().any(|f| f.path == CARD_FILE) {
        "This listing already carries an Orb model card. Pull it by its CID instead."
    } else {
        ""
    };
    json!({
        "id": l.id, "name": l.name, "description": l.description, "publisher": l.publisher,
        "license": l.license, "source": l.source, "btih": l.btih, "cid": l.cid,
        "trackers": l.trackers.len(), "updated_ms": l.updated_ms,
        "file_count": l.files.len(), "total_bytes": l.total(),
        "primary": primary.map(|p| p.path.clone()).unwrap_or_default(),
        "files": l.files.iter().take(12).collect::<Vec<_>>(),
        "have": have, "blocked": blocked,
    })
}

// ── The torrent service ──────────────────────────────────────────────────────

fn torrent_script(arg: &str) -> Result<String, String> {
    let out = Command::new(TORRENT_SCRIPT).arg(arg).output().map_err(|e| format!("spawn {TORRENT_SCRIPT}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.lines().last().unwrap_or("the torrent service could not start").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn torrent_service() -> Value {
    torrent_script("status")
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({"installed": false, "state": "inactive"}))
}

struct Rpc {
    auth: String,
    sid: String,
}

impl Rpc {
    fn open() -> Result<Rpc, String> {
        let secret = std::fs::read_to_string(RPC_SECRET).map_err(|_| "the torrent service is not set up yet".to_string())?;
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("aeon:{}", secret.trim()));
        Ok(Rpc { auth: format!("Basic {auth}"), sid: String::new() })
    }

    fn call(&mut self, method: &str, arguments: Value) -> Result<Value, String> {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).redirects(0).build();
        for _ in 0..2 {
            let sent = agent
                .post(RPC_URL)
                .set("Authorization", &self.auth)
                .set("X-Transmission-Session-Id", &self.sid)
                .send_json(json!({"method": method, "arguments": arguments}));
            match sent {
                Ok(resp) => {
                    let v: Value = resp.into_json().map_err(|e| format!("torrent service: {e}"))?;
                    return match v.get("result").and_then(|r| r.as_str()) {
                        Some("success") => Ok(v.get("arguments").cloned().unwrap_or_else(|| json!({}))),
                        Some(other) => Err(format!("torrent service: {other}")),
                        None => Err("torrent service: unreadable answer".into()),
                    };
                }
                // First contact: the service hands out a session id and expects it back.
                Err(ureq::Error::Status(409, resp)) => {
                    self.sid = resp.header("X-Transmission-Session-Id").unwrap_or("").to_string();
                }
                Err(ureq::Error::Status(401, _)) => return Err("the torrent service refused the supervisor's credentials".into()),
                Err(e) => return Err(format!("torrent service: {e}")),
            }
        }
        Err("torrent service: no session".into())
    }
}

/// Start the service if needed and wait until it answers.
fn torrent_up() -> Result<Rpc, String> {
    torrent_script("up")?;
    let mut rpc = Rpc::open()?;
    let start = Instant::now();
    loop {
        match rpc.call("session-get", json!({"fields": ["version"]})) {
            Ok(_) => return Ok(rpc),
            Err(e) if start.elapsed() > Duration::from_secs(25) => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Removes a torrent and its folder unless the pull finished and kept it.
struct TorrentGuard {
    btih: String,
    keep: bool,
}

impl Drop for TorrentGuard {
    fn drop(&mut self) {
        if !self.keep {
            torrent_forget(&self.btih);
        }
    }
}

/// Stop seeding one torrent and delete its folder. Called when a model is
/// removed from Model Share and when a pull fails or is not kept.
pub(super) fn torrent_forget(btih: &str) {
    if !is_hex(btih, 40) {
        return;
    }
    if let Ok(mut rpc) = Rpc::open() {
        let _ = rpc.call("torrent-remove", json!({"ids": [btih], "delete-local-data": true}));
    }
    let _ = std::fs::remove_dir_all(Path::new(TORRENT_DATA).join(btih));
}

/// Follow the seeding switch: off stops every torrent, on starts them again.
pub(super) fn torrents_seed(on: bool) {
    if let Ok(mut rpc) = Rpc::open() {
        let _ = rpc.call(if on { "torrent-start" } else { "torrent-stop" }, json!({}));
    }
}

// ── Safety checks before a pull ──────────────────────────────────────────────

/// BitTorrent is not run through Tor: it leaks the real address in its own
/// protocol messages and loads a network that is not built for it.
fn tor_guard() -> Result<(), String> {
    let transparent = std::fs::read_to_string(NETWORK_TOML)
        .ok()
        .and_then(|t| t.parse::<toml::Value>().ok())
        .and_then(|v| v.get("tor").cloned())
        .map(|tor| {
            tor.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false)
                && tor.get("mode").and_then(|m| m.as_str()) == Some("transparent")
        })
        .unwrap_or(false);
    if transparent {
        return Err("BitTorrent is switched off while all traffic is routed through Tor — change the Tor mode to pull this".into());
    }
    Ok(())
}

fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

/// The model is on disk three times while it is being added (the torrent's
/// copy, the checked copy, the IPFS blocks) and twice afterwards.
fn space_check(total: u64) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let existing = |p: &str| {
        let mut p = PathBuf::from(p);
        while !p.exists() {
            if !p.pop() {
                break;
            }
        }
        p
    };
    let places = [existing(TORRENT_BASE), existing(&staging_root().to_string_lossy()), existing("/var/lib/aeon/ipfs")];
    let mut per_dev: HashMap<u64, (u64, PathBuf)> = HashMap::new();
    for p in places {
        if let Ok(m) = std::fs::metadata(&p) {
            per_dev.entry(m.dev()).or_insert((0, p)).0 += 1;
        }
    }
    for (copies, path) in per_dev.values() {
        let need = total.saturating_mul(*copies).saturating_add(512 * 1024 * 1024);
        if let Some(free) = free_bytes(path) {
            if free < need {
                return Err(format!(
                    "not enough free space: this needs about {:.1} GB and {:.1} GB is free",
                    need as f64 / 1e9,
                    free as f64 / 1e9
                ));
            }
        }
    }
    Ok(())
}

// ── Copying out of the torrent folder ────────────────────────────────────────

/// Open `rel` under `base` for reading without following a symbolic link at any
/// step. The torrent folder belongs to an unprivileged service that handles
/// data from strangers; the supervisor runs as root and must not be steered
/// out of that folder by something planted in it.
fn open_nofollow(base: &Path, rel: &str) -> Result<std::fs::File, String> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let mut cur = std::fs::File::open(base).map_err(|e| format!("open {}: {e}", base.display()))?;
    let parts: Vec<&str> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let name = std::ffi::CString::new(*part).map_err(|_| "rejected: bad file name".to_string())?;
        let last = i + 1 == parts.len();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last { libc::O_NONBLOCK } else { libc::O_DIRECTORY };
        let fd = unsafe { libc::openat(cur.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(format!("rejected: “{rel}” could not be opened safely ({e})"));
        }
        cur = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let meta = cur.metadata().map_err(|e| format!("stat “{rel}”: {e}"))?;
    if !meta.is_file() {
        return Err(format!("rejected: “{rel}” is not a regular file"));
    }
    Ok(cur)
}

/// Copy every listed file from `base/prefix` into `staging`, hashing the bytes
/// as they are written, and hold each to the listing's size and SHA-256. On
/// success `staging` holds exactly the listed files and the returned map has
/// the hash of each — computed from the copy, which nothing else can change.
fn stage_verified(
    base: &Path,
    prefix: &str,
    files: &[ListFile],
    staging: &Path,
    mut progress: impl FnMut(u64),
) -> Result<HashMap<String, String>, String> {
    use std::io::{Read, Write};
    let mut known = HashMap::new();
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 1024 * 1024];
    for f in files {
        let rel = if prefix.is_empty() { f.path.clone() } else { format!("{prefix}/{}", f.path) };
        let mut src = open_nofollow(base, &rel)?;
        let dst_path = staging.join(&f.path);
        if let Some(parent) = dst_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dst_path)
            .map_err(|e| format!("create “{}”: {e}", f.path))?;
        let mut hasher = Sha256::new();
        let mut copied: u64 = 0;
        loop {
            let n = src.read(&mut buf).map_err(|e| format!("read “{}”: {e}", f.path))?;
            if n == 0 {
                break;
            }
            copied += n as u64;
            if copied > f.size {
                return Err(format!("rejected: “{}” is larger than the listing says", f.path));
            }
            hasher.update(&buf[..n]);
            dst.write_all(&buf[..n]).map_err(|e| format!("write “{}”: {e}", f.path))?;
            done += n as u64;
            progress(done);
        }
        if copied != f.size {
            return Err(format!("rejected: “{}” is {copied} bytes, the listing says {}", f.path, f.size));
        }
        let sha = hex::encode(hasher.finalize());
        if sha != f.sha256 {
            return Err(format!("rejected: “{}” does not match the listing's SHA-256", f.path));
        }
        known.insert(f.path.clone(), sha);
    }
    Ok(known)
}

// ── The pull ─────────────────────────────────────────────────────────────────

fn format_of(path: &str) -> &'static str {
    let low = path.to_ascii_lowercase();
    [".gguf", ".safetensors", ".onnx", ".hef"]
        .iter()
        .find(|e| low.ends_with(*e))
        .map(|e| &e[1..])
        .unwrap_or("")
}

fn do_pull(l: &Listing, key: &str) -> Result<(), String> {
    if l.btih.is_empty() {
        return Err("this listing has no torrent, so it cannot be pulled over BitTorrent".into());
    }
    if l.files.iter().any(|f| f.path == CARD_FILE) {
        return Err("this listing already carries an Orb model card — pull it by its CID instead".into());
    }
    if read_catalog().iter().any(|e| e.btih == l.btih) {
        return Err("this model is already in Model Share".into());
    }
    tor_guard()?;
    let total = l.total();
    space_check(total)?;

    task_set(key, "starting the torrent service…");
    let mut rpc = torrent_up()?;
    let ids = json!([l.btih]);
    let dest = format!("{TORRENT_DATA}/{}", l.btih);

    task_set(key, "looking for seeders…");
    rpc.call("torrent-add", json!({"filename": magnet(l), "download-dir": dest, "paused": false}))?;
    let mut guard = TorrentGuard { btih: l.btih.clone(), keep: false };

    let started = Instant::now();
    let mut moved = Instant::now();
    let mut last_done: u64 = 0;
    let mut wrapper: Option<String> = None;
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let mut fields = vec![
            "status", "error", "errorString", "metadataPercentComplete", "peersConnected", "sizeWhenDone", "leftUntilDone",
        ];
        if wrapper.is_none() {
            fields.push("files");
        }
        let got = rpc.call("torrent-get", json!({"ids": ids, "fields": fields}))?;
        let t = got
            .get("torrents")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .cloned()
            .ok_or("the torrent disappeared from the torrent service")?;
        // 3 = a local error (disk full, permissions). Tracker errors are not fatal.
        if t.get("error").and_then(|e| e.as_i64()) == Some(3) {
            return Err(format!("torrent error: {}", t.get("errorString").and_then(|s| s.as_str()).unwrap_or("unknown")));
        }
        let peers = t.get("peersConnected").and_then(|p| p.as_u64()).unwrap_or(0);
        if t.get("metadataPercentComplete").and_then(|m| m.as_f64()).unwrap_or(0.0) < 1.0 {
            if started.elapsed() > FIND_SEEDERS {
                return Err("nobody answered for this torrent within 10 minutes — it may have no seeders right now".into());
            }
            task_set(key, &format!("looking for seeders… {peers} connected"));
            continue;
        }
        if wrapper.is_none() {
            let files: Vec<(String, u64)> = t
                .get("files")
                .and_then(|f| f.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|f| Some((f.get("name")?.as_str()?.to_string(), f.get("length")?.as_u64()?)))
                        .collect()
                })
                .unwrap_or_default();
            match torrent_layout(&l.files, &files) {
                Some(w) => wrapper = Some(w),
                None => return Err("rejected: the torrent does not hold the files the directory listed".into()),
            }
            moved = Instant::now();
        }
        let size = t.get("sizeWhenDone").and_then(|s| s.as_u64()).unwrap_or(0);
        let left = t.get("leftUntilDone").and_then(|s| s.as_u64()).unwrap_or(size);
        let done = size.saturating_sub(left);
        task_progress(key, &format!("downloading over BitTorrent · {peers} peers"), done, total);
        if size > 0 && left == 0 {
            break;
        }
        if done > last_done {
            last_done = done;
            moved = Instant::now();
        } else if moved.elapsed() > STALL {
            return Err("the download stalled for 30 minutes — try again when more seeders are online".into());
        }
    }

    // The download is complete: stop the torrent. It stays stopped — seeding
    // nothing — until the files have passed every check below.
    let _ = rpc.call("torrent-stop", json!({"ids": ids}));
    std::thread::sleep(Duration::from_secs(1));

    let wrapper = wrapper.unwrap_or_default();
    let prefix = if wrapper.is_empty() { format!("data/{}", l.btih) } else { format!("data/{}/{wrapper}", l.btih) };
    let staging = StagingDir::new("dir")?;
    let phase = "checking every file against the listing…";
    task_set(key, phase);
    let known = stage_verified(Path::new(TORRENT_BASE), &prefix, &l.files, staging.path(), |done| {
        task_progress(key, phase, done, total)
    })?;

    task_set(key, "validating weights (file types)…");
    inspect_model_dir(staging.path())?;
    task_set(key, "scanning for viruses…");
    virus_scan(staging.path()).map_err(|e| format!("virus scan: {e}"))?;

    task_set(key, "adding to IPFS…");
    let primary = l.files.iter().max_by_key(|f| f.size).map(|f| f.path.clone()).unwrap_or_default();
    let card = ModelCard {
        kind: kind_from_pipeline("").to_string(),
        params: parse_params(&l.name),
        license: l.license.clone(),
        description: clean_text(&l.description, 500),
        format: format_of(&primary).to_string(),
        ..Default::default()
    };
    let spec = ShareSpec {
        name: l.name.clone(),
        file: primary,
        card,
        // Every file was held to the hashes the directory publishes.
        verified: true,
        source: l.source.clone(),
        // A mirror: the card carries nothing specific to this Orb, so every Orb
        // that pulls the same listing lands on the same CID and seeds one swarm.
        upstream: format!("directory:{}", if l.publisher.is_empty() { "unnamed" } else { &l.publisher }),
        identity: false,
    };
    let mut entry = finalize_share(staging.path(), spec, &known)?;
    entry.btih = l.btih.clone();

    if !seed_enabled() {
        // Seeding is off: keep a private copy, provide nothing on either network.
        let _ = ensure_in_library(&entry.cid, &model_slug(&l.name));
        let _ = run_script(&["unpin", &entry.cid]);
        return Ok(());
    }
    catalog_add(entry);
    guard.keep = true;
    rpc.call("torrent-start", json!({"ids": ids}))?;
    Ok(())
}

// ── HTTP handlers (all under /api/ipfs/, admin only) ─────────────────────────

/// GET /api/ipfs/directories — configured directories + torrent service state.
pub async fn list_directories(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| json!({"ok": true, "directories": read_dirs(), "torrent": torrent_service(), "seed": seed_enabled()}))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct AddDirReq {
    #[serde(default)]
    pub name: String,
    pub url: String,
}

/// POST /api/ipfs/directories — add a directory by address.
pub async fn add_directory(State(_s): State<AppState>, Json(req): Json<AddDirReq>) -> Json<Value> {
    let url = match clean_url(&req.url) {
        Ok(u) => u,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let mut dirs = read_dirs();
    if dirs.iter().any(|d| d.url == url) {
        return Json(json!({"ok": false, "err": "that directory is already added"}));
    }
    if dirs.len() >= MAX_DIRS {
        return Json(json!({"ok": false, "err": format!("at most {MAX_DIRS} directories — remove one first")}));
    }
    let host = url.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string();
    let name = clean_text(&req.name, 60);
    let dir = Directory { id: dir_id(&url), name: if name.is_empty() { host } else { name }, url };
    dirs.push(dir.clone());
    if let Err(e) = write_dirs(&dirs) {
        return Json(json!({"ok": false, "err": format!("save: {e}")}));
    }
    Json(json!({"ok": true, "directory": dir}))
}

#[derive(Deserialize)]
pub struct DirIdReq {
    pub id: String,
}

/// POST /api/ipfs/directories/remove — forget a directory. Models already
/// pulled from it stay.
pub async fn remove_directory(State(_s): State<AppState>, Json(req): Json<DirIdReq>) -> Json<Value> {
    let dirs: Vec<Directory> = read_dirs().into_iter().filter(|d| d.id != req.id).collect();
    match write_dirs(&dirs) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "err": format!("save: {e}")})),
    }
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub dir: String,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub cursor: String,
}

/// GET /api/ipfs/directory/search?dir=&q=&cursor= — search one directory.
pub async fn search(State(_s): State<AppState>, Query(req): Query<SearchQuery>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let Some(dir) = read_dirs().into_iter().find(|d| d.id == req.dir) else {
            return json!({"ok": false, "err": "unknown directory"});
        };
        let q = clean_text(&req.q, 80);
        let mut url = format!("{}/search?q={}&limit=24", dir.url, urlencoding::encode(&q));
        let cursor = clean_text(&req.cursor, 256);
        if !cursor.is_empty() {
            url.push_str(&format!("&cursor={}", urlencoding::encode(&cursor)));
        }
        let body = match get_json(&url) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let catalog = read_catalog();
        let mut items = Vec::new();
        let mut skipped = 0usize;
        for raw in body.get("items").and_then(|i| i.as_array()).map(|a| a.as_slice()).unwrap_or(&[]).iter().take(MAX_ITEMS) {
            match parse_listing(raw) {
                Ok(l) => {
                    remember(&dir.id, &l);
                    items.push(listing_view(&l, &catalog));
                }
                Err(_) => skipped += 1,
            }
        }
        let next = clean_text(body.get("cursor").and_then(|c| c.as_str()).unwrap_or(""), 256);
        json!({"ok": true, "items": items, "cursor": next, "skipped": skipped})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "search task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct PullDirReq {
    pub dir: String,
    pub id: String,
}

/// POST /api/ipfs/directory/pull — pull one listing over BitTorrent, check it,
/// then seed it on IPFS and BitTorrent. Runs in the background; progress shows
/// with the other Model Share tasks under the listing's name.
pub async fn pull(State(_s): State<AppState>, Json(req): Json<PullDirReq>) -> Json<Value> {
    if !read_config().enabled {
        return Json(json!({"ok": false, "err": "IPFS is off — enable it first"}));
    }
    let found = tokio::task::spawn_blocking(move || -> Result<Listing, String> {
        let dir = read_dirs().into_iter().find(|d| d.id == req.dir).ok_or("unknown directory")?;
        if let Some(l) = recall(&dir.id, &req.id) {
            return Ok(l);
        }
        if req.id.is_empty() || req.id.len() > 128 {
            return Err("unknown listing".into());
        }
        let l = parse_listing(&get_json(&format!("{}/listings/{}", dir.url, urlencoding::encode(&req.id)))?)?;
        if l.id != req.id {
            return Err("the directory answered with a different listing".into());
        }
        Ok(l)
    })
    .await
    .unwrap_or_else(|_| Err("lookup task failed".into()));
    let listing = match found {
        Ok(l) => l,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let key = listing.name.clone();
    let pulling = key.clone();
    task_set(&key, "preparing…");
    tokio::task::spawn_blocking(move || {
        if let Err(e) = do_pull(&listing, &key) {
            task_set(&key, &format!("error: {e}"));
        } else {
            task_clear(&key);
            announce();
        }
    });
    Json(json!({"ok": true, "pulling": pulling}))
}

/// GET /api/ipfs/torrents — the torrent service and what it is seeding.
pub async fn torrents(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let service = torrent_service();
        let mut list = Vec::new();
        if service.get("state").and_then(|s| s.as_str()) == Some("active") {
            if let Ok(mut rpc) = Rpc::open() {
                let fields = ["hashString", "name", "status", "percentDone", "rateDownload", "rateUpload", "peersConnected", "uploadedEver", "totalSize"];
                if let Ok(got) = rpc.call("torrent-get", json!({"fields": fields})) {
                    for t in got.get("torrents").and_then(|a| a.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
                        let state = match t.get("status").and_then(|s| s.as_i64()).unwrap_or(0) {
                            0 => "stopped",
                            1 | 2 => "checking",
                            3 | 4 => "downloading",
                            _ => "seeding",
                        };
                        list.push(json!({
                            "btih": t.get("hashString"), "name": t.get("name"), "state": state,
                            "percent": t.get("percentDone"), "down": t.get("rateDownload"), "up": t.get("rateUpload"),
                            "peers": t.get("peersConnected"), "uploaded": t.get("uploadedEver"), "size": t.get("totalSize"),
                        }));
                    }
                }
            }
        }
        json!({"ok": true, "service": service, "torrents": list})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "task failed"}));
    Json(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("aeon-dir-{tag}-{}-{}", std::process::id(), new_draft_id()));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn listing_json() -> Value {
        json!({
            "schema": "aeon-directory/1", "id": "acme.tiny-1", "name": "Tiny Model",
            "publisher": "acme", "license": "apache-2.0", "source": "https://example.org/acme/tiny",
            "btih": "0123456789ABCDEF0123456789abcdef01234567",
            "files": [
                {"path": "weights/model.gguf", "size": 5, "sha256": sha(b"hello")},
                {"path": "config.json", "size": 2, "sha256": sha(b"{}")},
            ],
            "trackers": ["https://tracker.example.org/announce", "ftp://nope", "udp://t.example.org:6969"],
            "extra": "dropped",
        })
    }

    #[test]
    fn a_directory_address_is_https_or_this_machine_only() {
        assert_eq!(clean_url(" https://dir.example.org/models/ ").unwrap(), "https://dir.example.org/models");
        assert_eq!(clean_url("http://127.0.0.1:8099").unwrap(), "http://127.0.0.1:8099");
        assert_eq!(clean_url("http://localhost:8099/x").unwrap(), "http://localhost:8099/x");
        assert_eq!(clean_url("http://[::1]:8099").unwrap(), "http://[::1]:8099");
        for bad in [
            "http://dir.example.org",          // plain http to another machine
            "http://192.168.1.10:8099",        // … including one on the LAN
            "https://user:pw@dir.example.org", // credentials
            "https://dir.example.org/a?token=1",
            "https://dir.example.org/a#frag",
            "https://dir.example.org/a/../b",
            "ftp://dir.example.org",
            "https://",
            "https://dir example.org",
        ] {
            assert!(clean_url(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn a_listing_is_rebuilt_from_its_own_fields() {
        let l = parse_listing(&listing_json()).unwrap();
        assert_eq!(l.btih, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(l.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["config.json", "weights/model.gguf"]);
        assert_eq!(l.total(), 7);
        // Only tracker URLs with a scheme BitTorrent uses survive.
        assert_eq!(l.trackers.len(), 2);
        assert_eq!(
            magnet(&l),
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Tiny%20Model\
             &tr=https%3A%2F%2Ftracker.example.org%2Fannounce&tr=udp%3A%2F%2Ft.example.org%3A6969"
        );
    }

    #[test]
    fn listings_that_could_write_outside_the_model_are_refused() {
        let with = |path: &str| {
            let mut v = listing_json();
            v["files"][0]["path"] = json!(path);
            parse_listing(&v)
        };
        for bad in ["../escape.gguf", "/etc/passwd", "a//b.gguf", "a/./b.gguf", ".ssh/authorized_keys", "weights/.hidden", "c:evil", "a\\b"] {
            assert!(with(bad).is_err(), "{bad} should be refused");
        }
        // The same name twice, differing only in case.
        assert!(with("CONFIG.json").is_err());
        // A path that is both a file and a folder.
        assert!(with("config.json/inner.bin").is_err());

        let mut no_hash = listing_json();
        no_hash["files"][0]["sha256"] = json!("abc");
        assert!(parse_listing(&no_hash).is_err());

        let mut nowhere = listing_json();
        nowhere["btih"] = json!("");
        assert!(parse_listing(&nowhere).is_err(), "a listing must say where to get the files");

        let mut bad_hash = listing_json();
        bad_hash["btih"] = json!("not-a-hash");
        assert!(parse_listing(&bad_hash).is_err());
    }

    #[test]
    fn the_torrent_must_hold_exactly_the_listed_files() {
        let l = parse_listing(&listing_json()).unwrap();
        let t = |names: &[(&str, u64)]| names.iter().map(|(n, s)| (n.to_string(), *s)).collect::<Vec<_>>();
        // Multi-file torrent: everything under one wrapper folder.
        assert_eq!(
            torrent_layout(&l.files, &t(&[("Tiny/weights/model.gguf", 5), ("Tiny/config.json", 2)])),
            Some("Tiny".to_string())
        );
        // No wrapper.
        assert_eq!(torrent_layout(&l.files, &t(&[("weights/model.gguf", 5), ("config.json", 2)])), Some(String::new()));
        // One file more, one file fewer, a different size, a second wrapper.
        assert_eq!(torrent_layout(&l.files, &t(&[("Tiny/weights/model.gguf", 5), ("Tiny/config.json", 2), ("Tiny/run.sh", 9)])), None);
        assert_eq!(torrent_layout(&l.files, &t(&[("Tiny/weights/model.gguf", 5)])), None);
        assert_eq!(torrent_layout(&l.files, &t(&[("Tiny/weights/model.gguf", 6), ("Tiny/config.json", 2)])), None);
        assert_eq!(torrent_layout(&l.files, &t(&[("Tiny/weights/model.gguf", 5), ("Other/config.json", 2)])), None);
    }

    fn torrent_folder(tag: &str) -> (Tmp, Vec<ListFile>) {
        let tmp = Tmp::new(tag);
        let root = tmp.0.join("data/abc/Tiny");
        std::fs::create_dir_all(root.join("weights")).unwrap();
        std::fs::write(root.join("weights/model.gguf"), b"hello").unwrap();
        std::fs::write(root.join("config.json"), b"{}").unwrap();
        (tmp, parse_listing(&listing_json()).unwrap().files)
    }

    #[test]
    fn matching_files_are_copied_and_their_hashes_returned() {
        let (tmp, files) = torrent_folder("ok");
        let staging = tmp.0.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let mut seen = 0;
        let known = stage_verified(&tmp.0, "data/abc/Tiny", &files, &staging, |d| seen = d).unwrap();
        assert_eq!(seen, 7);
        assert_eq!(known["weights/model.gguf"], sha(b"hello"));
        assert_eq!(std::fs::read(staging.join("weights/model.gguf")).unwrap(), b"hello");
        // The staged copy is exactly what the Orb's own manifest builder sees.
        let manifest = build_manifest(&staging, &known).unwrap();
        assert_eq!(manifest.len(), 2);
    }

    #[test]
    fn changed_truncated_or_padded_files_are_rejected() {
        for (tag, bytes) in [("changed", &b"hellp"[..]), ("short", &b"hell"[..]), ("long", &b"hello!"[..])] {
            let (tmp, files) = torrent_folder(tag);
            std::fs::write(tmp.0.join("data/abc/Tiny/weights/model.gguf"), bytes).unwrap();
            let staging = tmp.0.join("staging");
            std::fs::create_dir_all(&staging).unwrap();
            let err = stage_verified(&tmp.0, "data/abc/Tiny", &files, &staging, |_| {}).unwrap_err();
            assert!(err.starts_with("rejected:"), "{tag}: {err}");
        }
    }

    #[test]
    fn a_link_planted_in_the_torrent_folder_is_never_followed() {
        // The file itself replaced by a link to something outside.
        let (tmp, files) = torrent_folder("link-file");
        let secret = tmp.0.join("secret");
        std::fs::write(&secret, b"hello").unwrap();
        let target = tmp.0.join("data/abc/Tiny/weights/model.gguf");
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&secret, &target).unwrap();
        let staging = tmp.0.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(stage_verified(&tmp.0, "data/abc/Tiny", &files, &staging, |_| {}).is_err());

        // A folder on the way replaced by a link — the bytes behind it even match.
        let (tmp, files) = torrent_folder("link-dir");
        let elsewhere = tmp.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("model.gguf"), b"hello").unwrap();
        let weights = tmp.0.join("data/abc/Tiny/weights");
        std::fs::remove_dir_all(&weights).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &weights).unwrap();
        let staging = tmp.0.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(stage_verified(&tmp.0, "data/abc/Tiny", &files, &staging, |_| {}).is_err());

        // A missing file is an error, not a skip.
        let (tmp, files) = torrent_folder("missing");
        std::fs::remove_file(tmp.0.join("data/abc/Tiny/config.json")).unwrap();
        let staging = tmp.0.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(stage_verified(&tmp.0, "data/abc/Tiny", &files, &staging, |_| {}).is_err());
    }

    #[test]
    fn the_console_view_says_what_blocks_a_pull() {
        let l = parse_listing(&listing_json()).unwrap();
        let v = listing_view(&l, &[]);
        assert_eq!(v["blocked"], "");
        assert_eq!(v["have"], false);
        assert_eq!(v["total_bytes"], 7);

        let mut cid_only = listing_json();
        cid_only["btih"] = json!("");
        cid_only["cid"] = json!("bafybeigd2ezutbdgvafuhaxxtvdrhvuq5xgy6qo7okrnghj5qjaiczrb2i");
        let v = listing_view(&parse_listing(&cid_only).unwrap(), &[]);
        assert!(v["blocked"].as_str().unwrap().contains("no torrent"));

        let have = ModelEntry { btih: l.btih.clone(), ..Default::default() };
        assert_eq!(listing_view(&l, &[have])["have"], true);
    }
}
