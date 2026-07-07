---
name: aeon-bench
description: >
  Benchmark an open LLM on real hardware and submit an attested run to the AEON
  Bench leaderboard. Deploy the Aeon Bench Pod (pull → verify → serve →
  benchmark → sign → submit) onto a connected GPU server, watch it through its
  phases, and open its dashboard. Load this when the task is to EVALUATE /
  benchmark / leaderboard a model. The pod needs an NVIDIA GPU, so it deploys to
  a connected system, not the Orb itself.
---
# aeon-bench

The [Aeon Bench Pod](https://github.com/AEON-7/Aeon-Bench-Pod) runs the whole
pipeline: **pull → verify weights → serve (vLLM) → benchmark** (text · agentic ×3
harnesses · vision · audio · arena · perf) **→ ed25519-sign → submit** an attested
run to the aeon-bench.com leaderboard. It now ships as a **prebuilt GHCR
container** (`ghcr.io/aeon-7/aeon-pod`): deploy = `docker pull` + a single
`docker run` (Docker-out-of-Docker — the pod mounts the host docker socket to
launch its own engine + harness containers). It **serves the model co-located**
(`--gpus all --network host`), so serving needs an **NVIDIA GPU +
nvidia-container-toolkit**. The pod deploys onto a **connected GPU server**; this
Orb is the console/gateway, and its **dashboard (port 8091)** is where you
pick/scan/paste models and launch runs.

> **Credentials.** `Authorization: Bearer $AEON_TOKEN` against `https://$AEON_HOST`
> (self-signed → `curl -k`). Each capability is a **first-class MCP tool** (bold
> names); the REST below is the same handler.

## 1. pick a GPU host — **`connected_systems`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/agent/systems"
```

Take the `id` of a system with a GPU (a DGX / gateway). That id is the `target`
for every call below. **No systems?** There's nowhere to run — an operator must
link one in the Agent Dashboard first.

## 2. (optional) preview a model — **`bench_model_info`**

If pre-loading a model, you can preview what the pod will serve — quantization,
params, context, gated flag — from its HuggingFace config. This is informational
only; the pod derives the real recipe itself:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/model-info?hf_link=Qwen/Qwen2.5-7B-Instruct-AWQ"
# → {quant:"AWQ", params_b:7.6, native_ctx:32768, effective_ctx:32768,
#    gated:false, is_gguf:false, warnings:[…]}
```

**gated** → the deploy needs an `hf_token`; **GGUF** → vLLM's GGUF path is
experimental.

## 3. deploy — **`bench_deploy`**

```bash
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<system-id>"}' \
  "https://$AEON_HOST/api/bench/deploy"
# with a pre-loaded model (optional):
#   -d '{"target":"<system-id>","hf_link":"org/Model","hf_token":"<optional>"}'
```

Pulls `ghcr.io/aeon-7/aeon-pod:latest` and `docker run`s it on the target over the
Orb's SSH key (installing Docker on first run). **`hf_link` is optional** — leave
it out and pick models in the dashboard. Extra `env` keys (`AEON_PORT`,
`AEON_SYSTEM`, `AEON_PAUSE_CONTAINERS`) map to `-e` flags. Returns immediately;
pulls + starts in the **background**.

## 4. watch it — **`bench_status`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/status?target=<system-id>"
# → {phase:"pulling"|"starting"|"running"|"failed", log:"…", running:bool,
#    host:"<addr>", dash_port:8091}
```

Poll until `phase:"running"`. Pulling the image takes a minute or two on the first
deploy. When running, the **dashboard** (pick models, launch runs, keys, live
progress) is a browser page at **`http://<host>:<dash_port>`** (default 8091) —
surface that URL to the user; it's reachable from a browser or via the Orb console.

## 5. keep it current — **`bench_updates`** / **`bench_update`**

Check whether a newer pod **image** is published (pulled digest vs GHCR latest),
then hot-update — pull the newest image and recreate the container with the same
config:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/updates?target=<system-id>"
# → {deployed:"<digest>", latest:"<digest>", update_available:true|false}

curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<system-id>"}' "https://$AEON_HOST/api/bench/update"
```

`bench_update` runs `docker pull` + recreate in the **background** (the exact
`docker run` command is persisted at deploy and re-used, since flags don't survive
a recreate) — poll **`bench_status`** through `pulling → starting → running`. In
the console, an **Update Pod** button appears whenever `update_available` is true.
(Updating with no pod deployed is a no-op error — deploy first.)

## 6. stop — **`bench_stop`**

`{"target":"<system-id>"}` → `docker rm -f aeon-pod` on the target.

**Flow:** **`connected_systems`** (pick a GPU box) → **`bench_deploy`** (optionally
pre-load a model) → poll **`bench_status`** to `running` → hand off
`http://<host>:8091` (pick models + launch runs there) →
**`bench_updates`**/**`bench_update`** to stay current → **`bench_stop`** when done.
