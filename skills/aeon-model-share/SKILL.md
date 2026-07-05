---
name: aeon-model-share
description: >
  Find, import, and move AI models across the Intergalactic Model Share — a
  decentralized IPFS network every Orb joins at boot. List models shared by Orbs
  across the network, import one straight from HuggingFace / Ollama / Civitai
  (verified), pull it down to this Orb's library, and push it onto a connected
  GPU server to serve. Load this when the task is about GETTING A MODEL onto a
  machine; for benchmarking a model use aeon-bench, for driving a screen use the
  core aeon-magick skill.
---
# aeon-model-share

Every Orb runs an IPFS node and gossips the models it hosts on a well-known
pubsub topic, so one call sees models contributed by Orbs **everywhere** — no
account, no central server. A model is an IPFS **directory** (weights +
`model-card.json` + README + image) resolved by one content-address (CID), so
you always get exactly the published bytes. Content is fetched in parallel from
every Orb that holds it (Bitswap), so popular models download faster.

> **Credentials.** `Authorization: Bearer $AEON_TOKEN` against `https://$AEON_HOST`
> (self-signed → `curl -k`). `export AEON_TOKEN=…` or source
> `~/.openclaw/aeon-magick.env`. A `401` means the token is unset/wrong — fix
> that, don't switch tools. Every capability here is a **first-class MCP tool**
> too (names in **bold**); the REST calls below are the same handlers.

## see what's out there — `model_list`

```bash
curl -sk -H "Authorization: Bearer $AEON_TOKEN" "https://$AEON_HOST/api/ipfs/models"
```

`catalog` = shareable from this Orb, `network` = discovered over gossip,
`library` = already pulled here (ready to push). Each entry has `cid`, `name`,
`size_bytes`, and a `card` (kind, base_model, params, quant, tags, license). For
the richer network view with per-model host count + community stars, use
`GET /api/ipfs/models/registry`.

## bring a model in — **`model_import`**

Pull a model from an external registry into the network — it's verified, wrapped
in an IPFS directory, pinned, and announced. Runs in the **background**; poll
`model_list` until it lands under `library`/`catalog`.

```bash
# source = huggingface | ollama | civitai
curl -sk -X POST -H "Authorization: Bearer $AEON_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"url":"Qwen/Qwen2.5-0.5B-Instruct"}' \
  "https://$AEON_HOST/api/ipfs/models/import-hf"
```

- **HuggingFace** (`import-hf`, body `{"url":"org/model"}`) — verifies each LFS
  weight's SHA-256 against the hash HuggingFace publishes.
- **Ollama** (`import-ollama`, body `{"reference":"llama3.2:3b"}`) — pulls GGUF
  from the Ollama registry, verified against the layer digest.
- **Civitai** (`import-civitai`, body `{"reference":"<url or id>"}`) — checkpoints
  / LoRAs / VAEs; prefers the SafeTensor file, verifies SHA-256, pulls gallery
  images. Gated + mature ("red") content needs a Civitai token (set under
  Model Share → tokens; never in this skill).

The **`model_import`** MCP tool wraps all three: `{source, reference}`.

## pull it here, push it to serve

- **`model_pull`** — materialize a shared model into this Orb's library (fetching
  over IPFS if needed) so it can be pushed. Identify by `cid` and/or `name`.
  `POST /api/ipfs/models/fetch` with the model entry.
- **`model_push`** → **`model_push_status`** — rsync a library model onto a
  **connected system** (a DGX / gateway; see **`connected_systems`**) over the
  established SSH key, into `aeon-models/<slug>` or a `dest` you pass. Runs in the
  background — poll status for phase + percent.

**Flow:** `model_list` / registry → (missing? **`model_import`**) → **`model_pull`**
→ **`model_push`** to a GPU box → poll **`model_push_status`** → serve it there
(or benchmark it — see aeon-bench).

## download safety

Peer downloads go through a **quarantine sandbox** automatically: disk check →
fetch to quarantine → magic-byte weight validation (rejects executables, pickle/
zip weights, fake `.gguf`/`.safetensors`) → ClamAV scan → only a clean pass is
pinned. You don't invoke it; it's why a fetch shows phases and why a bad model
never reaches the library.

## human-only surface (not MCP)

Starring a model, Model Karma (served-vs-downloaded), the mature-content 18+
opt-in, and per-source auth tokens are console/REST features for the operator —
there's no agent tool for them.
