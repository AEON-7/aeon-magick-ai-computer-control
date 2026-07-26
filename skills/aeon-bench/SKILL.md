---
name: aeon-bench
description: >
  Benchmark an open LLM on real hardware and submit an attested run to the AEON
  Bench leaderboard. Deploy the Aeon Bench Pod, drive the verified path (including
  remote endpoint benches over SSH on any connected system), watch phases, and
  open the dashboard. Load when the task is to EVALUATE / benchmark / leaderboard
  a model. Co-located serve needs NVIDIA; remote verified path can use a GPU-less
  pod as control plane.
---
# aeon-bench

The [Aeon Bench Pod](https://github.com/AEON-7/Aeon-Bench-Pod) runs:

**pull → verify weights → serve (or point at a live serve) → benchmark → ed25519-sign → submit**

to [aeon-bench.com](https://aeon-bench.com). Image: `ghcr.io/aeon-7/aeon-pod`.

The Orb is the **console**: deploy the pod, authorize its SSH key on serve hosts,
scan endpoints, and launch **verified** runs without leaving the Pi UI / MCP.

> **Credentials.** `Authorization: Bearer $AEON_TOKEN` against `https://$AEON_HOST`
> (`curl -k`). Bold names are MCP tools.

## Two shapes

| Mode | When | Deploy |
|------|------|--------|
| **Co-located** | Pod pulls + serves + benches on one NVIDIA box | `bench_deploy` with `gpu:true` (default) on a DGX / GPU system |
| **Remote verified** | Model already serving on a connected system | Pod on GPU **or** Orb (`target:local`, `gpu:false`); then authorize + scan + `bench_run_verified` |

Remote verified path = `hf_link` + `serve_url` + `remote_host` + `verify_endpoint=true`
(see pod docs: `docs/remote-endpoint-bench.md`). Ranks as `endpoint_fingerprint` or
`endpoint_verified` (container-hash over SSH when no local GPU).

## Flow — remote verified (from the Pi)

### 1. Systems — **`connected_systems`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/agent/systems"
```

Need at least one **serve** host (GPU gateway). Optionally deploy the pod there too.

### 2. Deploy pod — **`bench_deploy`**

```bash
# Co-located on a GPU box:
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<gpu-system-id>","gpu":true}' \
  "https://$AEON_HOST/api/bench/deploy"

# Or control plane on this Orb (no --gpus all):
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"local","gpu":false}' \
  "https://$AEON_HOST/api/bench/deploy"
```

Poll **`bench_status`** until `phase:"running"`.

### 3. Authorize pod key on the serve host — **`bench_authorize`**

```bash
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{"target":"<pod-host-id-or-local>","serve_system":"<serve-system-id>"}' \
  "https://$AEON_HOST/api/bench/authorize"
```

Installs the pod’s ed25519 pubkey into that system’s `authorized_keys` over agent-connect.

### 4. Scan endpoints — **`bench_scan_endpoints`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/scan_endpoints?target=<pod-host>&remote=<serve-system-id>"
# → { endpoints: [{ url, hf_guess, models, … }] }
```

### 5. Launch verified run — **`bench_run_verified`**

```bash
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" -H 'content-type: application/json' \
  -d '{
    "target":"<pod-host>",
    "hf_link":"org/Exact-Quant-Repo",
    "serve_url":"http://<serve-ip>:8000/v1",
    "remote_host":"<serve-system-id>",
    "verify_endpoint":true,
    "preset":"comprehensive"
  }' \
  "https://$AEON_HOST/api/bench/run_verified"
# → { job_id: "…" }
```

Use the **exact quant** HF repo (not the base model). Prefer `hf_guess` from the scan.
Poll **`bench_jobs`** or open `http://<pod-host>:8091`.

### 6. Keep current — **`bench_updates`** / **`bench_update`**

Same as before: digest check + hot pull/recreate.

### 7. Stop — **`bench_stop`**

`{"target":"…"}` → `docker rm -f aeon-pod`.

## Optional preview — **`bench_model_info`**

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" \
  "https://$AEON_HOST/api/bench/model-info?hf_link=Qwen/Qwen2.5-7B-Instruct-AWQ"
```

## REST map

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/api/bench/deploy` | deploy pod (`gpu` bool) |
| GET | `/api/bench/status` | phase / log / dash_port |
| POST | `/api/bench/stop` | remove container |
| GET | `/api/bench/updates` | GHCR digest check |
| POST | `/api/bench/update` | pull + recreate |
| GET | `/api/bench/ssh_key` | pod public key |
| POST | `/api/bench/authorize` | install key on serve system |
| GET | `/api/bench/scan_endpoints` | live serves (+ `remote=`) |
| POST | `/api/bench/run_verified` | verified-path launch |
| GET | `/api/bench/jobs` | pod job list |
| GET | `/api/bench/model-info` | HF recipe preview |

**Console:** Orb → **Aeon Bench** page (works on Pi 4 and Pi 5 images).

**Never** use raw unverified endpoint-only benches when you can pass `hf_link` +
`verify_endpoint` — only the verified path ranks on the public board.
