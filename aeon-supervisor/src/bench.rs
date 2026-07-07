//! Aeon Bench — deploy the **Aeon Bench Pod** (open LLM benchmarking:
//! pull → verify → serve → benchmark → ed25519-sign → submit to the
//! aeon-bench.com leaderboard) onto a **GPU server linked in the Agent
//! Dashboard** (over the agent-connect SSH key), then reach the pod dashboard
//! (:8091) from the console or a browser.
//!
//! The pod now ships as a **prebuilt GHCR container** (`ghcr.io/aeon-7/aeon-pod`)
//! rather than a from-source docker-compose build: deploy = `docker pull` + a
//! single `docker run` (Docker-out-of-Docker — the pod mounts the host docker
//! socket to launch its own vLLM engine + harness sibling containers). It
//! co-locates the model server, so it needs an NVIDIA GPU (`--gpus all`) +
//! `--network host`. A model can be pre-loaded (`AEON_HF_LINK`) but is optional —
//! the dashboard lets you pick/scan models. The whole deploy is backgrounded
//! (pull → run), with phases surfaced through a status file the UI polls.
//!
//! Update = `docker pull` the newest image + recreate the container. Docker run
//! flags DON'T persist across a recreate, so the exact `docker run` command is
//! persisted to `.aeon-pod-run.sh` on the target at deploy time and re-executed
//! verbatim on update.

use crate::api::AppState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Command;

/// Dashboard port the pod listens on by default (override with AEON_PORT).
const DEFAULT_DASH_PORT: u16 = 8091;
const POD_IMAGE: &str = "ghcr.io/aeon-7/aeon-pod:latest";
const POD_NAME: &str = "aeon-pod";

/// The deploy worker, decoded + nohup-run on the target: ensure docker, pull the
/// prebuilt image, drop any old container, then exec the persisted `docker run`
/// command (`.aeon-pod-run.sh`). Writes `PHASE=…` to a status file the UI polls.
const RUN_SCRIPT: &str = r#"#!/bin/bash
WORK="$HOME/aeon-bench-pod"
S="$WORK/.aeon-bench.status"; L="$WORK/.aeon-bench.log"; : > "$L"
if ! command -v docker >/dev/null 2>&1; then
  echo PHASE=installing-docker > "$S"
  (apt-get update -y && apt-get install -y docker.io) >> "$L" 2>&1 \
    || { echo "Docker is not installed and could not be auto-installed — install Docker on the target, or pick a different GPU server." >> "$L"; echo PHASE=failed > "$S"; exit 1; }
  command -v systemctl >/dev/null 2>&1 && systemctl start docker >> "$L" 2>&1 || true
fi
mkdir -p "$HOME/aeon-models"
echo PHASE=pulling > "$S"
docker pull ghcr.io/aeon-7/aeon-pod:latest >> "$L" 2>&1 \
  || { echo "docker pull failed — the target needs network access to ghcr.io" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
docker rm -f aeon-pod >> "$L" 2>&1 || true
echo PHASE=starting > "$S"
if bash "$WORK/.aeon-pod-run.sh" >> "$L" 2>&1; then
  echo PHASE=running > "$S"
else
  echo "docker run failed — the pod serves the model co-located, so the target needs an NVIDIA GPU + nvidia-container-toolkit (for --gpus all). See the log above." >> "$L"
  echo PHASE=failed > "$S"
fi
"#;

/// Report the current phase + a log tail + whether the pod container is up + the
/// dashboard port (persisted at deploy). Markers delimit the sections for a
/// single round-trip.
const STATUS_SCRIPT: &str = r#"WORK="$HOME/aeon-bench-pod"
echo '@PHASE@'; cat "$WORK/.aeon-bench.status" 2>/dev/null
echo '@PORT@'; cat "$WORK/.aeon-bench.port" 2>/dev/null
echo '@PS@'; (docker ps --filter name=aeon-pod --format '{{.Names}} {{.Status}}' 2>/dev/null || true)
echo '@LOG@'; tail -n 40 "$WORK/.aeon-bench.log" 2>/dev/null
true
"#;

const STOP_SCRIPT: &str = r#"WORK="$HOME/aeon-bench-pod"
docker rm -f aeon-pod >> "$WORK/.aeon-bench.log" 2>&1 || true
echo PHASE=stopped > "$WORK/.aeon-bench.status"
echo STOPPED
"#;

/// Hot-update worker: pull the newest image + recreate the container from the
/// PERSISTED run command (docker run flags don't survive a recreate). Same status
/// file as deploy.
const UPDATE_SCRIPT: &str = r#"#!/bin/bash
WORK="$HOME/aeon-bench-pod"
S="$WORK/.aeon-bench.status"; L="$WORK/.aeon-bench.log"; : > "$L"
[ -f "$WORK/.aeon-pod-run.sh" ] || { echo "no pod is deployed here yet — deploy first, then update" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
echo PHASE=pulling > "$S"
docker pull ghcr.io/aeon-7/aeon-pod:latest >> "$L" 2>&1 \
  || { echo "docker pull failed — the target needs network access to ghcr.io" >> "$L"; echo PHASE=failed > "$S"; exit 1; }
docker rm -f aeon-pod >> "$L" 2>&1 || true
echo PHASE=starting > "$S"
if bash "$WORK/.aeon-pod-run.sh" >> "$L" 2>&1; then
  echo PHASE=running > "$S"
else
  echo "docker run failed after the update — see the log above" >> "$L"
  echo PHASE=failed > "$S"
fi
"#;

/// The pulled pod image's digest on the target (empty if never pulled).
const DEPLOYED_DIGEST_SCRIPT: &str = r#"docker inspect --format '{{index .RepoDigests 0}}' ghcr.io/aeon-7/aeon-pod:latest 2>/dev/null | sed 's/.*@//'; true"#;

#[derive(Deserialize)]
pub struct DeployReq {
    /// "local" (this Orb) or a connected-system id from the Agent Dashboard.
    pub target: String,
    /// OPTIONAL HuggingFace model id, e.g. "org/Model" — pre-loads AEON_HF_LINK.
    /// Empty is fine: the dashboard lets you pick/scan models after deploy.
    #[serde(default)]
    pub hf_link: String,
    #[serde(default)]
    pub hf_token: String,
    /// Extra `-e` env overrides for the pod (AEON_PORT, AEON_SYSTEM,
    /// AEON_PAUSE_CONTAINERS, …). Keys are validated; values are shell-quoted.
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

/// Single-quote a value for a POSIX shell command line (escaping embedded quotes),
/// so an env value can't break out into shell — every user-supplied `-e` value is
/// wrapped with this.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The exact `docker run` command for the GHCR pod, persisted to the target and
/// re-executed verbatim on update (flags don't survive a container recreate).
/// `$HOME` is left for the TARGET shell to expand; user values are shell-quoted.
/// Dedicated keys (models dir / port / token / hf link) can't be overridden or
/// duplicated by the free-form env map.
fn docker_run_command(req: &DeployReq, port: u16) -> String {
    let mut parts: Vec<String> = vec![
        "docker run -d".into(),
        format!("--name {POD_NAME}"),
        // Host networking (dashboard on the host port) + GPU passthrough — the
        // pod serves the model co-located.
        "--network host".into(),
        "--gpus all".into(),
        // Docker-out-of-Docker: the pod launches its engine + harness containers.
        "-v /var/run/docker.sock:/var/run/docker.sock".into(),
        // Persistent ed25519 device key + run history.
        "-v aeon-pod-state:/root/.aeon".into(),
        // Validated model weights, shared with sibling containers.
        "-v \"$HOME/aeon-models:/models\"".into(),
        "-e AEON_MODELS_HOST_DIR=\"$HOME/aeon-models\"".into(),
        format!("-e AEON_PORT={port}"),
    ];
    if !req.hf_token.trim().is_empty() {
        parts.push(format!("-e HF_TOKEN={}", shq(&clean_val(&req.hf_token))));
    }
    // Optional pre-loaded model for the headless pipeline.
    if !req.hf_link.trim().is_empty() {
        parts.push(format!("-e AEON_HF_LINK={}", shq(req.hf_link.trim())));
    }
    // Free-form AEON_* overrides (system label, pause-containers, …). Dedicated
    // keys are excluded so they can't be set twice.
    for (k, v) in &req.env {
        if !env_key_ok(k)
            || matches!(k.as_str(), "HF_TOKEN" | "AEON_HF_LINK" | "AEON_MODELS_HOST_DIR" | "AEON_PORT")
        {
            continue;
        }
        parts.push(format!("-e {}={}", k, shq(&clean_val(v))));
    }
    parts.push(POD_IMAGE.into());
    parts.join(" ")
}

/// The outer bootstrap: persist the `docker run` command (re-used on update) +
/// the dashboard port + the deploy worker to disk (base64, so no quoting/heredoc
/// hazards over SSH), then nohup the worker so it survives the session.
fn deploy_script(run_cmd_b64: &str, worker_b64: &str, port: u16) -> String {
    format!(
        r#"set -e
WORK="$HOME/aeon-bench-pod"; mkdir -p "$WORK"
printf '%s' '{run_cmd_b64}' | base64 -d > "$WORK/.aeon-pod-run.sh"
printf '%s' '{worker_b64}' | base64 -d > "$WORK/.aeon-bench-run.sh"
printf '{port}' > "$WORK/.aeon-bench.port"
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

/// Outer bootstrap for a hot-update: refuse (NO_POD) if nothing is deployed
/// (no persisted run command), else drop the update worker + nohup it.
fn update_bootstrap(worker_b64: &str) -> String {
    format!(
        r#"set -e
WORK="$HOME/aeon-bench-pod"
[ -f "$WORK/.aeon-pod-run.sh" ] || {{ echo "NO_POD"; exit 0; }}
printf '%s' '{worker_b64}' | base64 -d > "$WORK/.aeon-bench-update.sh"
printf 'PHASE=pulling\n' > "$WORK/.aeon-bench.status"
nohup bash "$WORK/.aeon-bench-update.sh" >/dev/null 2>&1 &
echo STARTED"#
    )
}

/// The latest published digest of the GHCR pod image. GHCR needs a (free,
/// anonymous) bearer token even for a public image; with it, the manifest
/// request returns the `Docker-Content-Digest` header without pulling the image.
fn ghcr_latest_digest() -> Option<String> {
    let tok: Value = ureq::get(
        "https://ghcr.io/token?service=ghcr.io&scope=repository:aeon-7/aeon-pod:pull",
    )
    .set("User-Agent", "aeon-magick-orb")
    .timeout(std::time::Duration::from_secs(12))
    .call()
    .ok()?
    .into_json()
    .ok()?;
    let token = tok.get("token").and_then(|v| v.as_str())?;
    let resp = ureq::get("https://ghcr.io/v2/aeon-7/aeon-pod/manifests/latest")
        .set("Authorization", &format!("Bearer {token}"))
        .set(
            "Accept",
            "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .set("User-Agent", "aeon-magick-orb")
        .timeout(std::time::Duration::from_secs(12))
        .call()
        .ok()?;
    resp.header("docker-content-digest")
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("sha256:"))
}

/// POST /api/bench/deploy — kick a (backgrounded) pod deploy on the target: pull
/// the GHCR image + `docker run` it. `hf_link` is optional (pre-load a model).
pub async fn deploy(State(_s): State<AppState>, Json(req): Json<DeployReq>) -> Json<Value> {
    if !req.hf_link.trim().is_empty() && !valid_hf_link(req.hf_link.trim()) {
        return Json(json!({"ok": false, "err": "invalid model id — expected \"org/model\""}));
    }
    if req.target != "local" && crate::agent_connect::ssh_target(&req.target).is_none() {
        return Json(json!({"ok": false, "err": "unknown deploy target"}));
    }
    let dash_port: u16 = req
        .env
        .get("AEON_PORT")
        .and_then(|v| v.trim().parse().ok())
        .filter(|p| *p > 0)
        .unwrap_or(DEFAULT_DASH_PORT);
    let run_cmd = docker_run_command(&req, dash_port);
    let script = deploy_script(&b64(run_cmd.as_bytes()), &b64(RUN_SCRIPT.as_bytes()), dash_port);
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

#[derive(Deserialize)]
pub struct ModelInfoQuery {
    pub hf_link: String,
}

/// Map a HuggingFace `quantization_config.quant_method` to the label we show
/// (and, if ever forced, what vLLM accepts). We only DISPLAY this — the pod's
/// derive_recipe() picks the actual (often marlin-accelerated) kernel from the
/// same config, so we don't override AEON_QUANT and risk a slower path in a
/// benchmark that measures throughput.
fn norm_quant(m: &str) -> String {
    match m.to_lowercase().replace('-', "_").as_str() {
        "awq" => "AWQ".into(),
        "gptq" => "GPTQ".into(),
        "fp8" => "FP8".into(),
        "bitsandbytes" => "bitsandbytes".into(),
        "compressed_tensors" => "compressed-tensors".into(),
        "marlin" => "Marlin".into(),
        "gguf" => "GGUF".into(),
        _ => m.to_string(),
    }
}

/// GET /api/bench/model-info?hf_link= — preview the serve recipe the pod will
/// derive for a model: quantization, context (native + rope-scaled), params,
/// dtype/arch, gated status, and any warnings (gated → token; context < 64k →
/// below what Hermes needs; GGUF → vLLM caveat). Uses the stored HF token.
pub async fn model_info(State(_s): State<AppState>, Query(q): Query<ModelInfoQuery>) -> Json<Value> {
    let id = q.hf_link.trim().trim_end_matches('/').to_string();
    if !valid_hf_link(&id) {
        return Json(json!({"ok": false, "err": "expected a model id like \"org/model\""}));
    }
    let v = tokio::task::spawn_blocking(move || {
        let api = match crate::ipfs::hf_get_json(&format!("https://huggingface.co/api/models/{id}")) {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "err": format!("HuggingFace: {e}")}),
        };
        // config.json is absent on GGUF-only / non-transformers repos — tolerate that.
        let cfg = crate::ipfs::hf_get_json(&format!("https://huggingface.co/{id}/resolve/main/config.json"))
            .unwrap_or(Value::Null);

        let mut warnings: Vec<String> = Vec::new();

        let gated = match api.get("gated") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s != "false",
            _ => false,
        };
        if gated {
            warnings.push("Gated model — add a HuggingFace token to pull the weights.".into());
        }

        let is_gguf = api
            .get("siblings")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().any(|f| f.get("rfilename").and_then(|r| r.as_str()).is_some_and(|n| n.ends_with(".gguf"))))
            .unwrap_or(false);
        if is_gguf {
            warnings.push("GGUF weights — vLLM's GGUF path is experimental; a safetensors build benchmarks more reliably.".into());
        }

        let quant = cfg
            .get("quantization_config")
            .and_then(|qc| qc.get("quant_method"))
            .and_then(|m| m.as_str())
            .map(norm_quant)
            .or_else(|| is_gguf.then(|| "GGUF".to_string()));

        let native_ctx = cfg.get("max_position_embeddings").and_then(|c| c.as_u64());
        let rope_factor = cfg
            .get("rope_scaling")
            .and_then(|r| r.get("factor").or_else(|| r.get("original_max_position_embeddings")))
            .and_then(|f| f.as_f64());
        let effective_ctx = match (native_ctx, rope_factor) {
            (Some(n), Some(f)) if f > 1.0 => Some((n as f64 * f) as u64),
            (Some(n), _) => Some(n),
            _ => None,
        };
        if let Some(ec) = effective_ctx {
            if ec < 65536 {
                warnings.push(format!(
                    "This model tops out near {}k context — below the 64k the agentic Hermes harness needs, so those scores may be capped.",
                    (ec + 512) / 1024
                ));
            }
        }

        let dtype = cfg.get("torch_dtype").and_then(|d| d.as_str()).map(String::from);
        let arch = cfg
            .get("architectures")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|a| a.as_str())
            .map(String::from);
        let params_b = api
            .get("safetensors")
            .and_then(|s| s.get("total"))
            .and_then(|t| t.as_u64())
            .map(|p| (p as f64 / 1e9 * 10.0).round() / 10.0);

        json!({
            "ok": true,
            "id": id,
            "quant": quant,               // display label, or null = full-precision
            "native_ctx": native_ctx,
            "effective_ctx": effective_ctx,
            "gated": gated,
            "is_gguf": is_gguf,
            "params_b": params_b,
            "dtype": dtype,
            "arch": arch,
            "warnings": warnings,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "model-info task failed"}));
    Json(v)
}

/// POST /api/bench/stop — `docker rm -f` the pod container on the target.
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

/// GET /api/bench/updates?target= — is a newer pod IMAGE published than the one
/// pulled on this target? Compares the pulled image's digest (docker inspect on
/// the target) against GHCR's latest. `update_available` is true only when an
/// image IS pulled and the digests differ, so the UI can show/hide the button.
pub async fn updates(State(_s): State<AppState>, Query(q): Query<TargetQuery>) -> Json<Value> {
    let target = q.target.clone();
    let v = tokio::task::spawn_blocking(move || {
        let deployed = run_on_target(&target, DEPLOYED_DIGEST_SCRIPT)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let deployed_ok = deployed.starts_with("sha256:");
        let latest = ghcr_latest_digest().unwrap_or_default();
        let update_available = deployed_ok && latest.starts_with("sha256:") && deployed != latest;
        // Show the 12 hex chars after "sha256:" — recognizable, like a short SHA.
        let short = |s: &str| s.trim_start_matches("sha256:").chars().take(12).collect::<String>();
        json!({
            "ok": true,
            "deployed": if deployed_ok { Some(short(&deployed)) } else { None },
            "latest": if latest.starts_with("sha256:") { Some(short(&latest)) } else { None },
            "update_available": update_available,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "updates task failed"}));
    Json(v)
}

/// POST /api/bench/update — hot-update the pod on the target: pull the newest
/// GHCR image and recreate the container from the persisted run command (same
/// model config). Backgrounded; the UI polls /api/bench/status through the phases.
pub async fn update(State(_s): State<AppState>, Json(q): Json<TargetQuery>) -> Json<Value> {
    let target = q.target.clone();
    let script = update_bootstrap(&b64(UPDATE_SCRIPT.as_bytes()));
    let v = tokio::task::spawn_blocking(move || match run_on_target(&target, &script) {
        Ok(out) if out.contains("NO_POD") => {
            json!({"ok": false, "err": "no pod is deployed on this target yet — deploy one first"})
        }
        Ok(out) => json!({"ok": true, "target": target, "out": out.trim()}),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "update task failed"}));
    Json(v)
}
