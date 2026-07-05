//! Aeon Bench — deploy the **Aeon Bench Pod** (open LLM benchmarking:
//! pull → verify → serve → benchmark → ed25519-sign → submit to the
//! aeon-bench.com leaderboard) either on **this Orb** (local docker) or on a
//! **GPU server linked in the Agent Dashboard** (over the agent-connect SSH
//! key), then reach the pod dashboard (:8080) from the console or a browser.
//!
//! The pod is a docker-compose stack (`deploy/pod/docker-compose.yml`): a vLLM
//! `model-under-test`, a `pull` verifier, harness builders, an orchestrator,
//! and `pod-dashboard`. Only `AEON_HF_LINK` is required. Serving + the full
//! benchmark need an NVIDIA GPU, so real runs target a connected GPU box; a
//! GPU-less Orb still hosts the dashboard/verification. The whole deploy is
//! backgrounded (clone/pull → write .env → `docker compose up -d --build`),
//! with phases surfaced through a status file the UI polls — same shape as the
//! Agent-Dashboard Easy Deploy flow.

use crate::api::AppState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

const DEFAULT_DASH_PORT: u16 = 8080;

/// The heavy worker, decoded + nohup-run on the target. Static (the repo URL is
/// inline) so it carries no untrusted interpolation; the model config arrives
/// separately as a base64 `.env`. Writes `PHASE=…` to a status file the UI polls.
const RUN_SCRIPT: &str = r#"#!/bin/bash
WORK="$HOME/aeon-bench-pod"; REPO="$WORK/repo"
S="$WORK/.aeon-bench.status"; L="$WORK/.aeon-bench.log"; : > "$L"
if ! command -v git >/dev/null 2>&1; then
  (apt-get update -y && apt-get install -y git) >> "$L" 2>&1 || { echo "git is not installed" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
fi
if [ -d "$REPO/.git" ]; then
  echo PHASE=updating > "$S"; git -C "$REPO" pull --ff-only >> "$L" 2>&1 || true
else
  echo PHASE=cloning > "$S"; rm -rf "$REPO"
  git clone --depth 1 https://github.com/AEON-7/Aeon-Bench-Pod.git "$REPO" >> "$L" 2>&1 \
    || { echo "git clone failed — need network access to github.com" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
fi
D="$REPO/deploy/pod"
[ -d "$D" ] || { echo "deploy/pod not found in the repo layout" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
base64 -d "$WORK/.aeon-bench.envb64" > "$D/.env" 2>> "$L"
if ! command -v docker >/dev/null 2>&1; then
  echo PHASE=installing-docker > "$S"
  (apt-get update -y && apt-get install -y docker.io) >> "$L" 2>&1 \
    || { echo "Docker is not installed and could not be auto-installed — install Docker, or deploy to a connected GPU server instead." >> "$L"; echo PHASE=failed > "$S"; exit 1; }
  command -v systemctl >/dev/null 2>&1 && systemctl start docker >> "$L" 2>&1 || true
fi
DC="docker compose"; docker compose version >/dev/null 2>&1 || DC="docker-compose"
cd "$D" || { echo PHASE=failed > "$S"; exit 1; }
echo PHASE=building > "$S"
if $DC up -d --build >> "$L" 2>&1; then
  echo PHASE=running > "$S"
else
  echo "docker compose up failed — see the log above (a GPU is required to serve the model)" >> "$L"
  echo PHASE=failed > "$S"
fi
"#;

/// Report the current phase + a log tail + whether the dashboard container is up
/// + the dashboard port (read back from the deployed .env). Markers delimit the
/// sections so we can parse a single round-trip.
const STATUS_SCRIPT: &str = r#"WORK="$HOME/aeon-bench-pod"; D="$WORK/repo/deploy/pod"
echo '@PHASE@'; cat "$WORK/.aeon-bench.status" 2>/dev/null
echo '@PORT@'; grep -E '^AEON_DASH_PORT=' "$D/.env" 2>/dev/null | tail -1 | cut -d= -f2
echo '@PS@'; (docker ps --filter name=pod-dashboard --format '{{.Names}} {{.Status}}' 2>/dev/null || true)
echo '@LOG@'; tail -n 40 "$WORK/.aeon-bench.log" 2>/dev/null
true
"#;

const STOP_SCRIPT: &str = r#"WORK="$HOME/aeon-bench-pod"; D="$WORK/repo/deploy/pod"
DC="docker compose"; docker compose version >/dev/null 2>&1 || DC="docker-compose"
if cd "$D" 2>/dev/null; then $DC down >> "$WORK/.aeon-bench.log" 2>&1; fi
echo PHASE=stopped > "$WORK/.aeon-bench.status"
echo STOPPED
"#;

#[derive(Deserialize)]
pub struct DeployReq {
    /// "local" (this Orb) or a connected-system id from the Agent Dashboard.
    pub target: String,
    /// HuggingFace model id, e.g. "org/Model" — becomes AEON_HF_LINK.
    pub hf_link: String,
    #[serde(default)]
    pub hf_token: String,
    /// Extra AEON_* / HF_* env overrides (mothership, judge, max tokens, dash
    /// port, quant, …). Keys are validated; values are newline-stripped.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct TargetQuery {
    pub target: String,
}

fn b64(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(d)
}

/// "org/model" shape (also accepts a bare repo id). No shell metacharacters.
fn valid_hf_link(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn env_key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn clean_val(v: &str) -> String {
    v.chars().filter(|c| *c != '\n' && *c != '\r').take(500).collect()
}

/// Build the pod `.env` from the request. Only whitelisted-shape keys pass, and
/// AEON_HF_LINK / HF_TOKEN come from the dedicated fields so they can't be
/// overridden or duplicated by the free-form map.
fn build_env_file(req: &DeployReq) -> String {
    let mut lines = vec![format!("AEON_HF_LINK={}", req.hf_link)];
    if !req.hf_token.is_empty() {
        lines.push(format!("HF_TOKEN={}", clean_val(&req.hf_token)));
    }
    let mut have_port = false;
    let mut have_maxlen = false;
    for (k, v) in &req.env {
        if !env_key_ok(k) || matches!(k.as_str(), "AEON_HF_LINK" | "HF_TOKEN") {
            continue;
        }
        match k.as_str() {
            "AEON_DASH_PORT" => have_port = true,
            "AEON_MAX_MODEL_LEN" => have_maxlen = true,
            _ => {}
        }
        lines.push(format!("{}={}", k, clean_val(v)));
    }
    if !have_port {
        lines.push(format!("AEON_DASH_PORT={DEFAULT_DASH_PORT}"));
    }
    // The agentic Hermes harness needs a 64k context; the pod itself refuses
    // anything under 65536 — so default it, never leaving it unset.
    if !have_maxlen {
        lines.push("AEON_MAX_MODEL_LEN=65536".to_string());
    }
    lines.join("\n") + "\n"
}

/// The outer bootstrap: drop the env + worker to disk (base64, so no quoting or
/// heredoc hazards over SSH) and nohup the worker so it survives the session.
fn deploy_script(env_b64: &str, run_b64: &str) -> String {
    format!(
        r#"set -e
WORK="$HOME/aeon-bench-pod"; mkdir -p "$WORK"
printf '%s' '{env_b64}' > "$WORK/.aeon-bench.envb64"
printf '%s' '{run_b64}' | base64 -d > "$WORK/.aeon-bench-run.sh"
printf 'PHASE=queued\n' > "$WORK/.aeon-bench.status"
nohup bash "$WORK/.aeon-bench-run.sh" >/dev/null 2>&1 &
echo STARTED"#
    )
}

/// Run a script on the target: locally via `bash -lc`, or on a connected system
/// over the agent-connect SSH key.
fn run_on_target(target: &str, script: &str) -> Result<String, String> {
    if target == "local" {
        let out = Command::new("bash")
            .arg("-lc")
            .arg(script)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("local command failed")
                .to_string())
        }
    } else {
        crate::agent_connect::run_remote(target, script)
    }
}

fn section<'a>(out: &'a str, marker: &str, next: &[&str]) -> &'a str {
    let Some(start) = out.find(marker) else { return "" };
    let after = &out[start + marker.len()..];
    let end = next
        .iter()
        .filter_map(|m| after.find(m))
        .min()
        .unwrap_or(after.len());
    after[..end].trim_matches(['\n', '\r'].as_ref())
}

/// POST /api/bench/deploy — kick a (backgrounded) pod deploy on the target.
pub async fn deploy(State(_s): State<AppState>, Json(req): Json<DeployReq>) -> Json<Value> {
    if !valid_hf_link(&req.hf_link) {
        return Json(json!({"ok": false, "err": "invalid model id — expected \"org/model\""}));
    }
    if req.target != "local" && crate::agent_connect::ssh_target(&req.target).is_none() {
        return Json(json!({"ok": false, "err": "unknown deploy target"}));
    }
    let dash_port: u16 = req
        .env
        .get("AEON_DASH_PORT")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_DASH_PORT);
    let script = deploy_script(&b64(build_env_file(&req).as_bytes()), &b64(RUN_SCRIPT.as_bytes()));
    let target = req.target.clone();
    let v = tokio::task::spawn_blocking(move || match run_on_target(&target, &script) {
        Ok(out) => json!({"ok": true, "target": target, "dash_port": dash_port, "out": out.trim()}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "deploy task failed"}));
    Json(v)
}

/// GET /api/bench/status?target= — phase, log tail, running-state, dashboard host+port.
pub async fn status(State(_s): State<AppState>, Query(q): Query<TargetQuery>) -> Json<Value> {
    let target = q.target.clone();
    let host = if target == "local" {
        None
    } else {
        crate::agent_connect::ssh_target(&target).map(|t| t.address)
    };
    let v = tokio::task::spawn_blocking(move || {
        let out = match run_on_target(&target, STATUS_SCRIPT) {
            Ok(o) => o,
            Err(e) => return json!({"ok": false, "err": e, "phase": "unknown"}),
        };
        let phase = section(&out, "@PHASE@", &["@PORT@", "@PS@", "@LOG@"])
            .strip_prefix("PHASE=")
            .unwrap_or("")
            .trim()
            .to_string();
        let port: u16 = section(&out, "@PORT@", &["@PS@", "@LOG@"])
            .trim()
            .parse()
            .unwrap_or(DEFAULT_DASH_PORT);
        let ps = section(&out, "@PS@", &["@LOG@"]).to_string();
        let log = section(&out, "@LOG@", &[]).to_string();
        let running = ps.to_lowercase().contains("up") || phase == "running";
        json!({
            "ok": true,
            "phase": if phase.is_empty() { "idle".into() } else { phase },
            "log": log,
            "running": running,
            "dash_port": port,
            "host": host,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/bench/stop — `docker compose down` the pod on the target.
pub async fn stop(State(_s): State<AppState>, Json(q): Json<TargetQuery>) -> Json<Value> {
    let target = q.target.clone();
    let v = tokio::task::spawn_blocking(move || match run_on_target(&target, STOP_SCRIPT) {
        Ok(_) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "stop task failed"}));
    Json(v)
}
