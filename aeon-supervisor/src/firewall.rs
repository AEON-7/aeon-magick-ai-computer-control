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
    /// Outbound interface match (`-o <name>`). Only meaningful in FORWARD
    /// (where both `-i` and `-o` qualify the rule), OUTPUT, and
    /// POSTROUTING chains. Ignored on INPUT / PREROUTING (no concept of
    /// "out interface" before routing).
    #[serde(default)]
    pub out_iface: String,
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

/// v39+: the central AEON_DROP chain is gone. Each DROP point in the
/// system (aeon-net-services.sh, aeon-usb-net.sh, user firewall
/// rules) now emits its own inline LOG + DROP pair with a unique
/// log prefix tag `AEON-DROP[<tag>]:` so blocked_log.rs can attribute
/// the block to a specific rule. We keep this function as a no-op
/// for ABI stability with anything that called it from main.rs etc.
pub fn ensure_drop_chain() {
    // Best-effort flush + delete of any lingering AEON_DROP chain
    // from a pre-v39 image, so it doesn't sit there confusingly with
    // no references. Safe — iptables errors if the chain is still
    // referenced or doesn't exist, both of which we ignore.
    use std::process::Command;
    let _ = Command::new("iptables").args(["-F", "AEON_DROP"]).status();
    let _ = Command::new("iptables").args(["-X", "AEON_DROP"]).status();
}

/// Build just the predicate prefix of an iptables -I command:
/// `[-t table] -I chain 1 [-i|-o iface] [-o out_iface] [-p proto]
///  [-s src] [-d dst] [--sport sp] [--dport dp]`
/// Used as the common prefix for both the DROP invocation AND its
/// paired LOG invocation, so they match the same packets.
fn predicate_prefix(r: &Rule) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if !r.table.is_empty() && r.table != "filter" {
        args.push("-t".into());
        args.push(r.table.clone());
    }
    args.push("-I".into());
    args.push(r.chain.clone());
    args.push("1".into());
    if !r.iface.is_empty() {
        let flag = match r.chain.as_str() {
            "OUTPUT" | "POSTROUTING" => "-o",
            _ => "-i",
        };
        args.push(flag.into());
        args.push(r.iface.clone());
    }
    if !r.out_iface.is_empty() && r.chain == "FORWARD" {
        args.push("-o".into());
        args.push(r.out_iface.clone());
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
    args
}

fn comment_args(r: &Rule, suffix: &str) -> Vec<String> {
    let sanitized: String = r
        .comment
        .replace(['\n', '\r'], " ")
        .chars()
        .take(100)
        .collect();
    vec![
        "-m".into(),
        "comment".into(),
        "--comment".into(),
        format!("aeon-fw {} {} {}", r.id, suffix, sanitized).trim_end().to_string(),
    ]
}

/// Build the iptables invocation(s) for one rule. User rules use
/// `-I <chain> 1` (insert at position 1) so they evaluate BEFORE
/// system-installed rules (aeon-vpn / aeon-usb-net) — that's what
/// makes the "allow this traffic" button actually take effect.
///
/// DROP rules emit TWO invocations (LOG + DROP) with identical
/// predicates. Each LOG carries a unique prefix `AEON-DROP[fw-<id>]:`
/// so blocked_log.rs can attribute the block to a specific rule. To
/// land both at the top in the right order, apply_rules calls them in
/// reverse: DROP gets inserted first (lands at pos 1), then LOG gets
/// inserted (lands at pos 1, pushing DROP to pos 2). End: [LOG, DROP].
///
/// To preserve user rule order within the user-rule space: apply_rules
/// iterates the on-disk array in REVERSE, so the first rule in
/// firewall.toml ends up at the topmost position after all inserts.
fn rule_to_invocations(r: &Rule) -> Vec<Vec<String>> {
    let prefix = predicate_prefix(r);
    let target_args = |action: &str, target_arg: &str| -> Vec<String> {
        let mut a = vec!["-j".into(), action.to_string()];
        match action {
            "REDIRECT" if !target_arg.is_empty() => {
                a.push("--to-ports".into());
                a.push(target_arg.to_string());
            }
            "DNAT" | "SNAT" if !target_arg.is_empty() => {
                a.push("--to-destination".into());
                a.push(target_arg.to_string());
            }
            _ => { /* nothing */ }
        }
        a
    };

    if r.action == "DROP" || r.action == "REJECT" {
        // Two invocations: DROP first, LOG second. apply_rules calls
        // them in array order; both use -I 1 so the SECOND one ends
        // up above the first. That puts LOG above DROP in the chain
        // — which is what we want (log then drop).
        let mut drop_inv = prefix.clone();
        drop_inv.extend(target_args(&r.action, &r.target_arg));
        drop_inv.extend(comment_args(r, "drop"));

        let mut log_inv = prefix.clone();
        log_inv.extend_from_slice(&[
            "-m".into(),
            "limit".into(),
            "--limit".into(),
            "5/sec".into(),
            "--limit-burst".into(),
            "10".into(),
            "-j".into(),
            "LOG".into(),
            "--log-prefix".into(),
            format!("AEON-DROP[fw-{}]: ", r.id),
            "--log-level".into(),
            "4".into(),
        ]);
        log_inv.extend(comment_args(r, "log"));

        vec![drop_inv, log_inv]
    } else {
        // Single invocation for ACCEPT / REDIRECT / DNAT / SNAT / etc.
        let mut inv = prefix;
        inv.extend(target_args(&r.action, &r.target_arg));
        inv.extend(comment_args(r, ""));
        vec![inv]
    }
}

/// Sweep any existing "aeon-fw"-tagged rules from the kernel (so we
/// don't accumulate duplicates each apply), then re-insert the current
/// rule set in order. Same line-number-driven delete pattern as
/// network.rs's iptables sweep.
fn apply_rules(rf: &RulesFile) -> Result<(), String> {
    // 0. Make sure the AEON_DROP chain exists before any rule that
    // jumps to it (any user-created DROP rule does).
    ensure_drop_chain();
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
    // 2. Re-apply in REVERSE order. Each invocation is `iptables -I <chain> 1 ...`
    //    so inserting in reverse puts the first rule on disk at the topmost
    //    position. For DROP/REJECT rules each emits TWO invocations
    //    (DROP first, LOG second) — both use -I 1, so LOG lands above
    //    DROP in the chain. End-to-end ordering: [r1-LOG, r1-DROP,
    //    r2-LOG, r2-DROP, ..., system rules].
    for r in rf.rules.iter().rev() {
        for inv in rule_to_invocations(r) {
            let st = Command::new("iptables")
                .args(inv.iter().map(|s| s.as_str()))
                .status()
                .map_err(|e| format!("spawn iptables: {e}"))?;
            if !st.success() {
                return Err(format!(
                    "iptables exited non-zero applying rule '{}'",
                    r.id
                ));
            }
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
                "out_iface": r.out_iface,
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

/// GET /api/firewall/system-rules — read-only view of every rule that
/// aeon-net-services.sh, aeon-usb-net.sh, or the AEON_DROP chain
/// installed. Parsed live from `iptables -nvL` so it always reflects
/// the actual kernel state. The UI uses this to show users what the
/// device is enforcing under the hood + lets them click "override"
/// to create a user ACCEPT with the same predicates flipped to allow.
pub async fn list_system_rules(State(_state): State<AppState>) -> Json<Value> {
    let mut out: Vec<Value> = Vec::new();
    for table in &["filter", "nat", "mangle"] {
        let cmd = Command::new("iptables")
            .args(["-t", table, "-nvL", "--line-numbers"])
            .output();
        let Ok(cmd) = cmd else { continue };
        let text = String::from_utf8_lossy(&cmd.stdout);
        let mut current_chain = String::new();
        for line in text.lines() {
            // Chain headers look like "Chain INPUT (policy ACCEPT 0 packets, 0 bytes)"
            if let Some(rest) = line.strip_prefix("Chain ") {
                if let Some(name) = rest.split_whitespace().next() {
                    current_chain = name.to_string();
                }
                continue;
            }
            // We want rules tagged aeon-vpn / aeon-usb-net (system) but
            // NOT aeon-fw (user) — user rules already show up in the
            // editable list.
            let source = if line.contains("aeon-vpn") {
                "aeon-net-services"
            } else if line.contains("aeon-usb-net") {
                "aeon-usb-net"
            } else {
                continue;
            };
            // Parse iptables -nvL columns:
            // num  pkts bytes target  prot  opt in   out  source dest   [match-options] /* comment */
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 9 {
                continue;
            }
            // Skip the AEON_DROP chain's own internal LOG/DROP rules
            // (they're plumbing, not policy).
            if current_chain == "AEON_DROP" {
                continue;
            }
            let _num = cols[0];
            let pkts: u64 = cols[1].parse().unwrap_or(0);
            let bts: u64 = cols[2].parse().unwrap_or(0);
            let target = cols[3].to_string();
            let proto = cols[4].to_string();
            // opt = cols[5]
            let in_if = cols[6].to_string();
            let out_if = cols[7].to_string();
            let src = cols[8].to_string();
            let dst = cols.get(9).copied().unwrap_or("").to_string();
            // Capture useful match options the user might want to see:
            // dpt, spt, etc. They show up as tokens like "dpt:53" or
            // "udp dpt:53". The comment is in /* */ delimiters.
            let mut extras: Vec<String> = Vec::new();
            let mut dport = String::new();
            let mut sport = String::new();
            for tok in &cols[10..] {
                if let Some(p) = tok.strip_prefix("dpt:") {
                    dport = p.to_string();
                    extras.push((*tok).to_string());
                } else if let Some(p) = tok.strip_prefix("spt:") {
                    sport = p.to_string();
                    extras.push((*tok).to_string());
                } else if let Some(p) = tok.strip_prefix("dpts:") {
                    dport = p.to_string();
                    extras.push((*tok).to_string());
                } else if !tok.starts_with("/*") && !tok.starts_with("*/") {
                    extras.push((*tok).to_string());
                }
                if tok.starts_with("*/") {
                    break;
                }
            }
            // 0.0.0.0/0 → "any" for cleaner display
            let pretty = |s: String| -> String {
                if s == "0.0.0.0/0" || s == "::/0" {
                    "any".to_string()
                } else {
                    s
                }
            };
            // Map kernel target to the user-facing "effect" label:
            // AEON_DROP → drop (with logging), AEON_USER_X → ignore, etc.
            let effect = if target == "AEON_DROP" { "DROP (logged)".to_string() }
                else { target.clone() };

            out.push(json!({
                "source": source,
                "table": table,
                "chain": current_chain,
                "target": target,
                "effect": effect,
                "proto": if proto == "all" || proto == "0" { "any".to_string() } else { proto },
                "iface": if in_if == "*" { String::new() } else { in_if.clone() },
                "out_iface": if out_if == "*" { String::new() } else { out_if.clone() },
                "src": pretty(src),
                "dst": pretty(dst),
                "sport": sport,
                "dport": dport,
                "packets": pkts,
                "bytes": bts,
                "match_options": extras.join(" "),
            }));
        }
    }
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
    pub out_iface: Option<String>,
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
        out_iface: req.out_iface.unwrap_or_default(),
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
