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
    let remote = "echo HOST:$(hostname); \
        echo LOAD:$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null); \
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
    let mut mac = String::new();
    let mut gpus: Vec<serde_json::Value> = Vec::new();
    let mut containers: Vec<String> = Vec::new();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("HOST:") {
            host = v.trim().into();
        } else if let Some(v) = line.strip_prefix("LOAD:") {
            load = v.trim().into();
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
    json!({"reachable": true, "host": host, "load": load, "mem": mem, "gpus": gpus, "containers": containers, "mac": mac})
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
            &target, "timeout", "12", "bash", "-lc", remote,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
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
/// user_id + access_token from the known cred locations, prints them as
/// `USER_ID\nTOKEN` (or `ERR ...`). base64'd over SSH (argv[1] = agent id).
const MATRIX_CREDS_PY: &str = r#"import json,os,sys,glob
aid=sys.argv[1]
h=os.path.expanduser('~')
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
print('ERR no Matrix creds for "%s" (looked in ~/.openclaw_%s_creds.json and ~/.openclaw/credentials/)'%(aid,aid))
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
    Some(json!({
        "name": get("Names"),
        "image": get("Image"),
        "state": get("State"),     // running | exited | created | paused | …
        "status": get("Status"),   // "Up 4 hours" | "Exited (137) 2 months ago"
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
            let running = c.get("state").and_then(|x| x.as_str()) == Some("running");
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
        let ar = a.get("state").and_then(|x| x.as_str()) == Some("running");
        let br = b.get("state").and_then(|x| x.as_str()) == Some("running");
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

    let running = containers.iter().filter(|c| c.get("state").and_then(|x| x.as_str()) == Some("running")).count();
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

// ── E5: Easy Deploy (AEON-7 GHCR images on the DGX) ───────────────────────
//
// Framework + v1. Restricted to systems with role "dgx" (the GB10 box). We hold
// a small *seeded, admin-editable* list of GHCR images (model servers + a
// ComfyUI image) — the live AEON-7 GHCR catalog needs `gh` / a registry token
// which isn't available here, so live-fetch is a TODO. From a chosen image +
// flags (max model length, max batch size, GPU allocation, max concurrent
// sessions) we generate a docker-compose snippet and, by default, SAVE it into a
// per-deploy dir on the DGX *without* pulling the (multi-GB) image. An explicit
// "deploy now" then runs `docker compose up -d` for it.
//
// TODO (needs the agent-task plumbing): "hand the repo's agents.md to an agent
// to set this up for me" — i.e. dispatch the generated compose + an agents.md
// brief to an OpenClaw agent that has SSH access to the DGX and let it do the
// pull/tune/launch. That requires an agent-task dispatch channel we don't have
// wired yet; left as a placeholder here + in the UI.

const DEPLOY_DIR: &str = "$HOME/aeon-deploy";

/// Seeded GHCR image catalog (admin-editable in the UI; this is just the
/// server-side default the UI seeds from). Marked clearly as placeholders.
fn seeded_deploy_catalog() -> serde_json::Value {
    json!([
        {
            "image": "ghcr.io/aeon-7/vllm-model-server:latest",
            "label": "vLLM model server (placeholder)",
            "kind": "model-server",
            "note": "Edit to your real AEON-7 GHCR tag. Flags map to vLLM args."
        },
        {
            "image": "ghcr.io/aeon-7/comfyui:latest",
            "label": "ComfyUI (placeholder)",
            "kind": "comfyui",
            "note": "Edit to your real AEON-7 GHCR tag."
        }
    ])
}

/// GET /agent/systems/:id/deploy/catalog — the seeded GHCR image list + the
/// deploy dir on the box. dgx-only.
pub async fn deploy_catalog(Path(id): Path<String>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    if !sys.roles.iter().any(|r| r == "dgx") {
        return Json(json!({"ok": false, "err": "Easy Deploy is only available for systems with the \"dgx\" role"}));
    }
    Json(json!({
        "ok": true,
        "catalog": seeded_deploy_catalog(),
        "deploy_dir": DEPLOY_DIR,
        "live_catalog_todo": "Live AEON-7 GHCR catalog fetch needs `gh`/a registry token (not available here) — edit the list above instead.",
    }))
}

#[derive(Deserialize, Default)]
pub struct DeployFlags {
    #[serde(default)]
    model_len: Option<i64>,
    #[serde(default)]
    max_batch: Option<i64>,
    /// GPU allocation: "all" | a count ("1") | a device list ("0,1").
    #[serde(default)]
    gpu: Option<String>,
    #[serde(default)]
    max_sessions: Option<i64>,
}

#[derive(Deserialize)]
pub struct DeployReq {
    image: String,
    name: String,
    #[serde(default)]
    flags: DeployFlags,
    /// "model-server" (vLLM-style flags) | "comfyui" | anything else (generic).
    #[serde(default)]
    kind: String,
    /// When true, actually `docker compose up -d` after writing (pulls + runs).
    /// Default false = generate + save only (safe; no multi-GB pull).
    #[serde(default)]
    deploy_now: bool,
}

/// Validate a GHCR-ish image ref to a safe token (no shell-meta, sane charset).
fn sanitize_image_ref(image: &str) -> String {
    let s = image.trim();
    let ok = !s.is_empty()
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '@'));
    if ok { s.to_string() } else { String::new() }
}

/// Build a docker-compose.yml string for the deploy from the chosen flags. We
/// keep it minimal + readable: GPU via `deploy.resources.reservations.devices`,
/// the model/batch flags as the container `command`, and max-sessions surfaced
/// as an env var the server can read.
fn render_deploy_compose(name: &str, image: &str, kind: &str, f: &DeployFlags) -> String {
    let gpu = f.gpu.as_deref().unwrap_or("all").trim().to_string();
    // GPU reservation: "all" → count: all; a bare number → that count; a device
    // list "0,1" → device_ids.
    let gpu_block = if gpu == "all" || gpu.is_empty() {
        "          - driver: nvidia\n            count: all\n            capabilities: [gpu]".to_string()
    } else if gpu.chars().all(|c| c.is_ascii_digit()) {
        format!("          - driver: nvidia\n            count: {gpu}\n            capabilities: [gpu]")
    } else {
        // device list
        let ids = gpu
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("          - driver: nvidia\n            device_ids: [{ids}]\n            capabilities: [gpu]")
    };

    // Command + env derived from flags. For a model-server we emit vLLM-style
    // args; otherwise we just surface the values as env so any server can read
    // them, and leave command unset (image default entrypoint).
    let mut cmd_args: Vec<String> = Vec::new();
    let mut env_lines: Vec<String> = Vec::new();
    if kind == "model-server" {
        if let Some(ml) = f.model_len {
            cmd_args.push(format!("--max-model-len {ml}"));
        }
        if let Some(mb) = f.max_batch {
            cmd_args.push(format!("--max-num-seqs {mb}"));
        }
    } else {
        if let Some(ml) = f.model_len {
            env_lines.push(format!("      MAX_MODEL_LEN: \"{ml}\""));
        }
        if let Some(mb) = f.max_batch {
            env_lines.push(format!("      MAX_BATCH_SIZE: \"{mb}\""));
        }
    }
    if let Some(ms) = f.max_sessions {
        env_lines.push(format!("      MAX_CONCURRENT_SESSIONS: \"{ms}\""));
    }
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

    format!(
        "# Generated by AEON Magick — Agent Dash Easy Deploy (E5).\n\
         # Review before deploying. Flags: model_len={ml:?} max_batch={mb:?} gpu={gpu} max_sessions={ms:?}\n\
         services:\n  \
         {name}:\n    \
         image: {image}\n    \
         container_name: {name}\n    \
         restart: unless-stopped\n\
         {command_line}\
         {env_block}    \
         deploy:\n      \
         resources:\n        \
         reservations:\n          \
         devices:\n{gpu_block}\n",
        ml = f.model_len,
        mb = f.max_batch,
        ms = f.max_sessions,
    )
}

/// POST /agent/systems/:id/deploy — generate a compose snippet from the image +
/// flags, write it to a per-deploy dir on the DGX, and (only if deploy_now)
/// `docker compose up -d`. dgx-only. Returns the generated compose + the path.
pub async fn deploy_image(Path(id): Path<String>, Json(req): Json<DeployReq>) -> impl IntoResponse {
    let Some(sys) = load_systems().into_iter().find(|s| s.id == id) else {
        return Json(json!({"ok": false, "err": "no such system"}));
    };
    if !sys.roles.iter().any(|r| r == "dgx") {
        return Json(json!({"ok": false, "err": "Easy Deploy is only available for systems with the \"dgx\" role"}));
    }
    let image = sanitize_image_ref(&req.image);
    if image.is_empty() {
        return Json(json!({"ok": false, "err": "invalid image reference"}));
    }
    let name = sanitize_docker_name(&req.name);
    if name.is_empty() {
        return Json(json!({"ok": false, "err": "invalid deploy name (use [A-Za-z0-9._-])"}));
    }
    let compose = render_deploy_compose(&name, &image, req.kind.trim(), &req.flags);
    let deploy_now = req.deploy_now;
    let compose_b64 = b64(compose.as_bytes());
    let name_c = name.clone();
    let res = tokio::task::spawn_blocking(move || {
        // Write the compose into $DEPLOY_DIR/<name>/docker-compose.yml. Only run
        // it when deploy_now — otherwise just save (no pull, no launch).
        let dir = format!("{DEPLOY_DIR}/{name_c}");
        let run = if deploy_now {
            format!(" && cd \"{dir}\" && docker compose up -d 2>&1")
        } else {
            String::new()
        };
        let remote = format!(
            "mkdir -p \"{dir}\" && echo {compose_b64} | base64 -d > \"{dir}/docker-compose.yml\" && echo \"WROTE {dir}/docker-compose.yml\"{run}"
        );
        ssh_capture(&sys, &remote)
    })
    .await
    .unwrap_or_else(|_| Err("join error".into()));
    match res {
        Ok(out) => Json(json!({
            "ok": true,
            "deployed": deploy_now,
            "name": name,
            "image": image,
            "compose": compose,
            "path": format!("{DEPLOY_DIR}/{name}/docker-compose.yml"),
            "out": out.trim(),
        })),
        Err(e) => Json(json!({"ok": false, "err": e, "compose": compose})),
    }
}
