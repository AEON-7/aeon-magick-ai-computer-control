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

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
