//! Upstream hash records, witness statements, the local history and the
//! re-check (docs/MODEL_SHARE_PROTOCOL.md §10).
//!
//! When this Orb imports a model from a provider — HuggingFace, the Ollama
//! registry, Civitai — it pins the exact revision it read and keeps, for every
//! file, the SHA-256 the provider itself published. Where the provider
//! published none, the hash this Orb computed is kept instead and marked as
//! computed, never as the provider's. That is the *upstream record*. It rides
//! on the catalog entry and sits in model-card.json.
//!
//! A *witness* is the Orb key's signed statement of what it saw the provider
//! publish, and when (`aeon-witness/1`). It covers only the hashes the
//! provider published. Witnesses travel with the catalog entry, so every Orb
//! can count how many separate publishers recorded the same hashes — which is
//! what still stands if the provider later changes or removes the model. A
//! signature proves who made a statement, not when: another Orb's date is
//! always that Orb's claim.
//!
//! The *history* is this Orb's own append-only log, one JSON object per line,
//! each carrying the SHA-256 of the line before it. Nothing in it is edited or
//! deleted. A link that does not hold is reported and left as found. The chain
//! shows an edit, an insertion or a removal anywhere but at the very end; it
//! cannot stop someone with root on this Orb from rebuilding the whole file.
//!
//! The *re-check* asks the provider what it publishes now — at the pinned
//! revision and on its default branch — and appends the answer. It reads
//! metadata only and never downloads weights.

use axum::extract::Query;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use super::*;

pub(super) const HISTORY_FILE: &str = "/var/lib/aeon/ipfs-models/upstream-history.jsonl";
/// `{"auto": false}` switches the weekly background re-check off.
const RECHECK_JSON: &str = "/etc/aeon/modelshare-recheck.json";

pub const FROM_PROVIDER: &str = "provider";
pub const FROM_COMPUTED: &str = "computed";

const MAX_RECORD_FILES: usize = 512;
/// Witnesses one catalog entry carries.
pub(super) const MAX_WITNESSES: usize = 16;
/// One serialized catalog entry must stay under the gossip daemon's 16 KiB
/// limit or every peer drops the whole entry. Measured the way the daemon
/// measures it, with room to spare.
const ENTRY_BUDGET: usize = 15 * 1024;
/// A provider's metadata answer larger than this is not read.
const MAX_META_BYTES: u64 = 8 * 1024 * 1024;

// ── The record ───────────────────────────────────────────────────────────────

/// One file of an imported model as the provider described it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamFile {
    pub path: String, // inside the shared directory, '/'-separated
    pub size: u64,
    pub sha256: String, // lowercase hex
    /// "provider": the provider itself published this SHA-256 and the bytes
    /// matched it. "computed": the provider published none; the importing Orb
    /// hashed what it received.
    pub hash_from: String,
}

/// What a provider published for a model at one pinned revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamRecord {
    pub provider: String, // "huggingface" | "ollama" | "civitai"
    /// huggingface: "org/model". ollama: "namespace/model:tag". civitai: the model id.
    pub repo: String,
    /// huggingface: the commit id. ollama: the manifest digest ("sha256:…").
    /// civitai: the model-version id.
    pub revision: String,
    pub files: Vec<UpstreamFile>,
}

fn one_line(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(|c| c.is_control())
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl UpstreamRecord {
    /// The record with lowercase hashes and its files in path order, or None
    /// when it is not a well-formed record. Everything that enters this module
    /// from a file or another Orb goes through here.
    pub(super) fn cleaned(mut self) -> Option<Self> {
        let provider_ok = !self.provider.is_empty()
            && self.provider.len() <= 32
            && self.provider.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !provider_ok || !one_line(&self.repo, 200) || !one_line(&self.revision, 200) {
            return None;
        }
        if self.files.is_empty() || self.files.len() > MAX_RECORD_FILES {
            return None;
        }
        let mut seen = HashSet::new();
        for f in &mut self.files {
            if f.path.len() > 1024 || !safe_rel_path(&f.path) || !is_sha256(&f.sha256) {
                return None;
            }
            if f.hash_from != FROM_PROVIDER && f.hash_from != FROM_COMPUTED {
                return None;
            }
            f.sha256.make_ascii_lowercase();
            if !seen.insert(f.path.clone()) {
                return None;
            }
        }
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        Some(self)
    }

    /// The files whose SHA-256 the provider itself published — all a witness
    /// covers — as (path, size, sha256) in path order.
    pub(super) fn published(&self) -> Vec<(String, u64, String)> {
        self.files
            .iter()
            .filter(|f| f.hash_from == FROM_PROVIDER)
            .map(|f| (f.path.clone(), f.size, f.sha256.clone()))
            .collect()
    }

    pub(super) fn witness_bytes(&self, observed_ms: i64) -> Vec<u8> {
        crate::publisher::witness_digest(&self.provider, &self.repo, &self.revision, observed_ms, &self.published())
    }

    /// Equal for two records exactly when they name the same provider, the
    /// same repository and the same provider-published hashes. The revision is
    /// left out on purpose: a later commit that leaves the weights alone still
    /// publishes the same hashes.
    pub(super) fn hashes_key(&self) -> String {
        let mut s = format!("{}\n{}\n", self.provider, self.repo);
        for (path, size, sha256) in self.published() {
            s.push_str(&format!("{sha256} {size} {path}\n"));
        }
        sha256_hex(s.as_bytes())
    }

    /// Names this exact record — revision and every file — in the history.
    pub(super) fn id(&self) -> String {
        let mut s = format!("aeon-upstream-record/1\n{}\n{}\n{}\n", self.provider, self.repo, self.revision);
        for f in &self.files {
            s.push_str(&format!("{} {} {} {}\n", f.sha256, f.size, f.hash_from, f.path));
        }
        sha256_hex(s.as_bytes())
    }

    /// True when every recorded file is one of `files`, byte for byte.
    pub(super) fn within(&self, files: &[FileRec]) -> bool {
        self.files
            .iter()
            .all(|u| files.iter().any(|f| f.path == u.path && f.size == u.size && f.sha256.eq_ignore_ascii_case(&u.sha256)))
    }

    /// Where a person can look at this revision on the provider's own site.
    pub(super) fn link(&self) -> String {
        match self.provider.as_str() {
            "huggingface" if hf_repo_ok(&self.repo) && is_commit(&self.revision) => {
                format!("https://huggingface.co/{}/tree/{}", self.repo, self.revision)
            }
            "ollama" => match ollama_parts(&self.repo) {
                Some((ns, model, tag)) if ns == "library" => format!("https://ollama.com/library/{model}:{tag}"),
                Some((ns, model, tag)) => format!("https://ollama.com/{ns}/{model}:{tag}"),
                None => String::new(),
            },
            "civitai" if all_digits(&self.repo) && all_digits(&self.revision) => {
                format!("https://civitai.com/models/{}?modelVersionId={}", self.repo, self.revision)
            }
            _ => String::new(),
        }
    }
}

/// Build a record from what an import learned: each kept file with its size,
/// the hash computed while it streamed in, and whether the provider published
/// that hash. A file the provider gave no hash for is `computed`.
pub(super) fn record_from_import(
    provider: &str,
    repo: &str,
    revision: &str,
    files: impl IntoIterator<Item = (String, u64, String, bool)>,
) -> Option<UpstreamRecord> {
    UpstreamRecord {
        provider: provider.to_string(),
        repo: repo.to_string(),
        revision: revision.to_string(),
        files: files
            .into_iter()
            .map(|(path, size, sha256, published)| UpstreamFile {
                path,
                size,
                sha256,
                hash_from: if published { FROM_PROVIDER } else { FROM_COMPUTED }.to_string(),
            })
            .collect(),
    }
    .cleaned()
}

/// A record field that arrived from another Orb or an older file: anything
/// that is not a well-formed record reads as "no record" instead of failing
/// the entry it sits in.
pub(super) fn lenient_record<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<UpstreamRecord>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value::<UpstreamRecord>(v).ok().and_then(UpstreamRecord::cleaned))
}

/// As `lenient_record`: a malformed witness is skipped, the rest are kept.
pub(super) fn lenient_witnesses<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Witness>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(v.as_array()
        .map(|a| a.iter().take(MAX_WITNESSES * 4).filter_map(|w| serde_json::from_value(w.clone()).ok()).collect())
        .unwrap_or_default())
}

// ── Witnesses ────────────────────────────────────────────────────────────────

/// One key's signature over an entry's upstream record: "I saw the provider
/// publish these hashes for this repository at this revision, at
/// `observed_ms`". The record it signs is the one beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    pub key: String, // the signing Orb key, hex
    #[serde(default)]
    pub observed_ms: i64,
    pub sig: String, // base64 ed25519 over publisher::witness_digest
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation: Option<crate::publisher::Delegation>,
}

fn cached(cache: &'static OnceLock<Mutex<HashMap<String, String>>>, key: String, compute: impl FnOnce() -> String) -> String {
    let cache = cache.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let v = compute();
    if let Ok(mut c) = cache.lock() {
        if c.len() > 20_000 {
            c.clear();
        }
        c.insert(key, v.clone());
    }
    v
}

/// True when `w.key` really signed this record with this time. Cached: the
/// registry view re-reads every gossiped witness on each poll.
pub(super) fn witness_ok(rec: &UpstreamRecord, w: &Witness) -> bool {
    if w.key.len() != 64 || w.sig.is_empty() || w.sig.len() > 128 || w.observed_ms <= 0 {
        return false;
    }
    if rec.published().is_empty() {
        return false; // nothing the provider published: nothing to witness
    }
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let bytes = rec.witness_bytes(w.observed_ms);
    let key = format!("{}|{}|{}", w.key, w.sig, sha256_hex(&bytes));
    cached(&CACHE, key, || crate::publisher::verify(&w.key, &bytes, &w.sig).to_string()) == "true"
}

/// The identity a witness is credited to: the delegating user key when the
/// signer carries a valid delegation, else the signing key. Cached.
pub(super) fn witness_identity(w: &Witness) -> String {
    let Some(d) = &w.delegation else { return w.key.clone() };
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let key = format!("{}|{}|{}|{}|{}", w.key, d.user_key, d.orb_key, d.issued_ms, d.sig);
    cached(&CACHE, key, || crate::publisher::identity_of(&w.key, Some(d)))
}

/// The witnesses among `ws` whose signature holds for `rec`: one per signing
/// key (the earliest it made), this Orb's own first, then by the time each
/// claims, at most `MAX_WITNESSES`. Everything else is dropped.
pub(super) fn valid_witnesses(rec: &UpstreamRecord, ws: &[Witness]) -> Vec<Witness> {
    let me = crate::publisher::orb_pubkey();
    let mut by_key: BTreeMap<String, Witness> = BTreeMap::new();
    for w in ws.iter().filter(|w| witness_ok(rec, w)) {
        match by_key.get(&w.key) {
            Some(have) if have.observed_ms <= w.observed_ms => {}
            _ => {
                by_key.insert(w.key.clone(), w.clone());
            }
        }
    }
    let mut out: Vec<Witness> = by_key.into_values().collect();
    out.sort_by(|a, b| (a.key != me, a.observed_ms, &a.key).cmp(&(b.key != me, b.observed_ms, &b.key)));
    out.truncate(MAX_WITNESSES);
    out
}

/// This Orb's own witness for `rec`, as seen at `observed_ms`. None when the
/// provider published no hash at all (there is nothing to witness) or the Orb
/// has no key. `identity` attaches the signed-in publisher identity.
pub(super) fn sign_witness(rec: &UpstreamRecord, observed_ms: i64, identity: bool) -> Option<Witness> {
    if rec.published().is_empty() || observed_ms <= 0 {
        return None;
    }
    let s = crate::publisher::sign_as_orb_with(&rec.witness_bytes(observed_ms), identity).ok()?;
    Some(Witness { key: s.pubkey, observed_ms, sig: s.sig, delegation: s.delegation })
}

/// Hold an entry's record and witnesses to what can be checked without the
/// files: the record must name the entry's own primary file with the hash the
/// entry was signed with, and every witness signature must hold for it.
pub(super) fn scrub(entry: &mut ModelEntry) {
    match checked(entry).map(|(_, witnesses)| witnesses) {
        Some(witnesses) => entry.witnesses = witnesses,
        None => {
            entry.upstream_record = None;
            entry.witnesses.clear();
        }
    }
}

/// An entry's record, if it names the entry's own primary file with the hash
/// the entry was signed with, and the witnesses whose signatures hold for it.
fn checked(entry: &ModelEntry) -> Option<(&UpstreamRecord, Vec<Witness>)> {
    let rec = entry.upstream_record.as_ref()?;
    let consistent = entry.file.is_empty()
        || rec.files.iter().any(|f| f.path == entry.file && f.sha256.eq_ignore_ascii_case(&entry.sha256));
    consistent.then(|| (rec, valid_witnesses(rec, &entry.witnesses)))
}

/// How long the gossip daemon will find this entry: Python's `json.dumps`
/// puts a space after every separator and escapes every non-ASCII character.
/// Counts separators inside strings too, so it never comes out short.
fn gossip_len(entry: &ModelEntry) -> usize {
    let s = serde_json::to_string(entry).unwrap_or_default();
    let separators = s.bytes().filter(|b| *b == b',' || *b == b':').count();
    let escapes: usize = s
        .chars()
        .filter(|c| !c.is_ascii())
        .map(|c| if (c as u32) > 0xFFFF { 12 } else { 6 } - c.len_utf8())
        .sum();
    s.len() + separators + escapes
}

/// Keep an entry inside the gossip size limit. Other publishers' witnesses go
/// first, then this Orb's, then the record itself — which still stands in
/// model-card.json and in the history. An entry that is too large for some
/// other reason is left alone.
pub(super) fn fit_for_gossip(entry: &mut ModelEntry) {
    if entry.upstream_record.is_none() && entry.witnesses.is_empty() {
        return;
    }
    while gossip_len(entry) > ENTRY_BUDGET && entry.witnesses.len() > 1 {
        entry.witnesses.pop();
    }
    if gossip_len(entry) > ENTRY_BUDGET {
        entry.witnesses.clear();
    }
    if gossip_len(entry) > ENTRY_BUDGET {
        entry.upstream_record = None;
    }
}

/// Every verified witness this Orb can see, grouped so one model's record can
/// be compared with everyone else's.
pub(super) struct WitnessIndex {
    local: HashSet<String>,
    /// hashes_key → credited identity → (earliest time it claims, its name)
    same: HashMap<String, BTreeMap<String, (i64, String)>>,
    /// "provider\nrepo\nrevision" → hashes_key → (the hashes, who signed them)
    at_revision: HashMap<String, HashMap<String, (Vec<(String, u64, String)>, HashSet<String>)>>,
}

impl WitnessIndex {
    pub(super) fn new() -> Self {
        Self {
            local: crate::publisher::local_keys().into_iter().collect(),
            same: HashMap::new(),
            at_revision: HashMap::new(),
        }
    }

    /// Take in one entry's witnesses, held to the same rules as `scrub`.
    pub(super) fn add(&mut self, entry: &ModelEntry) {
        let Some((rec, witnesses)) = checked(entry) else { return };
        if witnesses.is_empty() {
            return;
        }
        let hk = rec.hashes_key();
        let rev = format!("{}\n{}\n{}", rec.provider, rec.repo, rec.revision);
        for w in &witnesses {
            let identity = witness_identity(w);
            let name = match &w.delegation {
                Some(d) if identity != w.key => d.user_name.chars().filter(|c| !c.is_control()).take(40).collect(),
                _ => String::new(),
            };
            let seen = self.same.entry(hk.clone()).or_default().entry(identity.clone()).or_insert((w.observed_ms, name.clone()));
            if w.observed_ms < seen.0 {
                *seen = (w.observed_ms, name);
            }
            self.at_revision
                .entry(rev.clone())
                .or_default()
                .entry(hk.clone())
                .or_insert_with(|| (rec.published(), HashSet::new()))
                .1
                .insert(identity);
        }
    }

    /// Who recorded the same provider-published hashes as `rec`, and whether
    /// anyone signed different hashes for the same revision.
    pub(super) fn tally(&self, rec: &UpstreamRecord) -> Value {
        let hk = rec.hashes_key();
        let empty = BTreeMap::new();
        let same = self.same.get(&hk).unwrap_or(&empty);
        let mut list: Vec<Value> = same
            .iter()
            .map(|(id, (ms, name))| {
                json!({"key": id, "fp": id.get(..12).unwrap_or(id), "name": name, "observed_ms": ms, "mine": self.local.contains(id)})
            })
            .collect();
        list.sort_by_key(|w| (w["observed_ms"].as_i64().unwrap_or(0), w["key"].as_str().unwrap_or("").to_string()));
        let others: Vec<i64> = same.iter().filter(|(id, _)| !self.local.contains(*id)).map(|(_, (ms, _))| *ms).collect();

        // Same provider, repository and revision, but a different hash or size
        // for a file both name. A pinned revision cannot honestly have two.
        let mine = rec.published();
        let mut differing: HashSet<&String> = HashSet::new();
        let rev = format!("{}\n{}\n{}", rec.provider, rec.repo, rec.revision);
        for (other_hk, (theirs, who)) in self.at_revision.get(&rev).into_iter().flatten() {
            if *other_hk == hk {
                continue;
            }
            let conflict = theirs
                .iter()
                .any(|(p, s, h)| mine.iter().any(|(mp, ms, mh)| mp == p && (ms != s || mh != h)));
            if conflict {
                differing.extend(who.iter());
            }
        }
        list.truncate(32);
        json!({
            "identities": same.len(),
            "others": others.len(),
            "mine": same.keys().any(|id| self.local.contains(id)),
            "earliest_ms": same.values().map(|(ms, _)| *ms).min().unwrap_or(0),
            "earliest_other_ms": others.iter().min().copied().unwrap_or(0),
            "differing": differing.len(),
            "list": list,
        })
    }
}

// ── The history: an append-only chain ────────────────────────────────────────

/// What changed for one file, in a `changed` or `removed` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub path: String,
    pub scope: String, // "pinned" | "head"
    /// "changed": a different hash or size. "gone": no longer listed.
    /// "unpublished": the provider no longer publishes a SHA-256 for it.
    pub what: String,
    pub was: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub now: String,
}

/// One line of the history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// SHA-256 of the line before this one (64 zeros on the first line).
    pub prev: String,
    pub at_ms: i64,
    /// recorded | confirmed | changed | removed | unreachable
    pub event: String,
    pub cid: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `UpstreamRecord::id` of the record this line is about.
    pub record: String,
    pub provider: String,
    pub repo: String,
    pub revision: String,
    /// `recorded` only: when the provider's metadata was read, and every file.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub observed_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<UpstreamFile>,
    /// Re-checks: the verdict at the pinned revision and on the default branch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pinned: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head_revision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff: Vec<Diff>,
    /// import | operator | timer
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

const CHAIN_START: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The history as it is on disk.
#[derive(Debug, Default)]
pub(super) struct Chain {
    /// (1-based line number, event) for every line that parses.
    pub events: Vec<(usize, Event)>,
    pub lines: usize,
    /// Lines that do not follow from the line before them, or cannot be read.
    pub broken: Vec<usize>,
}

/// The lines of the history file. A last line with no newline (a write cut
/// short) still counts as a line.
fn chain_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Read the history and check every link. Never changes the file.
pub(super) fn read_chain(path: &Path) -> Chain {
    let bytes = std::fs::read(path).unwrap_or_default();
    let lines = chain_lines(&bytes);
    let mut chain = Chain { lines: lines.len(), ..Default::default() };
    let mut expected = CHAIN_START.to_string();
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_slice::<Event>(line) {
            Ok(ev) => {
                if ev.prev != expected {
                    chain.broken.push(i + 1);
                }
                chain.events.push((i + 1, ev));
            }
            Err(_) => chain.broken.push(i + 1),
        }
        expected = sha256_hex(line);
    }
    chain
}

fn history_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Append one event, linked to whatever line is last in the file right now —
/// including a damaged one, which is left exactly as it is. Returns the event
/// as written.
pub(super) fn append_event(path: &Path, mut ev: Event) -> std::io::Result<Event> {
    use std::io::Write;
    let _g = history_lock().lock();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    ev.prev = chain_lines(&existing).last().map(|l| sha256_hex(l)).unwrap_or_else(|| CHAIN_START.to_string());
    let mut out = Vec::new();
    // A write that was cut short left no newline: end that line where it
    // stops, so it stays a (broken) line of its own.
    if existing.last().is_some_and(|b| *b != b'\n') {
        out.push(b'\n');
    }
    out.extend_from_slice(serde_json::to_string(&ev).map_err(std::io::Error::other)?.as_bytes());
    out.push(b'\n');
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(&out)?;
    f.sync_data()?;
    Ok(ev)
}

/// The line an import writes: this Orb read the provider's metadata and kept
/// these hashes.
pub(super) fn recorded_event(cid: &str, name: &str, rec: &UpstreamRecord, observed_ms: i64) -> Event {
    Event {
        at_ms: epoch_ms(),
        event: "recorded".into(),
        cid: cid.to_string(),
        name: name.to_string(),
        record: rec.id(),
        provider: rec.provider.clone(),
        repo: rec.repo.clone(),
        revision: rec.revision.clone(),
        observed_ms,
        files: rec.files.clone(),
        by: "import".into(),
        ..Default::default()
    }
}

/// Called once an import has its CID. Failing to write the history does not
/// undo the import; it is logged.
pub(super) fn note_recorded(entry: &ModelEntry) {
    let Some(rec) = &entry.upstream_record else { return };
    let me = crate::publisher::orb_pubkey();
    let observed = entry.witnesses.iter().find(|w| w.key == me).map(|w| w.observed_ms).unwrap_or_else(epoch_ms);
    if let Err(e) = append_event(Path::new(HISTORY_FILE), recorded_event(&entry.cid, &entry.name, rec, observed)) {
        eprintln!("model share: could not write the upstream history: {e}");
    }
}

// ── Asking the provider ──────────────────────────────────────────────────────

/// One metadata request. Tests replace the function that answers it.
pub(super) struct Req {
    pub method: &'static str, // GET | HEAD | POST
    pub url: String,
    pub accept: &'static str,
    pub body: Option<Value>,
    pub auth: Option<String>,
}

pub(super) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Answers a `Req`: `Ok` for any HTTP answer, `Err` when there was none.
pub(super) type Fetch<'a> = &'a (dyn Fn(&Req) -> Result<Reply, String> + Sync);

const JSON: &str = "application/json";
const OLLAMA_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The real `Fetch`. Reads at most `MAX_META_BYTES` of any answer and nothing
/// at all of a HEAD's, so it cannot be turned into a weights download.
pub(super) fn http_fetch(req: &Req) -> Result<Reply, String> {
    use std::io::Read;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(45))
        .redirects(5)
        .user_agent("aeon-magick-orb/modelshare")
        .build();
    let mut r = agent.request(req.method, &req.url).set("Accept", req.accept);
    if let Some(t) = &req.auth {
        r = r.set("Authorization", &format!("Bearer {t}"));
    }
    let sent = match &req.body {
        Some(b) => r.send_json(b.clone()),
        None => r.call(),
    };
    let resp = match sent {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Ok(Reply { status: code, body: Vec::new() }),
        Err(e) => return Err(redact(&e.to_string(), req.auth.as_deref())),
    };
    let status = resp.status();
    let mut body = Vec::new();
    if req.method != "HEAD" {
        resp.into_reader()
            .take(MAX_META_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| format!("reading the provider's answer: {e}"))?;
        if body.len() as u64 > MAX_META_BYTES {
            return Err("the provider's answer is too large to be metadata".into());
        }
    }
    Ok(Reply { status, body })
}

/// What a re-check concluded, from least to most telling. A re-check as a
/// whole reports the most telling verdict of its two scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Verdict {
    /// The provider still publishes what was recorded.
    Confirmed,
    /// No usable answer: a network error, a timeout, a server error, a rate
    /// limit. Evidence of nothing.
    Unreachable,
    /// The repository, the revision or a file is gone, or is now gated.
    Removed,
    /// The provider publishes a different hash for a path that was recorded.
    Changed,
}

impl Verdict {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Verdict::Confirmed => "confirmed",
            Verdict::Unreachable => "unreachable",
            Verdict::Removed => "removed",
            Verdict::Changed => "changed",
        }
    }
}

enum Lookup<T> {
    Found(T),
    Gone(String),
    Unreachable(String),
}

/// What an HTTP status says about the thing asked for. Only a clear "it is not
/// here" or "you may not see it" counts as gone; everything else that is not
/// a success says nothing about the model.
fn ask(fetch: Fetch, req: Req) -> Lookup<Vec<u8>> {
    match fetch(&req) {
        Err(e) => Lookup::Unreachable(e.chars().take(200).collect()),
        Ok(r) => match r.status {
            200..=299 => Lookup::Found(r.body),
            401 | 403 => Lookup::Gone("access is now refused: it is gated, private or gone".into()),
            404 | 410 => Lookup::Gone("it is no longer listed".into()),
            451 => Lookup::Gone("it has been taken down".into()),
            s => Lookup::Unreachable(format!("the provider answered {s}")),
        },
    }
}

fn ask_json(fetch: Fetch, req: Req) -> Lookup<Value> {
    match ask(fetch, req) {
        Lookup::Found(b) => match serde_json::from_slice(&b) {
            Ok(v) => Lookup::Found(v),
            Err(_) => Lookup::Unreachable("the provider's answer could not be read".into()),
        },
        Lookup::Gone(w) => Lookup::Gone(w),
        Lookup::Unreachable(w) => Lookup::Unreachable(w),
    }
}

fn get(url: String, accept: &'static str, auth: Option<&str>) -> Req {
    Req { method: "GET", url, accept, body: None, auth: auth.map(String::from) }
}

/// The outcome for one scope: the pinned revision, or the default branch.
#[derive(Debug, Clone)]
pub(super) struct ScopeReport {
    pub verdict: Verdict,
    pub detail: String,
    /// What the provider calls the revision that was looked at.
    pub revision: String,
    pub diff: Vec<Diff>,
}

impl ScopeReport {
    fn new(verdict: Verdict, detail: impl Into<String>, revision: &str) -> Self {
        Self { verdict, detail: detail.into(), revision: revision.to_string(), diff: Vec::new() }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CheckReport {
    pub pinned: ScopeReport,
    pub head: ScopeReport,
}

impl CheckReport {
    fn both(verdict: Verdict, detail: &str) -> Self {
        Self { pinned: ScopeReport::new(verdict, detail, ""), head: ScopeReport::new(verdict, detail, "") }
    }

    pub(super) fn verdict(&self) -> Verdict {
        self.pinned.verdict.max(self.head.verdict)
    }
}

/// Compare the record with what the provider lists now: per recorded path, the
/// size it gives (None = not comparable) and the SHA-256 it publishes (None =
/// it publishes none). A path missing from `now` is gone.
fn compare(rec: &UpstreamRecord, now: &HashMap<String, (Option<u64>, Option<String>)>, scope: &str) -> ScopeReport {
    let (mut same_hash, mut unhashed) = (0usize, 0usize);
    let mut diff: Vec<Diff> = Vec::new();
    let mut note = |f: &UpstreamFile, what: &str, now: &str| {
        diff.push(Diff { path: f.path.clone(), scope: scope.into(), what: what.into(), was: f.sha256.clone(), now: now.into() });
    };
    for f in &rec.files {
        match now.get(&f.path) {
            None => note(f, "gone", ""),
            Some((_, Some(h))) if h.eq_ignore_ascii_case(&f.sha256) => same_hash += 1,
            Some((_, Some(h))) => note(f, "changed", &h.to_ascii_lowercase()),
            // It published a hash when this was recorded and publishes none now.
            Some((_, None)) if f.hash_from == FROM_PROVIDER => note(f, "unpublished", ""),
            Some((Some(size), None)) if *size != f.size => note(f, "changed", ""),
            Some((_, None)) => unhashed += 1,
        }
    }
    let first = |what: &dyn Fn(&Diff) -> bool| diff.iter().find(|d| what(d)).map(|d| d.path.clone()).unwrap_or_default();
    let more = |n: usize| if n > 1 { format!(" (and {} more)", n - 1) } else { String::new() };
    let altered = diff.iter().filter(|d| d.what != "gone").count();
    let (verdict, detail) = if altered > 0 {
        (Verdict::Changed, format!("“{}” is published with a different hash now{}", first(&|d| d.what != "gone"), more(altered)))
    } else if !diff.is_empty() {
        (Verdict::Removed, format!("“{}” is no longer listed{}", first(&|_| true), more(diff.len())))
    } else if same_hash == 0 {
        (Verdict::Confirmed, format!("the provider publishes no SHA-256 for these files; all {unhashed} are still listed"))
    } else if unhashed == 0 {
        (Verdict::Confirmed, format!("all {same_hash} published SHA-256 hashes are the same"))
    } else {
        (Verdict::Confirmed, format!("all {same_hash} published SHA-256 hashes are the same; {unhashed} more files have no published hash and are still listed"))
    };
    diff.truncate(20);
    ScopeReport { verdict, detail, revision: String::new(), diff }
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn hf_repo_ok(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    parts.len() == 2 && parts.iter().all(|p| name_ok(p))
}

fn is_commit(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// "namespace/model:tag" → its three parts.
fn ollama_parts(repo: &str) -> Option<(&str, &str, &str)> {
    let (path, tag) = repo.rsplit_once(':')?;
    let (ns, model) = path.split_once('/')?;
    (name_ok(ns) && name_ok(model) && name_ok(tag)).then_some((ns, model, tag))
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

const HF_API: &str = "https://huggingface.co/api/models";

/// What HuggingFace lists for the recorded paths at one revision.
fn hf_scope(rec: &UpstreamRecord, repo: &str, revision: &str, scope: &str, token: Option<&str>, fetch: Fetch) -> ScopeReport {
    let mut now: HashMap<String, (Option<u64>, Option<String>)> = HashMap::new();
    for chunk in rec.files.chunks(200) {
        let paths: Vec<&str> = chunk.iter().map(|f| f.path.as_str()).collect();
        let req = Req {
            method: "POST",
            url: format!("{HF_API}/{repo}/paths-info/{revision}"),
            accept: JSON,
            body: Some(json!({"paths": paths, "expand": false})),
            auth: token.map(String::from),
        };
        let listed = match ask_json(fetch, req) {
            Lookup::Found(v) => v,
            Lookup::Gone(why) => return ScopeReport::new(Verdict::Removed, why, revision),
            Lookup::Unreachable(why) => return ScopeReport::new(Verdict::Unreachable, why, revision),
        };
        let Some(items) = listed.as_array() else {
            return ScopeReport::new(Verdict::Unreachable, "the provider's answer could not be read", revision);
        };
        for item in items.iter().filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("file")) {
            let Some(path) = item.get("path").and_then(|p| p.as_str()) else { continue };
            let size = item.get("size").and_then(|s| s.as_u64());
            // The LFS object id IS the file's SHA-256. A file kept in git
            // itself has only a git blob id, which is not a SHA-256.
            let sha = item
                .get("lfs")
                .and_then(|l| l.get("oid"))
                .and_then(|o| o.as_str())
                .filter(|o| is_sha256(o))
                .map(|o| o.to_ascii_lowercase());
            now.insert(path.to_string(), (size, sha));
        }
    }
    let mut report = compare(rec, &now, scope);
    report.revision = revision.to_string();
    // An unknown revision answers with an empty list, exactly as a revision
    // holding none of these files would. Ask which it is.
    if now.is_empty() {
        if let Lookup::Gone(_) = ask_json(fetch, get(format!("{HF_API}/{repo}/revision/{revision}"), JSON, token)) {
            report.detail = "the provider no longer serves that revision".into();
        }
    }
    report
}

fn check_hf(rec: &UpstreamRecord, token: Option<&str>, fetch: Fetch) -> Result<CheckReport, String> {
    if !hf_repo_ok(&rec.repo) || !is_commit(&rec.revision) {
        return Err("this record does not name a HuggingFace repository and commit".into());
    }
    let meta = match ask_json(fetch, get(format!("{HF_API}/{}", rec.repo), JSON, token)) {
        Lookup::Found(v) => v,
        Lookup::Gone(why) => return Ok(CheckReport::both(Verdict::Removed, &format!("the repository: {why}"))),
        Lookup::Unreachable(why) => return Ok(CheckReport::both(Verdict::Unreachable, &why)),
    };
    if hf_is_gated(&meta) || meta.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false) {
        return Ok(CheckReport::both(Verdict::Removed, "the repository is now gated, private or disabled"));
    }
    // A renamed repository answers under its new name.
    let repo_now = meta.get("id").and_then(|i| i.as_str()).filter(|i| hf_repo_ok(i)).unwrap_or(&rec.repo).to_string();
    let head_rev = meta.get("sha").and_then(|s| s.as_str()).filter(|s| is_commit(s)).unwrap_or("").to_string();

    let mut pinned = hf_scope(rec, &repo_now, &rec.revision, "pinned", token, fetch);
    let mut head = if head_rev.is_empty() {
        ScopeReport::new(Verdict::Unreachable, "the provider did not say which revision its default branch is at", "")
    } else if head_rev == rec.revision {
        // The default branch has not moved: one answer serves both scopes.
        let mut same = pinned.clone();
        for d in &mut same.diff {
            d.scope = "head".into();
        }
        same
    } else {
        hf_scope(rec, &repo_now, &head_rev, "head", token, fetch)
    };
    if repo_now != rec.repo {
        let renamed = format!("; the repository is now named {repo_now}");
        pinned.detail.push_str(&renamed);
        head.detail.push_str(&renamed);
    }
    Ok(CheckReport { pinned, head })
}

fn check_ollama(rec: &UpstreamRecord, token: Option<&str>, fetch: Fetch) -> Result<CheckReport, String> {
    let (ns, model, tag) = ollama_parts(&rec.repo).ok_or("this record does not name an Ollama model and tag")?;
    if !rec.revision.strip_prefix("sha256:").is_some_and(is_sha256) {
        return Err("this record does not name an Ollama manifest digest".into());
    }
    let base = format!("https://registry.ollama.ai/v2/{ns}/{model}");

    // Pinned: a blob is addressed by its own SHA-256, so the registry either
    // still serves each recorded digest or it does not. (The registry does not
    // serve a manifest by its digest, only by tag.) HEAD: no bytes are read.
    let mut now: HashMap<String, (Option<u64>, Option<String>)> = HashMap::new();
    let mut unreachable = None;
    for f in &rec.files {
        if f.hash_from != FROM_PROVIDER {
            now.insert(f.path.clone(), (None, None));
            continue;
        }
        let req = Req {
            method: "HEAD",
            url: format!("{base}/blobs/sha256:{}", f.sha256),
            accept: "*/*",
            body: None,
            auth: token.map(String::from),
        };
        match ask(fetch, req) {
            Lookup::Found(_) => {
                now.insert(f.path.clone(), (None, Some(f.sha256.clone())));
            }
            Lookup::Gone(_) => {}
            Lookup::Unreachable(why) => {
                unreachable = Some(why);
                break;
            }
        }
    }
    let mut pinned = match unreachable {
        Some(why) => ScopeReport::new(Verdict::Unreachable, why, ""),
        None => compare(rec, &now, "pinned"),
    };
    pinned.revision = rec.revision.clone();

    // Head: what the tag points to now. A tag can be moved.
    let head = match ask(fetch, get(format!("{base}/manifests/{tag}"), OLLAMA_MANIFEST, token)) {
        Lookup::Gone(_) => ScopeReport::new(Verdict::Removed, "that tag is no longer on the registry", ""),
        Lookup::Unreachable(why) => ScopeReport::new(Verdict::Unreachable, why, ""),
        Lookup::Found(body) => match serde_json::from_slice::<Value>(&body) {
            Err(_) => ScopeReport::new(Verdict::Unreachable, "the provider's answer could not be read", ""),
            Ok(manifest) => {
                let digest = format!("sha256:{}", sha256_hex(&body));
                let layers: Vec<(String, String)> = manifest
                    .get("layers")
                    .and_then(|l| l.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| {
                                let d = l.get("digest")?.as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
                                Some((l.get("mediaType").and_then(|m| m.as_str()).unwrap_or("").to_string(), d))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let weights = layers.iter().find(|(mt, _)| mt == "application/vnd.ollama.image.model").map(|(_, d)| d.clone());
                let mut now: HashMap<String, (Option<u64>, Option<String>)> = HashMap::new();
                for f in &rec.files {
                    if f.hash_from != FROM_PROVIDER {
                        now.insert(f.path.clone(), (None, None));
                    } else if layers.iter().any(|(_, d)| *d == f.sha256) {
                        now.insert(f.path.clone(), (None, Some(f.sha256.clone())));
                    } else if let Some(w) = &weights {
                        now.insert(f.path.clone(), (None, Some(w.clone())));
                    }
                }
                let mut r = compare(rec, &now, "head");
                if digest == rec.revision && r.verdict == Verdict::Confirmed {
                    r.detail = "the tag still points to the same manifest".into();
                }
                r.revision = digest;
                r
            }
        },
    };
    Ok(CheckReport { pinned, head })
}

/// The SHA-256 Civitai publishes for one of its files, lowercase.
fn civitai_sha(file: &Value) -> Option<String> {
    file.get("hashes")
        .and_then(|h| h.get("SHA256"))
        .and_then(|s| s.as_str())
        .filter(|s| is_sha256(s))
        .map(|s| s.to_ascii_lowercase())
}

/// Find each recorded file among a Civitai version's files: by its hash, else
/// by the name it was saved under.
fn civitai_now(rec: &UpstreamRecord, files: &[Value]) -> HashMap<String, (Option<u64>, Option<String>)> {
    let mut now = HashMap::new();
    for f in &rec.files {
        let by_hash = files.iter().find(|p| civitai_sha(p).as_deref() == Some(f.sha256.as_str()));
        let by_name = || files.iter().find(|p| p.get("name").and_then(|n| n.as_str()).map(civitai_leaf).as_deref() == Some(f.path.as_str()));
        if let Some(p) = by_hash.or_else(by_name) {
            // Civitai gives sizes in fractional kilobytes: not comparable.
            now.insert(f.path.clone(), (None, civitai_sha(p)));
        }
    }
    now
}

fn check_civitai(rec: &UpstreamRecord, token: Option<&str>, fetch: Fetch) -> Result<CheckReport, String> {
    if !all_digits(&rec.repo) || !all_digits(&rec.revision) {
        return Err("this record does not name a Civitai model and version".into());
    }
    let files_of = |v: &Value| v.get("files").and_then(|f| f.as_array()).cloned().unwrap_or_default();

    // Pinned: the version itself.
    let version_url = format!("https://civitai.com/api/v1/model-versions/{}", rec.revision);
    let mut pinned = match ask_json(fetch, get(version_url, JSON, token)) {
        Lookup::Found(v) => compare(rec, &civitai_now(rec, &files_of(&v)), "pinned"),
        Lookup::Gone(why) => ScopeReport::new(Verdict::Removed, format!("the version: {why}"), ""),
        Lookup::Unreachable(why) => ScopeReport::new(Verdict::Unreachable, why, ""),
    };
    pinned.revision = rec.revision.clone();

    // Head: the model's own page — is this version still listed, unchanged?
    let head = match ask_json(fetch, get(format!("https://civitai.com/api/v1/models/{}", rec.repo), JSON, token)) {
        Lookup::Gone(why) => ScopeReport::new(Verdict::Removed, format!("the model: {why}"), ""),
        Lookup::Unreachable(why) => ScopeReport::new(Verdict::Unreachable, why, ""),
        Lookup::Found(m) => {
            let versions = m.get("modelVersions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let id_of = |v: &Value| v.get("id").and_then(|i| i.as_u64()).map(|i| i.to_string()).unwrap_or_default();
            let latest = versions.first().map(id_of).unwrap_or_default();
            let mut r = match versions.iter().find(|v| id_of(v) == rec.revision) {
                Some(v) => compare(rec, &civitai_now(rec, &files_of(v)), "head"),
                None => ScopeReport::new(Verdict::Removed, "this version is no longer listed under the model", ""),
            };
            if !latest.is_empty() && latest != rec.revision && r.verdict == Verdict::Confirmed {
                r.detail.push_str("; a newer version is listed too");
            }
            r.revision = latest;
            r
        }
    };
    Ok(CheckReport { pinned, head })
}

/// Ask the provider what it publishes now for a record's files. `Err` only
/// when the record does not name something this Orb knows how to ask about —
/// then nothing was asked.
pub(super) fn check_upstream(rec: &UpstreamRecord, token: Option<&str>, fetch: Fetch) -> Result<CheckReport, String> {
    match rec.provider.as_str() {
        "huggingface" => check_hf(rec, token, fetch),
        "ollama" => check_ollama(rec, token, fetch),
        "civitai" => check_civitai(rec, token, fetch),
        other => Err(format!("this Orb does not know how to ask “{other}” what it publishes")),
    }
}

/// The history line for a finished re-check.
pub(super) fn check_event(cid: &str, name: &str, rec: &UpstreamRecord, report: &CheckReport, by: &str, at_ms: i64) -> Event {
    let detail = if report.pinned.detail == report.head.detail {
        report.pinned.detail.clone()
    } else {
        format!("pinned revision: {}; current listing: {}", report.pinned.detail, report.head.detail)
    };
    let mut diff = report.pinned.diff.clone();
    diff.extend(report.head.diff.iter().cloned());
    diff.truncate(20);
    Event {
        at_ms,
        event: report.verdict().as_str().into(),
        cid: cid.to_string(),
        name: name.to_string(),
        record: rec.id(),
        provider: rec.provider.clone(),
        repo: rec.repo.clone(),
        revision: rec.revision.clone(),
        pinned: report.pinned.verdict.as_str().into(),
        head: report.head.verdict.as_str().into(),
        head_revision: report.head.revision.clone(),
        detail,
        diff,
        by: by.to_string(),
        ..Default::default()
    }
}

/// The record for a model in this Orb's catalog: the one on its entry, else
/// the one this Orb wrote into its history when it imported the model (an
/// entry too large for gossip carries no record of its own).
fn record_of(entry: &ModelEntry, chain: &Chain) -> Option<UpstreamRecord> {
    entry.upstream_record.clone().or_else(|| {
        chain.events.iter().rev().find(|(_, e)| e.event == "recorded" && e.cid == entry.cid).and_then(|(_, e)| {
            UpstreamRecord { provider: e.provider.clone(), repo: e.repo.clone(), revision: e.revision.clone(), files: e.files.clone() }
                .cleaned()
        })
    })
}

/// Re-check one model of this Orb's catalog now and append the outcome.
/// One provider conversation at a time.
pub(super) fn recheck(cid: &str, by: &str) -> Result<Event, String> {
    static ONE: OnceLock<Mutex<()>> = OnceLock::new();
    let _one = ONE.get_or_init(|| Mutex::new(())).lock();
    let history = Path::new(HISTORY_FILE);
    let entry = read_catalog().into_iter().find(|e| e.cid == cid).ok_or("that model is not in this Orb's catalog")?;
    let rec = record_of(&entry, &read_chain(history)).ok_or(
        "this model has no upstream record: it was not imported from a provider, or it was shared before records were kept",
    )?;
    let token = source_token(&rec.provider);
    let report = check_upstream(&rec, token.as_deref(), &http_fetch)?;
    let ev = append_event(history, check_event(cid, &entry.name, &rec, &report, by, epoch_ms()))
        .map_err(|e| format!("the check ran, but its result could not be written to the history: {e}"))?;
    if report.pinned.verdict == Verdict::Confirmed {
        add_own_witness(cid, &rec);
    }
    Ok(ev)
}

/// This Orb has now seen the provider publish these hashes itself. If its
/// catalog entry does not carry this Orb's witness yet — a model mirrored from
/// another Orb — sign one and announce it.
fn add_own_witness(cid: &str, rec: &UpstreamRecord) {
    let me = crate::publisher::orb_pubkey();
    let mut entries = read_catalog();
    let Some(e) = entries.iter_mut().find(|e| e.cid == cid) else { return };
    if e.upstream_record.as_ref() != Some(rec) || e.witnesses.iter().any(|w| w.key == me) {
        return;
    }
    if let Some(w) = sign_witness(rec, epoch_ms(), true) {
        e.witnesses.insert(0, w);
        if write_catalog(&entries).is_ok() {
            announce();
        }
    }
}

// ── The background timer ─────────────────────────────────────────────────────

const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;
const DAY_MS: i64 = 24 * 3600 * 1000;

pub(super) fn auto_enabled() -> bool {
    std::fs::read_to_string(RECHECK_JSON)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("auto").and_then(|b| b.as_bool()))
        .unwrap_or(true)
}

/// When a record is next due for the timer, from its own history. None for a
/// record this Orb did not import itself: the timer only asks a provider about
/// models this Orb already fetched from that provider, so mirroring a model
/// never makes the Orb contact its provider behind the operator's back.
/// A week after the last answer, moved up to half a day either way by the
/// record's own id so neither one Orb's models nor many Orbs ask together; a
/// day after an attempt that got no answer.
pub(super) fn next_due_ms(record_id: &str, events: &[&Event]) -> Option<i64> {
    if !events.iter().any(|e| e.event == "recorded") {
        return None;
    }
    let last = events.iter().max_by_key(|e| e.at_ms)?;
    if last.event == "unreachable" {
        return Some(last.at_ms + DAY_MS);
    }
    let spread = record_id.get(..8).and_then(|h| i64::from_str_radix(h, 16).ok()).unwrap_or(0) % DAY_MS - DAY_MS / 2;
    Some(last.at_ms + WEEK_MS + spread)
}

/// The catalog models that are due, oldest first.
fn due_now(now_ms: i64) -> Vec<String> {
    let chain = read_chain(Path::new(HISTORY_FILE));
    let mut due: Vec<(i64, String)> = Vec::new();
    for entry in read_catalog() {
        let Some(rec) = record_of(&entry, &chain) else { continue };
        let id = rec.id();
        let events: Vec<&Event> = chain.events.iter().map(|(_, e)| e).filter(|e| e.record == id).collect();
        if let Some(at) = next_due_ms(&id, &events) {
            if at <= now_ms {
                due.push((at, entry.cid.clone()));
            }
        }
    }
    due.sort();
    due.into_iter().map(|(_, cid)| cid).collect()
}

/// Spawned once at startup. It never checks at startup: the first pass comes
/// 30–60 minutes in, later passes roughly hourly, and a pass only touches
/// models whose own week is up — a few at most, with a pause between them.
pub async fn recheck_loop() {
    use rand::Rng;
    let pause = |lo: u64, hi: u64| std::time::Duration::from_secs(rand::thread_rng().gen_range(lo..=hi));
    tokio::time::sleep(pause(30 * 60, 60 * 60)).await;
    loop {
        if auto_enabled() && read_config().enabled {
            let due = tokio::task::spawn_blocking(|| due_now(epoch_ms())).await.unwrap_or_default();
            for cid in due.into_iter().take(8) {
                let _ = tokio::task::spawn_blocking(move || recheck(&cid, "timer")).await;
                tokio::time::sleep(pause(20, 90)).await;
            }
        }
        tokio::time::sleep(pause(45 * 60, 75 * 60)).await;
    }
}

// ── HTTP handlers (under /api/ipfs/, admin only) ─────────────────────────────

fn event_view(line: usize, e: &Event) -> Value {
    json!({
        "line": line, "at_ms": e.at_ms, "event": e.event, "pinned": e.pinned, "head": e.head,
        "head_revision": e.head_revision, "detail": e.detail, "diff": e.diff, "by": e.by,
        "observed_ms": e.observed_ms, "revision": e.revision,
    })
}

/// Everything the console's "Original weights" block shows for one model.
fn view(cid: &str) -> Value {
    let local = read_catalog();
    let peers = read_registry();
    let chain = read_chain(Path::new(HISTORY_FILE));
    let chain_view = json!({"ok": chain.broken.is_empty(), "lines": chain.lines, "broken": chain.broken.iter().take(10).collect::<Vec<_>>()});
    let mine = local.iter().find(|e| e.cid == cid).cloned();
    let from_peers = || {
        let mut copies: Vec<ModelEntry> = peers.values().flat_map(|p| p.models.iter()).filter(|e| e.cid == cid).cloned().collect();
        copies.iter_mut().for_each(scrub);
        copies.sort_by_key(|e| e.upstream_record.is_none());
        copies.into_iter().next()
    };
    let Some(mut entry) = mine.clone().or_else(from_peers) else {
        return json!({"ok": false, "err": "unknown model"});
    };
    scrub(&mut entry);
    let record = if mine.is_some() { record_of(&entry, &chain) } else { entry.upstream_record.clone() };
    let Some(rec) = record else {
        return json!({"ok": true, "cid": cid, "record": Value::Null, "local": mine.is_some(), "auto": auto_enabled(), "chain": chain_view});
    };

    let id = rec.id();
    let events: Vec<&(usize, Event)> = chain.events.iter().filter(|(_, e)| e.record == id).collect();
    let recorded = events.iter().find(|(_, e)| e.event == "recorded");
    let checks: Vec<&&(usize, Event)> = events.iter().filter(|(_, e)| e.event != "recorded").collect();
    let confirmed_ms = checks.iter().rev().find(|(_, e)| e.pinned == "confirmed").map(|(_, e)| e.at_ms).unwrap_or(0);

    let mut index = WitnessIndex::new();
    for e in local.iter().chain(peers.values().flat_map(|p| p.models.iter())) {
        index.add(e);
    }
    json!({
        "ok": true, "cid": cid, "local": mine.is_some(), "auto": auto_enabled(),
        "record": rec, "link": rec.link(),
        // What THIS Orb did itself, from its own history.
        "recorded_here_ms": recorded.map(|(_, e)| if e.observed_ms > 0 { e.observed_ms } else { e.at_ms }).unwrap_or(0),
        "confirmed_here_ms": confirmed_ms,
        "last_check": checks.last().map(|(line, e)| event_view(*line, e)),
        "history": events.iter().rev().take(60).map(|(line, e)| event_view(*line, e)).collect::<Vec<_>>(),
        "chain": chain_view,
        // What signed statements say, this Orb's included.
        "witnessed": index.tally(&rec),
    })
}

/// GET /api/ipfs/models/upstream?cid= — the upstream record of one model, what
/// this Orb checked itself, and what other publishers signed.
pub async fn get_upstream(State(_s): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let cid = q.get("cid").cloned().unwrap_or_default();
    if !valid_cid(&cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    let v = tokio::task::spawn_blocking(move || view(&cid))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "err": "upstream task failed"}));
    Json(v)
}

/// POST /api/ipfs/models/upstream/recheck {cid} — ask the provider now what it
/// publishes for this model and append the answer to the history. Metadata
/// only. At most once every 20 seconds per model.
pub async fn post_recheck(State(_s): State<AppState>, Json(req): Json<CidReq>) -> Json<Value> {
    if !valid_cid(&req.cid) {
        return Json(json!({"ok": false, "err": "invalid CID"}));
    }
    static LAST: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    {
        let mut last = LAST.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|p| p.into_inner());
        if last.get(&req.cid).is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(20)) {
            return Json(json!({"ok": false, "err": "this model was checked a moment ago — try again shortly"}));
        }
        if last.len() > 2000 {
            last.clear();
        }
        last.insert(req.cid.clone(), std::time::Instant::now());
    }
    let v = tokio::task::spawn_blocking(move || match recheck(&req.cid, "operator") {
        Ok(ev) => json!({"ok": true, "event": event_view(0, &ev)}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "re-check task failed"}));
    Json(v)
}

#[derive(Deserialize)]
pub struct AutoReq {
    pub auto: bool,
}

/// GET /api/ipfs/models/upstream/auto — whether the weekly re-check is on.
pub async fn get_auto(State(_s): State<AppState>) -> Json<Value> {
    Json(json!({"ok": true, "auto": auto_enabled()}))
}

/// POST /api/ipfs/models/upstream/auto {auto} — switch the weekly re-check.
pub async fn set_auto(State(_s): State<AppState>, Json(req): Json<AutoReq>) -> Json<Value> {
    write_json_file(RECHECK_JSON, &json!({"auto": req.auto}));
    Json(json!({"ok": true, "auto": auto_enabled()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("aeon-upstream-{tag}-{}-{}", std::process::id(), new_draft_id()));
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("upstream-history.jsonl")
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const REV: &str = "0123456789abcdef0123456789abcdef01234567";
    const HEAD: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn hf_record() -> UpstreamRecord {
        record_from_import(
            "huggingface",
            "org/model",
            REV,
            vec![
                ("unet/model.safetensors".to_string(), 4096, "BB".repeat(32), true),
                ("model.safetensors".to_string(), 1024, "aa".repeat(32), true),
                ("config.json".to_string(), 12, "cc".repeat(32), false),
            ],
        )
        .unwrap()
    }

    // ── records and witnesses ───────────────────────────────────────────────

    #[test]
    fn a_record_keeps_what_the_provider_published_apart_from_what_was_computed() {
        let rec = hf_record();
        // Path order, lowercase hashes.
        assert_eq!(rec.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["config.json", "model.safetensors", "unet/model.safetensors"]);
        assert_eq!(rec.files[2].sha256, "bb".repeat(32));
        // The file the provider gave no hash for is computed, never provider.
        assert_eq!(rec.files[0].hash_from, FROM_COMPUTED);
        assert_eq!(rec.files[1].hash_from, FROM_PROVIDER);
        // …and it is not in what a witness signs.
        let signed = String::from_utf8(rec.witness_bytes(7)).unwrap();
        assert_eq!(
            signed,
            format!("aeon-witness/1\nhuggingface\norg/model\n{REV}\n7\n{} 1024 model.safetensors\n{} 4096 unet/model.safetensors\n", "aa".repeat(32), "bb".repeat(32))
        );
        assert!(!signed.contains("config.json"));
        assert_eq!(rec.link(), format!("https://huggingface.co/org/model/tree/{REV}"));
    }

    #[test]
    fn malformed_records_read_as_no_record() {
        let ok = serde_json::to_value(hf_record()).unwrap();
        let read = |v: Value| -> Option<UpstreamRecord> {
            #[derive(Deserialize)]
            struct Holder {
                #[serde(default, deserialize_with = "lenient_record")]
                r: Option<UpstreamRecord>,
            }
            serde_json::from_value::<Holder>(json!({"r": v})).unwrap().r
        };
        assert_eq!(read(ok.clone()), Some(hf_record()));
        let mut bad = Vec::new();
        for (field, value) in [
            ("provider", json!("Hugging Face")),
            ("provider", json!("")),
            ("repo", json!("org/model\nextra line")),
            ("revision", json!("")),
            ("files", json!([])),
            ("files", json!("nope")),
        ] {
            let mut v = ok.clone();
            v[field] = value;
            bad.push(v);
        }
        for (field, value) in [
            ("path", json!("../escape.safetensors")),
            ("path", json!("/etc/passwd")),
            ("sha256", json!("abc")),
            ("hash_from", json!("trust me")),
            ("size", json!("big")),
            ("path", json!("model.safetensors")), // the same path twice
        ] {
            let mut v = ok.clone();
            v["files"][0][field] = value;
            bad.push(v);
        }
        bad.extend([json!(null), json!("x"), json!(7), json!([])]);
        for v in bad {
            assert_eq!(read(v.clone()), None, "{v}");
        }
        // A catalog entry with a broken record or witness list still loads.
        let entry: ModelEntry = serde_json::from_value(json!({
            "cid": "bafytestcid000", "name": "m", "upstream_record": {"provider": 5}, "witnesses": [{"key": 1}, "x", {"key": "k", "sig": "s", "observed_ms": 3}],
        }))
        .unwrap();
        assert!(entry.upstream_record.is_none());
        assert_eq!(entry.witnesses.len(), 1);
    }

    #[test]
    fn a_witness_holds_only_for_the_record_and_time_it_signed() {
        let rec = hf_record();
        let w = sign_witness(&rec, 1_760_000_000_000, true).unwrap();
        assert_eq!(w.key, crate::publisher::orb_pubkey());
        assert!(witness_ok(&rec, &w));
        assert_eq!(witness_identity(&w), w.key); // no delegation in tests

        // A different time, hash, revision, repository or provider: not signed.
        assert!(!witness_ok(&rec, &Witness { observed_ms: w.observed_ms - 1, ..w.clone() }));
        let mut other = rec.clone();
        other.files[1].sha256 = "dd".repeat(32);
        assert!(!witness_ok(&other, &w));
        let mut other = rec.clone();
        other.revision = HEAD.into();
        assert!(!witness_ok(&other, &w));
        let mut other = rec.clone();
        other.repo = "mallory/model".into();
        assert!(!witness_ok(&other, &w));
        // The computed file is outside the statement: changing it changes
        // nothing a witness says.
        let mut other = rec.clone();
        other.files[0].sha256 = "ee".repeat(32);
        assert!(witness_ok(&other, &w));
        // A record with no provider-published hash has nothing to witness.
        let computed_only = record_from_import("huggingface", "org/model", REV, vec![("a.json".to_string(), 1, "aa".repeat(32), false)]).unwrap();
        assert!(sign_witness(&computed_only, 5, true).is_none());
    }

    #[test]
    fn witnesses_that_fail_are_dropped_and_the_rest_deduplicated() {
        let rec = hf_record();
        let mine_late = sign_witness(&rec, 2_000, true).unwrap();
        let mine_early = sign_witness(&rec, 1_000, true).unwrap();
        let forged = Witness { key: "ab".repeat(32), observed_ms: 500, sig: mine_early.sig.clone(), delegation: None };
        let garbage = Witness { key: "zz".into(), observed_ms: 1, sig: "!".into(), delegation: None };
        let kept = valid_witnesses(&rec, &[mine_late, forged, garbage, mine_early.clone()]);
        assert_eq!(kept, vec![mine_early]);

        // On an entry: the record must name the entry's own signed primary file.
        let mut entry = ModelEntry {
            cid: "bafytestcid000".into(),
            name: "m".into(),
            file: "model.safetensors".into(),
            sha256: "aa".repeat(32),
            upstream_record: Some(rec.clone()),
            witnesses: kept.clone(),
            ..Default::default()
        };
        scrub(&mut entry);
        assert!(entry.upstream_record.is_some() && entry.witnesses.len() == 1);
        entry.sha256 = "ff".repeat(32); // the record is about some other weights
        scrub(&mut entry);
        assert!(entry.upstream_record.is_none() && entry.witnesses.is_empty());
    }

    #[test]
    fn the_tally_counts_identities_that_signed_the_same_hashes() {
        use ed25519_dalek::{Signer, SigningKey};
        let rec = hf_record();
        let sign_as = |seed: u8, rec: &UpstreamRecord, at: i64| {
            let sk = SigningKey::from_bytes(&[seed; 32]);
            Witness {
                key: hex::encode(sk.verifying_key().to_bytes()),
                observed_ms: at,
                sig: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sk.sign(&rec.witness_bytes(at)).to_bytes()),
                delegation: None,
            }
        };
        let entry = |cid: &str, rec: &UpstreamRecord, ws: Vec<Witness>| ModelEntry {
            cid: cid.into(),
            name: "m".into(),
            upstream_record: Some(rec.clone()),
            witnesses: ws,
            ..Default::default()
        };
        let mine = sign_witness(&rec, 5_000, true).unwrap();
        // The same hashes seen at a later commit of the same repository.
        let mut later = rec.clone();
        later.revision = HEAD.into();
        // Someone signing a different hash for the SAME pinned revision.
        let mut liar = rec.clone();
        liar.files[1].sha256 = "dd".repeat(32);

        let mut index = WitnessIndex::new();
        index.add(&entry("bafyaaaaaaaaaa", &rec, vec![mine.clone(), sign_as(7, &rec, 3_000)]));
        index.add(&entry("bafybbbbbbbbbb", &later, vec![sign_as(8, &later, 4_000), sign_as(7, &later, 9_000)]));
        index.add(&entry("bafycccccccccc", &liar, vec![sign_as(9, &liar, 1)]));
        // A witness copied onto a record it did not sign counts for nothing.
        index.add(&entry("bafydddddddddd", &liar, vec![sign_as(10, &rec, 2)]));

        let t = index.tally(&rec);
        assert_eq!(t["identities"], 3, "{t}"); // this Orb, key 7, key 8
        assert_eq!(t["others"], 2);
        assert_eq!(t["mine"], true);
        assert_eq!(t["earliest_ms"], 3_000);
        assert_eq!(t["earliest_other_ms"], 3_000);
        assert_eq!(t["differing"], 1);
        assert_eq!(t["list"][0]["observed_ms"], 3_000);
        // Nobody else agrees with the differing record.
        assert_eq!(index.tally(&liar)["identities"], 1);
    }

    #[test]
    fn an_oversized_entry_gives_up_witnesses_then_the_record_never_the_entry() {
        let rec = hf_record();
        let w = sign_witness(&rec, 1_000, true).unwrap();
        let mut small = ModelEntry { cid: "bafytestcid000".into(), name: "m".into(), upstream_record: Some(rec.clone()), witnesses: vec![w.clone()], ..Default::default() };
        fit_for_gossip(&mut small);
        assert!(small.upstream_record.is_some() && small.witnesses.len() == 1);

        // Many witnesses: the tail goes, this Orb's own (first) stays.
        let mut crowded = small.clone();
        crowded.witnesses = (0..200).map(|i| Witness { observed_ms: 1_000 + i, ..w.clone() }).collect();
        fit_for_gossip(&mut crowded);
        assert!(gossip_len(&crowded) <= ENTRY_BUDGET);
        assert!(!crowded.witnesses.is_empty() && crowded.witnesses[0] == w);
        assert!(crowded.upstream_record.is_some());

        // A record too large on its own is left to the card and the history.
        let huge = record_from_import(
            "huggingface",
            "org/model",
            REV,
            (0..400).map(|i| (format!("shards/model-{i:05}-of-00400.safetensors"), 1, "aa".repeat(32), true)),
        )
        .unwrap();
        let mut big = ModelEntry { cid: "bafytestcid000".into(), name: "m".into(), upstream_record: Some(huge), witnesses: vec![w], ..Default::default() };
        fit_for_gossip(&mut big);
        assert!(big.upstream_record.is_none() && big.witnesses.is_empty());
        assert!(gossip_len(&big) <= ENTRY_BUDGET);
        // Non-ASCII text is measured the way the daemon will see it.
        let accents = ModelEntry { cid: "bafytestcid000".into(), name: "é😀".into(), ..Default::default() };
        assert!(gossip_len(&accents) >= serde_json::to_string(&accents).unwrap().len() + 4 + 8);
    }

    // ── the history chain ───────────────────────────────────────────────────

    fn three_lines(tag: &str) -> Tmp {
        let t = Tmp::new(tag);
        let rec = hf_record();
        append_event(&t.file(), recorded_event("bafytestcid000", "m", &rec, 1_000)).unwrap();
        for at in [2_000, 3_000] {
            let report = CheckReport::both(Verdict::Confirmed, "all 2 published SHA-256 hashes are the same");
            append_event(&t.file(), check_event("bafytestcid000", "m", &rec, &report, "timer", at)).unwrap();
        }
        t
    }

    #[test]
    fn each_line_carries_the_hash_of_the_line_before_it() {
        let t = three_lines("chain");
        let text = std::fs::read_to_string(t.file()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let chain = read_chain(&t.file());
        assert!(chain.broken.is_empty());
        assert_eq!(chain.lines, 3);
        assert_eq!(chain.events[0].1.prev, CHAIN_START);
        assert_eq!(chain.events[1].1.prev, sha256_hex(lines[0].as_bytes()));
        assert_eq!(chain.events[2].1.prev, sha256_hex(lines[1].as_bytes()));
        assert_eq!(chain.events.iter().map(|(_, e)| e.event.as_str()).collect::<Vec<_>>(), ["recorded", "confirmed", "confirmed"]);
        // The recorded line holds the whole record; the re-checks do not repeat it.
        assert_eq!(chain.events[0].1.files.len(), 3);
        assert!(chain.events[1].1.files.is_empty());
        assert_eq!(chain.events[0].1.record, hf_record().id());
    }

    #[test]
    fn a_tampered_line_is_reported_and_left_as_it_is() {
        // An edit in the middle: the line after it no longer follows.
        let t = three_lines("edit");
        let text = std::fs::read_to_string(t.file()).unwrap();
        let edited = text.replacen("\"at_ms\":2000", "\"at_ms\":2001", 1);
        assert_ne!(text, edited);
        std::fs::write(t.file(), &edited).unwrap();
        let chain = read_chain(&t.file());
        assert_eq!(chain.broken, vec![3]);
        assert_eq!(chain.events.len(), 3); // still shown, as found
        // Nothing repairs it: reading twice and appending leave those bytes alone.
        let report = CheckReport::both(Verdict::Unreachable, "no route to host");
        append_event(&t.file(), check_event("bafytestcid000", "m", &hf_record(), &report, "operator", 4_000)).unwrap();
        let after = std::fs::read_to_string(t.file()).unwrap();
        assert!(after.starts_with(&edited));
        let chain = read_chain(&t.file());
        assert_eq!(chain.broken, vec![3]); // the new line links to the last line as it stands
        assert_eq!(chain.lines, 4);

        // A line removed from the middle, and one from the start.
        let t = three_lines("cut");
        let text = std::fs::read_to_string(t.file()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(t.file(), format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert_eq!(read_chain(&t.file()).broken, vec![2]);
        std::fs::write(t.file(), format!("{}\n{}\n", lines[1], lines[2])).unwrap();
        assert_eq!(read_chain(&t.file()).broken, vec![1]);

        // A line inserted, and a line that is not an event at all.
        let t = three_lines("insert");
        let text = std::fs::read_to_string(t.file()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        std::fs::write(t.file(), format!("{}\n{}\n{}\n{}\n", lines[0], lines[1], lines[1], lines[2])).unwrap();
        assert_eq!(read_chain(&t.file()).broken, vec![3]);
        std::fs::write(t.file(), format!("{}\nnot json\n{}\n", lines[0], lines[1])).unwrap();
        assert_eq!(read_chain(&t.file()).broken, vec![2, 3]);
    }

    #[test]
    fn a_write_cut_short_stays_a_broken_line_and_the_chain_goes_on() {
        let t = three_lines("torn");
        let mut bytes = std::fs::read(t.file()).unwrap();
        bytes.extend_from_slice(b"{\"prev\":\"abc\",\"at_ms\":35"); // no newline: the power went
        std::fs::write(t.file(), &bytes).unwrap();
        assert_eq!(read_chain(&t.file()).broken, vec![4]);
        let report = CheckReport::both(Verdict::Confirmed, "ok");
        let written = append_event(&t.file(), check_event("bafytestcid000", "m", &hf_record(), &report, "timer", 4_000)).unwrap();
        let chain = read_chain(&t.file());
        assert_eq!(chain.lines, 5);
        assert_eq!(chain.broken, vec![4]); // the torn line, nothing else
        assert_eq!(written.prev, sha256_hex(b"{\"prev\":\"abc\",\"at_ms\":35"));
        // A history that does not exist yet is an empty, unbroken chain.
        let none = read_chain(&t.0.join("missing.jsonl"));
        assert!(none.broken.is_empty() && none.lines == 0);
    }

    // ── the re-check ────────────────────────────────────────────────────────

    /// A provider that answers from a table: "METHOD url" → (status, body).
    /// Anything not in the table is a network error. Records what was asked.
    struct Fake {
        table: HashMap<String, (u16, String)>,
        asked: Mutex<Vec<String>>,
    }
    impl Fake {
        fn new(rows: &[(&str, u16, Value)]) -> Self {
            Self {
                table: rows.iter().map(|(k, s, v)| (k.to_string(), (*s, if v.is_string() { v.as_str().unwrap().to_string() } else { v.to_string() }))).collect(),
                asked: Mutex::new(Vec::new()),
            }
        }
        fn answer(&self, req: &Req) -> Result<Reply, String> {
            let key = format!("{} {}", req.method, req.url);
            self.asked.lock().unwrap().push(key.clone());
            match self.table.get(&key) {
                Some((status, body)) => Ok(Reply { status: *status, body: body.clone().into_bytes() }),
                None => Err("connection refused".into()),
            }
        }
        fn check(&self, rec: &UpstreamRecord) -> CheckReport {
            check_upstream(rec, None, &|r| self.answer(r)).unwrap()
        }
        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    fn hf_file(path: &str, size: u64, lfs: Option<&str>) -> Value {
        let mut v = json!({"type": "file", "path": path, "size": size, "oid": "0f".repeat(20)});
        if let Some(oid) = lfs {
            v["lfs"] = json!({"oid": oid, "size": size, "pointerSize": 134});
        }
        v
    }

    fn hf_listing() -> Value {
        json!([
            hf_file("config.json", 12, None),
            hf_file("model.safetensors", 1024, Some(&"aa".repeat(32))),
            hf_file("unet/model.safetensors", 4096, Some(&"bb".repeat(32))),
        ])
    }

    const META: &str = "GET https://huggingface.co/api/models/org/model";
    fn paths(rev: &str) -> String {
        format!("POST https://huggingface.co/api/models/org/model/paths-info/{rev}")
    }

    #[test]
    fn unchanged_upstream_is_confirmed_at_the_pinned_revision_and_the_head() {
        // The default branch has not moved: two requests, no more.
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": REV})), (&paths(REV), 200, hf_listing())]);
        let r = p.check(&hf_record());
        assert_eq!((r.pinned.verdict, r.head.verdict, r.verdict()), (Verdict::Confirmed, Verdict::Confirmed, Verdict::Confirmed));
        assert_eq!(p.asked().len(), 2);
        assert!(r.pinned.detail.contains("all 2 published SHA-256 hashes are the same"), "{}", r.pinned.detail);

        // The default branch moved on but still publishes the same files.
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": HEAD})), (&paths(REV), 200, hf_listing()), (&paths(HEAD), 200, hf_listing())]);
        let r = p.check(&hf_record());
        assert_eq!(r.verdict(), Verdict::Confirmed);
        assert_eq!(r.head.revision, HEAD);
        // Only metadata was asked for: never a file.
        assert!(p.asked().iter().all(|a| a.contains("/api/models/")), "{:?}", p.asked());
    }

    #[test]
    fn a_renamed_repository_is_followed_and_said_so() {
        // The old name answers (after HuggingFace's redirect) with the new id;
        // the file listing is then asked for under the new name.
        let p = Fake::new(&[
            (META, 200, json!({"id": "new-home/model", "sha": REV})),
            (&format!("POST https://huggingface.co/api/models/new-home/model/paths-info/{REV}"), 200, hf_listing()),
        ]);
        let r = p.check(&hf_record());
        assert_eq!(r.verdict(), Verdict::Confirmed);
        assert!(r.pinned.detail.ends_with("; the repository is now named new-home/model"), "{}", r.pinned.detail);
        // A name that could not be a repository is not followed anywhere.
        let p = Fake::new(&[(META, 200, json!({"id": "../../elsewhere", "sha": REV})), (&paths(REV), 200, hf_listing())]);
        assert_eq!(p.check(&hf_record()).verdict(), Verdict::Confirmed);
        assert!(p.asked().iter().all(|a| a.contains("/api/models/org/model")), "{:?}", p.asked());
    }

    #[test]
    fn a_different_hash_for_the_same_path_is_changed() {
        let mut moved = hf_listing();
        moved[1]["lfs"]["oid"] = json!("dd".repeat(32));
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": HEAD})), (&paths(REV), 200, hf_listing()), (&paths(HEAD), 200, moved.clone())]);
        let r = p.check(&hf_record());
        // The original revision is still what it was; the default branch is not.
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Confirmed, Verdict::Changed));
        assert_eq!(r.verdict(), Verdict::Changed);
        assert_eq!(r.head.diff, vec![Diff { path: "model.safetensors".into(), scope: "head".into(), what: "changed".into(), was: "aa".repeat(32), now: "dd".repeat(32) }]);

        // The same at the PINNED revision — content swapped under a commit id.
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": REV})), (&paths(REV), 200, moved)]);
        let r = p.check(&hf_record());
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Changed, Verdict::Changed));
        assert_eq!(r.pinned.diff[0].scope, "pinned");

        // A computed file whose size differs, and a file the provider stopped
        // publishing a hash for, are changes too.
        let mut resized = hf_listing();
        resized[0]["size"] = json!(13);
        resized[2].as_object_mut().unwrap().remove("lfs");
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": REV})), (&paths(REV), 200, resized)]);
        let r = p.check(&hf_record());
        assert_eq!(r.pinned.verdict, Verdict::Changed);
        assert_eq!(r.pinned.diff.iter().map(|d| (d.path.as_str(), d.what.as_str())).collect::<Vec<_>>(), [("config.json", "changed"), ("unet/model.safetensors", "unpublished")]);
    }

    #[test]
    fn a_missing_repository_revision_or_file_is_removed() {
        // The repository is gone (HuggingFace answers 401 to a stranger).
        for status in [401, 403, 404, 410, 451] {
            let p = Fake::new(&[(META, status, json!({"error": "Repository not found"}))]);
            let r = p.check(&hf_record());
            assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Removed, Verdict::Removed), "{status}");
        }
        // Now gated or private.
        for meta in [json!({"id": "org/model", "sha": REV, "gated": "auto"}), json!({"id": "org/model", "sha": REV, "private": true}), json!({"id": "org/model", "sha": REV, "disabled": true})] {
            let p = Fake::new(&[(META, 200, meta)]);
            assert_eq!(p.check(&hf_record()).verdict(), Verdict::Removed);
            assert_eq!(p.asked().len(), 1);
        }
        // One file deleted from the default branch; the pinned revision intact.
        let mut fewer = hf_listing();
        fewer.as_array_mut().unwrap().remove(2);
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": HEAD})), (&paths(REV), 200, hf_listing()), (&paths(HEAD), 200, fewer)]);
        let r = p.check(&hf_record());
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Confirmed, Verdict::Removed));
        assert_eq!(r.head.diff[0].what, "gone");
        // History rewritten: the pinned commit itself no longer exists.
        let gone_rev = format!("GET https://huggingface.co/api/models/org/model/revision/{REV}");
        let p = Fake::new(&[
            (META, 200, json!({"id": "org/model", "sha": HEAD})),
            (&paths(REV), 200, json!([])),
            (&gone_rev, 404, json!({"error": "Invalid rev id"})),
            (&paths(HEAD), 200, hf_listing()),
        ]);
        let r = p.check(&hf_record());
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Removed, Verdict::Confirmed));
        assert_eq!(r.pinned.detail, "the provider no longer serves that revision");
        assert_eq!(r.verdict(), Verdict::Removed);
    }

    #[test]
    fn no_answer_is_unreachable_and_is_evidence_of_nothing() {
        // Nothing answers at all.
        let p = Fake::new(&[]);
        let r = p.check(&hf_record());
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Unreachable, Verdict::Unreachable));
        assert!(r.pinned.diff.is_empty());
        // A rate limit, a server error, a redirect, a bad request, and an
        // answer that is not JSON: none of them says the model changed.
        for status in [429, 500, 502, 503, 307, 400] {
            let p = Fake::new(&[(META, status, json!("slow down"))]);
            assert_eq!(p.check(&hf_record()).verdict(), Verdict::Unreachable, "{status}");
        }
        let p = Fake::new(&[(META, 200, json!("<html>captive portal</html>"))]);
        assert_eq!(p.check(&hf_record()).verdict(), Verdict::Unreachable);
        // The repository answers but the file listing does not.
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": REV}))]);
        let r = p.check(&hf_record());
        assert_eq!(r.verdict(), Verdict::Unreachable);
        // Half an answer never outranks a real finding.
        assert!(Verdict::Changed > Verdict::Removed && Verdict::Removed > Verdict::Unreachable && Verdict::Unreachable > Verdict::Confirmed);
    }

    #[test]
    fn a_record_that_names_nothing_askable_asks_nothing() {
        let p = Fake::new(&[]);
        for (provider, repo, revision) in [
            ("huggingface", "org/model/../../other", REV),
            ("huggingface", "org/model", "main"),
            ("ollama", "library/model", "sha256:00"),
            ("ollama", "evil.example/x/y:tag", &format!("sha256:{}", "aa".repeat(32))[..]),
            ("civitai", "12/../3", "5"),
            ("somewhere-else", "x", "y"),
        ] {
            let rec = UpstreamRecord { provider: provider.into(), repo: repo.into(), revision: revision.into(), files: hf_record().files };
            assert!(check_upstream(&rec, None, &|r| p.answer(r)).is_err(), "{provider} {repo} {revision}");
        }
        assert!(p.asked().is_empty());
    }

    #[test]
    fn ollama_is_checked_by_blob_digest_and_by_where_the_tag_points() {
        let sha = "aa".repeat(32);
        let manifest = json!({"layers": [
            {"mediaType": "application/vnd.ollama.image.model", "digest": format!("sha256:{sha}"), "size": 9},
            {"mediaType": "application/vnd.ollama.image.license", "digest": format!("sha256:{}", "cc".repeat(32)), "size": 1},
        ]});
        let digest = format!("sha256:{}", sha256_hex(manifest.to_string().as_bytes()));
        let rec = record_from_import("ollama", "library/tiny:1b", &digest, vec![("tiny-1b.gguf".to_string(), 9, sha.clone(), true)]).unwrap();
        assert_eq!(rec.link(), "https://ollama.com/library/tiny:1b");
        let blob = format!("HEAD https://registry.ollama.ai/v2/library/tiny/blobs/sha256:{sha}");
        let tag = "GET https://registry.ollama.ai/v2/library/tiny/manifests/1b";

        let p = Fake::new(&[(&blob, 200, json!("")), (tag, 200, manifest.clone())]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Confirmed, Verdict::Confirmed));
        assert_eq!(r.head.revision, digest);
        assert_eq!(r.head.detail, "the tag still points to the same manifest");
        // The weights are asked about with HEAD only.
        assert!(p.asked().iter().filter(|a| a.contains("/blobs/")).all(|a| a.starts_with("HEAD ")));

        // The tag was moved to other weights; the old blob is still served.
        let mut moved = manifest.clone();
        moved["layers"][0]["digest"] = json!(format!("sha256:{}", "dd".repeat(32)));
        let p = Fake::new(&[(&blob, 200, json!("")), (tag, 200, moved)]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Confirmed, Verdict::Changed));
        assert_eq!(r.head.diff[0].now, "dd".repeat(32));

        // A record whose revision matches the tag but whose weights do not (a
        // record someone made up): the verdict and its wording agree.
        let forged = record_from_import("ollama", "library/tiny:1b", &digest, vec![("tiny-1b.gguf".to_string(), 9, "ee".repeat(32), true)]).unwrap();
        let forged_blob = format!("HEAD https://registry.ollama.ai/v2/library/tiny/blobs/sha256:{}", "ee".repeat(32));
        let p = Fake::new(&[(&forged_blob, 404, json!("")), (tag, 200, manifest.clone())]);
        let r = p.check(&forged);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Removed, Verdict::Changed));
        assert!(r.head.detail.contains("different hash"), "{}", r.head.detail);

        // The blob and the tag are both gone; and a registry that is down.
        let p = Fake::new(&[(&blob, 404, json!("")), (tag, 404, json!(""))]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Removed, Verdict::Removed));
        let r = Fake::new(&[]).check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Unreachable, Verdict::Unreachable));
    }

    #[test]
    fn civitai_is_checked_by_version_and_by_the_models_own_listing() {
        let sha = "aa".repeat(32);
        let rec = record_from_import("civitai", "4384", "128713", vec![("Dream_8.safetensors".to_string(), 2048, sha.clone(), true)]).unwrap();
        assert_eq!(rec.link(), "https://civitai.com/models/4384?modelVersionId=128713");
        let file = |name: &str, sha: &str| json!({"name": name, "sizeKB": 2.0, "hashes": {"SHA256": sha.to_ascii_uppercase()}});
        let version = "GET https://civitai.com/api/v1/model-versions/128713";
        let model = "GET https://civitai.com/api/v1/models/4384";
        let listed = |files: Value| json!({"id": 4384, "modelVersions": [{"id": 999, "files": []}, {"id": 128713, "files": files}]});

        let p = Fake::new(&[(version, 200, json!({"id": 128713, "files": [file("Dream 8.safetensors", &sha)]})), (model, 200, listed(json!([file("Dream 8.safetensors", &sha)])))]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Confirmed, Verdict::Confirmed));
        assert_eq!(r.head.revision, "999");
        assert!(r.head.detail.contains("a newer version is listed"));

        // The file under that version was replaced: same name, other bytes.
        let p = Fake::new(&[(version, 200, json!({"id": 128713, "files": [file("Dream 8.safetensors", &"dd".repeat(32))]})), (model, 200, listed(json!([file("Dream 8.safetensors", &"dd".repeat(32))])))]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Changed, Verdict::Changed));
        assert_eq!(r.pinned.diff[0].now, "dd".repeat(32));

        // The version was taken down and is no longer listed under the model.
        let p = Fake::new(&[(version, 404, json!({})), (model, 200, json!({"id": 4384, "modelVersions": [{"id": 999, "files": []}]}))]);
        let r = p.check(&rec);
        assert_eq!((r.pinned.verdict, r.head.verdict), (Verdict::Removed, Verdict::Removed));
        // Civitai's answer for an id it cannot parse is not evidence.
        let p = Fake::new(&[(version, 400, json!({})), (model, 400, json!({}))]);
        assert_eq!(p.check(&rec).verdict(), Verdict::Unreachable);
    }

    #[test]
    fn a_re_check_becomes_one_history_line_naming_both_scopes() {
        let rec = hf_record();
        let mut moved = hf_listing();
        moved[1]["lfs"]["oid"] = json!("dd".repeat(32));
        let p = Fake::new(&[(META, 200, json!({"id": "org/model", "sha": HEAD})), (&paths(REV), 200, hf_listing()), (&paths(HEAD), 200, moved)]);
        let ev = check_event("bafytestcid000", "m", &rec, &p.check(&rec), "operator", 9_000);
        assert_eq!((ev.event.as_str(), ev.pinned.as_str(), ev.head.as_str()), ("changed", "confirmed", "changed"));
        assert_eq!(ev.head_revision, HEAD);
        assert_eq!(ev.record, rec.id());
        assert!(ev.detail.starts_with("pinned revision: all 2 published"), "{}", ev.detail);
        assert_eq!(ev.diff.len(), 1);
        assert!(ev.files.is_empty());

        let t = Tmp::new("event");
        append_event(&t.file(), recorded_event("bafytestcid000", "m", &rec, 1_000)).unwrap();
        append_event(&t.file(), ev.clone()).unwrap();
        let chain = read_chain(&t.file());
        assert!(chain.broken.is_empty());
        assert_eq!(chain.events[1].1.diff, ev.diff);
        // An entry too large to carry its record finds it again in the history.
        let bare = ModelEntry { cid: "bafytestcid000".into(), name: "m".into(), ..Default::default() };
        assert_eq!(record_of(&bare, &chain), Some(rec));
    }

    #[test]
    fn the_timer_waits_about_a_week_and_only_for_this_orbs_own_imports() {
        let rec = hf_record();
        let id = rec.id();
        let recorded = recorded_event("bafytestcid000", "m", &rec, 0);
        let recorded = Event { at_ms: 1_000_000, ..recorded };
        let check = |event: &str, at: i64| Event { event: event.into(), at_ms: at, record: id.clone(), ..Default::default() };

        // A mirror (no `recorded` line of this Orb's own) is never due.
        assert_eq!(next_due_ms(&id, &[&check("confirmed", 5)]), None);
        assert_eq!(next_due_ms(&id, &[]), None);

        // An import: first due a week later, give or take half a day.
        let due = next_due_ms(&id, &[&recorded]).unwrap();
        assert!((due - recorded.at_ms - WEEK_MS).abs() <= DAY_MS / 2, "{due}");
        // …counted from the latest answer, whatever it was.
        let later = check("changed", recorded.at_ms + 10 * DAY_MS);
        let due2 = next_due_ms(&id, &[&recorded, &later]).unwrap();
        assert_eq!(due2 - later.at_ms, due - recorded.at_ms);
        // No answer: try again after a day, not a week.
        let failed = check("unreachable", recorded.at_ms + 20 * DAY_MS);
        assert_eq!(next_due_ms(&id, &[&recorded, &later, &failed]).unwrap(), failed.at_ms + DAY_MS);
        // Different records are spread apart.
        let spread: HashSet<i64> = (0..20u8).map(|i| next_due_ms(&sha256_hex(&[i]), &[&recorded]).unwrap()).collect();
        assert!(spread.len() > 15);
    }
}
