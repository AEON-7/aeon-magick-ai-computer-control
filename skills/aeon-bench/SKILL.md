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
pipeline for one model: **pull → verify weights → serve (vLLM) → benchmark**
(text · agentic ×3 harnesses · vision · audio · arena · perf) **→ ed25519-sign →
submit** an attested run to the aeon-bench.com leaderboard. It **serves the model
and benchmarks it co-located on one host** (hits `127.0.0.1:8000`, host-networked
— there's no external-endpoint split), and serving needs an **NVIDIA GPU**. So
the pod deploys onto a **connected GPU server**; this Orb is the console/gateway.

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

## 2. preview the recipe — **`bench_model_info`**

Before deploying, see what the pod will serve — quantization, params, context,
gated flag, and warnings — from the model's HuggingFace config:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/model-info?hf_link=Qwen/Qwen2.5-7B-Instruct-AWQ"
# → {quant:"AWQ", params_b:7.6, native_ctx:32768, effective_ctx:32768,
#    gated:false, is_gguf:false, warnings:["…below the 64k Hermes needs…"]}
```

Heed the warnings: **gated** → the deploy needs an `hf_token`; **context < 64k**
→ the agentic **Hermes** harness is capped (it needs 64k); **GGUF** → vLLM's GGUF
path is experimental. The pod auto-picks the quantization from the config — you
don't set it.

## 3. deploy — **`bench_deploy`**

```bash
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<system-id>","hf_link":"org/Model","hf_token":"<optional>"}' \
  "https://$AEON_HOST/api/bench/deploy"
```

Writes the pod `.env` (context defaults to **64k / 65536** for Hermes) and brings
the docker-compose stack up on the target over the Orb's SSH key — installing
Docker there on first run. Returns immediately; it builds + pulls in the
**background**.

## 4. watch it — **`bench_status`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/status?target=<system-id>"
# → {phase:"building"|"running"|"failed", log:"…", running:bool,
#    host:"<addr>", dash_port:8080}
```

Poll until `phase:"running"`. Building the images + pulling weights takes several
minutes on the first run. When running, the **dashboard** (launch runs, keys,
live progress) is a browser page at **`http://<host>:<dash_port>`** — surface
that URL to the user; it's reachable from a browser or via the Orb console.

## 5. keep it current — **`bench_updates`** / **`bench_update`**

The pod tracks the [Aeon-Bench-Pod](https://github.com/AEON-7/Aeon-Bench-Pod)
repo (it's a git checkout on the target). Check whether a newer build shipped,
then hot-update in place — the model config (`.env`) is preserved, only the pod
code changes:

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/updates?target=<system-id>"
# → {deployed:"<sha>", latest:"<sha>", update_available:true|false}

curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<system-id>"}' "https://$AEON_HOST/api/bench/update"
```

`bench_update` fetches the latest and rebuilds in the **background** (`git fetch`
→ `reset --hard` → `docker compose up -d --build`), driving the same phases —
poll **`bench_status`** through `updating → building → running`. In the console,
an **Update Pod** button appears on the Bench page whenever `update_available` is
true. (Updating with no pod deployed is a no-op error — deploy first.)

## 6. stop — **`bench_stop`**

`{"target":"<system-id>"}` → `docker compose down` on the target.

**Flow:** **`connected_systems`** (pick a GPU box) → **`bench_model_info`** (sanity-
check the recipe + warnings) → **`bench_deploy`** → poll **`bench_status`** to
`running` → hand off `http://<host>:8080` → **`bench_updates`**/**`bench_update`**
to stay current → **`bench_stop`** when done.
