//! Aeon Bench — deploy the **Aeon Bench Pod** and drive its **verified path**
//! (including remote endpoint benches over SSH) from the Orb console.
//!
//! The pod ships as `ghcr.io/aeon-7/aeon-pod`. Deploy = `docker pull` + `docker run`
//! (Docker-out-of-Docker). Two attested shapes the Orb can drive:
//!
//! 1. **Co-located** — pod on a GPU host, pull/serve/bench there (classic).
//! 2. **Remote verified** — pod (any host) points at a live OpenAI-compatible
//!    serve on a *connected system* via `serve_url` + `remote_host` +
//!    `verify_endpoint` (and optional `deep_verify`). The pod's SSH key is
//!    authorized on the serving box so it can probe hardware, read the real
//!    docker recipe, and hash-verify running weights (`endpoint_verified`).
//!
//! See Aeon-Bench-Pod `docs/remote-endpoint-bench.md`.

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

/// Deploy worker: ensure docker, pull image, run persisted `.aeon-pod-run.sh`.
const RUN_SCRIPT: &str = r#"#!/bin/bash
WORK="$HOME/aeon-bench-pod"
S="$WORK/.aeon-bench.status"; L="$WORK/.aeon-bench.log"; : > "$L"
if ! command -v docker >/dev/null 2>&1; then
  echo PHASE=installing-docker > "$S"
  (apt-get update -y && apt-get install -y docker.io) >> "$L" 2>&1 \
    || { echo "Docker is not installed and could not be auto-installed — install Docker on the target, or pick a different host." >> "$L"; echo PHASE=failed > "$S"; exit 1; }
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
  echo "docker run failed — for co-located serve/bench the target needs an NVIDIA GPU + nvidia-container-toolkit (--gpus all). For remote-endpoint-only mode redeploy with gpu=false." >> "$L"
  echo PHASE=failed > "$S"
fi
"#;

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

const DEPLOYED_DIGEST_SCRIPT: &str = r#"docker inspect --format '{{index .RepoDigests 0}}' ghcr.io/aeon-7/aeon-pod:latest 2>/dev/null | sed 's/.*@//'; true"#;

#[derive(Deserialize)]
pub struct DeployReq {
    /// "local" (this Orb) or a connected-system id from the Agent Dashboard.
    pub target: String,
    /// OPTIONAL HuggingFace model id, e.g. "org/Model" — pre-loads AEON_HF_LINK.
    #[serde(default)]
    pub hf_link: String,
    #[serde(default)]
    pub hf_token: String,
    /// Extra `-e` env overrides (AEON_PORT, AEON_SYSTEM, AEON_PAUSE_CONTAINERS, …).
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// When true (default), pass `--gpus all` for co-located serve/bench.
    /// Set false for a GPU-less pod used only as a control plane for **remote
    /// verified** endpoint benches (`serve_url` + `remote_host`).
    #[serde(default = "default_true")]
    pub gpu: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct TargetQuery {
    pub target: String,
}

#[derive(Deserialize)]
pub struct SshKeyQuery {
    pub target: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Deserialize)]
pub struct AuthorizeReq {
    /// Pod host (local | system id).
    pub target: String,
    /// Connected system that will *serve* the model (receives the pod's pubkey).
    pub serve_system: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Deserialize)]
pub struct ScanQuery {
    /// Pod host (local | system id).
    pub target: String,
    /// Optional connected-system id OR raw `user@host` to scan over SSH.
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub port: Option<u16>,
}

#[derive(Deserialize)]
pub struct VerifiedRunReq {
    /// Pod host (local | system id) — where the dashboard/API runs.
    pub target: String,
    /// HF repo of the **exact artifact** being served (required for attested).
    pub hf_link: String,
    /// Live OpenAI-compatible base URL, e.g. `http://192.168.1.10:8000/v1`.
    #[serde(default)]
    pub serve_url: String,
    /// Connected-system id OR `user@host` of the machine serving `serve_url`.
    #[serde(default)]
    pub remote_host: String,
    /// Served model id when the endpoint has several.
    #[serde(default)]
    pub endpoint_model: String,
    /// Bind the live endpoint to hash-verified weights (default true when serve_url set).
    #[serde(default = "default_true")]
    pub verify_endpoint: bool,
    /// Force container weight sha256 (`endpoint_verified`) even if a GPU fingerprint is possible.
    #[serde(default)]
    pub deep_verify: bool,
    /// Suite preset: comprehensive | hard-bench | god-mode (default comprehensive).
    #[serde(default = "default_comprehensive")]
    pub preset: String,
    #[serde(default)]
    pub port: Option<u16>,
}

fn default_comprehensive() -> String {
    "comprehensive".into()
}

#[derive(Deserialize)]
pub struct JobsQuery {
    pub target: String,
    #[serde(default)]
    pub port: Option<u16>,
}

fn b64(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(d)
}

fn valid_hf_link(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'))
}

fn env_key_ok(k: &str) -> bool {
    !k.is_empty() && k.len() <= 64 && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn clean_val(v: &str) -> String {
    v.chars().filter(|c| *c != '\n' && *c != '\r').take(500).collect()
}

fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve a connected-system id (or already-formed `user@host`) to an SSH destination.
fn remote_ssh_dest(id_or_host: &str) -> Result<String, String> {
    let s = id_or_host.trim();
    if s.is_empty() {
        return Err("remote host is empty".into());
    }
    if s.contains('@') {
        // user@host — allow only safe characters
        if s.len() <= 200
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | ':'))
        {
            return Ok(s.to_string());
        }
        return Err("invalid remote host".into());
    }
    let t = crate::agent_connect::ssh_target(s).ok_or_else(|| "unknown connected system".to_string())?;
    Ok(format!("{}@{}", t.ssh_user, t.address))
}

/// Host + port where the pod dashboard listens for a deploy target.
fn pod_http_base(target: &str, port: u16) -> Result<String, String> {
    if target == "local" {
        return Ok(format!("http://127.0.0.1:{port}"));
    }
    let t = crate::agent_connect::ssh_target(target).ok_or_else(|| "unknown deploy target".to_string())?;
    Ok(format!("http://{}:{port}", t.address))
}

fn pod_http_get(base: &str, path: &str) -> Result<Value, String> {
    let url = format!("{base}{path}");
    match ureq::get(&url)
        .set("User-Agent", "aeon-magick-orb")
        .timeout(std::time::Duration::from_secs(45))
        .call()
    {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("pod response parse: {e}")),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(format!("pod HTTP {code}: {}", body.chars().take(300).collect::<String>()))
        }
        Err(ureq::Error::Transport(t)) => Err(format!(
            "cannot reach pod at {base} ({t}) — is it running? open the dashboard or check bench_status"
        )),
    }
}

fn pod_http_post(base: &str, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{base}{path}");
    match ureq::post(&url)
        .set("User-Agent", "aeon-magick-orb")
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(60))
        .send_json(body.clone())
    {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("pod response parse: {e}")),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            Err(format!("pod HTTP {code}: {}", body.chars().take(300).collect::<String>()))
        }
        Err(ureq::Error::Transport(t)) => Err(format!(
            "cannot reach pod at {base} ({t}) — is it running? open the dashboard or check bench_status"
        )),
    }
}

/// Resolve dashboard port: query param → status file on target → default.
fn resolve_dash_port(target: &str, override_port: Option<u16>) -> u16 {
    if let Some(p) = override_port.filter(|p| *p > 0) {
        return p;
    }
    run_on_target(
        target,
        r#"cat "$HOME/aeon-bench-pod/.aeon-bench.port" 2>/dev/null || true"#,
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .filter(|p: &u16| *p > 0)
    .unwrap_or(DEFAULT_DASH_PORT)
}

/// Exact `docker run` for the GHCR pod (persisted + re-used on update).
fn docker_run_command(req: &DeployReq, port: u16) -> String {
    let mut parts: Vec<String> = vec![
        "docker run -d".into(),
        format!("--name {POD_NAME}"),
        // Host networking so dashboard binds on the host port.
        "--network host".into(),
    ];
    if req.gpu {
        // Co-located serve needs GPU passthrough.
        parts.push("--gpus all".into());
    }
    parts.extend([
        // Docker-out-of-Docker: engine + harness containers.
        "-v /var/run/docker.sock:/var/run/docker.sock".into(),
        // Persistent ed25519 device key + run history.
        "-v aeon-pod-state:/root/.aeon".into(),
        // Validated model weights, shared with sibling containers.
        "-v \"$HOME/aeon-models:/models\"".into(),
        "-e AEON_MODELS_HOST_DIR=\"$HOME/aeon-models\"".into(),
        // Scan host model homes (HF cache, LM Studio, ~/models) without copy.
        "-v \"$HOME:/host-home:ro\"".into(),
        "-e AEON_HOST_HOME_DIR=\"$HOME\"".into(),
        format!("-e AEON_PORT={port}"),
    ]);
    if !req.hf_token.trim().is_empty() {
        parts.push(format!("-e HF_TOKEN={}", shq(&clean_val(&req.hf_token))));
    }
    if !req.hf_link.trim().is_empty() {
        parts.push(format!("-e AEON_HF_LINK={}", shq(req.hf_link.trim())));
    }
    for (k, v) in &req.env {
        if !env_key_ok(k)
            || matches!(
                k.as_str(),
                "HF_TOKEN" | "AEON_HF_LINK" | "AEON_MODELS_HOST_DIR" | "AEON_PORT" | "AEON_HOST_HOME_DIR"
            )
        {
            continue;
        }
        parts.push(format!("-e {}={}", k, shq(&clean_val(v))));
    }
    parts.push(POD_IMAGE.into());
    parts.join(" ")
}

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
    let Some(start) = out.find(marker) else {
        return "";
    };
    let after = &out[start + marker.len()..];
    let end = next
        .iter()
        .filter_map(|m| after.find(m))
        .min()
        .unwrap_or(after.len());
    after[..end].trim_matches(['\n', '\r'].as_ref())
}

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

/// POST /api/bench/deploy
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
    let gpu = req.gpu;
    let v = tokio::task::spawn_blocking(move || match run_on_target(&target, &script) {
        Ok(out) => json!({
            "ok": true,
            "target": target,
            "dash_port": dash_port,
            "gpu": gpu,
            "out": out.trim(),
        }),
        Err(e) => json!({"ok": false, "err": e}),
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "deploy task failed"}));
    Json(v)
}

/// GET /api/bench/status?target=
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

/// GET /api/bench/model-info?hf_link=
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
            .map(|a| {
                a.iter().any(|f| {
                    f.get("rfilename")
                        .and_then(|r| r.as_str())
                        .is_some_and(|n| n.ends_with(".gguf"))
                })
            })
            .unwrap_or(false);
        if is_gguf {
            warnings.push(
                "GGUF weights — vLLM's GGUF path is experimental; a safetensors build benchmarks more reliably."
                    .into(),
            );
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
            "quant": quant,
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

/// POST /api/bench/stop
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

/// GET /api/bench/updates?target=
pub async fn updates(State(_s): State<AppState>, Query(q): Query<TargetQuery>) -> Json<Value> {
    let target = q.target.clone();
    let v = tokio::task::spawn_blocking(move || {
        let deployed = run_on_target(&target, DEPLOYED_DIGEST_SCRIPT)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let deployed_ok = deployed.starts_with("sha256:");
        let latest = ghcr_latest_digest().unwrap_or_default();
        let update_available = deployed_ok && latest.starts_with("sha256:") && deployed != latest;
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

/// POST /api/bench/update
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

// ── Remote verified path (pod APIs proxied through the Orb) ────────────────

/// GET /api/bench/ssh_key?target= — the pod's public SSH key (for authorizing on serve hosts).
pub async fn ssh_key(State(_s): State<AppState>, Query(q): Query<SshKeyQuery>) -> Json<Value> {
    let target = q.target.clone();
    let port = q.port;
    let v = tokio::task::spawn_blocking(move || {
        if target != "local" && crate::agent_connect::ssh_target(&target).is_none() {
            return json!({"ok": false, "err": "unknown deploy target"});
        }
        let port = resolve_dash_port(&target, port);
        let base = match pod_http_base(&target, port) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        match pod_http_get(&base, "/api/pod/ssh_key") {
            Ok(mut body) => {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("ok".into(), json!(true));
                    obj.insert("dash_port".into(), json!(port));
                }
                body
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "ssh_key task failed"}));
    Json(v)
}

/// POST /api/bench/authorize — install the pod's pubkey on a connected serve system.
pub async fn authorize(State(_s): State<AppState>, Json(req): Json<AuthorizeReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || {
        if req.target != "local" && crate::agent_connect::ssh_target(&req.target).is_none() {
            return json!({"ok": false, "err": "unknown pod target"});
        }
        if crate::agent_connect::ssh_target(&req.serve_system).is_none() {
            return json!({"ok": false, "err": "unknown serve system — link it in the Agent Dashboard first"});
        }
        let port = resolve_dash_port(&req.target, req.port);
        let base = match pod_http_base(&req.target, port) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let key_json = match pod_http_get(&base, "/api/pod/ssh_key") {
            Ok(v) => v,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let pubkey = key_json
            .get("pubkey")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .trim();
        if pubkey.is_empty() || !pubkey.starts_with("ssh-") {
            return json!({"ok": false, "err": "pod returned no public key — is the pod running?"});
        }
        // One line only; refuse anything with shell metacharacters.
        if pubkey.contains('\n')
            || pubkey.contains(';')
            || pubkey.contains('`')
            || pubkey.contains('$')
            || pubkey.len() > 800
        {
            return json!({"ok": false, "err": "invalid pod public key"});
        }
        let dest = match remote_ssh_dest(&req.serve_system) {
            Ok(d) => d,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        // Install via agent-connect on the serve system (not the pod).
        let script = format!(
            r#"set -e
mkdir -p "$HOME/.ssh" && chmod 700 "$HOME/.ssh"
touch "$HOME/.ssh/authorized_keys" && chmod 600 "$HOME/.ssh/authorized_keys"
KEY={key}
if grep -qxF "$KEY" "$HOME/.ssh/authorized_keys" 2>/dev/null; then
  echo ALREADY
else
  printf '%s\n' "$KEY" >> "$HOME/.ssh/authorized_keys"
  echo ADDED
fi
"#,
            key = shq(pubkey)
        );
        match crate::agent_connect::run_remote(&req.serve_system, &script) {
            Ok(out) => {
                let status = if out.contains("ALREADY") {
                    "already_authorized"
                } else {
                    "authorized"
                };
                json!({
                    "ok": true,
                    "status": status,
                    "serve_system": req.serve_system,
                    "remote_host": dest,
                    "pubkey": pubkey,
                })
            }
            Err(e) => json!({"ok": false, "err": format!("could not install key on serve system: {e}")}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "authorize task failed"}));
    Json(v)
}

/// GET /api/bench/scan_endpoints?target=&remote=
/// Proxies the pod's endpoint scan; `remote` may be a connected-system id.
pub async fn scan_endpoints(State(_s): State<AppState>, Query(q): Query<ScanQuery>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || {
        if q.target != "local" && crate::agent_connect::ssh_target(&q.target).is_none() {
            return json!({"ok": false, "err": "unknown pod target"});
        }
        let port = resolve_dash_port(&q.target, q.port);
        let base = match pod_http_base(&q.target, port) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        let path = if q.remote.trim().is_empty() {
            "/api/pod/scan_endpoints".to_string()
        } else {
            let dest = match remote_ssh_dest(q.remote.trim()) {
                Ok(d) => d,
                Err(e) => return json!({"ok": false, "err": e}),
            };
            format!(
                "/api/pod/scan_endpoints?remote={}",
                urlencoding_lite(&dest)
            )
        };
        match pod_http_get(&base, &path) {
            Ok(body) => {
                // Pod returns a list or object; wrap consistently.
                if body.is_array() {
                    json!({"ok": true, "endpoints": body, "dash_port": port})
                } else if let Some(obj) = body.as_object() {
                    let mut out = json!({"ok": true, "dash_port": port});
                    if let Some(m) = out.as_object_mut() {
                        for (k, v) in obj {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    out
                } else {
                    json!({"ok": true, "result": body, "dash_port": port})
                }
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "scan task failed"}));
    Json(v)
}

/// POST /api/bench/run_verified — verified-path launch via the pod API.
pub async fn run_verified(State(_s): State<AppState>, Json(req): Json<VerifiedRunReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || {
        if !valid_hf_link(req.hf_link.trim()) {
            return json!({"ok": false, "err": "hf_link required — exact HF quant repo, e.g. \"org/Model-AWQ\""});
        }
        if req.target != "local" && crate::agent_connect::ssh_target(&req.target).is_none() {
            return json!({"ok": false, "err": "unknown pod target"});
        }
        let preset = req.preset.trim();
        if !preset.is_empty() && !matches!(preset, "comprehensive" | "hard-bench" | "god-mode") {
            return json!({"ok": false, "err": "preset must be comprehensive, hard-bench, or god-mode"});
        }
        let port = resolve_dash_port(&req.target, req.port);
        let base = match pod_http_base(&req.target, port) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };

        let serve_url = req.serve_url.trim().to_string();
        let remote_host = if req.remote_host.trim().is_empty() {
            None
        } else {
            match remote_ssh_dest(req.remote_host.trim()) {
                Ok(d) => Some(d),
                Err(e) => return json!({"ok": false, "err": e}),
            }
        };

        // Cross-machine serve_url without remote_host misattributes hardware.
        if !serve_url.is_empty() && remote_host.is_none() {
            // Allow same-host endpoint (serve on the pod host) without remote_host.
        }

        let mut body = json!({
            "hf_link": req.hf_link.trim(),
            "preset": if preset.is_empty() { "comprehensive" } else { preset },
            "verify_endpoint": req.verify_endpoint,
            "deep_verify": req.deep_verify,
        });
        if !serve_url.is_empty() {
            body.as_object_mut()
                .unwrap()
                .insert("serve_url".into(), json!(serve_url));
        }
        if let Some(rh) = &remote_host {
            body.as_object_mut()
                .unwrap()
                .insert("remote_host".into(), json!(rh));
        }
        if !req.endpoint_model.trim().is_empty() {
            body.as_object_mut()
                .unwrap()
                .insert("endpoint_model".into(), json!(req.endpoint_model.trim()));
        }

        match pod_http_post(&base, "/api/pod/run/verified", &body) {
            Ok(resp) => {
                let job_id = resp.get("job_id").cloned().unwrap_or(Value::Null);
                json!({
                    "ok": true,
                    "job_id": job_id,
                    "target": req.target,
                    "dash_port": port,
                    "remote_host": remote_host,
                    "preset": body.get("preset"),
                    "pod": resp,
                })
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "run_verified task failed"}));
    Json(v)
}

/// GET /api/bench/jobs?target= — list jobs on the pod (progress of verified runs).
pub async fn jobs(State(_s): State<AppState>, Query(q): Query<JobsQuery>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || {
        if q.target != "local" && crate::agent_connect::ssh_target(&q.target).is_none() {
            return json!({"ok": false, "err": "unknown pod target"});
        }
        let port = resolve_dash_port(&q.target, q.port);
        let base = match pod_http_base(&q.target, port) {
            Ok(b) => b,
            Err(e) => return json!({"ok": false, "err": e}),
        };
        match pod_http_get(&base, "/api/pod/jobs") {
            Ok(body) => {
                if body.is_array() {
                    json!({"ok": true, "jobs": body, "dash_port": port})
                } else {
                    json!({"ok": true, "result": body, "dash_port": port})
                }
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "jobs task failed"}));
    Json(v)
}

/// Minimal URL-encoding for query values (user@host).
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
