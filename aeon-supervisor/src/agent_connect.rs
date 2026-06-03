//! Agent Dash — Connected Systems registry + SSH key provisioning.
//!
//! Foundation for connecting the Pi to AI-agent gateways (OpenClaw / Hermes)
//! and DGX Sparks. The Pi keeps its OWN agent-connect SSH keypair. The user
//! registers a system by address; a one-time SSH password handshake appends
//! the Pi's public key to the target's `~/.ssh/authorized_keys`, so the Pi
//! then has key-auth access for metrics / info / provisioning. If password
//! auth isn't available, the UI shows a command to run on the target instead.
//!
//! Metrics, the per-agent roster, and per-agent provisioning hang off this
//! registry (built incrementally on top).

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::AppState;

const DIR: &str = "/var/lib/aeon/agent-connect";

fn key_path() -> PathBuf {
    PathBuf::from(DIR).join("id_ed25519")
}
fn pub_path() -> PathBuf {
    PathBuf::from(DIR).join("id_ed25519.pub")
}
fn systems_path() -> PathBuf {
    PathBuf::from(DIR).join("systems.json")
}

#[derive(Serialize, Deserialize, Clone)]
struct System {
    id: String,
    label: String,
    address: String,
    #[serde(default = "default_user")]
    ssh_user: String,
    #[serde(default = "default_port")]
    port: u16,
    /// "openclaw" | "hermes" | "dgx" (a unified box can have several).
    #[serde(default)]
    roles: Vec<String>,
    /// "pending" | "connected" | "unreachable"
    #[serde(default)]
    status: String,
    #[serde(default)]
    last_checked_ms: i64,
}
fn default_user() -> String {
    "root".into()
}
fn default_port() -> u16 {
    22
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Generate the agent-connect keypair if absent; return the public key.
fn ensure_pubkey() -> Result<String, String> {
    let _ = std::fs::create_dir_all(DIR);
    if !key_path().exists() {
        let st = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "aeon-magick-agent-connect", "-f"])
            .arg(key_path())
            .output()
            .map_err(|e| format!("ssh-keygen: {e}"))?;
        if !st.status.success() {
            return Err(format!(
                "ssh-keygen failed: {}",
                String::from_utf8_lossy(&st.stderr)
            ));
        }
    }
    std::fs::read_to_string(pub_path())
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("read pubkey: {e}"))
}

fn authorize_command(pubkey: &str) -> String {
    format!("mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo '{pubkey}' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys")
}

fn load_systems() -> Vec<System> {
    std::fs::read(systems_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The agent-connect SSH private key path, authorized on every registered
/// system (ssh_user). The E2 web terminal SSHes in using THIS key.
pub fn agent_key_path() -> PathBuf {
    key_path()
}

/// SSH connection parameters for a registered system (resolved by id).
/// Used by the E2 web-terminal bridge to spawn `ssh` against the box.
pub struct SshTarget {
    pub label: String,
    pub address: String,
    pub ssh_user: String,
    pub port: u16,
}

/// Look up a registered system by id and return its SSH connection params,
/// or `None` if no such system. (Keeps the `System` struct private.)
pub fn ssh_target(id: &str) -> Option<SshTarget> {
    load_systems().into_iter().find(|s| s.id == id).map(|s| SshTarget {
        label: s.label,
        address: s.address,
        ssh_user: s.ssh_user,
        port: s.port,
    })
}

fn save_systems(v: &[System]) -> Result<(), String> {
    let _ = std::fs::create_dir_all(DIR);
    let text = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    let tmp = systems_path().with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, systems_path()).map_err(|e| format!("rename: {e}"))
}

/// Does the Pi's key authenticate to the target? (quick `echo` probe)
fn key_auth_works(sys: &System) -> bool {
    let target = format!("{}@{}", sys.ssh_user, sys.address);
    Command::new("ssh")
        .arg("-i")
        .arg(key_path())
        .args([
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=8",
            "-o", "PreferredAuthentications=publickey",
            "-p", &sys.port.to_string(),
            &target, "echo", "aeon-ok",
        ])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("aeon-ok"))
        .unwrap_or(false)
}

// ── handlers ────────────────────────────────────────────────────────────

/// GET /agent/pubkey — the Pi's agent-connect public key + the command to
/// authorize it on a target box manually.
pub async fn get_pubkey() -> impl IntoResponse {
    match ensure_pubkey() {
        Ok(pk) => Json(json!({"ok": true, "pubkey": pk, "authorize_command": authorize_command(&pk)})),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// GET /agent/systems
pub async fn list_systems() -> impl IntoResponse {
    Json(json!({"ok": true, "systems": load_systems()}))
}

#[derive(Deserialize)]
pub struct AddSystemReq {
    #[serde(default)]
    label: String,
    address: String,
    #[serde(default = "default_user")]
    ssh_user: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default)]
    roles: Vec<String>,
}

/// POST /agent/systems
pub async fn add_system(Json(req): Json<AddSystemReq>) -> impl IntoResponse {
    if req.address.trim().is_empty() {
        return Json(json!({"ok": false, "err": "address required"}));
    }
    let mut systems = load_systems();
    let id = format!("sys_{}", now_ms());
    let label = if req.label.trim().is_empty() {
        req.address.clone()
    } else {
        req.label
    };
    systems.push(System {
        id: id.clone(),
        label,
        address: req.address,
        ssh_user: req.ssh_user,
        port: req.port,
        roles: req.roles,
        status: "pending".into(),
        last_checked_ms: 0,
    });
    match save_systems(&systems) {
        Ok(_) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// DELETE /agent/systems/:id
pub async fn remove_system(Path(id): Path<String>) -> impl IntoResponse {
    let mut systems = load_systems();
    let before = systems.len();
    systems.retain(|s| s.id != id);
    if systems.len() == before {
        return Json(json!({"ok": false, "err": "no such system"}));
    }
    match save_systems(&systems) {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize)]
pub struct RegisterReq {
    /// One-time SSH password for the pubkey handshake. NEVER stored.
    #[serde(default)]
    password: String,
}

/// POST /agent/systems/:id/register — push the Pi's pubkey to the target with
/// a one-time password SSH, then verify key auth. On failure, return the
/// manual authorize command so the user can run it on the box themselves.
pub async fn register_system(
    Path(id): Path<String>,
    Json(req): Json<RegisterReq>,
) -> impl IntoResponse {
    let pubkey = match ensure_pubkey() {
        Ok(p) => p,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let mut systems = load_systems();
    let Some(sys) = systems.iter().find(|s| s.id == id).cloned() else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let target = format!("{}@{}", sys.ssh_user, sys.address);
    let port = sys.port.to_string();

    // Try the one-time password handshake (sshpass + ssh-copy-id) if given.
    let mut handshake_err: Option<String> = None;
    if !req.password.is_empty() {
        let out = Command::new("sshpass")
            .args(["-p", &req.password, "ssh-copy-id"])
            .args(["-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10", "-i"])
            .arg(pub_path())
            .args(["-p", &port, &target])
            .output();
        match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                handshake_err = Some(
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .last()
                        .unwrap_or("password handshake failed")
                        .to_string(),
                )
            }
            Err(e) => handshake_err = Some(format!("sshpass not available ({e})")),
        }
    }

    // Verify key auth regardless (covers the "user ran the command manually" path).
    let ok = key_auth_works(&sys);
    for s in systems.iter_mut() {
        if s.id == id {
            s.status = if ok { "connected" } else { "pending" }.into();
            s.last_checked_ms = now_ms();
        }
    }
    let _ = save_systems(&systems);

    if ok {
        Json(json!({"ok": true, "status": "connected"}))
    } else {
        Json(json!({
            "ok": false,
            "status": "pending",
            "err": handshake_err.unwrap_or_else(|| "key auth not working yet".into()),
            "authorize_command": authorize_command(&pubkey),
            "hint": format!("Run the authorize command on {} as {}, then Test.", sys.address, sys.ssh_user),
        }))
    }
}

/// POST /agent/systems/:id/test — re-probe key-auth connectivity.
pub async fn test_system(Path(id): Path<String>) -> impl IntoResponse {
    let mut systems = load_systems();
    let Some(sys) = systems.iter().find(|s| s.id == id).cloned() else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let ok = key_auth_works(&sys);
    for s in systems.iter_mut() {
        if s.id == id {
            s.status = if ok { "connected" } else { "unreachable" }.into();
            s.last_checked_ms = now_ms();
        }
    }
    let _ = save_systems(&systems);
    Json(json!({"ok": ok, "status": if ok {"connected"} else {"unreachable"}}))
}

/// GET /agent/systems/:id/metrics — a live SSH-gathered snapshot of a system
/// (host, load, mem, GPUs, docker containers). The per-agent roster (pantheon)
/// is a separate, gateway-specific integration layered on top later.
pub async fn system_metrics(Path(id): Path<String>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let m = tokio::task::spawn_blocking(move || gather_metrics(&sys))
        .await
        .unwrap_or_else(|_| json!({"reachable": false}));
    Json(json!({"ok": true, "metrics": m}))
}

/// SSH in (one round-trip) and emit key:value lines we parse into metrics.
fn gather_metrics(sys: &System) -> serde_json::Value {
    let target = format!("{}@{}", sys.ssh_user, sys.address);
    let remote = "echo __pad__; \
        echo HOST:$(hostname); \
        echo LOAD:$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null); \
        echo CPU:$(A=$(awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8, $5}' /proc/stat); sleep 0.25; B=$(awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8, $5}' /proc/stat); echo \"$A $B\" | awk '{dt=$3-$1; di=$4-$2; if(dt>0) printf \"%.0f\", 100*(dt-di)/dt}'); \
        echo MEM:$(free -m 2>/dev/null | awk '/Mem:/{print $3\"/\"$2}'); \
        echo GPU:$(nvidia-smi --query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu --format=csv,noheader,nounits 2>/dev/null | head -4 | tr '\\n' ';'); \
        echo DOCKER:$(docker ps --format '{{.Names}}' 2>/dev/null | tr '\\n' ','); \
        echo MAC:$(cat /sys/class/net/$(ip route show default 2>/dev/null | awk '/default/{print $5; exit}')/address 2>/dev/null)";
    let out = Command::new("ssh")
        .arg("-i")
        .arg(key_path())
        .args([
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=8",
            "-o", "ServerAliveInterval=3",
            "-o", "ServerAliveCountMax=3",
            "-p", &sys.port.to_string(),
            // `timeout` caps a slow remote command (e.g. a wedged docker/nvidia)
            // so the metrics endpoint can never hang on a misbehaving box.
            &target, "timeout", "9", "bash", "-lc", remote,
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => parse_metrics(&String::from_utf8_lossy(&o.stdout)),
        Ok(o) => json!({"reachable": false, "err": String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("ssh failed").to_string()}),
        Err(e) => json!({"reachable": false, "err": e.to_string()}),
    }
}

fn parse_metrics(s: &str) -> serde_json::Value {
    let (mut host, mut load, mut mem) = (String::new(), String::new(), String::new());
    let mut cpu = String::new();
    let mut mac = String::new();
    let mut gpus: Vec<serde_json::Value> = Vec::new();
    let mut containers: Vec<String> = Vec::new();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("HOST:") {
            host = v.trim().into();
        } else if let Some(v) = line.strip_prefix("LOAD:") {
            load = v.trim().into();
        } else if let Some(v) = line.strip_prefix("CPU:") {
            cpu = v.trim().into();
        } else if let Some(v) = line.strip_prefix("MEM:") {
            mem = v.trim().into();
        } else if let Some(v) = line.strip_prefix("GPU:") {
            for g in v.split(';').map(str::trim).filter(|x| !x.is_empty()) {
                let f: Vec<&str> = g.split(',').map(str::trim).collect();
                if f.len() >= 5 {
                    gpus.push(json!({"name": f[0], "util": f[1], "mem_used": f[2], "mem_total": f[3], "temp": f[4]}));
                }
            }
        } else if let Some(v) = line.strip_prefix("DOCKER:") {
            containers = v.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
        } else if let Some(v) = line.strip_prefix("MAC:") {
            mac = v.trim().to_string();
        }
    }
    json!({"reachable": true, "host": host, "load": load, "cpu": cpu, "mem": mem, "gpus": gpus, "containers": containers, "mac": mac})
}

// ── per-agent roster (the OpenClaw pantheon) ─────────────────────────────

/// OpenClaw's agents HTTP API port. `GET http://<addr>:9787/agents` returns the
/// live roster — every agent with its emoji/name/model plus token, session and
/// activity stats. (Same endpoint the presto-cockpit dashboard consumes.)
const OPENCLAW_AGENTS_PORT: u16 = 9787;

/// GET /agent/systems/:id/agents — the gateway's pantheon roster for a system.
pub async fn system_agents(Path(id): Path<String>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let v = tokio::task::spawn_blocking(move || fetch_agents(&sys))
        .await
        .unwrap_or_else(|_| json!({"ok": false, "reachable": false, "err": "join error"}));
    Json(v)
}

fn fetch_agents(sys: &System) -> serde_json::Value {
    let url = format!("http://{}:{}/agents", sys.address, OPENCLAW_AGENTS_PORT);
    let out = Command::new("curl")
        .args(["-s", "--max-time", "6", "-H", "Accept: application/json", &url])
        .output();
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            match serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                Ok(j) => json!({
                    "ok": true,
                    "reachable": true,
                    "ts": j.get("ts").cloned().unwrap_or(json!(null)),
                    "warming": j.get("warming").cloned().unwrap_or(json!(false)),
                    "agents": j.get("agents").cloned().unwrap_or_else(|| json!([])),
                }),
                Err(e) => json!({"ok": false, "reachable": false, "err": format!("parse: {e}")}),
            }
        }
        Ok(_) => json!({"ok": false, "reachable": false,
            "err": format!("no agents API at {url} — is the OpenClaw agents endpoint up on :{OPENCLAW_AGENTS_PORT}?")}),
        Err(e) => json!({"ok": false, "reachable": false, "err": format!("curl: {e}")}),
    }
}

// ── local token-usage history (long-timeframe tracking) ──────────────────
//
// The gateway only reports a short rolling window of token usage, so to show
// 30-day / 90-day / 1-year / month / year breakdowns we sample /agents on a
// timer and accumulate per-agent, per-DAY usage locally — taking deltas of the
// gateway's cumulative counters (with reset detection, and a baseline-skip on
// first sight so pre-existing history isn't dumped onto day one). The frontend
// slices the daily series into any timeframe.

const TOKENS_DIR: &str = "/var/lib/aeon/agent-tokens";
const SAMPLE_INTERVAL_S: u64 = 600; // 10 min
const MAX_DAYS: usize = 800; // ~2 years of daily rollups

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}
fn tokens_path(system_id: &str) -> PathBuf {
    PathBuf::from(TOKENS_DIR).join(format!("{}.json", sanitize_id(system_id)))
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct AgentName {
    name: String,
    emoji: String,
}
#[derive(Serialize, Deserialize, Default)]
struct TokenStore {
    #[serde(default)]
    first_day: String,
    /// last cumulative counters seen per agent → [total, in, out] (for deltas)
    #[serde(default)]
    last: std::collections::HashMap<String, [i64; 3]>,
    /// daily usage deltas: date → agent_id → [total, in, out] used that day
    #[serde(default)]
    daily: std::collections::BTreeMap<String, std::collections::HashMap<String, [i64; 3]>>,
    #[serde(default)]
    names: std::collections::HashMap<String, AgentName>,
}

/// Local date (system timezone) as YYYY-MM-DD, via `date +%F`.
fn today_str() -> String {
    Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| s.len() == 10)
        .unwrap_or_default()
}
fn load_token_store(system_id: &str) -> TokenStore {
    std::fs::read(tokens_path(system_id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn save_token_store(system_id: &str, store: &TokenStore) {
    let _ = std::fs::create_dir_all(TOKENS_DIR);
    if let Ok(text) = serde_json::to_vec(store) {
        let tmp = tokens_path(system_id).with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, tokens_path(system_id));
        }
    }
}

/// Fold one /agents snapshot into the daily store (delta accumulation).
fn record_token_sample(system_id: &str, agents: &[serde_json::Value]) {
    let day = today_str();
    if day.is_empty() {
        return;
    }
    let mut store = load_token_store(system_id);
    if store.first_day.is_empty() {
        store.first_day = day.clone();
    }
    let getn = |a: &serde_json::Value, k: &str| a.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let mut deltas: Vec<(String, [i64; 3])> = Vec::new();
    for a in agents {
        let Some(id) = a.get("id").and_then(|x| x.as_str()) else { continue };
        let cur = [getn(a, "total_tokens"), getn(a, "in_tokens"), getn(a, "out_tokens")];
        let d = match store.last.get(id) {
            None => [0, 0, 0], // baseline: don't attribute pre-existing history to a day
            Some(p) => [
                if cur[0] >= p[0] { cur[0] - p[0] } else { cur[0] }, // reset → count current
                if cur[1] >= p[1] { cur[1] - p[1] } else { cur[1] },
                if cur[2] >= p[2] { cur[2] - p[2] } else { cur[2] },
            ],
        };
        store.last.insert(id.to_string(), cur);
        store.names.insert(
            id.to_string(),
            AgentName {
                name: a.get("name").and_then(|x| x.as_str()).unwrap_or(id).to_string(),
                emoji: a.get("emoji").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            },
        );
        if d != [0, 0, 0] {
            deltas.push((id.to_string(), d));
        }
    }
    if !deltas.is_empty() {
        let bucket = store.daily.entry(day).or_default();
        for (id, d) in deltas {
            let e = bucket.entry(id).or_insert([0, 0, 0]);
            e[0] += d[0];
            e[1] += d[1];
            e[2] += d[2];
        }
    }
    while store.daily.len() > MAX_DAYS {
        let Some(oldest) = store.daily.keys().next().cloned() else { break };
        store.daily.remove(&oldest);
    }
    save_token_store(system_id, &store);
}

/// GET /agent/systems/:id/usage — locally-tracked token-usage history (daily
/// per-agent deltas) + the gateway's current cumulative totals. The frontend
/// slices `daily` into 30d / 90d / 1y / month / year views.
pub async fn system_usage(Path(id): Path<String>) -> impl IntoResponse {
    let store = load_token_store(&id);
    let gateway_total: i64 = store.last.values().map(|v| v[0]).sum();
    let mut daily = serde_json::Map::new();
    for (date, agents) in &store.daily {
        let mut total = 0i64;
        let mut amap = serde_json::Map::new();
        for (aid, v) in agents {
            total += v[0];
            amap.insert(aid.clone(), json!(v[0]));
        }
        daily.insert(date.clone(), json!({"total": total, "agents": amap}));
    }
    let names: serde_json::Map<String, serde_json::Value> = store
        .names
        .iter()
        .map(|(k, v)| (k.clone(), json!({"name": v.name, "emoji": v.emoji})))
        .collect();
    Json(json!({
        "ok": true,
        "first_day": store.first_day,
        "today": today_str(),
        "gateway_total": gateway_total,
        "daily": daily,
        "names": names,
    }))
}

/// Background task: every SAMPLE_INTERVAL_S, sample each OpenClaw system's
/// /agents roster and fold it into the local token-usage history. Runs for the
/// life of the process; samples once immediately on startup.
pub async fn token_sampler_loop() {
    loop {
        for sys in load_systems().into_iter().filter(|s| s.roles.iter().any(|r| r == "openclaw")) {
            let id = sys.id.clone();
            let v = tokio::task::spawn_blocking(move || fetch_agents(&sys))
                .await
                .unwrap_or_else(|_| json!({"reachable": false}));
            if let Some(agents) = v.get("agents").and_then(|x| x.as_array()) {
                if !agents.is_empty() {
                    let agents = agents.clone();
                    let _ = tokio::task::spawn_blocking(move || record_token_sample(&id, &agents)).await;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(SAMPLE_INTERVAL_S)).await;
    }
}

// ── E1: per-agent detail + provisioning ──────────────────────────────────
//
// "Provision access" is deliberately NON-INVASIVE: it issues a real Aeon Magick
// API token (scoped Full) through the running AuthStore, drops a per-agent
// access file into that agent's gateway dir, and *returns* the exact one-line
// `skills` change to apply — it does NOT blind-edit the live 70KB openclaw.json.

fn b64(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(d)
}

/// Run a remote command (shell pipeline) over the agent-connect key, capture stdout.
fn ssh_capture(sys: &System, remote: &str) -> Result<String, String> {
    let target = format!("{}@{}", sys.ssh_user, sys.address);
    // The Pi→gateway agent-connect SSH eats the FIRST line of the remote
    // command's stdout (observed: a leading `echo X` comes back blank), which
    // silently corrupted every JSON/clean-output handler (detail/avatar/corpus/
    // soul/persona/containers). Prepend a throwaway marker line to absorb that
    // loss, then strip it back off so downstream output is intact.
    let wrapped = format!("echo __AEONHDR__; {remote}");
    let out = Command::new("ssh")
        .arg("-i")
        .arg(key_path())
        .args([
            "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ConnectTimeout=8",
            "-o", "PreferredAuthentications=publickey",
            "-o", "ServerAliveInterval=3",
            "-o", "ServerAliveCountMax=3",
            "-p", &sys.port.to_string(),
            &target, "timeout", "12", "bash", "-lc", &wrapped,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        // Marker survived → take everything after its line; marker eaten (the
        // usual case) → drop the now-blank first line.
        let cleaned = if let Some(pos) = s.find("__AEONHDR__") {
            let after = &s[pos..];
            after.find('\n').map(|i| &after[i + 1..]).unwrap_or("")
        } else {
            s.find('\n').map(|i| &s[i + 1..]).unwrap_or(&s)
        };
        Ok(cleaned.to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("ssh failed").to_string())
    }
}

type ProvMap = std::collections::HashMap<String, std::collections::HashMap<String, serde_json::Value>>;
fn provisioned_path() -> PathBuf {
    PathBuf::from(DIR).join("provisioned.json")
}
fn load_provisioned() -> ProvMap {
    std::fs::read(provisioned_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}
fn save_provisioned(m: &ProvMap) {
    let _ = std::fs::create_dir_all(DIR);
    if let Ok(t) = serde_json::to_vec_pretty(m) {
        let tmp = provisioned_path().with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, provisioned_path());
        }
    }
}

/// Read-from-stdin python that extracts one agent's config (skills/voice/corpus/
/// model) from openclaw.json + lists available shared skills. base64'd over SSH
/// so there are zero quoting concerns. argv[1] = agent id.
const DETAIL_PY: &str = r#"import json,os,sys
h=os.path.expanduser('~')
try:
    d=json.load(open(h+'/.openclaw/openclaw.json'))
except Exception as e:
    print(json.dumps({'err':'config: '+str(e)}))
    sys.exit(0)
aid=sys.argv[1]
lst=(d.get('agents') or {}).get('list') or []
a={}
for x in lst:
    if isinstance(x,dict) and x.get('id')==aid:
        a=x
        break
sd=h+'/.openclaw/workspace/skills'
av=sorted([n for n in os.listdir(sd) if os.path.isdir(os.path.join(sd,n))]) if os.path.isdir(sd) else []
print(json.dumps({'skills':a.get('skills',[]),'voice':a.get('voice'),'corpus':a.get('corpus'),'model':a.get('model') or a.get('thinkingDefault'),'name':a.get('name'),'is_default':a.get('default',False),'available_skills':av,'found':bool(a)}))
"#;

/// GET /agent/systems/:id/agents/:aid/detail — per-agent skills/voice/corpus/
/// model (from the gateway config) + local provisioned state.
pub async fn agent_detail(Path((id, agent_id)): Path<(String, String)>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let safe_aid = sanitize_id(&agent_id);
    let prov = load_provisioned().get(&id).and_then(|m| m.get(&safe_aid)).cloned();
    let ssh = load_ssh_prov().get(&id).and_then(|m| m.get(&safe_aid)).cloned();
    let aid2 = safe_aid.clone();
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!("echo {} | base64 -d | python3 - {}", b64(DETAIL_PY.as_bytes()), aid2);
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({}));
            Json(json!({
                "ok": true,
                "skills": p.get("skills").cloned().unwrap_or_else(|| json!([])),
                "voice": p.get("voice").cloned().unwrap_or(serde_json::Value::Null),
                "corpus": p.get("corpus").cloned().unwrap_or(serde_json::Value::Null),
                "model": p.get("model").cloned().unwrap_or(serde_json::Value::Null),
                "available_skills": p.get("available_skills").cloned().unwrap_or_else(|| json!([])),
                "provisioned": prov,
                "ssh": ssh.clone(),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e, "provisioned": prov, "ssh": ssh})),
    }
}

#[derive(Deserialize)]
pub struct ProvisionReq {
    /// The Pi's API base URL the agent should call (the browser passes its origin).
    #[serde(default)]
    api_base: String,
}

/// POST /agent/systems/:id/agents/:aid/provision — issue a per-agent Aeon Magick
/// token, drop an access file into the agent's gateway dir, return the token ONCE
/// plus the exact (non-invasive) skills-array change to apply.
pub async fn provision_agent(
    State(state): State<AppState>,
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<ProvisionReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let safe_aid = sanitize_id(&agent_id);
    let (token_id, token_plain) =
        match state.auth.create_token(&format!("agent:{safe_aid}"), crate::auth::TokenScope::Full) {
            Ok(t) => t,
            Err(e) => return Json(json!({"ok": false, "err": format!("token: {e}")})),
        };
    let api_base = if req.api_base.trim().is_empty() {
        format!("https://{}/api", "<this-pi>")
    } else {
        req.api_base.trim().trim_end_matches('/').to_string()
    };
    let drop_res = {
        let (sys, aid, tok, ab) = (sys.clone(), safe_aid.clone(), token_plain.clone(), api_base.clone());
        tokio::task::spawn_blocking(move || drop_agent_access(&sys, &aid, &tok, &ab))
            .await
            .unwrap_or_else(|_| Err("join error".into()))
    };
    let mut prov = load_provisioned();
    prov.entry(id.clone()).or_default().insert(
        safe_aid.clone(),
        json!({"token_id": token_id, "at_ms": now_ms(), "skill": "aeon-magick", "dropped": drop_res.as_ref().ok()}),
    );
    save_provisioned(&prov);
    Json(json!({
        "ok": true,
        "token_id": token_id,
        "token": token_plain,
        "skill": "aeon-magick",
        "dropped": drop_res.as_ref().ok(),
        "drop_err": drop_res.as_ref().err(),
        "config_change": format!(
            "Add \"aeon-magick\" to the skills array of the agents.list entry with id==\"{safe_aid}\" in ~/.openclaw/openclaw.json (create a \"skills\":[] array on that entry if absent), then reload OpenClaw.",
        ),
    }))
}

fn drop_agent_access(sys: &System, agent_id: &str, token: &str, api_base: &str) -> Result<String, String> {
    let content = json!({"api_base": api_base, "token": token, "skill": "aeon-magick"}).to_string();
    let dir = format!("$HOME/.openclaw/agents/{agent_id}/agent");
    let file = format!("{dir}/aeon-magick-access.json");
    let remote = format!(
        "mkdir -p {dir} && echo {} | base64 -d > {file} && chmod 600 {file} && echo {file}",
        b64(content.as_bytes())
    );
    ssh_capture(sys, &remote).map(|s| s.trim().to_string())
}

/// DELETE /agent/systems/:id/agents/:aid/provision — revoke the agent's token +
/// remove its access file.
pub async fn deprovision_agent(
    State(state): State<AppState>,
    Path((id, agent_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let safe_aid = sanitize_id(&agent_id);
    let mut prov = load_provisioned();
    let token_id = prov
        .get(&id)
        .and_then(|m| m.get(&safe_aid))
        .and_then(|v| v.get("token_id"))
        .and_then(|x| x.as_str())
        .map(String::from);
    if let Some(tid) = &token_id {
        let _ = state.auth.revoke_token(tid);
    }
    if let Some(sys) = load_systems().into_iter().find(|s| s.id == id) {
        let aid = safe_aid.clone();
        let _ = tokio::task::spawn_blocking(move || {
            ssh_capture(&sys, &format!("rm -f $HOME/.openclaw/agents/{aid}/agent/aeon-magick-access.json && echo removed"))
        })
        .await;
    }
    if let Some(m) = prov.get_mut(&id) {
        m.remove(&safe_aid);
    }
    save_provisioned(&prov);
    Json(json!({"ok": true, "revoked_token": token_id}))
}

// ── E1 sub-step 3: per-agent SSH access to the Pi (HUMAN-ADMIN ONLY) ──────
//
// Gated to Admin scope by the /api/agent/ deny-list in auth.rs — no agent API
// or MCP path reaches this. Creates an `aeon-agent-<id>` Unix user on the Pi
// (the supervisor runs as root), installs a generated keypair, optionally
// grants passwordless sudo (FULL ADMIN — the UI warns), and pushes the private
// key to the agent's gateway workspace. Revoke deletes the user + sudoers.

fn ssh_prov_path() -> PathBuf {
    PathBuf::from(DIR).join("ssh_provisioned.json")
}
fn load_ssh_prov() -> ProvMap {
    std::fs::read(ssh_prov_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}
fn save_ssh_prov(m: &ProvMap) {
    let _ = std::fs::create_dir_all(DIR);
    if let Ok(t) = serde_json::to_vec_pretty(m) {
        let tmp = ssh_prov_path().with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, ssh_prov_path());
        }
    }
}
fn agent_unix_user(agent_id: &str) -> String {
    format!("aeon-agent-{}", sanitize_id(agent_id))
}
/// Run a root bash script locally (the supervisor process runs as root).
fn root_bash(script: &str) -> Result<String, String> {
    let out = Command::new("bash").arg("-c").arg(script).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("script failed").to_string())
    }
}
fn local_ip() -> String {
    root_bash("hostname -I 2>/dev/null | awk '{print $1}'").map(|s| s.trim().to_string()).unwrap_or_default()
}

#[derive(Deserialize)]
pub struct SshGrantReq {
    #[serde(default)]
    admin: bool,
}

/// POST /agent/systems/:id/agents/:aid/ssh — create the agent's Pi SSH user.
pub async fn grant_ssh(
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<SshGrantReq>,
) -> impl IntoResponse {
    let safe_aid = sanitize_id(&agent_id);
    if load_ssh_prov().get(&id).and_then(|m| m.get(&safe_aid)).is_some() {
        return Json(json!({"ok": false, "err": "SSH already provisioned for this agent — revoke first"}));
    }
    let user = agent_unix_user(&safe_aid);
    let admin = req.admin;
    let (u, af) = (user.clone(), if admin { "1" } else { "0" });
    let privkey = match tokio::task::spawn_blocking(move || {
        let script = format!(
            "set -e\nU={u}\nid \"$U\" >/dev/null 2>&1 || useradd -m -s /bin/bash \"$U\"\n\
             KD=$(mktemp -d)\nssh-keygen -t ed25519 -N '' -C \"$U@aeon-magick\" -f \"$KD/k\" >/dev/null\n\
             install -d -m700 -o \"$U\" -g \"$U\" \"/home/$U/.ssh\"\n\
             cat \"$KD/k.pub\" > \"/home/$U/.ssh/authorized_keys\"\n\
             chmod 600 \"/home/$U/.ssh/authorized_keys\"; chown \"$U:$U\" \"/home/$U/.ssh/authorized_keys\"\n\
             if [ \"{af}\" = \"1\" ]; then printf '%s ALL=(ALL) NOPASSWD:ALL\\n' \"$U\" > \"/etc/sudoers.d/$U\"; chmod 440 \"/etc/sudoers.d/$U\"; else rm -f \"/etc/sudoers.d/$U\"; fi\n\
             cat \"$KD/k\"\nrm -rf \"$KD\""
        );
        root_bash(&script)
    })
    .await
    .unwrap_or_else(|_| Err("join".into()))
    {
        Ok(k) => k,
        Err(e) => return Json(json!({"ok": false, "err": format!("user setup: {e}")})),
    };
    let pi_addr = tokio::task::spawn_blocking(local_ip).await.unwrap_or_default();
    let drop = if let Some(sys) = load_systems().into_iter().find(|s| s.id == id) {
        let (sys, aid, pk) = (sys, safe_aid.clone(), privkey.clone());
        tokio::task::spawn_blocking(move || drop_ssh_key(&sys, &aid, &pk)).await.unwrap_or_else(|_| Err("join".into()))
    } else {
        Err("system not registered".into())
    };
    let mut sp = load_ssh_prov();
    sp.entry(id.clone()).or_default().insert(
        safe_aid.clone(),
        json!({"user": user, "admin": admin, "at_ms": now_ms(), "pi_address": pi_addr, "dropped": drop.as_ref().ok()}),
    );
    save_ssh_prov(&sp);
    Json(json!({
        "ok": true,
        "user": user,
        "admin": admin,
        "pi_address": pi_addr,
        "private_key": privkey,
        "dropped": drop.as_ref().ok(),
        "drop_err": drop.as_ref().err(),
        "ssh_command": format!("ssh -i ~/.ssh/aeon-magick-pi {user}@{pi_addr}"),
    }))
}

fn drop_ssh_key(sys: &System, agent_id: &str, privkey: &str) -> Result<String, String> {
    let dir = format!("$HOME/.openclaw/agents/{agent_id}/agent");
    let file = format!("{dir}/aeon-magick-pi-ssh-key");
    let remote = format!(
        "mkdir -p {dir} && echo {} | base64 -d > {file} && chmod 600 {file} && echo {file}",
        b64(privkey.as_bytes())
    );
    ssh_capture(sys, &remote).map(|s| s.trim().to_string())
}

#[derive(Deserialize)]
pub struct SshAdminReq {
    admin: bool,
}

/// PATCH /agent/systems/:id/agents/:aid/ssh — toggle sudo without re-keying.
pub async fn toggle_ssh_admin(
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<SshAdminReq>,
) -> impl IntoResponse {
    let safe_aid = sanitize_id(&agent_id);
    let mut sp = load_ssh_prov();
    if sp.get(&id).and_then(|m| m.get(&safe_aid)).is_none() {
        return Json(json!({"ok": false, "err": "no SSH provision for this agent"}));
    }
    let user = agent_unix_user(&safe_aid);
    let (u, on) = (user.clone(), req.admin);
    let res = tokio::task::spawn_blocking(move || {
        let script = if on {
            format!("printf '%s ALL=(ALL) NOPASSWD:ALL\\n' {u} > /etc/sudoers.d/{u} && chmod 440 /etc/sudoers.d/{u} && echo ok")
        } else {
            format!("rm -f /etc/sudoers.d/{u} && echo ok")
        };
        root_bash(&script)
    })
    .await
    .unwrap_or_else(|_| Err("join".into()));
    if let Err(e) = res {
        return Json(json!({"ok": false, "err": e}));
    }
    if let Some(o) = sp.get_mut(&id).and_then(|m| m.get_mut(&safe_aid)).and_then(|v| v.as_object_mut()) {
        o.insert("admin".into(), json!(req.admin));
    }
    save_ssh_prov(&sp);
    Json(json!({"ok": true, "admin": req.admin}))
}

/// DELETE /agent/systems/:id/agents/:aid/ssh — delete the agent's Pi SSH user.
pub async fn revoke_ssh(Path((id, agent_id)): Path<(String, String)>) -> impl IntoResponse {
    let safe_aid = sanitize_id(&agent_id);
    let user = agent_unix_user(&safe_aid);
    let u = user.clone();
    let _ = tokio::task::spawn_blocking(move || {
        root_bash(&format!("rm -f /etc/sudoers.d/{u}; pkill -u {u} 2>/dev/null; userdel -r {u} 2>/dev/null; echo done"))
    })
    .await;
    if let Some(sys) = load_systems().into_iter().find(|s| s.id == id) {
        let aid = safe_aid.clone();
        let _ = tokio::task::spawn_blocking(move || {
            ssh_capture(&sys, &format!("rm -f $HOME/.openclaw/agents/{aid}/agent/aeon-magick-pi-ssh-key && echo ok"))
        })
        .await;
    }
    let mut sp = load_ssh_prov();
    if let Some(m) = sp.get_mut(&id) {
        m.remove(&safe_aid);
    }
    save_ssh_prov(&sp);
    Json(json!({"ok": true, "user": user}))
}

// ── E3: per-system power controls (shutdown / reboot / Wake-on-LAN) ───────

#[derive(Deserialize)]
pub struct PowerReq {
    action: String, // "shutdown" | "reboot" | "wake"
    #[serde(default)]
    mac: String,
}

/// POST /agent/systems/:id/power — shutdown/reboot (ssh sudo) or wake (WoL).
pub async fn power_system(Path(id): Path<String>, Json(req): Json<PowerReq>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    match req.action.as_str() {
        "wake" => match send_wol(&req.mac) {
            Ok(_) => Json(json!({"ok": true, "action": "wake", "mac": req.mac})),
            Err(e) => Json(json!({"ok": false, "err": e})),
        },
        "shutdown" | "reboot" => {
            let cmd = if req.action == "reboot" {
                "sudo -n systemctl reboot"
            } else {
                "sudo -n systemctl poweroff"
            };
            let action = req.action.clone();
            let res = tokio::task::spawn_blocking(move || ssh_capture(&sys, cmd))
                .await
                .unwrap_or_else(|_| Err("join".into()));
            match res {
                Ok(_) => Json(json!({"ok": true, "action": action})),
                Err(e) => {
                    let el = e.to_lowercase();
                    if el.is_empty()
                        || el.contains("closed")
                        || el.contains("reset")
                        || el.contains("timed out")
                        || el.contains("timeout")
                    {
                        Json(json!({"ok": true, "action": action, "note": "connection dropped (expected on power-down)"}))
                    } else {
                        Json(json!({"ok": false, "err": e, "hint": "the ssh user likely needs passwordless sudo for systemctl poweroff/reboot"}))
                    }
                }
            }
        }
        other => Json(json!({"ok": false, "err": format!("unknown action: {other}")})),
    }
}

/// Send a Wake-on-LAN magic packet to a MAC (broadcast UDP :9).
fn send_wol(mac: &str) -> Result<(), String> {
    let bytes: Vec<u8> = mac
        .split(|c| c == ':' || c == '-')
        .filter_map(|h| u8::from_str_radix(h.trim(), 16).ok())
        .collect();
    if bytes.len() != 6 {
        return Err(format!("invalid MAC: {mac:?}"));
    }
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&bytes);
    }
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    sock.set_broadcast(true).map_err(|e| e.to_string())?;
    sock.send_to(&packet, "255.255.255.255:9").map_err(|e| e.to_string())?;
    Ok(())
}

// ── E1: per-agent profile photo → Matrix avatar ──────────────────────────
//
// Each pantheon agent has its own Matrix account on the gateway's Dendrite
// homeserver (server_name matrix.unhash.me, reachable as 127.0.0.1:8008 on
// .155). Creds live in ~/.openclaw_<id>_creds.json (and/or
// ~/.openclaw/credentials/<id>.json) as {user_id, access_token, ...}. We run
// the whole avatar flow ON the gateway via ssh_capture so the token + the
// homeserver never leave .155: resolve creds → POST image to the media repo
// → PUT the agent's avatar_url. A GET path returns the current avatar_url +
// a download URL the browser can render. ON_HS is localhost on the gateway.

const MATRIX_HS: &str = "http://127.0.0.1:8008";

/// Resolve-creds python shared by GET + POST: finds the agent's Matrix
/// user_id + access_token, prints them as `USER_ID\nTOKEN` (or `ERR ...`).
/// base64'd over SSH (argv[1] = agent id).
///
/// Per the create-agentic-personas blueprint, the canonical per-agent
/// Matrix/VoIP credentials live in a standard `.env` at `~/voip-<id>/.env`
/// (mode 600) with vars MATRIX_HOMESERVER_URL / MATRIX_USER_ID /
/// MATRIX_ACCESS_TOKEN (verified on the live gateway: @<id>:matrix.unhash.me,
/// homeserver http://127.0.0.1:8008). We resolve that .env FIRST and fall back
/// to the legacy ~/.openclaw_<id>_creds.json record only if the .env is absent
/// or incomplete.
const MATRIX_CREDS_PY: &str = r#"import json,os,sys,glob
aid=sys.argv[1]
h=os.path.expanduser('~')

def parse_env(path):
    """Minimal .env reader: KEY=VALUE, skipping comments/blanks, stripping
    surrounding quotes and trailing inline comments on unquoted values."""
    out={}
    try:
        with open(path) as f:
            for line in f:
                s=line.strip()
                if not s or s.startswith('#') or '=' not in s: continue
                k,v=s.split('=',1)
                k=k.strip()
                if k.startswith('export '): k=k[7:].strip()
                v=v.strip()
                if len(v)>=2 and v[0]==v[-1] and v[0] in ('"',"'"):
                    v=v[1:-1]
                else:
                    # drop an inline comment on an unquoted value (" # ...").
                    hp=v.find(' #')
                    if hp>=0: v=v[:hp].rstrip()
                out[k]=v
    except Exception:
        return {}
    return out

# 1) Standard per-agent .env (the create-agentic-personas layout).
env=parse_env(h+'/voip-%s/.env'%aid)
uid=env.get('MATRIX_USER_ID')
tok=env.get('MATRIX_ACCESS_TOKEN')
if uid and tok and not tok.startswith('<'):
    print(uid); print(tok); sys.exit(0)

# 2) Fall back to the legacy JSON credential records.
cands=[h+'/.openclaw_%s_creds.json'%aid,
       h+'/.openclaw/credentials/%s.json'%aid,
       h+'/.openclaw/credentials/%s_creds.json'%aid,
       h+'/.openclaw/matrix/%s.json'%aid]
cands+= [p for p in glob.glob(h+'/.openclaw/credentials/*%s*.json'%aid) if p not in cands]
for p in cands:
    try:
        d=json.load(open(p))
    except Exception:
        continue
    uid=d.get('user_id') or d.get('userId') or d.get('mxid')
    tok=d.get('access_token') or d.get('accessToken') or d.get('token')
    if uid and tok:
        print(uid); print(tok); sys.exit(0)
print('ERR no Matrix creds for "%s" (looked in ~/voip-%s/.env and ~/.openclaw_%s_creds.json + ~/.openclaw/credentials/)'%(aid,aid,aid))
"#;

/// Resolve `(user_id, access_token)` for an agent by running MATRIX_CREDS_PY
/// on the gateway. Returns a UI-friendly error if creds can't be found.
fn matrix_creds(sys: &System, agent_id: &str) -> Result<(String, String), String> {
    let remote = format!(
        "echo {} | base64 -d | python3 - {}",
        b64(MATRIX_CREDS_PY.as_bytes()),
        agent_id
    );
    let out = ssh_capture(sys, &remote)?;
    let mut lines = out.lines();
    let first = lines.next().unwrap_or("").trim();
    if first.is_empty() || first.starts_with("ERR") {
        return Err(if first.is_empty() { "no Matrix creds found".into() } else { first[3..].trim().to_string() });
    }
    let tok = lines.next().unwrap_or("").trim().to_string();
    if tok.is_empty() {
        return Err("creds file missing access_token".into());
    }
    Ok((first.to_string(), tok))
}

/// GET /agent/systems/:id/agents/:aid/avatar — the agent's current Matrix
/// avatar: user_id + avatar_url (mxc://…) + a download URL the browser can
/// render. (The download URL is served by the gateway's homeserver; the
/// browser must be able to reach matrix.unhash.me to actually load it.)
pub async fn agent_avatar_get(Path((id, agent_id)): Path<(String, String)>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let aid = sanitize_id(&agent_id);
    let res = tokio::task::spawn_blocking(move || {
        let (uid, tok) = matrix_creds(&sys, &aid)?;
        // Fetch current avatar_url via the agent's own token (works even if
        // the profile is non-public).
        let remote = format!(
            "curl -s --max-time 8 -H 'Authorization: Bearer {tok}' \
             '{MATRIX_HS}/_matrix/client/v3/profile/{uid}/avatar_url'"
        );
        let body = ssh_capture(&sys, &remote)?;
        Ok::<_, String>((uid, body))
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok((uid, body)) => {
            let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap_or_else(|_| json!({}));
            let mxc = v.get("avatar_url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let download = mxc_download_url(&uid, &mxc);
            Json(json!({"ok": true, "user_id": uid, "avatar_url": mxc, "download_url": download}))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// mxc://server/mediaId → a public download URL the browser can <img src=>.
/// Uses the homeserver derived from the user_id (matrix.unhash.me) so the
/// browser doesn't need to reach the Pi's localhost-only 8008.
fn mxc_download_url(user_id: &str, mxc: &str) -> Option<String> {
    let rest = mxc.strip_prefix("mxc://")?;
    let (server, media_id) = rest.split_once('/')?;
    if server.is_empty() || media_id.is_empty() {
        return None;
    }
    let hs = user_id.split(':').nth(1).unwrap_or(server);
    Some(format!("https://{hs}/_matrix/media/v3/download/{server}/{media_id}"))
}

#[derive(Deserialize)]
pub struct AvatarReq {
    /// base64-encoded image bytes (the browser strips the data: prefix).
    image_b64: String,
    #[serde(default = "default_image_ct")]
    content_type: String,
}
fn default_image_ct() -> String {
    "image/png".into()
}

/// POST /agent/systems/:id/agents/:aid/avatar — set the agent's Matrix avatar:
/// upload the image to the gateway's Dendrite media repo, then PUT the agent's
/// avatar_url to the returned mxc:// URI. Runs entirely on .155 so the token
/// stays local. Returns ok + the mxc uri + a browser download URL.
pub async fn agent_avatar_set(
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<AvatarReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    // Validate the base64 up front (and cap size: Matrix media repos reject
    // huge uploads; 8 MB is plenty for an avatar).
    use base64::Engine;
    let raw = match base64::engine::general_purpose::STANDARD.decode(req.image_b64.trim()) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return Json(json!({"ok": false, "err": "empty image"})),
        Err(e) => return Json(json!({"ok": false, "err": format!("bad base64: {e}")})),
    };
    if raw.len() > 8 * 1024 * 1024 {
        return Json(json!({"ok": false, "err": "image too large (max 8 MB)"}));
    }
    let img_b64 = b64(&raw);
    let ct = sanitize_content_type(&req.content_type);
    let aid = sanitize_id(&agent_id);
    let res = tokio::task::spawn_blocking(move || {
        let (uid, tok) = matrix_creds(&sys, &aid)?;
        // 1) Upload to the media repo. Write the bytes to a temp file on the
        //    gateway (base64-decoded) and curl --data-binary it. Capture the
        //    content_uri from the JSON response.
        let upload = format!(
            "TMP=$(mktemp); echo {img_b64} | base64 -d > \"$TMP\"; \
             RESP=$(curl -s --max-time 30 -X POST \
               -H 'Authorization: Bearer {tok}' \
               -H 'Content-Type: {ct}' \
               --data-binary @\"$TMP\" \
               '{MATRIX_HS}/_matrix/media/v3/upload'); \
             rm -f \"$TMP\"; echo \"$RESP\""
        );
        let up_body = ssh_capture(&sys, &upload)?;
        let upv: serde_json::Value = serde_json::from_str(up_body.trim())
            .map_err(|_| format!("upload returned non-JSON: {}", up_body.trim().chars().take(200).collect::<String>()))?;
        let mxc = upv
            .get("content_uri")
            .and_then(|x| x.as_str())
            .ok_or_else(|| {
                let err = upv.get("error").and_then(|x| x.as_str()).unwrap_or("no content_uri in response");
                format!("media upload failed: {err}")
            })?
            .to_string();
        // 2) Set avatar_url on the agent's profile.
        let body = json!({"avatar_url": mxc}).to_string();
        let setav = format!(
            "echo {} | base64 -d | curl -s --max-time 12 -X PUT \
               -H 'Authorization: Bearer {tok}' \
               -H 'Content-Type: application/json' \
               --data-binary @- \
               '{MATRIX_HS}/_matrix/client/v3/profile/{uid}/avatar_url'",
            b64(body.as_bytes())
        );
        let set_body = ssh_capture(&sys, &setav)?;
        // A successful PUT returns "{}". Anything with an "errcode" is a failure.
        let setv: serde_json::Value = serde_json::from_str(set_body.trim()).unwrap_or_else(|_| json!({}));
        if let Some(ec) = setv.get("errcode").and_then(|x| x.as_str()) {
            let em = setv.get("error").and_then(|x| x.as_str()).unwrap_or("");
            return Err(format!("set avatar_url failed: {ec} {em}"));
        }
        Ok::<_, String>((uid, mxc))
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok((uid, mxc)) => {
            let download = mxc_download_url(&uid, &mxc);
            Json(json!({"ok": true, "user_id": uid, "avatar_url": mxc, "download_url": download}))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// Keep a content-type to a safe `image/<token>` so it can't break out of the
/// curl header. Falls back to image/png.
fn sanitize_content_type(ct: &str) -> String {
    let ct = ct.trim();
    let ok = ct.len() <= 64
        && ct.starts_with("image/")
        && ct.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '+'));
    if ct.is_empty() || !ok { "image/png".into() } else { ct.to_string() }
}

// ── E1: per-agent corpus browse / view ───────────────────────────────────
//
// Each agent's knowledge corpus is an Obsidian-style vault of markdown notes
// at  <workspace>/memory/<id>-corpus  on the gateway (the build_*_corpus.py
// scripts write there). We resolve the root from the agent's actual `workspace`
// config (falling back to the ~/.openclaw/workspace-<id> convention), list its
// files (relative path + size), and read a single file. The read path is
// guarded against traversal: the python resolves realpath(root + path) and
// refuses anything that escapes the corpus root, so `?path=../../secret` can't
// reach outside the vault.

/// LIST mode python: resolve the corpus root + emit {root, files:[{path,size}]}.
/// argv[1] = agent id. base64'd over SSH.
const CORPUS_LIST_PY: &str = r#"import json,os,sys
h=os.path.expanduser('~')
aid=sys.argv[1]
# Resolve root from the agent's workspace config, else the convention.
root=None
try:
    d=json.load(open(h+'/.openclaw/openclaw.json'))
    for a in ((d.get('agents') or {}).get('list') or []):
        if isinstance(a,dict) and a.get('id')==aid:
            ws=a.get('workspace')
            if ws: root=os.path.join(ws,'memory',aid+'-corpus')
            break
except Exception:
    pass
if not root:
    root=h+'/.openclaw/workspace-%s/memory/%s-corpus'%(aid,aid)
out={'root':root,'exists':os.path.isdir(root),'files':[]}
if os.path.isdir(root):
    for dp,_,fns in os.walk(root):
        for fn in fns:
            fp=os.path.join(dp,fn)
            try: sz=os.path.getsize(fp)
            except OSError: sz=0
            out['files'].append({'path':os.path.relpath(fp,root),'size':sz})
    out['files'].sort(key=lambda x:x['path'])
out['count']=len(out['files'])
print(json.dumps(out))
"#;

/// READ mode python: print one corpus file's contents, traversal-guarded.
/// argv[1] = agent id, argv[2] = base64'd relative path. base64'd over SSH.
const CORPUS_READ_PY: &str = r#"import json,os,sys,base64
h=os.path.expanduser('~')
aid=sys.argv[1]
rel=base64.b64decode(sys.argv[2]).decode('utf-8','replace')
root=None
try:
    d=json.load(open(h+'/.openclaw/openclaw.json'))
    for a in ((d.get('agents') or {}).get('list') or []):
        if isinstance(a,dict) and a.get('id')==aid:
            ws=a.get('workspace')
            if ws: root=os.path.join(ws,'memory',aid+'-corpus')
            break
except Exception:
    pass
if not root:
    root=h+'/.openclaw/workspace-%s/memory/%s-corpus'%(aid,aid)
rootr=os.path.realpath(root)
target=os.path.realpath(os.path.join(rootr,rel))
# Guard: target must stay under the corpus root.
if not (target==rootr or target.startswith(rootr+os.sep)):
    print(json.dumps({'err':'path escapes corpus root'})); sys.exit(0)
if not os.path.isfile(target):
    print(json.dumps({'err':'not a file'})); sys.exit(0)
try:
    sz=os.path.getsize(target)
    if sz>2*1024*1024:
        print(json.dumps({'err':'file too large to view (%d bytes)'%sz})); sys.exit(0)
    data=open(target,'rb').read()
    txt=data.decode('utf-8','replace')
    print(json.dumps({'path':rel,'size':sz,'content':txt}))
except Exception as e:
    print(json.dumps({'err':'read: '+str(e)}))
"#;

/// GET /agent/systems/:id/agents/:aid/corpus — list the agent's corpus files
/// (relative path + size) + the resolved root.
pub async fn agent_corpus_list(Path((id, agent_id)): Path<(String, String)>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let aid = sanitize_id(&agent_id);
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!("echo {} | base64 -d | python3 - {}", b64(CORPUS_LIST_PY.as_bytes()), aid);
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({}));
            Json(json!({
                "ok": true,
                "root": p.get("root").cloned().unwrap_or(serde_json::Value::Null),
                "exists": p.get("exists").cloned().unwrap_or(json!(false)),
                "count": p.get("count").cloned().unwrap_or(json!(0)),
                "files": p.get("files").cloned().unwrap_or_else(|| json!([])),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize)]
pub struct CorpusFileQuery {
    path: String,
}

// ── E1: per-agent add-skill ──────────────────────────────────────────────
//
// Two non-invasive ways to add a skill to an agent (mirrors provision_agent:
// we drop files into the gateway + RETURN the exact one-line skills-array
// change to apply, never blind-editing the live config):
//
//  (a) custom upload — the admin uploads a skill as a single file: a `.md`
//      SKILL file (dropped as <name>/SKILL.md) or a tar/tgz (extracted into
//      <name>/). Lands in ~/.openclaw/workspace/skills/<name>/ on the gateway.
//  (b) quick-add — an existing gateway skill (from `available_skills`): nothing
//      to drop, just return the config_change to add it to this agent.
//
// (The AEON-7 GitHub marketplace path needs `gh`, which isn't installed — left
// as a TODO in the UI.)

/// Sanitize a skill name to a safe dir component (letters/digits/_-.).
fn sanitize_skill_name(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '-' })
        .collect();
    s.trim_matches(|c| c == '.' || c == '-').to_string()
}

/// The exact non-invasive skills-array change to apply on the gateway.
fn skill_config_change(skill: &str, agent_id: &str) -> String {
    format!(
        "Add \"{skill}\" to the skills array of the agents.list entry with id==\"{agent_id}\" in ~/.openclaw/openclaw.json (create a \"skills\":[] array on that entry if absent), then reload OpenClaw."
    )
}

#[derive(Deserialize)]
pub struct AddSkillReq {
    /// Skill name → the dir under workspace/skills/<name>/ + the value added to
    /// the agent's skills array.
    name: String,
    /// "existing" = quick-add a gateway skill (no upload). "md" = file_b64 is a
    /// SKILL.md. "tar" = file_b64 is a tar / tar.gz of the skill dir contents.
    #[serde(default)]
    kind: String,
    /// base64'd file bytes (for kind = "md" | "tar"); empty for "existing".
    #[serde(default)]
    file_b64: String,
}

/// POST /agent/systems/:id/agents/:aid/skill — add a skill to an agent.
/// For custom uploads, drops the skill into the gateway's shared skills dir;
/// for any kind, returns the non-invasive config_change to enable it.
pub async fn add_skill(
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<AddSkillReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let safe_aid = sanitize_id(&agent_id);
    let skill = sanitize_skill_name(&req.name);
    if skill.is_empty() {
        return Json(json!({"ok": false, "err": "skill name required"}));
    }
    let kind = req.kind.trim();
    // Quick-add an existing gateway skill: nothing to drop.
    if kind.is_empty() || kind == "existing" {
        return Json(json!({
            "ok": true,
            "skill": skill,
            "dropped": serde_json::Value::Null,
            "config_change": skill_config_change(&skill, &safe_aid),
        }));
    }
    if kind != "md" && kind != "tar" {
        return Json(json!({"ok": false, "err": format!("unknown kind: {kind}")}));
    }
    // Decode + size-cap the upload before shipping it over SSH.
    use base64::Engine;
    let raw = match base64::engine::general_purpose::STANDARD.decode(req.file_b64.trim()) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return Json(json!({"ok": false, "err": "empty file"})),
        Err(e) => return Json(json!({"ok": false, "err": format!("bad base64: {e}")})),
    };
    if raw.len() > 32 * 1024 * 1024 {
        return Json(json!({"ok": false, "err": "file too large (max 32 MB)"}));
    }
    let file_b64 = b64(&raw);
    let kind_s = kind.to_string();
    let drop_res = {
        let (sys, sk, k, fb) = (sys.clone(), skill.clone(), kind_s.clone(), file_b64);
        tokio::task::spawn_blocking(move || drop_skill(&sys, &sk, &k, &fb))
            .await
            .unwrap_or_else(|_| Err("join error".into()))
    };
    match drop_res {
        Ok(dir) => Json(json!({
            "ok": true,
            "skill": skill,
            "dropped": dir,
            "config_change": skill_config_change(&skill, &safe_aid),
        })),
        Err(e) => Json(json!({"ok": false, "err": e, "skill": skill})),
    }
}

/// Drop a custom skill into ~/.openclaw/workspace/skills/<name>/ on the gateway.
/// `md` → write the bytes as <name>/SKILL.md. `tar` → extract (auto-detect gzip)
/// into <name>/. Returns the created dir path.
fn drop_skill(sys: &System, skill: &str, kind: &str, file_b64: &str) -> Result<String, String> {
    let dir = format!("$HOME/.openclaw/workspace/skills/{skill}");
    let remote = if kind == "md" {
        format!(
            "mkdir -p {dir} && echo {file_b64} | base64 -d > {dir}/SKILL.md && echo {dir}",
        )
    } else {
        // tar: write to a temp file, detect gzip by magic bytes, extract into dir.
        format!(
            "mkdir -p {dir} && TMP=$(mktemp) && echo {file_b64} | base64 -d > \"$TMP\" && \
             if gzip -t \"$TMP\" 2>/dev/null; then tar -xzf \"$TMP\" -C {dir}; else tar -xf \"$TMP\" -C {dir}; fi && \
             rm -f \"$TMP\" && echo {dir}",
        )
    };
    ssh_capture(sys, &remote).map(|s| s.trim().to_string())
}

// ── E1: per-agent voice ──────────────────────────────────────────────────
//
// The agent objects carry NO per-agent voice field — the effective TTS voice
// is the gateway-global `messages.tts.providers.openai.voice` (currently the
// named clone "azelma"). A per-agent override COULD live on the agent object
// (voice / identity.voice / tts.*) so we check those first, then fall back to
// the global. We also classify the value: a short single-token name → a named
// clone; a long descriptive sentence → a voice-designer description.

/// VOICE python: resolve the agent's effective voice + its source + the global
/// default. argv[1] = agent id. base64'd over SSH.
const VOICE_PY: &str = r#"import json,os,sys
h=os.path.expanduser('~')
aid=sys.argv[1]
d=json.load(open(h+'/.openclaw/openclaw.json'))
def dig(o,path):
    cur=o
    for k in path:
        if isinstance(cur,dict) and k in cur: cur=cur[k]
        else: return None
    return cur
glob=dig(d,['messages','tts','providers','openai','voice'])
glob_provider=dig(d,['messages','tts','provider'])
a={}
for x in ((d.get('agents') or {}).get('list') or []):
    if isinstance(x,dict) and x.get('id')==aid: a=x; break
# per-agent override candidates (first non-null wins)
ov=None; ov_src=None
for name,path in [('voice',['voice']),('identity.voice',['identity','voice']),
                  ('tts.voice',['tts','voice']),
                  ('tts.providers.openai.voice',['tts','providers','openai','voice'])]:
    v=dig(a,path)
    if isinstance(v,str) and v.strip():
        ov=v; ov_src=name; break
eff = ov if ov is not None else glob
src = ('agent.'+ov_src) if ov is not None else 'gateway-default (messages.tts.providers.openai.voice)'
# classify: long sentence-ish => designer description; short token => named clone
def kind(v):
    if not isinstance(v,str) or not v.strip(): return 'none'
    s=v.strip()
    if len(s)>60 or s.count(' ')>=4 or '.' in s: return 'designer'
    return 'clone'
print(json.dumps({'voice':eff,'source':src,'kind':kind(eff),
                  'is_override':ov is not None,'global':glob,'provider':glob_provider,
                  'found':bool(a)}))
"#;

/// GET /agent/systems/:id/agents/:aid/voice — the agent's effective TTS voice
/// (per-agent override if any, else the gateway default) + whether it looks
/// like a named clone vs a voice-designer description.
pub async fn agent_voice(Path((id, agent_id)): Path<(String, String)>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let aid = sanitize_id(&agent_id);
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!("echo {} | base64 -d | python3 - {}", b64(VOICE_PY.as_bytes()), aid);
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({}));
            Json(json!({
                "ok": true,
                "voice": p.get("voice").cloned().unwrap_or(serde_json::Value::Null),
                "source": p.get("source").cloned().unwrap_or(serde_json::Value::Null),
                "kind": p.get("kind").cloned().unwrap_or(json!("none")),
                "is_override": p.get("is_override").cloned().unwrap_or(json!(false)),
                "global": p.get("global").cloned().unwrap_or(serde_json::Value::Null),
                "provider": p.get("provider").cloned().unwrap_or(serde_json::Value::Null),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// GET /agent/systems/:id/agents/:aid/corpus/file?path=… — read one corpus
/// file's contents (read-only, traversal-guarded, 2 MB cap).
pub async fn agent_corpus_file(
    Path((id, agent_id)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<CorpusFileQuery>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    if q.path.trim().is_empty() {
        return Json(json!({"ok": false, "err": "path required"}));
    }
    let aid = sanitize_id(&agent_id);
    let path_b64 = b64(q.path.as_bytes());
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!(
            "echo {} | base64 -d | python3 - {} {}",
            b64(CORPUS_READ_PY.as_bytes()),
            aid,
            path_b64
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({"err": "bad response"}));
            if let Some(e) = p.get("err").and_then(|x| x.as_str()) {
                return Json(json!({"ok": false, "err": e}));
            }
            Json(json!({
                "ok": true,
                "path": p.get("path").cloned().unwrap_or(serde_json::Value::Null),
                "size": p.get("size").cloned().unwrap_or(json!(0)),
                "content": p.get("content").cloned().unwrap_or(json!("")),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

// ── E6/F7a: per-agent persona files (Soul + Identity) ─────────────────────
//
// A persona's character lives in markdown in its workspace (per the
// create-agentic-personas blueprint, verified on the gateway):
//   <workspace>/SOUL.md      — the essence: voice, values, manner (the system prompt)
//   <workspace>/IDENTITY.md  — the facts: name, era, domain, emoji
// We resolve <workspace> from the agent's `workspace` config (falling back to
// the ~/.openclaw/workspace-<id> convention, which is what every persona on the
// gateway actually uses), then read/write exactly SOUL.md or IDENTITY.md. Both
// paths are traversal-guarded: only the two whitelisted filenames are ever
// touched, and the resolved target must stay directly under the workspace root.

/// Map the `which` query value to the exact persona filename. Whitelist-only —
/// anything else is rejected so the param can never select an arbitrary file.
fn persona_filename(which: &str) -> Option<&'static str> {
    match which.trim().to_ascii_lowercase().as_str() {
        "soul" => Some("SOUL.md"),
        "identity" => Some("IDENTITY.md"),
        _ => None,
    }
}

/// READ mode python: print one persona file's contents, traversal-guarded.
/// argv[1] = agent id, argv[2] = filename (SOUL.md|IDENTITY.md). base64'd over SSH.
const PERSONA_READ_PY: &str = r#"import json,os,sys
h=os.path.expanduser('~')
aid=sys.argv[1]; fn=sys.argv[2]
# Resolve the workspace root from config, else the convention.
ws=None
try:
    d=json.load(open(h+'/.openclaw/openclaw.json'))
    for a in ((d.get('agents') or {}).get('list') or []):
        if isinstance(a,dict) and a.get('id')==aid:
            ws=a.get('workspace'); break
except Exception:
    pass
if not ws:
    ws=h+'/.openclaw/workspace-%s'%aid
rootr=os.path.realpath(ws)
target=os.path.realpath(os.path.join(rootr,fn))
# Guard: target must be a direct child of the workspace root.
if os.path.dirname(target)!=rootr:
    print(json.dumps({'err':'path escapes workspace root'})); sys.exit(0)
out={'workspace':ws,'path':fn,'exists':os.path.isfile(target)}
if os.path.isfile(target):
    try:
        sz=os.path.getsize(target)
        if sz>1024*1024:
            print(json.dumps({'err':'file too large to edit (%d bytes)'%sz})); sys.exit(0)
        out['size']=sz
        out['content']=open(target,'rb').read().decode('utf-8','replace')
    except Exception as e:
        print(json.dumps({'err':'read: '+str(e)})); sys.exit(0)
else:
    out['size']=0; out['content']=''
print(json.dumps(out))
"#;

/// RESOLVE-PATH python for the write path: print the absolute, traversal-checked
/// target path for `<workspace>/<fn>` (or `ERR ...`). argv[1]=aid, argv[2]=fn.
/// We resolve the path on the gateway (where the config lives) and then do the
/// base64 write to that exact path, so the write target can never escape.
const PERSONA_PATH_PY: &str = r#"import json,os,sys
h=os.path.expanduser('~')
aid=sys.argv[1]; fn=sys.argv[2]
ws=None
try:
    d=json.load(open(h+'/.openclaw/openclaw.json'))
    for a in ((d.get('agents') or {}).get('list') or []):
        if isinstance(a,dict) and a.get('id')==aid:
            ws=a.get('workspace'); break
except Exception:
    pass
if not ws:
    ws=h+'/.openclaw/workspace-%s'%aid
rootr=os.path.realpath(ws)
target=os.path.realpath(os.path.join(rootr,fn))
if os.path.dirname(target)!=rootr:
    print('ERR path escapes workspace root'); sys.exit(0)
if not os.path.isdir(rootr):
    print('ERR workspace does not exist: '+rootr); sys.exit(0)
print(target)
"#;

#[derive(Deserialize)]
pub struct PersonaFileQuery {
    /// "soul" | "identity" — selects SOUL.md / IDENTITY.md.
    which: String,
}

#[derive(Deserialize)]
pub struct PersonaFileWriteReq {
    /// "soul" | "identity".
    which: String,
    /// Full new file contents (UTF-8 markdown).
    content: String,
}

/// GET /agent/systems/:id/agents/:aid/persona-file?which=soul|identity — read
/// the agent's SOUL.md or IDENTITY.md (traversal-guarded, 1 MB cap).
pub async fn agent_persona_file_get(
    Path((id, agent_id)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<PersonaFileQuery>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let Some(fname) = persona_filename(&q.which) else {
        return Json(json!({"ok": false, "err": "which must be 'soul' or 'identity'"}));
    };
    let aid = sanitize_id(&agent_id);
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!(
            "echo {} | base64 -d | python3 - {} {}",
            b64(PERSONA_READ_PY.as_bytes()),
            aid,
            fname
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({"err": "bad response"}));
            if let Some(e) = p.get("err").and_then(|x| x.as_str()) {
                return Json(json!({"ok": false, "err": e}));
            }
            Json(json!({
                "ok": true,
                "which": q.which,
                "filename": fname,
                "workspace": p.get("workspace").cloned().unwrap_or(serde_json::Value::Null),
                "path": p.get("path").cloned().unwrap_or(serde_json::Value::Null),
                "exists": p.get("exists").cloned().unwrap_or(json!(false)),
                "size": p.get("size").cloned().unwrap_or(json!(0)),
                "content": p.get("content").cloned().unwrap_or(json!("")),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// PUT /agent/systems/:id/agents/:aid/persona-file?which=soul|identity — write
/// the agent's SOUL.md or IDENTITY.md. The new content is shipped base64'd over
/// SSH and written to the gateway-resolved, traversal-checked target path.
pub async fn agent_persona_file_put(
    Path((id, agent_id)): Path<(String, String)>,
    Json(req): Json<PersonaFileWriteReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let Some(fname) = persona_filename(&req.which) else {
        return Json(json!({"ok": false, "err": "which must be 'soul' or 'identity'"}));
    };
    // Cap the payload (a persona file is prose; 1 MB is very generous).
    if req.content.len() > 1024 * 1024 {
        return Json(json!({"ok": false, "err": "content too large (max 1 MB)"}));
    }
    let aid = sanitize_id(&agent_id);
    let content_b64 = b64(req.content.as_bytes());
    let which = req.which.clone();
    let res = tokio::task::spawn_blocking(move || {
        // 1) Resolve + traversal-check the absolute target path on the gateway.
        let path_cmd = format!(
            "echo {} | base64 -d | python3 - {} {}",
            b64(PERSONA_PATH_PY.as_bytes()),
            aid,
            fname
        );
        let target = ssh_capture(&sys, &path_cmd)?;
        let target = target.trim();
        if target.is_empty() || target.starts_with("ERR") {
            return Err(if target.is_empty() {
                "could not resolve persona file path".into()
            } else {
                target[3..].trim().to_string()
            });
        }
        // 2) Write the new contents atomically (temp + mv) to that exact path.
        //    `target` came from realpath() on the gateway and is single-quoted.
        let write_cmd = format!(
            "echo {content_b64} | base64 -d > '{target}.aeon.tmp' && mv '{target}.aeon.tmp' '{target}' && wc -c < '{target}'"
        );
        let bytes = ssh_capture(&sys, &write_cmd)?;
        Ok::<_, String>((target.to_string(), bytes.trim().to_string()))
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok((path, bytes)) => Json(json!({
            "ok": true,
            "which": which,
            "filename": fname,
            "path": path,
            "bytes_written": bytes.parse::<i64>().ok(),
        })),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

// ── E4: Container Management ──────────────────────────────────────────────
//
// Human-admin only (gated by the /api/agent/ deny-list in auth.rs). Drives
// `docker` over the agent-connect SSH key on a registered system:
//   - list containers (running + stopped) with a live `docker stats` snapshot
//     merged in per-container, plus discovered docker-compose files and whether
//     each compose project is currently up;
//   - start / stop / restart a single container by name;
//   - read / write a compose file (traversal-guarded), and bring a compose
//     project up / down.
//
// All `docker` boxes here run Compose v2 (`docker compose …`), not the v1
// `docker-compose` shim, so we always invoke the v2 subcommand form.

/// Sanitize a docker object name (container / project) to a safe shell token:
/// docker names are [a-zA-Z0-9][a-zA-Z0-9_.-]+, so anything outside that set is
/// rejected by returning empty (callers treat empty as "invalid name").
fn sanitize_docker_name(name: &str) -> String {
    let s = name.trim();
    if s.is_empty()
        || s.len() > 256
        || !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return String::new();
    }
    s.to_string()
}

/// Parse one `docker ps -a --format '{{json .}}'` line into our slim shape +
/// pull out the compose project + config-file from the labels if present.
fn parse_container_line(line: &str) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    // Labels is a comma-joined "k=v,k2=v2" string; dig the compose project +
    // its config file path out of it (used to mark compose projects up/down).
    let labels = get("Labels");
    let mut project = String::new();
    let mut config_files = String::new();
    for kv in labels.split(',') {
        if let Some(p) = kv.strip_prefix("com.docker.compose.project=") {
            project = p.to_string();
        } else if let Some(c) = kv.strip_prefix("com.docker.compose.project.config_files=") {
            config_files = c.to_string();
        }
    }
    // `State` (running|exited|…) is unreliable/absent on some of these boxes, so
    // derive a robust `running` flag: trust State when it says "running", else
    // fall back to the always-present `Status` string ("Up 4 hours" → running,
    // "Exited (137) 2 months ago" → not).
    let state = get("State");
    let status = get("Status");
    let running = state == "running" || status.trim_start().starts_with("Up");
    Some(json!({
        "name": get("Names"),
        "image": get("Image"),
        "state": state,            // running | exited | created | paused | …
        "status": status,          // "Up 4 hours" | "Exited (137) 2 months ago"
        "running": running,        // robust: State=="running" || Status starts "Up"
        "ports": get("Ports"),
        "compose_project": project,
        "compose_config_files": config_files,
    }))
}

/// Parse a `docker stats` MemUsage token ("53.43MiB / 30.14GiB") → just the
/// used side, trimmed. Returns "" if it doesn't look right.
fn stats_mem_used(mem_usage: &str) -> String {
    mem_usage.split('/').next().unwrap_or("").trim().to_string()
}

/// GET /agent/systems/:id/containers — running + stopped containers with a live
/// stats snapshot merged per container, plus discovered compose files (path +
/// whether the project is up).
pub async fn list_containers(Path(id): Path<String>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let res = tokio::task::spawn_blocking(move || gather_containers(&sys))
        .await
        .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(v) => Json(v),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

/// One SSH round-trip: emit three delimited sections (ps / stats / compose) we
/// split + parse, so the whole container view is a single connection.
fn gather_containers(sys: &System) -> Result<serde_json::Value, String> {
    // `||true` on each block so a section being empty (e.g. no compose files)
    // never fails the whole pipeline. Markers delimit the three outputs.
    let remote = "echo '@@PS@@'; \
        docker ps -a --no-trunc --format '{{json .}}' 2>/dev/null || true; \
        echo '@@STATS@@'; \
        docker stats --no-stream --format '{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}' 2>/dev/null || true; \
        echo '@@COMPOSE@@'; \
        find /home /opt /srv /root /etc ~ -maxdepth 4 \\( -name docker-compose.yml -o -name docker-compose.yaml -o -name compose.yaml -o -name compose.yml \\) 2>/dev/null || true";
    let out = ssh_capture(sys, remote)?;

    // Split into the three sections.
    let mut section = "";
    let mut ps_lines: Vec<&str> = Vec::new();
    let mut stats_lines: Vec<&str> = Vec::new();
    let mut compose_lines: Vec<&str> = Vec::new();
    for line in out.lines() {
        match line.trim() {
            "@@PS@@" => section = "ps",
            "@@STATS@@" => section = "stats",
            "@@COMPOSE@@" => section = "compose",
            _ => match section {
                "ps" => ps_lines.push(line),
                "stats" => stats_lines.push(line),
                "compose" => compose_lines.push(line),
                _ => {}
            },
        }
    }

    // Live stats: name → {cpu, mem, mem_pct}.
    let mut stats: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    for l in stats_lines {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 4 && !f[0].trim().is_empty() {
            stats.insert(
                f[0].trim().to_string(),
                json!({"cpu": f[1].trim(), "mem": stats_mem_used(f[2]), "mem_full": f[2].trim(), "mem_pct": f[3].trim()}),
            );
        }
    }

    // Containers, with stats merged in by name.
    let mut containers: Vec<serde_json::Value> = Vec::new();
    // Track which compose config-file paths have a *running* container (→ up).
    let mut up_config_files: std::collections::HashSet<String> = std::collections::HashSet::new();
    for l in ps_lines {
        if l.trim().is_empty() {
            continue;
        }
        if let Some(mut c) = parse_container_line(l) {
            let name = c.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let st = stats.get(&name).cloned().unwrap_or(serde_json::Value::Null);
            if let Some(o) = c.as_object_mut() {
                o.insert("stats".into(), st);
            }
            let running = c.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
            if running {
                // config_files is comma-separated when there are several (a
                // single path may itself contain spaces, so DON'T split on space).
                let cfg = c.get("compose_config_files").and_then(|x| x.as_str()).unwrap_or("");
                for p in cfg.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    up_config_files.insert(p.to_string());
                }
            }
            containers.push(c);
        }
    }
    // Sort: running first, then by name.
    containers.sort_by(|a, b| {
        let ar = a.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
        let br = b.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
        br.cmp(&ar).then_with(|| {
            a.get("name").and_then(|x| x.as_str()).unwrap_or("")
                .cmp(b.get("name").and_then(|x| x.as_str()).unwrap_or(""))
        })
    });

    // Compose files: dedup (the `~`/`/home` overlap in the find roots dupes
    // paths) and mark each up if a running container references it.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut compose: Vec<serde_json::Value> = Vec::new();
    for p in compose_lines {
        let p = p.trim();
        if p.is_empty() || !seen.insert(p.to_string()) {
            continue;
        }
        let up = up_config_files.contains(p);
        compose.push(json!({"path": p, "up": up}));
    }
    compose.sort_by(|a, b| {
        a.get("path").and_then(|x| x.as_str()).unwrap_or("")
            .cmp(b.get("path").and_then(|x| x.as_str()).unwrap_or(""))
    });

    let running = containers.iter().filter(|c| c.get("running").and_then(|x| x.as_bool()).unwrap_or(false)).count();
    Ok(json!({
        "ok": true,
        "containers": containers,
        "running": running,
        "total": containers.len(),
        "compose": compose,
    }))
}

#[derive(Deserialize)]
pub struct ContainerActionReq {
    action: String, // "start" | "stop" | "restart"
}

/// POST /agent/systems/:id/containers/:name/action — start/stop/restart one
/// container (name sanitized to a safe docker token).
pub async fn container_action(
    Path((id, name)): Path<(String, String)>,
    Json(req): Json<ContainerActionReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let safe = sanitize_docker_name(&name);
    if safe.is_empty() {
        return Json(json!({"ok": false, "err": "invalid container name"}));
    }
    let verb = match req.action.as_str() {
        "start" => "start",
        "stop" => "stop",
        "restart" => "restart",
        other => return Json(json!({"ok": false, "err": format!("unknown action: {other}")})),
    };
    let action = req.action.clone();
    let res = tokio::task::spawn_blocking(move || {
        // `docker <verb> <name>` prints the name on success.
        ssh_capture(&sys, &format!("docker {verb} {safe} 2>&1"))
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => Json(json!({"ok": true, "action": action, "out": out.trim()})),
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize)]
pub struct ComposeQuery {
    path: String,
}

/// Guard a compose-file path: must be absolute, contain no `..` segment, and end
/// in a recognized compose filename. (We also re-confirm existence on the box in
/// the read/write remote commands.) Returns the cleaned path or an error string.
fn guard_compose_path(path: &str) -> Result<String, String> {
    let p = path.trim();
    if p.is_empty() {
        return Err("path required".into());
    }
    if !p.starts_with('/') {
        return Err("path must be absolute".into());
    }
    if p.split('/').any(|seg| seg == "..") {
        return Err("path may not contain ..".into());
    }
    // Reject shell-meta so the path can't break out of the single-quoted remote.
    if p.contains('\'') || p.contains('\n') || p.contains('\0') {
        return Err("illegal characters in path".into());
    }
    let fname = p.rsplit('/').next().unwrap_or("");
    let ok = matches!(
        fname,
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yaml" | "compose.yml"
    );
    if !ok {
        return Err("not a compose filename (docker-compose.yml / compose.yaml / …)".into());
    }
    Ok(p.to_string())
}

/// GET /agent/systems/:id/compose?path=<file> — read a compose file (guarded).
pub async fn compose_get(
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ComposeQuery>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let path = match guard_compose_path(&q.path) {
        Ok(p) => p,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let path_c = path.clone();
    let res = tokio::task::spawn_blocking(move || {
        // base64-encode the contents on the box so arbitrary YAML rides back
        // cleanly; guard existence + a sane size cap remotely too.
        let remote = format!(
            "F='{path_c}'; if [ ! -f \"$F\" ]; then echo '@@NOFILE@@'; \
             elif [ $(wc -c < \"$F\") -gt 1048576 ]; then echo '@@TOOBIG@@'; \
             else base64 \"$F\"; fi"
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => {
            let t = out.trim();
            if t == "@@NOFILE@@" {
                return Json(json!({"ok": false, "err": "file not found on system"}));
            }
            if t == "@@TOOBIG@@" {
                return Json(json!({"ok": false, "err": "file too large to edit (>1 MB)"}));
            }
            use base64::Engine;
            match base64::engine::general_purpose::STANDARD.decode(t.replace(['\n', '\r'], "")) {
                Ok(bytes) => Json(json!({
                    "ok": true,
                    "path": path,
                    "content": String::from_utf8_lossy(&bytes),
                })),
                Err(e) => Json(json!({"ok": false, "err": format!("decode: {e}")})),
            }
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize)]
pub struct ComposeWriteReq {
    content: String,
}

/// PUT /agent/systems/:id/compose?path=<file> — write a compose file back
/// (guarded). The file must already exist (we don't create new compose roots
/// here — Easy Deploy owns that). Writes via base64-over-ssh.
pub async fn compose_put(
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ComposeQuery>,
    Json(req): Json<ComposeWriteReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let path = match guard_compose_path(&q.path) {
        Ok(p) => p,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    if req.content.len() > 1024 * 1024 {
        return Json(json!({"ok": false, "err": "content too large (>1 MB)"}));
    }
    let content_b64 = b64(req.content.as_bytes());
    let path_c = path.clone();
    let res = tokio::task::spawn_blocking(move || {
        // Refuse to create a brand-new file: it must already exist (we only edit
        // discovered compose files here). Write atomically via a temp + mv.
        let remote = format!(
            "F='{path_c}'; if [ ! -f \"$F\" ]; then echo '@@NOFILE@@'; \
             else TMP=$(mktemp) && echo {content_b64} | base64 -d > \"$TMP\" && mv \"$TMP\" \"$F\" && echo '@@OK@@'; fi"
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => {
            let t = out.trim();
            if t.contains("@@NOFILE@@") {
                Json(json!({"ok": false, "err": "file not found on system (refusing to create new compose roots here)"}))
            } else if t.contains("@@OK@@") {
                Json(json!({"ok": true, "path": path}))
            } else {
                Json(json!({"ok": false, "err": format!("write failed: {t}")}))
            }
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize)]
pub struct ComposeActionReq {
    path: String,
    action: String, // "up" | "down"
}

/// POST /agent/systems/:id/compose/action — `docker compose -f <path> up -d|down`
/// in the file's directory (path guarded, action validated).
pub async fn compose_action(
    Path(id): Path<String>,
    Json(req): Json<ComposeActionReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let path = match guard_compose_path(&req.path) {
        Ok(p) => p,
        Err(e) => return Json(json!({"ok": false, "err": e})),
    };
    let sub = match req.action.as_str() {
        "up" => "up -d",
        "down" => "down",
        other => return Json(json!({"ok": false, "err": format!("unknown action: {other}")})),
    };
    let action = req.action.clone();
    let res = tokio::task::spawn_blocking(move || {
        // cd into the compose dir (so relative volumes/env_file resolve) then run
        // the v2 subcommand. Capture combined output for the UI.
        let remote = format!("cd \"$(dirname '{path}')\" && docker compose -f '{path}' {sub} 2>&1");
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => Json(json!({"ok": true, "action": action, "out": out.trim()})),
        Err(e) => Json(json!({"ok": false, "err": e, "action": action})),
    }
}

// ── E5: Easy Deploy — model + container picker, template flags, progress ──────
//
// The "easy button" for standing up a model server (or ComfyUI) on a GPU box.
// Redesigned from the raw flag-form v1 into a curated picker:
//
//   • A CURATED CATALOG (`deploy_catalog`) pairs each model with the serving
//     container image that runs it (+ a version tag) and SENSIBLE TEMPLATE FLAGS
//     (max model length, GPU allocation, max batched tokens, max seqs/sessions).
//     vLLM-served LLMs → a vLLM image; image-gen → a ComfyUI image. The list is
//     editable in code below — a live GHCR/registry catalog fetch is a future
//     enhancement (needs `gh`/a registry token, not installed here).
//   • The same catalog response reports WHAT'S ALREADY ON THE BOX: `docker ps -a`
//     plus any models we can detect (HF cache dirs, common model roots, and the
//     `--served-model-name` of any running vLLM) so the UI can flag "already
//     deployed / available".
//   • `deploy_image` (POST /deploy) renders a compose into a UNIQUE per-deploy
//     dir `~/aeon-deploy/<name>/`, then KICKS OFF `docker compose pull && up -d`
//     in the BACKGROUND (nohup, writes a status file) and returns immediately
//     with a deploy id — the pull is multi-GB and slow.
//   • `deploy_status` (GET /deploy/status) reads that status file and reports a
//     phase (writing|pulling|starting|running|failed) + a rough percent for the
//     UI's progress bar. Bounded + robust (never hangs the request).
//
// SAFETY: we only ever CREATE under `~/aeon-deploy/<name>/` (refuse to clobber an
// existing dir) and pull/launch — we never touch or delete pre-existing stacks.
//
// Restricted to systems that can actually run this: the `dgx` role OR any system
// reporting docker + an NVIDIA GPU (probed live).

const DEPLOY_DIR: &str = "$HOME/aeon-deploy";

/// Default context window for new deploys: 128k (131072). The big-VRAM boxes we
/// target (GB10 / DGX Spark) comfortably hold this for the curated model sizes.
const DEFAULT_MODEL_LEN: i64 = 131072;
/// Default GPU VRAM utilization (NOT 1.0 — that OOMs the box). vLLM's
/// `--gpu-memory-utilization` is a 0.0–1.0 fraction; the UI presents it as a
/// 0–100% slider (default 70%).
const DEFAULT_GPU_UTIL: f64 = 0.7;

/// One curated catalog entry: a model paired with the serving-container image
/// (+ version) that runs it, plus sensible default ("template") flags.
#[allow(clippy::too_many_arguments)]
fn cat(
    id: &str,
    model: &str,
    container_image: &str,
    kind: &str,
    description: &str,
    max_model_len: i64,
    gpu: &str,
    gpu_mem_util: f64,
    max_num_batched_tokens: i64,
    max_num_seqs: i64,
    extra: &str,
) -> serde_json::Value {
    json!({
        "id": id,
        "model": model,
        "container_image": container_image,
        "kind": kind,                 // llm | imagegen | tts | embedding
        "description": description,
        "template_flags": {
            "max_model_len": max_model_len,
            "gpu": gpu,               // "all" | "1" | "0,1" — which/how many devices
            "gpu_mem_util": gpu_mem_util, // 0.0–1.0 → vLLM --gpu-memory-utilization (the % VRAM slider)
            "max_num_batched_tokens": max_num_batched_tokens,
            "max_num_seqs": max_num_seqs,
            "extra": extra,           // additional server args, space-separated
        }
    })
}

// vLLM OpenAI-compatible server image (pin a known-good tag; bump as needed).
const VLLM_IMAGE: &str = "vllm/vllm-openai:v0.6.6";

// ── Live AEON-7 catalog (HuggingFace models + GHCR containers) ────────────────
//
// "AEON-7 easy model deployment" pulls LIVE from AEON-7's PUBLIC HuggingFace repos
// (no auth) so newly-published models show up automatically, paired with the
// matching AEON-7 GHCR serving container. GHCR's packages API requires a token
// (it 401s unauthenticated), so the container side is a SEEDED list of the known
// images (admin-editable in code) until a GHCR token is wired in.
//
// EXCLUDED per spec: any model tagged/named step3.7 (marked broken) and any
// Nemotron model (marked experimental).
//
// The fetch is cached briefly (CATALOG_TTL) so the catalog endpoint stays fast;
// on a fetch failure we fall back to the curated list + a note.

const HF_MODELS_URL: &str = "https://huggingface.co/api/models?author=AEON-7&limit=100";
/// GHCR org packages API — surfaced for documentation; it 401s without a token.
const GHCR_PACKAGES_URL: &str = "https://api.github.com/orgs/AEON-7/packages?package_type=container";
/// How long a live HF fetch is cached before we re-fetch.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(420); // 7 min

/// SEEDED AEON-7 GHCR serving containers (GHCR needs a token → can't list live).
/// `(image_ref, [model-name substrings it serves], short quickstart description)`.
/// Image refs confirmed from the models' HuggingFace cards; admin can edit here.
fn aeon7_ghcr_seed() -> Vec<(&'static str, &'static [&'static str], &'static str)> {
    vec![
        ("ghcr.io/aeon-7/vllm-aeon-ultimate-dflash:qwen36-v3",
         &["Qwen3.6-27B-AEON-Ultimate"][..],
         "vLLM DFlash serving container for Qwen3.6-27B AEON-Ultimate (DGX Spark / GB10, NVFP4, speculative decoding). 32 tok/s median, ~350ms TTFT."),
        ("ghcr.io/aeon-7/aeon-gemma-4-26b-a4b-dflash:v2",
         &["Gemma-4-26B-A4B"][..],
         "vLLM DFlash serving container for Gemma-4 26B-A4B (DGX Spark, NVFP4 + speculative decoding)."),
        ("ghcr.io/aeon-7/vllm-spark-gemma4-nvfp4:latest",
         &["Gemma-4-31B", "Gemma-4-E4B", "supergemma4"][..],
         "vLLM DGX-Spark serving container for the Gemma-4 NVFP4 family (31B / E4B / SuperGemma4)."),
        ("ghcr.io/aeon-7/vllm-dflash:latest",
         &["DFlash-Qwen3.5"][..],
         "Generic vLLM DFlash serving container for the Qwen3.5 DFlash drafters."),
        ("ghcr.io/aeon-7/vllm-spark-omni-q36:latest",
         &["Qwen3.6-35B-A3B", "Multimodal"][..],
         "vLLM DGX-Spark omni container for the Qwen3.6-35B-A3B / multimodal NVFP4 builds."),
    ]
}

/// Standalone GHCR images that aren't tied to one HF model but the admin wants
/// surfaced. `(id, image_ref, kind, description)`.
fn aeon7_ghcr_standalone() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        // ComfyUI optimized for DGX Spark. (No image ref was published on the HF
        // cards; this is a sensible seeded ghcr.io/aeon-7 ref the admin can edit.)
        ("aeon7-comfyui-dgx-spark", "ghcr.io/aeon-7/comfyui-dgx-spark:latest", "imagegen",
         "ComfyUI container optimized for DGX Spark (Blackwell / GB10) — image generation (SD/SDXL/Flux). Mount or download checkpoints into the container. Seeded GHCR ref (edit if the tag differs)."),
    ]
}

/// ComfyUI catalog entry (DGX-Spark-optimized) — always offered for image-gen.
fn comfyui_entry() -> serde_json::Value {
    let (id, image, kind, desc) = aeon7_ghcr_standalone()[0];
    cat(id, "ComfyUI (bring your own checkpoints)", image, kind, desc,
        0, "1", DEFAULT_GPU_UTIL, 0, 0, "")
}

/// Pick the best-matching seeded GHCR serving container for an HF model name.
/// Returns (image_ref, short_container_desc) or falls back to the stock vLLM image.
fn pair_container(model_name: &str) -> (String, Option<String>) {
    let lower = model_name.to_lowercase();
    for (image, needles, desc) in aeon7_ghcr_seed() {
        if needles.iter().any(|n| lower.contains(&n.to_lowercase())) {
            return (image.to_string(), Some(desc.to_string()));
        }
    }
    (VLLM_IMAGE.to_string(), None)
}

/// True if a model must be EXCLUDED: step3.7 (broken) or Nemotron (experimental).
/// Checks both the id (name) and the tag list.
fn excluded_model(id: &str, tags: &[String]) -> bool {
    let l = id.to_lowercase();
    let name_hit = l.contains("step-3.7") || l.contains("step3.7") || l.contains("step3p7")
        || l.contains("nemotron");
    let tag_hit = tags.iter().any(|t| {
        let t = t.to_lowercase();
        t == "step3p7" || t.contains("step-3.7") || t.contains("step3.7") || t.contains("nemotron")
    });
    name_hit || tag_hit
}

/// Derive a short, human description for an HF model from its id + tags (the
/// list API carries no card text; this stays fast — no per-model README fetch).
fn describe_model(short: &str, tags: &[String], pipeline: &str, downloads: i64) -> String {
    let has = |t: &str| tags.iter().any(|x| x.eq_ignore_ascii_case(t));
    let mut bits: Vec<String> = Vec::new();
    if has("uncensored") || has("abliterated") || has("heretic") || has("decensored") {
        bits.push("abliterated / uncensored".into());
    }
    if has("nvfp4") || has("fp4") { bits.push("NVFP4".into()); }
    else if has("bf16") || has("bfloat16") { bits.push("BF16".into()); }
    else if has("gguf") { bits.push("GGUF".into()); }
    if has("dflash") || has("speculative-decoding") { bits.push("DFlash spec-decode".into()); }
    if has("multimodal") || has("vision") || pipeline.contains("image-text") || pipeline == "any-to-any" {
        bits.push("multimodal".into());
    }
    if has("dgx-spark") || has("gb10") || has("blackwell") { bits.push("DGX Spark".into()); }
    if has("moe") || has("mixture-of-experts") { bits.push("MoE".into()); }
    let suffix = if bits.is_empty() { String::new() } else { format!(" — {}", bits.join(", ")) };
    let dl = if downloads >= 1000 { format!("  ({}k downloads)", downloads / 1000) }
        else if downloads > 0 { format!("  ({} downloads)", downloads) } else { String::new() };
    format!("AEON-7 {short}{suffix}.{dl}")
}

/// Fetch AEON-7's public HuggingFace models and turn them into catalog entries
/// (filtered + paired with a GHCR serving container). Returns (entries, note).
/// Note is set on fetch failure (UI shows "live fetch unavailable").
fn fetch_aeon7_from_hf() -> (Vec<serde_json::Value>, Option<String>) {
    let v = match http_get_json_simple(HF_MODELS_URL) {
        Ok(v) => v,
        Err(e) => return (Vec::new(), Some(format!("AEON-7 HuggingFace fetch unavailable ({e}) — showing curated models only."))),
    };
    let Some(arr) = v.as_array() else {
        return (Vec::new(), Some("AEON-7 HuggingFace returned an unexpected shape — showing curated models only.".into()));
    };
    // Build (downloads, entry) pairs so we can present the most-used first.
    let mut scored: Vec<(i64, serde_json::Value)> = Vec::new();
    for m in arr {
        let id = m.get("id").and_then(|x| x.as_str()).unwrap_or("");
        if id.is_empty() { continue; }
        let tags: Vec<String> = m.get("tags").and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if excluded_model(id, &tags) { continue; }
        // Skip obvious non-servable staging/draft artifacts.
        let lower = id.to_lowercase();
        if lower.contains("staging") { continue; }
        let pipeline = m.get("pipeline_tag").and_then(|x| x.as_str()).unwrap_or("");
        let library = m.get("library_name").and_then(|x| x.as_str()).unwrap_or("");
        let downloads = m.get("downloads").and_then(|x| x.as_i64()).unwrap_or(0);
        let short = id.strip_prefix("AEON-7/").unwrap_or(id);
        // GGUF builds aren't vLLM-served; surface them but mark kind generic.
        let kind = if library == "gguf" || tags.iter().any(|t| t.eq_ignore_ascii_case("gguf")) {
            "llm-gguf"
        } else {
            "llm"
        };
        let (image, cdesc) = pair_container(short);
        let mut desc = describe_model(short, &tags, pipeline, downloads);
        if let Some(cd) = cdesc { desc = format!("{desc}  Serving container: {cd}"); }
        // id: a docker-safe slug from the model name.
        let slug = short.to_lowercase().chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>();
        let slug = slug.trim_matches('-').to_string();
        scored.push((downloads, cat(
            &format!("aeon7-{slug}"), id, &image, kind, &desc,
            DEFAULT_MODEL_LEN, "1", DEFAULT_GPU_UTIL, 8192, 8, "",
        )));
    }
    // Most-downloaded AEON-7 models first.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    (scored.into_iter().map(|(_, e)| e).collect(), None)
}

/// Cached live AEON-7 catalog. Returns (entries, note). Re-fetches at most every
/// CATALOG_TTL; serves the cached copy in between. Thread-safe via a Mutex.
fn aeon7_live_catalog() -> (Vec<serde_json::Value>, Option<String>) {
    use std::sync::{Mutex, OnceLock};
    struct Cache {
        at: std::time::Instant,
        entries: Vec<serde_json::Value>,
        note: Option<String>,
    }
    static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.at.elapsed() < CATALOG_TTL {
                return (c.entries.clone(), c.note.clone());
            }
        }
    }
    let (entries, note) = fetch_aeon7_from_hf();
    // On a fetch failure (empty + note), keep any prior good cache rather than
    // blanking the live section — only overwrite the cache on a non-empty result.
    let mut guard = lock.lock().unwrap_or_else(|p| p.into_inner());
    if entries.is_empty() {
        if let Some(c) = guard.as_ref() {
            // Stale-but-usable: return the previous good entries, with the new note.
            return (c.entries.clone(), note.or_else(|| c.note.clone()));
        }
    }
    *guard = Some(Cache { at: std::time::Instant::now(), entries: entries.clone(), note: note.clone() });
    (entries, note)
}

/// Minimal blocking GET → JSON (public, no auth) for the HF catalog fetch.
/// HF rejects requests without a User-Agent, so we always send one.
fn http_get_json_simple(url: &str) -> Result<serde_json::Value, String> {
    match ureq::get(url)
        .set("User-Agent", "AEON-Magick-AgentDash/1.0")
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(resp) => {
            let body = resp.into_string().map_err(|e| format!("read body: {e}"))?;
            serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
        }
        Err(ureq::Error::Status(code, _)) => Err(format!("HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("network: {t}")),
    }
}

/// CURATED upstream vLLM models — NO LONGER merged into the catalog (AEON-7 wants
/// an AEON-7-only / live-HF catalog). Retained for reference / quick re-enable.
/// Defaults: 128k context, 70% VRAM util (NOT 1.0 — that OOMs), single-GPU.
#[allow(dead_code)]
fn curated_commonly_used() -> Vec<serde_json::Value> {
    const VLLM: &str = VLLM_IMAGE;
    const ML: i64 = DEFAULT_MODEL_LEN; // 128k
    const U: f64 = DEFAULT_GPU_UTIL; // 0.7
    vec![
        // ── LLMs (vLLM-served) ──
        cat("qwen3-8b", "Qwen/Qwen3-8B", VLLM, "llm",
            "Qwen3 8B — strong general/agentic model; comfortable on a single GPU.",
            ML, "1", U, 8192, 16, ""),
        cat("qwen3-14b", "Qwen/Qwen3-14B", VLLM, "llm",
            "Qwen3 14B — bigger Qwen3; great quality/throughput tradeoff.",
            ML, "1", U, 8192, 12, ""),
        cat("qwen3-32b", "Qwen/Qwen3-32B", VLLM, "llm",
            "Qwen3 32B — high quality; wants a large-VRAM GPU (or tensor-parallel).",
            ML, "1", U, 8192, 8, ""),
        cat("llama31-8b", "meta-llama/Llama-3.1-8B-Instruct", VLLM, "llm",
            "Llama 3.1 8B Instruct — Meta's solid 8B chat model (HF-gated; accept the license).",
            ML, "1", U, 8192, 16, ""),
        cat("llama33-70b", "meta-llama/Llama-3.3-70B-Instruct", VLLM, "llm",
            "Llama 3.3 70B Instruct — frontier-class open weights; needs lots of VRAM (multi-GPU).",
            ML, "all", U, 8192, 6, ""),
        cat("mistral-small-24b", "mistralai/Mistral-Small-24B-Instruct-2501", VLLM, "llm",
            "Mistral Small 24B Instruct — efficient mid-size instruct model.",
            ML, "1", U, 8192, 10, ""),
        cat("gemma2-9b", "google/gemma-2-9b-it", VLLM, "llm",
            "Gemma 2 9B Instruct — Google's compact instruct model (HF-gated).",
            ML, "1", U, 8192, 16, ""),
        // ── Embeddings (vLLM in embedding mode) ──
        cat("bge-m3-embed", "BAAI/bge-m3", VLLM, "embedding",
            "BGE-M3 multilingual embeddings — served by vLLM in --task embed mode.",
            8192, "1", U, 8192, 32, "--task embed"),
    ]
}

/// The full Easy-Deploy catalog = live AEON-7 (HF models + GHCR containers,
/// cached) merged with the curated commonly-used vLLM list (deduped by model id).
/// `live` carries (entries, note) where note flags fallback/auth issues for the UI.
fn deploy_catalog_merged() -> (Vec<serde_json::Value>, Option<String>) {
    // AEON-7 ONLY: the catalog is a live pull of AEON-7's whole HuggingFace
    // collection — no generic/upstream models. The DGX-Spark-optimized ComfyUI
    // image is the one always-offered AEON-7 container.
    let (mut live, note) = aeon7_live_catalog();
    live.push(comfyui_entry());
    (live, note)
}

/// Back-compat shim used by `deploy_image`'s catalog-id resolution. Returns the
/// merged catalog as a JSON array (live + curated + comfyui).
fn curated_deploy_catalog() -> serde_json::Value {
    serde_json::Value::Array(deploy_catalog_merged().0)
}

/// Shell snippet (no quoting hazards) that prints what's already on the box: a
/// `docker ps -a` section and a best-effort list of detected models (HF cache
/// repo dirs, common model roots, and the `--served-model-name` of any running
/// vLLM). Sections are delimited by markers we split on.
const INSTALLED_PROBE: &str = r#"echo '@@CONTAINERS@@'
docker ps -a --no-trunc --format '{{json .}}' 2>/dev/null || true
echo '@@MODELS@@'
# HF hub cache repos: ~/.cache/huggingface/hub/models--<org>--<name> → org/name
for base in "$HOME/.cache/huggingface/hub" /root/.cache/huggingface/hub; do
  [ -d "$base" ] && ls -1 "$base" 2>/dev/null | sed -n 's#^models--##p' | sed 's#--#/#g'
done
# Common model roots: top-level dir names (one level deep)
for d in "$HOME/models" /models /opt/models /raid/models /srv/models; do
  [ -d "$d" ] && find "$d" -maxdepth 1 -mindepth 1 -type d -printf '%f\n' 2>/dev/null
done
# Any running vLLM's served-model-name(s) from its cmdline
for p in $(pgrep -f vllm 2>/dev/null); do
  tr '\0' '\n' < "/proc/$p/cmdline" 2>/dev/null | grep -A1 -- '--served-model-name' | tail -n1
  tr '\0' '\n' < "/proc/$p/cmdline" 2>/dev/null | grep -A1 -- '--model' | tail -n1
done
echo '@@END@@'
"#;

/// Probe the box for installed/known containers + detectable models. Returns
/// (containers, models). Never fails hard — a probe error just yields empties.
fn gather_installed(sys: &System) -> (Vec<serde_json::Value>, Vec<String>) {
    let out = match ssh_capture(sys, INSTALLED_PROBE) {
        Ok(o) => o,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let mut section = "";
    let mut containers: Vec<serde_json::Value> = Vec::new();
    let mut models: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in out.lines() {
        match line.trim() {
            "@@CONTAINERS@@" => section = "containers",
            "@@MODELS@@" => section = "models",
            "@@END@@" => section = "",
            _ => match section {
                "containers" => {
                    if let Some(c) = parse_container_line(line) {
                        containers.push(c);
                    }
                }
                "models" => {
                    let m = line.trim();
                    // keep org/name or plain dir names; drop obvious junk/paths
                    if !m.is_empty() && m.len() <= 200 && !m.starts_with('-') && !m.starts_with('/') {
                        models.insert(m.to_string());
                    }
                }
                _ => {}
            },
        }
    }
    (containers, models.into_iter().collect())
}

/// True if this system may use Easy Deploy: the `dgx` role, OR it reports docker
/// AND an NVIDIA GPU (probed live, bounded by ssh_capture's timeout).
fn deploy_allowed(sys: &System) -> (bool, String) {
    if sys.roles.iter().any(|r| r == "dgx") {
        return (true, "dgx".into());
    }
    // Probe: does docker exist + is there an NVIDIA GPU?
    let probe = "command -v docker >/dev/null 2>&1 && echo HAVE_DOCKER; \
                 (command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L 2>/dev/null | grep -qi gpu && echo HAVE_GPU) || true";
    match ssh_capture(sys, probe) {
        Ok(o) => {
            let docker = o.contains("HAVE_DOCKER");
            let gpu = o.contains("HAVE_GPU");
            if docker && gpu {
                (true, "docker+gpu".into())
            } else if !docker {
                (false, "no docker on this system".into())
            } else {
                (false, "no NVIDIA GPU detected on this system".into())
            }
        }
        Err(e) => (false, format!("could not probe system: {e}")),
    }
}

/// GET /agent/systems/:id/deploy/catalog — the MERGED model+container catalog
/// (live AEON-7 HuggingFace models + paired GHCR serving containers, cached,
/// merged with the curated commonly-used vLLM list) + what's already on the box
/// (containers + detected models). Allowed for dgx systems or any docker+GPU box.
pub async fn deploy_catalog(Path(id): Path<String>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let res = tokio::task::spawn_blocking(move || {
        let (allowed, why) = deploy_allowed(&sys);
        if !allowed {
            return Err(format!("Easy Deploy needs a GPU + docker — {why}"));
        }
        let (containers, models) = gather_installed(&sys);
        // Build the merged catalog inside the blocking task (it makes the cached
        // HF fetch). `note` flags live-fetch fallback for the UI.
        let (catalog, note) = deploy_catalog_merged();
        Ok((why, containers, models, catalog, note))
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok((why, containers, models, catalog, note)) => {
            // GHCR's packages API needs a token (it 401s unauthenticated), so the
            // AEON-7 container side is a seeded list until a token is wired in.
            let ghcr_note = format!(
                "AEON-7 GHCR containers are seeded (the GitHub packages API {GHCR_PACKAGES_URL} \
                 returns 401 without a token). Edit aeon7_ghcr_seed()/aeon7_ghcr_standalone() to adjust."
            );
            let live_note = note.unwrap_or_else(|| {
                "Live AEON-7 HuggingFace catalog (cached ~7m), step3.7 + Nemotron filtered out, merged with curated vLLM models.".into()
            });
            Json(json!({
                "ok": true,
                "catalog": catalog,
                "deploy_dir": DEPLOY_DIR,
                "allowed_via": why,                 // "dgx" | "docker+gpu"
                "installed_containers": containers, // docker ps -a (slim shape)
                "installed_models": models,         // detected model ids / dir names
                "live_catalog_todo": format!("{live_note} {ghcr_note}"),
                "ghcr_needs_token": true,
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

#[derive(Deserialize, Default)]
pub struct DeployFlags {
    /// vLLM `--max-model-len` (context window). 0/None = omit (image default).
    #[serde(default)]
    max_model_len: Option<i64>,
    /// GPU allocation: "all" | a count ("1") | a device list ("0,1") — WHICH /
    /// HOW MANY devices (drives the compose device reservation + tensor-parallel).
    #[serde(default)]
    gpu: Option<String>,
    /// GPU VRAM utilization as a 0.0–1.0 fraction → vLLM `--gpu-memory-utilization`.
    /// This is the headline control (the UI's % VRAM slider). Default 0.7 if
    /// omitted; clamped to (0,1]. NEVER defaults to 1.0 (that OOMs the box).
    #[serde(default)]
    gpu_mem_util: Option<f64>,
    /// vLLM `--max-num-batched-tokens`. 0/None = omit.
    #[serde(default)]
    max_num_batched_tokens: Option<i64>,
    /// vLLM `--max-num-seqs` = max concurrent sequences/sessions. 0/None = omit.
    #[serde(default)]
    max_num_seqs: Option<i64>,
    /// Extra raw server args, space-separated (the "advanced" escape hatch).
    #[serde(default)]
    extra: Option<String>,
}

#[derive(Deserialize)]
pub struct DeployReq {
    /// Catalog entry id (preferred) — resolves model + container_image + kind.
    #[serde(default)]
    model_id: Option<String>,
    /// Direct image override (used when not picking a catalog id).
    #[serde(default)]
    image: Option<String>,
    /// Model / HF id (positional arg for vLLM). Optional for non-LLM images.
    #[serde(default)]
    model: Option<String>,
    /// Deploy name → container name + `~/aeon-deploy/<name>/`.
    name: String,
    #[serde(default)]
    flags: DeployFlags,
    /// "llm" | "embedding" | "imagegen" | "tts" | … (drives flag→arg mapping).
    #[serde(default)]
    kind: String,
}

/// Validate a GHCR-ish image ref to a safe token (no shell-meta, sane charset).
fn sanitize_image_ref(image: &str) -> String {
    let s = image.trim();
    let ok = !s.is_empty()
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '@'));
    if ok { s.to_string() } else { String::new() }
}

/// Validate a HuggingFace model id / path to a safe token.
fn sanitize_model_ref(model: &str) -> String {
    let s = model.trim();
    let ok = !s.is_empty()
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'));
    if ok { s.to_string() } else { String::new() }
}

/// Filter the "extra" args field to a safe subset: vLLM-style flags, numbers,
/// model-ish tokens. Anything with shell metacharacters is dropped (defense in
/// depth — the whole compose is base64'd to the box, but we still avoid letting
/// junk into the generated command line).
fn sanitize_extra_args(extra: &str) -> String {
    extra
        .split_whitespace()
        .filter(|t| {
            t.len() <= 128
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ','))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a docker-compose.yml string for the deploy. GPU via
/// `deploy.resources.reservations.devices`; for vLLM-served kinds (llm /
/// embedding) we emit the model + the four highlighted flags (+ extra) as the
/// container `command`; for other kinds we surface the values as env so any
/// server can read them.
fn render_deploy_compose(
    name: &str,
    image: &str,
    model: &str,
    kind: &str,
    f: &DeployFlags,
) -> String {
    let gpu = f.gpu.as_deref().unwrap_or("all").trim().to_string();
    let gpu_block = if gpu == "all" || gpu.is_empty() {
        "          - driver: nvidia\n            count: all\n            capabilities: [gpu]".to_string()
    } else if gpu.chars().all(|c| c.is_ascii_digit()) {
        format!("          - driver: nvidia\n            count: {gpu}\n            capabilities: [gpu]")
    } else {
        let ids = gpu
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("          - driver: nvidia\n            device_ids: [{ids}]\n            capabilities: [gpu]")
    };
    // tensor-parallel-size = the GPU count when a bare number / device list.
    let tp_size: Option<i64> = if gpu.chars().all(|c| c.is_ascii_digit()) && !gpu.is_empty() {
        gpu.parse().ok()
    } else if gpu.contains(',') {
        Some(gpu.split(',').filter(|s| !s.trim().is_empty()).count() as i64)
    } else {
        None
    };

    // GPU VRAM utilization (the % slider) → vLLM --gpu-memory-utilization.
    // Clamp to (0,1]; default to 0.7 if omitted/invalid — NEVER 1.0 (OOMs).
    let mem_util = {
        let raw = f.gpu_mem_util.unwrap_or(DEFAULT_GPU_UTIL);
        let v = if raw.is_finite() && raw > 0.0 { raw } else { DEFAULT_GPU_UTIL };
        (v.min(1.0) * 100.0).round() / 100.0 // 2-dp, capped at 1.0
    };
    // Context window: default to 128k when not supplied (0 still means "omit").
    let model_len: Option<i64> = match f.max_model_len {
        Some(0) => None,                       // explicit 0 = omit (image default)
        Some(v) if v > 0 => Some(v),
        _ => Some(DEFAULT_MODEL_LEN),          // unset → 128k
    };

    let is_vllm = kind == "llm" || kind == "embedding";
    let mut cmd_args: Vec<String> = Vec::new();
    let mut env_lines: Vec<String> = Vec::new();
    if is_vllm {
        if !model.is_empty() {
            cmd_args.push(format!("--model {model}"));
            cmd_args.push(format!("--served-model-name {model}"));
        }
        if let Some(tp) = tp_size {
            if tp > 1 {
                cmd_args.push(format!("--tensor-parallel-size {tp}"));
            }
        }
        // Headline VRAM% control.
        cmd_args.push(format!("--gpu-memory-utilization {mem_util}"));
        if let Some(v) = model_len {
            cmd_args.push(format!("--max-model-len {v}"));
        }
        if let Some(v) = f.max_num_batched_tokens.filter(|v| *v > 0) {
            cmd_args.push(format!("--max-num-batched-tokens {v}"));
        }
        if let Some(v) = f.max_num_seqs.filter(|v| *v > 0) {
            cmd_args.push(format!("--max-num-seqs {v}"));
        }
        if let Some(extra) = f.extra.as_deref() {
            let e = sanitize_extra_args(extra);
            if !e.is_empty() {
                cmd_args.push(e);
            }
        }
    } else {
        // Non-vLLM kinds (image-gen / gguf / generic): surface values as env so
        // any server image can read them (incl. the VRAM fraction).
        env_lines.push(format!("      GPU_MEMORY_UTILIZATION: \"{mem_util}\""));
        if let Some(v) = model_len {
            env_lines.push(format!("      MAX_MODEL_LEN: \"{v}\""));
        }
        if let Some(v) = f.max_num_batched_tokens.filter(|v| *v > 0) {
            env_lines.push(format!("      MAX_NUM_BATCHED_TOKENS: \"{v}\""));
        }
        if let Some(v) = f.max_num_seqs.filter(|v| *v > 0) {
            env_lines.push(format!("      MAX_NUM_SEQS: \"{v}\""));
        }
        if let Some(extra) = f.extra.as_deref() {
            let e = sanitize_extra_args(extra);
            if !e.is_empty() {
                env_lines.push(format!("      EXTRA_ARGS: \"{e}\""));
            }
        }
    }
    // Pass HF token through from the host env if present (gated models).
    env_lines.push("      HUGGING_FACE_HUB_TOKEN: \"${HUGGING_FACE_HUB_TOKEN:-}\"".to_string());

    let command_line = if cmd_args.is_empty() {
        String::new()
    } else {
        format!("    command: {}\n", cmd_args.join(" "))
    };
    let env_block = if env_lines.is_empty() {
        String::new()
    } else {
        format!("    environment:\n{}\n", env_lines.join("\n"))
    };
    // vLLM serves OpenAI API on 8000; ComfyUI on 8188. Surface a sensible port.
    let port = if is_vllm { 8000 } else { 8188 };

    format!(
        "# Generated by AEON Magick — Agent Dash Easy Deploy.\n\
         # model={model} kind={kind} gpu={gpu} gpu_mem_util={mem_util} max_model_len={ml:?} max_num_batched_tokens={mbt:?} max_num_seqs={ms:?}\n\
         services:\n  \
         {name}:\n    \
         image: {image}\n    \
         container_name: {name}\n    \
         restart: unless-stopped\n    \
         ipc: host\n    \
         ports:\n      \
         - \"{port}:{port}\"\n\
         {command_line}\
         {env_block}    \
         deploy:\n      \
         resources:\n        \
         reservations:\n          \
         devices:\n{gpu_block}\n",
        ml = model_len,
        mbt = f.max_num_batched_tokens,
        ms = f.max_num_seqs,
    )
}

/// Remote launcher script. Writes the compose into a UNIQUE per-deploy dir
/// (refusing to clobber), seeds a status file, then backgrounds
/// `docker compose pull && up -d` with nohup, streaming progress into the log so
/// `deploy_status` can report a phase + percent. Returns immediately.
///
/// Status file format (first line is the phase token, rest is the live log):
///   PHASE=<writing|pulling|starting|running|failed>
///   …docker output…
fn render_launch_script(dir: &str, compose_b64: &str) -> String {
    format!(
        r#"set -e
DIR="{dir}"
if [ -d "$DIR" ]; then echo "EXISTS"; exit 3; fi
mkdir -p "$DIR"
echo "{compose_b64}" | base64 -d > "$DIR/docker-compose.yml"
STATUS="$DIR/deploy.status"
LOG="$DIR/deploy.log"
printf 'PHASE=writing\n' > "$STATUS"
# Background the slow pull+up. We re-write the phase line as we go and append
# docker's stdout/stderr (which carries pull progress) to the log + status.
nohup bash -c '
  cd "'"$DIR"'"
  printf "PHASE=pulling\n" > "'"$STATUS"'"
  if docker compose pull >>"'"$LOG"'" 2>&1; then
    printf "PHASE=starting\n" > "'"$STATUS"'"
    if docker compose up -d >>"'"$LOG"'" 2>&1; then
      printf "PHASE=running\n" > "'"$STATUS"'"
    else
      printf "PHASE=failed\n" > "'"$STATUS"'"
    fi
  else
    printf "PHASE=failed\n" > "'"$STATUS"'"
  fi
  cat "'"$LOG"'" >> "'"$STATUS"'"
' >/dev/null 2>&1 &
echo "STARTED $DIR"
"#
    )
}

/// POST /agent/systems/:id/deploy — resolve a catalog entry (or a direct image),
/// render a compose into a unique `~/aeon-deploy/<name>/`, and KICK OFF
/// pull+up in the background. Returns immediately with the deploy name so the UI
/// can poll `deploy_status`. Allowed for dgx or any docker+GPU box.
pub async fn deploy_image(Path(id): Path<String>, Json(req): Json<DeployReq>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };

    // Resolve image / model / kind from the catalog id when given, else direct.
    let (mut image, mut model, mut kind) = (
        req.image.clone().unwrap_or_default(),
        req.model.clone().unwrap_or_default(),
        req.kind.clone(),
    );
    if let Some(mid) = req.model_id.as_deref() {
        if let serde_json::Value::Array(entries) = curated_deploy_catalog() {
            if let Some(e) = entries.iter().find(|e| e.get("id").and_then(|x| x.as_str()) == Some(mid)) {
                if image.is_empty() {
                    image = e.get("container_image").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
                if model.is_empty() {
                    model = e.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
                if kind.is_empty() {
                    kind = e.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
                }
            }
        }
    }

    let image = sanitize_image_ref(&image);
    if image.is_empty() {
        return Json(json!({"ok": false, "err": "invalid or missing image reference"}));
    }
    // Model is required for vLLM-served kinds (it's the positional/--model arg);
    // for image-gen / generic it can be empty.
    let kind = kind.trim().to_string();
    let model = if model.trim().is_empty() {
        String::new()
    } else {
        let m = sanitize_model_ref(&model);
        if m.is_empty() {
            return Json(json!({"ok": false, "err": "invalid model reference"}));
        }
        m
    };
    if (kind == "llm" || kind == "embedding") && model.is_empty() {
        return Json(json!({"ok": false, "err": "this kind needs a model id"}));
    }
    let name = sanitize_docker_name(&req.name);
    if name.is_empty() {
        return Json(json!({"ok": false, "err": "invalid deploy name (use [A-Za-z0-9._-])"}));
    }

    let compose = render_deploy_compose(&name, &image, &model, &kind, &req.flags);
    let compose_b64 = b64(compose.as_bytes());
    let name_c = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        let dir = format!("{DEPLOY_DIR}/{name_c}");
        let script = render_launch_script(&dir, &compose_b64);
        // base64 the whole launcher so the nested quoting survives the ssh hop.
        let remote = format!("echo {} | base64 -d | bash", b64(script.as_bytes()));
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => {
            if out.contains("EXISTS") {
                return Json(json!({
                    "ok": false,
                    "err": format!("a deploy named \"{name}\" already exists at {DEPLOY_DIR}/{name} — pick another name"),
                }));
            }
            Json(json!({
                "ok": true,
                "name": name,                 // deploy id for status polling
                "image": image,
                "model": model,
                "kind": kind,
                "compose": compose,
                "path": format!("{DEPLOY_DIR}/{name}/docker-compose.yml"),
                "phase": "writing",
                "out": out.trim(),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e, "compose": compose})),
    }
}

#[derive(Deserialize)]
pub struct DeployStatusQuery {
    name: String,
}

/// Map a phase token → a rough percent for the progress bar (when we can't parse
/// a finer pull percentage from the log).
fn phase_percent(phase: &str) -> i64 {
    match phase {
        "writing" => 10,
        "pulling" => 50,
        "starting" => 90,
        "running" => 100,
        "failed" => 100,
        _ => 0,
    }
}

/// Try to refine pull progress from the docker log tail. `docker compose pull`
/// prints per-layer lines; we approximate overall % as the mean of the latest
/// per-component percentages we can see, scaled into the pulling band (10–85%).
fn parse_pull_percent(log: &str) -> Option<i64> {
    let mut pcts: Vec<f64> = Vec::new();
    for line in log.lines().rev().take(200) {
        // Lines like "Pulling 3f4d… 45%" or "… Downloading [===> ] 45%".
        if let Some(idx) = line.rfind('%') {
            let start = line[..idx]
                .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
                .map(|p| p + 1)
                .unwrap_or(0);
            if let Ok(p) = line[start..idx].parse::<f64>() {
                if (0.0..=100.0).contains(&p) {
                    pcts.push(p);
                    if pcts.len() >= 12 {
                        break;
                    }
                }
            }
        }
    }
    if pcts.is_empty() {
        return None;
    }
    let mean = pcts.iter().sum::<f64>() / pcts.len() as f64;
    // Scale 0–100 of the pull into the 10–85 band so the bar still advances to
    // "starting" (90) and "running" (100) afterwards.
    Some((10.0 + mean * 0.75).round() as i64)
}

/// GET /agent/systems/:id/deploy/status?name=<name> — report deploy progress for
/// the UI's progress bar: a phase + a rough percent + a short log tail. Bounded
/// + robust (a missing/empty status file just reads as "writing").
pub async fn deploy_status(
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeployStatusQuery>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let name = sanitize_docker_name(&q.name);
    if name.is_empty() {
        return Json(json!({"ok": false, "err": "invalid deploy name"}));
    }
    let name_c = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        let dir = format!("{DEPLOY_DIR}/{name_c}");
        // Read the phase line + the last ~25 log lines in one shot. `|| true`
        // keeps it from failing when files don't exist yet.
        let remote = format!(
            "S=\"{dir}/deploy.status\"; L=\"{dir}/deploy.log\"; \
             echo '@@PHASE@@'; (head -n1 \"$S\" 2>/dev/null) || true; \
             echo '@@LOG@@'; (tail -n 25 \"$L\" 2>/dev/null) || true; \
             echo '@@END@@'"
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => {
            let mut section = "";
            let mut phase_line = String::new();
            let mut log = String::new();
            for line in out.lines() {
                match line.trim() {
                    "@@PHASE@@" => section = "phase",
                    "@@LOG@@" => section = "log",
                    "@@END@@" => section = "",
                    _ => match section {
                        "phase" => {
                            if phase_line.is_empty() {
                                phase_line = line.trim().to_string();
                            }
                        }
                        "log" => {
                            log.push_str(line);
                            log.push('\n');
                        }
                        _ => {}
                    },
                }
            }
            let phase = phase_line
                .strip_prefix("PHASE=")
                .unwrap_or("writing")
                .trim()
                .to_string();
            let phase = if phase.is_empty() { "writing".to_string() } else { phase };
            // Percent: prefer parsed pull % while pulling, else phase-based.
            let percent = if phase == "pulling" {
                parse_pull_percent(&log).unwrap_or_else(|| phase_percent(&phase))
            } else {
                phase_percent(&phase)
            };
            let done = phase == "running" || phase == "failed";
            Json(json!({
                "ok": true,
                "name": name,
                "phase": phase,    // writing | pulling | starting | running | failed
                "percent": percent,
                "done": done,
                "log": log.trim_end(),
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e})),
    }
}

// ── F7b: deploy a NEW agent persona ───────────────────────────────────────
//
// Provision a brand-new persona on the OpenClaw gateway, following the method
// from the create-agentic-personas repo. That repo's `new-persona.sh` is not
// installed on the gateway, so we replicate its SAFE, idempotent steps directly
// over the agent-connect SSH key:
//
//   1. create the workspace  ~/.openclaw/workspace-<id>/  (refuse to clobber)
//   2. write SOUL.md + IDENTITY.md from the admin's input (the two files that
//      *are* the persona), plus a minimal AGENTS.md/USER.md; optional corpus seed
//   3. register the OpenClaw agent:  `openclaw agents add <id> --workspace … --model …`
//   4. set its identity:             `openclaw agents set-identity --agent <id> --name … --emoji …`
//   5. reload the gateway:           `openclaw gateway restart`
//
// What we DELIBERATELY do NOT automate (it needs homeserver admin + minted
// secrets that must live in mode-600 files, never flow through this API): the
// Matrix account creation and the per-agent ~/voip-<id>/.env. Those exact
// commands are RETURNED to the admin (the non-invasive `config_change` pattern
// used by provision_agent / add_skill). The voice clip/description and call-line
// systemd bring-up are likewise returned as the remaining steps.
//
// (Letting an existing operator agent run the whole loop via the repo's
// `create-agentic-persona` SKILL is the eventual "let an agent set it up" path —
// it needs task-dispatch plumbing to the gateway that doesn't exist here yet, so
// it's surfaced as a TODO in the response rather than executed.)

/// Validate a persona id to the repo's rule: lowercase, starts with a letter,
/// then [a-z0-9_-]. Returns the cleaned id or None.
fn valid_persona_id(id: &str) -> Option<String> {
    let s = id.trim().to_ascii_lowercase();
    let mut chars = s.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_lowercase());
    let rest_ok = s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if first_ok && rest_ok && s.len() <= 40 { Some(s) } else { None }
}

/// Keep an emoji/short identity token shell-safe (no quotes/backticks/newlines).
/// We pass it single-quoted in the remote command, so just strip single quotes
/// and control chars and cap the length.
fn clean_short_field(s: &str, max: usize) -> String {
    s.trim()
        .chars()
        .filter(|c| *c != '\'' && *c != '\\' && !c.is_control())
        .take(max)
        .collect()
}

#[derive(Deserialize)]
pub struct NewPersonaReq {
    /// Persona handle / id (lowercase; also the Matrix localpart). Required.
    id: String,
    /// Display name (e.g. "Ada Lovelace"). Defaults to capitalized id.
    #[serde(default)]
    name: String,
    /// Identity emoji.
    #[serde(default)]
    emoji: String,
    /// IDENTITY.md body (facts). If empty, a template is seeded.
    #[serde(default)]
    identity: String,
    /// SOUL.md body (manner/voice). If empty, a template is seeded.
    #[serde(default)]
    soul: String,
    /// Voice: a clone name (short token) OR a designer description (a sentence).
    /// Recorded into IDENTITY/notes + echoed in the returned voice step. Optional.
    #[serde(default)]
    voice: String,
    /// Optional corpus seed: a single markdown note dropped into the agent's
    /// corpus vault so retrieval has something to ground on from day one.
    #[serde(default)]
    corpus_seed: String,
    /// Model id to pin (defaults to the roster's chat model).
    #[serde(default)]
    model: String,
}

/// The provisioning python: creates the workspace + persona files, optional
/// corpus seed, then runs the openclaw CLI steps. Emits a JSON report of every
/// step. All inputs arrive base64'd via argv to avoid any quoting issues.
/// argv: 1=id 2=display 3=emoji 4=model 5=soul_b64 6=identity_b64 7=corpus_b64
const NEW_PERSONA_PY: &str = r#"import json,os,sys,base64,subprocess,shutil
h=os.path.expanduser('~')
aid=sys.argv[1]; disp=sys.argv[2]; emoji=sys.argv[3]; model=sys.argv[4]
def d64(i): return base64.b64decode(sys.argv[i]).decode('utf-8','replace')
soul=d64(5); ident=d64(6); corpus=d64(7)
ws=h+'/.openclaw/workspace-%s'%aid
rep={'id':aid,'workspace':ws,'steps':[]}
def step(name,ok,detail=''):
    rep['steps'].append({'step':name,'ok':bool(ok),'detail':str(detail)[:600]})

# Guard: never clobber an existing persona.
if os.path.exists(ws):
    print(json.dumps({'err':'workspace already exists: '+ws,'id':aid})); sys.exit(0)
# Also refuse if the id is already in the roster.
try:
    cfg=json.load(open(h+'/.openclaw/openclaw.json'))
    if any(isinstance(a,dict) and a.get('id')==aid for a in ((cfg.get('agents') or {}).get('list') or [])):
        print(json.dumps({'err':'agent id already in roster: '+aid,'id':aid})); sys.exit(0)
except Exception:
    pass

# 1. workspace + persona files
try:
    os.makedirs(ws+'/knowledge',exist_ok=True)
    os.makedirs(ws+'/skills',exist_ok=True)
    open(ws+'/SOUL.md','w').write(soul)
    open(ws+'/IDENTITY.md','w').write(ident)
    # Minimal housekeeping files so the workspace matches the standard layout.
    if not os.path.exists(ws+'/AGENTS.md'):
        open(ws+'/AGENTS.md','w').write('# %s — workspace notes\n\nThis is %s\'s home. Memory lives in memory/. Edit SOUL.md/IDENTITY.md to shape who you are.\n'%(disp,disp))
    if not os.path.exists(ws+'/USER.md'):
        open(ws+'/USER.md','w').write('# Who you serve\n\nYour operator. Be direct, honest, and useful.\n')
    step('workspace+persona-files',True,ws)
except Exception as e:
    print(json.dumps({'err':'workspace setup: '+str(e),'id':aid,'steps':rep['steps']})); sys.exit(0)

# 2. optional corpus seed
if corpus.strip():
    try:
        cdir=ws+'/memory/%s-corpus'%aid
        os.makedirs(cdir,exist_ok=True)
        open(cdir+'/seed.md','w').write(corpus)
        step('corpus-seed',True,cdir+'/seed.md')
    except Exception as e:
        step('corpus-seed',False,str(e))

# locate the openclaw CLI (on PATH for a login shell; fall back to the known bin)
oc=shutil.which('openclaw') or (h+'/.npm-global/bin/openclaw')
def run(args):
    try:
        p=subprocess.run([oc]+args,capture_output=True,text=True,timeout=120)
        return p.returncode==0,(p.stdout or '')+(p.stderr or '')
    except Exception as e:
        return False,str(e)

# 3. register the agent
ok,out=run(['agents','add',aid,'--workspace',ws,'--model',model,'--non-interactive','--json'])
step('openclaw agents add',ok,out.strip())
registered=ok

# 4. set identity (name + emoji) — best-effort, non-fatal
if registered:
    ia=['agents','set-identity','--agent',aid,'--name',disp]
    if emoji: ia+=['--emoji',emoji]
    ok2,out2=run(ia)
    step('openclaw agents set-identity',ok2,out2.strip())

# 5. reload the gateway so the new agent is live
if registered:
    ok3,out3=run(['gateway','restart'])
    step('openclaw gateway restart',ok3,out3.strip())

rep['registered']=registered
print(json.dumps(rep))
"#;

/// POST /agent/systems/:id/personas — provision a new persona on the gateway.
/// Creates the workspace + SOUL/IDENTITY files (+ optional corpus seed) and runs
/// the openclaw register/identity/reload steps, then returns the report plus the
/// exact remaining manual commands (Matrix account, voip .env, voice, call line)
/// that need secrets and so are intentionally NOT automated here.
pub async fn agent_create_persona(
    Path(id): Path<String>,
    Json(req): Json<NewPersonaReq>,
) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    let Some(pid) = valid_persona_id(&req.id) else {
        return Json(json!({"ok": false, "err": "id must be lowercase, start with a letter, and use only [a-z0-9_-]"}));
    };
    // Display name: provided, else Capitalize(id).
    let display = {
        let d = req.name.trim();
        if d.is_empty() {
            let mut c = pid.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => pid.clone(),
            }
        } else {
            d.to_string()
        }
    };
    let display = clean_short_field(&display, 80);
    let emoji = clean_short_field(&req.emoji, 16);
    let model = {
        let m = req.model.trim();
        if m.is_empty() { "vllm/qwen36-deep".to_string() } else { clean_short_field(m, 64) }
    };
    // Seed SOUL.md / IDENTITY.md from the input, or a sensible starter template
    // (mirrors templates/persona/{SOUL,IDENTITY}.md but pre-filled). Cap sizes.
    let soul = {
        let s = req.soul.trim();
        if s.is_empty() {
            format!(
                "# You are {display}\n\nYou are {display}.\n\n## How you sound\n- (cadence, vocabulary, humor)\n\n## What you care about\n- (obsessions, values you defend)\n\n## How you treat the person you talk to\n- You are their collaborator. When uncertain, you say so plainly.\n\n## Boundaries\n- You are an AI *interpretation* of {display}, not the person; say so when it matters.\n"
            )
        } else {
            s.to_string()
        }
    };
    let identity = {
        let s = req.identity.trim();
        let voice_line = if req.voice.trim().is_empty() {
            String::new()
        } else {
            format!("\n## Voice\n{}\n", req.voice.trim())
        };
        if s.is_empty() {
            format!(
                "# Identity\n\n- **Display name:** {display}\n- **Handle:** {pid}\n- **Emoji:** {emoji}\n- **Domain of authority:** (what they genuinely know — and its edges)\n\n## One-line self-description\n> \"...\"\n\n## Honesty clause\nThis is an AI interpretation, not the real person; when a listener could be misled, say so.\n{voice_line}"
            )
        } else {
            format!("{s}{voice_line}")
        }
    };
    if soul.len() > 256 * 1024 || identity.len() > 256 * 1024 || req.corpus_seed.len() > 512 * 1024 {
        return Json(json!({"ok": false, "err": "persona text too large"}));
    }
    let soul_b64 = b64(soul.as_bytes());
    let ident_b64 = b64(identity.as_bytes());
    let corpus_b64 = b64(req.corpus_seed.trim().as_bytes());
    let (pid_c, disp_c, emoji_c, model_c) = (pid.clone(), display.clone(), emoji.clone(), model.clone());
    let sys_address = sys.address.clone();
    let res = tokio::task::spawn_blocking(move || {
        let remote = format!(
            "echo {} | base64 -d | python3 - {} '{}' '{}' '{}' {} {} {}",
            b64(NEW_PERSONA_PY.as_bytes()),
            pid_c,
            disp_c,
            emoji_c,
            model_c,
            soul_b64,
            ident_b64,
            corpus_b64
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));

    // The remaining manual steps (need secrets / homeserver admin — not automated).
    let hs = "matrix.unhash.me";
    let manual_steps = json!([
        {
            "title": "Create the Matrix account + mint a token",
            "why": "Needs homeserver admin; the token is a secret that must live in a mode-600 .env, not flow through this API.",
            "cmd": format!(
                "# on the gateway ({}): register @{}:{} and log in for a token\ncurl -s -XPOST http://127.0.0.1:8008/_matrix/client/v3/login -d '{{\"type\":\"m.login.password\",\"identifier\":{{\"type\":\"m.id.user\",\"user\":\"{}\"}},\"password\":\"<{}-password>\",\"initial_device_display_name\":\"{}-voip\"}}'",
                sys_address, pid, hs, pid, pid, pid
            )
        },
        {
            "title": "Create the per-agent voip .env (Matrix/VoIP creds)",
            "why": "This is where the avatar/voice creds live (Feature 6 reads it). Fill the token from the step above.",
            "cmd": format!(
                "mkdir -p ~/voip-{pid}/secrets ~/voip-{pid}/crypto-store && chmod 700 ~/voip-{pid}/secrets ~/voip-{pid}/crypto-store\ncat > ~/voip-{pid}/.env <<'ENV'\nMATRIX_HOMESERVER_URL=http://127.0.0.1:8008\nMATRIX_USER_ID=@{pid}:{hs}\nMATRIX_ACCESS_TOKEN=<paste-token>\nMATRIX_DEVICE_NAME=OpenClaw Voice {pid}\nAUTHORIZED_USERS=@you:{hs}\nENV\nchmod 600 ~/voip-{pid}/.env"
            )
        },
        {
            "title": "Give it a voice (Qwen3-TTS)",
            "why": "Pick clone (reference audio) or design (description) in the voip .env.",
            "cmd": if req.voice.trim().is_empty() {
                "# set VOXTRAL_TTS_MODE=voice_design + VOXTRAL_VOICE_DESCRIPTION=\"...\" in the voip .env (docs/06)".to_string()
            } else {
                format!("# voice you entered: {:?}\n# add to ~/voip-{}/.env: VOXTRAL_VOICE={} (clone) OR VOXTRAL_TTS_MODE=voice_design + VOXTRAL_VOICE_DESCRIPTION=\"{}\"", req.voice.trim(), pid, pid, req.voice.trim())
            }
        },
        {
            "title": "Gate who may summon it, then (optionally) bring up the call line",
            "why": "Mention-gating + per-agent systemd voip services.",
            "cmd": format!(
                "echo '{{\"allowFrom\":[\"@you:{hs}\"]}}' > ~/.openclaw/credentials/matrix-{pid}-allowFrom.json\n# call line (after the .env + scaffold from create-agentic-personas):\n# systemctl --user enable --now {pid}-voip-stt {pid}-voip-tts matrix-voip-agent-{pid}"
            )
        }
    ]);
    let todo_agent_path = "Letting an existing operator agent run the whole create-agentic-persona SKILL end-to-end (incl. the Matrix account + voice) would need task-dispatch plumbing from the supervisor into the gateway, which doesn't exist yet — tracked as a TODO.";

    match res {
        Ok(stdout) => {
            let p: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({"err": format!("bad response: {}", stdout.trim().chars().take(300).collect::<String>())}));
            if let Some(e) = p.get("err").and_then(|x| x.as_str()) {
                return Json(json!({"ok": false, "err": e, "report": p}));
            }
            Json(json!({
                "ok": true,
                "id": pid,
                "display": display,
                "emoji": emoji,
                "model": model,
                "registered": p.get("registered").cloned().unwrap_or(json!(false)),
                "report": p,
                "manual_steps": manual_steps,
                "todo": todo_agent_path,
            }))
        }
        Err(e) => Json(json!({"ok": false, "err": e, "manual_steps": manual_steps, "todo": todo_agent_path})),
    }
}
