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
        echo DOCKER:$(docker ps --format '{{.Names}}' 2>/dev/null | tr '\\n' ',')";
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
        }
    }
    json!({"reachable": true, "host": host, "load": load, "mem": mem, "gpus": gpus, "containers": containers})
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
