//! Firewall + NAT + port-forward rule editor.
//!
//! Rules live in /etc/aeon/firewall.toml as an ordered array. On each
//! change we regenerate a `iptables-restore`-compatible script, atomic-
//! rename it into place, and run iptables-restore --noflush so user
//! rules don't blow away the system rules from aeon-net-services or
//! aeon-usb-net.
//!
//! Hit counts come from `iptables -nvL --line-numbers -t <table>` —
//! parsed once per GET. We tag each rule with a stable comment string
//! (`aeon-fw <id>`) so we can correlate file state with kernel state.
//!
//! Redundancy detection: a later rule is flagged "redundant_with N" if
//! some earlier rule in the same chain has the same action AND its
//! source/destination/port predicates are a superset (each non-empty
//! predicate on the later rule must also be set the same on the earlier
//! one, OR the earlier rule's predicate must be empty = "any").

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

const FIREWALL_TOML: &str = "/etc/aeon/firewall.toml";

/// One firewall rule on disk. Order matters — the array index IS the
/// iptables insertion order (within its chain).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    pub id: String,
    #[serde(default = "default_chain")]
    pub chain: String,
    #[serde(default = "default_table")]
    pub table: String,
    #[serde(default = "default_direction")]
    pub direction: String,
    #[serde(default)]
    pub iface: String,
    #[serde(default)]
    pub proto: String,
    #[serde(default)]
    pub src: String,
    #[serde(default)]
    pub dst: String,
    #[serde(default)]
    pub sport: String,
    #[serde(default)]
    pub dport: String,
    #[serde(default = "default_action")]
    pub action: String,
    #[serde(default)]
    pub comment: String,
    /// REDIRECT --to-ports / DNAT --to-destination. Free-form string we
    /// pass-through to iptables. For SNAT/DNAT this is "ip:port", for
    /// REDIRECT it's a port number.
    #[serde(default)]
    pub target_arg: String,
}

fn default_chain() -> String { "INPUT".to_string() }
fn default_table() -> String { "filter".to_string() }
fn default_direction() -> String { "inbound".to_string() }
fn default_action() -> String { "ACCEPT".to_string() }

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RulesFile {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn read_rules() -> RulesFile {
    std::fs::read_to_string(FIREWALL_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_rules(rf: &RulesFile) -> std::io::Result<()> {
    let text = toml::to_string_pretty(rf)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(parent) = std::path::Path::new(FIREWALL_TOML).parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Atomic write: tmpfile + rename.
    let tmp = format!("{FIREWALL_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, FIREWALL_TOML)?;
    Ok(())
}

/// Build the iptables CLI arguments for one rule.
fn rule_to_args(r: &Rule) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if !r.table.is_empty() && r.table != "filter" {
        args.push("-t".into());
        args.push(r.table.clone());
    }
    // -A appends. Reorder is handled by rewriting the chain entirely.
    args.push("-A".into());
    args.push(r.chain.clone());
    if !r.iface.is_empty() {
        // Inbound rules use -i, outbound use -o; PREROUTING/INPUT/FORWARD
        // see packets on the inbound interface; OUTPUT/POSTROUTING see
        // them on outbound. We pick based on chain.
        let flag = match r.chain.as_str() {
            "OUTPUT" | "POSTROUTING" => "-o",
            _ => "-i",
        };
        args.push(flag.into());
        args.push(r.iface.clone());
    }
    if !r.proto.is_empty() {
        args.push("-p".into());
        args.push(r.proto.clone());
    }
    if !r.src.is_empty() {
        args.push("-s".into());
        args.push(r.src.clone());
    }
    if !r.dst.is_empty() {
        args.push("-d".into());
        args.push(r.dst.clone());
    }
    if !r.sport.is_empty() {
        args.push("--sport".into());
        args.push(r.sport.clone());
    }
    if !r.dport.is_empty() {
        args.push("--dport".into());
        args.push(r.dport.clone());
    }
    args.push("-j".into());
    args.push(r.action.clone());
    if !r.target_arg.is_empty() {
        match r.action.as_str() {
            "REDIRECT" => {
                args.push("--to-ports".into());
                args.push(r.target_arg.clone());
            }
            "DNAT" | "SNAT" => {
                args.push("--to-destination".into());
                args.push(r.target_arg.clone());
            }
            _ => { /* MASQUERADE / ACCEPT / DROP / REJECT take no arg */ }
        }
    }
    // Tag every aeon-fw rule with a comment so we can match it to disk
    // state later. iptables -m comment requires the match module.
    args.push("-m".into());
    args.push("comment".into());
    args.push("--comment".into());
    args.push(format!("aeon-fw {} {}", r.id,
        // Keep the user comment as part of the tag but sanitize whitespace.
        r.comment.replace(['\n', '\r'], " ").chars().take(100).collect::<String>()));
    args
}

/// Sweep any existing "aeon-fw"-tagged rules from the kernel (so we
/// don't accumulate duplicates each apply), then re-insert the current
/// rule set in order. Same line-number-driven delete pattern as
/// network.rs's iptables sweep.
fn apply_rules(rf: &RulesFile) -> Result<(), String> {
    // 1. Sweep.
    for table in &["filter", "nat", "mangle"] {
        for chain in &["INPUT", "OUTPUT", "FORWARD", "PREROUTING", "POSTROUTING"] {
            let out = Command::new("iptables")
                .args(["-t", table, "-L", chain, "--line-numbers", "-n"])
                .output();
            let Ok(out) = out else { continue };
            let text = String::from_utf8_lossy(&out.stdout);
            // Collect line numbers (column 1) of rules containing "aeon-fw".
            let mut nums: Vec<u32> = text
                .lines()
                .filter(|l| l.contains("aeon-fw"))
                .filter_map(|l| l.split_whitespace().next())
                .filter_map(|s| s.parse::<u32>().ok())
                .collect();
            // Delete top-down to keep numbering stable.
            nums.sort_unstable();
            nums.reverse();
            for n in nums {
                let _ = Command::new("iptables")
                    .args(["-t", table, "-D", chain, &n.to_string()])
                    .status();
            }
        }
    }
    // 2. Re-apply in the file's order.
    for r in &rf.rules {
        let args = rule_to_args(r);
        let st = Command::new("iptables")
            .args(args.iter().map(|s| s.as_str()))
            .status()
            .map_err(|e| format!("spawn iptables: {e}"))?;
        if !st.success() {
            return Err(format!(
                "iptables exited non-zero applying rule '{}'",
                r.id
            ));
        }
    }
    Ok(())
}

/// Parse `iptables -t <table> -nvL <chain>` to extract (packets, bytes)
/// for each "aeon-fw"-tagged rule. Returns a map id → (packets, bytes).
fn read_hit_counts() -> HashMap<String, (u64, u64)> {
    let mut out: HashMap<String, (u64, u64)> = HashMap::new();
    for table in &["filter", "nat", "mangle"] {
        let cmd = Command::new("iptables")
            .args(["-t", table, "-nvL", "--line-numbers"])
            .output();
        let Ok(cmd) = cmd else { continue };
        let text = String::from_utf8_lossy(&cmd.stdout);
        for line in text.lines() {
            if !line.contains("aeon-fw") {
                continue;
            }
            // iptables -nvL format:
            // num   pkts bytes target prot opt in     out source dest ... /* aeon-fw <id> ... */
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                continue;
            }
            let pkts: u64 = parts[1].parse().unwrap_or(0);
            let bts: u64 = parts[2].parse().unwrap_or(0);
            // Find the id in the comment: ".../* aeon-fw <id> ..."
            if let Some(after) = line.find("aeon-fw ") {
                let tail = &line[after + "aeon-fw ".len()..];
                let id: String = tail
                    .chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect();
                if !id.is_empty() {
                    out.insert(id, (pkts, bts));
                }
            }
        }
    }
    out
}

/// Predicate-superset check used to flag redundant rules. Given a rule
/// candidate, walk earlier rules in the same chain: if any matches the
/// same action AND every non-empty predicate on `candidate` is matched
/// (either equal, or earlier rule predicate is empty = "any"), the
/// candidate is redundant.
fn find_redundant_with(rules: &[Rule], idx: usize) -> Option<usize> {
    let candidate = &rules[idx];
    for (j, prior) in rules.iter().enumerate().take(idx) {
        if prior.chain != candidate.chain || prior.table != candidate.table {
            continue;
        }
        if prior.action != candidate.action {
            continue;
        }
        // For each predicate: prior must be empty OR equal to candidate.
        // (If the candidate predicate is empty, that means "any" — so a
        // matching prior with the same "any" or any specific value still
        // covers the candidate from a deeper sense; but to keep the
        // heuristic safe we require: prior_empty OR prior == candidate.)
        let preds = [
            (&prior.iface,  &candidate.iface),
            (&prior.proto,  &candidate.proto),
            (&prior.src,    &candidate.src),
            (&prior.dst,    &candidate.dst),
            (&prior.sport,  &candidate.sport),
            (&prior.dport,  &candidate.dport),
        ];
        let covers = preds.iter().all(|(p, c)|
            p.is_empty() || p.trim() == c.trim()
        );
        if covers {
            return Some(j + 1); // 1-based for display
        }
    }
    None
}

/// GET /api/firewall/rules — list all rules + live hit counts +
/// redundancy flags.
pub async fn list_rules(State(_state): State<AppState>) -> Json<Value> {
    let rf = read_rules();
    let hits = read_hit_counts();
    let out: Vec<Value> = rf
        .rules
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (pkts, bts) = hits.get(&r.id).copied().unwrap_or((0, 0));
            let redundant = find_redundant_with(&rf.rules, i);
            json!({
                "id": r.id,
                "chain": r.chain,
                "table": r.table,
                "direction": r.direction,
                "iface": r.iface,
                "proto": r.proto,
                "src": r.src,
                "dst": r.dst,
                "sport": r.sport,
                "dport": r.dport,
                "action": r.action,
                "comment": r.comment,
                "packets": pkts,
                "bytes": bts,
                "redundant_with": redundant,
            })
        })
        .collect();
    Json(json!({"ok": true, "rules": out}))
}

#[derive(Deserialize)]
pub struct RuleDraft {
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(rename = "interface", default)]
    pub iface: Option<String>,
    #[serde(default)]
    pub proto: Option<String>,
    #[serde(default)]
    pub src: Option<String>,
    #[serde(default)]
    pub dst: Option<String>,
    #[serde(default)]
    pub sport: Option<String>,
    #[serde(default)]
    pub dport: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub target_arg: Option<String>,
}

const VALID_CHAINS: &[&str] =
    &["INPUT", "OUTPUT", "FORWARD", "PREROUTING", "POSTROUTING"];
const VALID_TABLES: &[&str] = &["filter", "nat", "mangle"];
const VALID_ACTIONS: &[&str] = &[
    "ACCEPT", "DROP", "REJECT", "DNAT", "SNAT", "REDIRECT", "MASQUERADE", "LOG",
];

fn safe_str(s: &str) -> bool {
    !s.contains('\n')
        && !s.contains('\r')
        && !s.contains(';')
        && !s.contains('`')
        && !s.contains('$')
        && s.len() <= 128
}

/// POST /api/firewall/rules — add a new rule.
pub async fn add_rule(
    State(_state): State<AppState>,
    Json(req): Json<RuleDraft>,
) -> impl IntoResponse {
    let chain = req.chain.unwrap_or_else(default_chain);
    let table = req.table.unwrap_or_else(default_table);
    let action = req.action.unwrap_or_else(default_action);
    if !VALID_CHAINS.contains(&chain.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": format!("unknown chain '{chain}'")})),
        )
            .into_response();
    }
    if !VALID_TABLES.contains(&table.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": format!("unknown table '{table}'")})),
        )
            .into_response();
    }
    if !VALID_ACTIONS.contains(&action.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "err": format!("unknown action '{action}'")})),
        )
            .into_response();
    }
    // Reject shell-injection-flavored input. iptables itself is
    // exec'd via argv (no shell), but persistence to TOML + later
    // parsing wants strings that can't break the file format either.
    let to_check = [
        req.iface.as_deref().unwrap_or(""),
        req.proto.as_deref().unwrap_or(""),
        req.src.as_deref().unwrap_or(""),
        req.dst.as_deref().unwrap_or(""),
        req.sport.as_deref().unwrap_or(""),
        req.dport.as_deref().unwrap_or(""),
        req.target_arg.as_deref().unwrap_or(""),
    ];
    for s in &to_check {
        if !safe_str(s) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "err": format!("invalid characters in field '{s}'")})),
            )
                .into_response();
        }
    }

    let id = generate_id();
    let rule = Rule {
        id: id.clone(),
        chain,
        table,
        direction: req.direction.unwrap_or_else(default_direction),
        iface: req.iface.unwrap_or_default(),
        proto: req.proto.unwrap_or_default(),
        src: req.src.unwrap_or_default(),
        dst: req.dst.unwrap_or_default(),
        sport: req.sport.unwrap_or_default(),
        dport: req.dport.unwrap_or_default(),
        action,
        comment: req.comment.unwrap_or_default(),
        target_arg: req.target_arg.unwrap_or_default(),
    };

    let mut rf = read_rules();
    rf.rules.push(rule);
    if let Err(e) = write_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    if let Err(e) = apply_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("apply: {e}")})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "id": id})).into_response()
}

/// DELETE /api/firewall/rules/:id
pub async fn delete_rule(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut rf = read_rules();
    let before = rf.rules.len();
    rf.rules.retain(|r| r.id != id);
    if rf.rules.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "err": "unknown rule"})),
        )
            .into_response();
    }
    if let Err(e) = write_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    if let Err(e) = apply_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("apply: {e}")})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub struct MoveReq {
    pub delta: i32,
}

/// POST /api/firewall/rules/:id/move — shift a rule up/down within its
/// chain. delta=-1 moves up, +1 moves down.
pub async fn move_rule(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<MoveReq>,
) -> impl IntoResponse {
    let mut rf = read_rules();
    let Some(idx) = rf.rules.iter().position(|r| r.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "err": "unknown rule"})),
        )
            .into_response();
    };
    let target_chain = rf.rules[idx].chain.clone();
    // Find neighbor in same chain to swap with.
    let neighbor = if req.delta < 0 {
        rf.rules[..idx].iter().rposition(|r| r.chain == target_chain)
    } else {
        rf.rules[idx + 1..]
            .iter()
            .position(|r| r.chain == target_chain)
            .map(|p| idx + 1 + p)
    };
    let Some(j) = neighbor else {
        return Json(json!({"ok": true, "moved": false})).into_response();
    };
    rf.rules.swap(idx, j);
    if let Err(e) = write_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("persist: {e}")})),
        )
            .into_response();
    }
    if let Err(e) = apply_rules(&rf) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "err": format!("apply: {e}")})),
        )
            .into_response();
    }
    Json(json!({"ok": true, "moved": true})).into_response()
}

/// Generate a short random id (no auth, just disambiguation in iptables
/// comments). Hex of 6 random bytes = 12 chars, plenty unique.
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // Mix nanos with a per-process counter to avoid same-tick collisions.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mixed = nanos.wrapping_add(n as u128);
    format!("{:012x}", (mixed as u64) & 0xffff_ffff_ffff)
}
