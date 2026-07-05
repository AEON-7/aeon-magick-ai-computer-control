# MCP transport — talk to the device over Model Context Protocol

If your agent speaks MCP (Claude Desktop, anything on
`@modelcontextprotocol/sdk`), skip the curl recipes entirely and point your MCP
client at the device. Load this when you want the MCP surface instead of HTTP.

## Connect

```
url:       https://${AEON_HOST}/api/mcp
auth:      Basic admin:<password>
tlsVerify: false
```

The server uses **Streamable HTTP** transport: one `POST /api/mcp` endpoint
takes JSON-RPC 2.0 and replies inline (no long-lived SSE for these
request/response tools — every tool call is one HTTP round trip). It implements
`initialize`, `tools/list`, `tools/call`, `prompts/list`, `prompts/get`,
`resources/list`, `resources/read`.

Pick one transport per session — mixing curl and MCP works but creates
ambiguity in audit logs.

## Tools (agent-facing, grouped)

The server's full catalog is **67 tools**. You won't see all 67: the MCP
handler enforces **per-tool scope**, so `tools/list` returns — and `tools/call`
permits — only the tools your token's tier allows. A **read** token sees the
read-only tools (`state`, `snapshot`, `screen_text`, and the observability +
`*_state` / `*_status` queries); a **full** token additionally sees the
interactive surface (HID input + network config + `screen_find` /
`describe_screen`). The admin-only tools (see *Not exposed to agents* below) are
in the 67 but are never listed or callable for any agent token.

Each tool has the same shape as the curl endpoint in the matching reference;
the linked file has the detail.

### Vision + state — `references/vision.md`
| Tool | What it does |
|---|---|
| `state` | combined streamer + HID status |
| `snapshot` | one JPEG of the target's screen, as an MCP image block |
| `screen_text` | on-device OCR — all text on screen now, each with a box in 0..1 fractions (Hailo Orb) |
| `screen_find` | find on-screen text matching a query → ranked matches with a click-ready `center` {x,y}; the fast "find text → click it" path. No NPU GenAI slot needed |
| `describe_screen` | natural-language read of the screen via on-device Qwen2-VL (`prompt?`/`max_tokens?`); semantic understanding. ~3s warm/~13s cold; shares the NPU's single GenAI slot with the local LLM |

**Scope note:** `screen_text` is **Read**-tier (a read token sees it);
`screen_find` and `describe_screen` are **Full**-tier — a read-only token won't
see them in `tools/list`.

**REST-only vision controls (no MCP tool yet):** capture **source-switching**
(`GET/PUT /api/streamer/config {source}` — flip between `cam-link-usb` /
`hdmi-csi` screen capture and a `camera-csi` physical-thing camera) and
**webcam passthrough** (`GET/PUT /api/webcam` — present a source to the OTG
target as a UVC webcam) are **HTTP-only** — there is no `set_source`,
`view_source`, `set_webcam`, or `webcam_state` tool in the catalog. To change
what the Orb sees or expose a webcam over MCP, fall back to curl (same pattern
as AirVPN `generate` and Tor `rotate` below). See `references/vision.md` and
the `aeon-webcam` skill.

### Input — `references/input.md`
| Tool | What it does |
|---|---|
| `type_text` | type a string |
| `key_chord` | fire a key combo |
| `click` | left/right/middle click at the current cursor |
| `move_cursor` | relative cursor move (int8 deltas) |
| `click_at` | click at an absolute point — `x`,`y` fractions 0..1; needs `generic-absolute` |
| `move_pointer` | move the absolute pointer without clicking (hover) |
| `drag` | press → move → release between two absolute points |
| `scroll` | wheel scroll |
| `set_persona` | hot-swap HID identity |
| `release_all` | panic-release everything |

### Macros — `references/macros.md`
| Tool | What it does |
|---|---|
| `list_macros` | enumerate stored macros |
| `run_macro` | execute a stored macro by name (with optional params) |

### Recording — `references/recording.md`
| Tool | What it does |
|---|---|
| `record_start` | start an MP4 recording (optional `duration_s`; default 30 s, 0 = open-ended capped at 3 h) |
| `record_stop` | stop + finalize the in-progress recording |
| `recording_state` | active recording (id, elapsed) + finished list |
| `list_recordings` | finished recordings, newest first |

(Download + thumbnail are HTTP-only: `GET /api/streamer/recordings/:id` and
`…/:id/thumb`.)

### Clipboard — `references/clipboard.md`
| Tool | What it does |
|---|---|
| `get_clipboard` | read the shared text buffer |
| `set_clipboard` | write to it |
| `type_clipboard` | type the buffer onto the target via HID |

### Files + ISOs — `references/files.md`
| Tool | What it does |
|---|---|
| `list_files` | enumerate `/var/lib/aeon/files/` |
| `read_file` | read a staged file as base64 |
| `delete_file` | remove a staged file |
| `list_isos` | enumerate the ISO library |
| `set_active_iso` | mount or eject the USB-CDROM |

### Network / DNS / Tor / I2P / WiFi — `references/network.md`
| Tool | What it does |
|---|---|
| `network_status` | combined network-layer snapshot |
| `vpn_state` | clearnet VPN config |
| `set_vpn_provider` | switch provider |
| `vpn_providers_catalog` | provider catalog with privacy badges |
| `vpn_provider_state` | setup state of one provider |
| `vpn_provider_select` | pick a specific server |
| `vpn_provider_pick_fastest` | RTT-probe ranking (optional `no_eyes`) |
| `dnscrypt_state` | full DNSCrypt + relay catalog + current picks |
| `set_dnscrypt_criteria` | update the auto-picker criteria |
| `set_anonymized_relays` | update anonymized-relay config |
| `set_tor` | toggle Tor + mode + over_vpn |
| `set_i2p` | toggle I2P + over_vpn |
| `i2p_status` | i2pd daemon state + bound addresses |
| `wifi_state` / `wifi_scan` / `wifi_connect` | WiFi uplink management |

### Onions + IPFS — anonymous publishing
| Tool | What it does |
|---|---|
| `hidden_service_list` | list Tor v3 onion services |
| `hidden_service_create` | publish a new onion service |
| `hidden_service_remove` | tear one down |
| `ipfs_status` | IPFS node state |
| `ipfs_pin` | pin a CID |
| `ipfs_add` | add content to IPFS |

### Model Share — the decentralized AI-model network
| Tool | What it does |
|---|---|
| `connected_systems` | list the DGX / gateway systems this Orb manages over SSH (targets for push + bench) |
| `model_list` | catalog / network / library models (cid + name + size + card) |
| `model_import` | import from HuggingFace / Ollama / Civitai (SHA-256 / digest-verified) into the network |
| `model_pull` | materialize a shared model into this Orb's library |
| `model_push` / `model_push_status` | rsync a library model onto a connected system + poll progress |

### Aeon Bench — benchmark a model on a GPU server
| Tool | What it does |
|---|---|
| `bench_model_info` | preview a model's serve recipe — quant, params, context, gated, warnings |
| `bench_deploy` | deploy the benchmarking pod onto a connected GPU server (co-located serve + bench) |
| `bench_status` | poll deploy phase + log tail + the dashboard `http://<host>:8080` |
| `bench_stop` | tear the pod down |

### Read-only observability
| Tool | What it does |
|---|---|
| `pi_system_info` | Pi health — uptime, temp, load, memory |
| `security_metrics` | throughput + drop counters + top clients |
| `firewall_rules` | user-defined rules + hit counters |
| `dns_blacklist` | blacklist + recent query log |
| `dns_sources` | subscription sources |
| `audit_log` | mutation + login history, filterable |
| `blocked_log` | recent iptables drops with cause attribution |
| `target_info` | target power bindings (read-only) |

There is **no** `ups`/`battery` tool — UPS/power state (`GET /api/ups`) is
**REST-only**; curl it if you need to know whether an Orb is on battery / low.
Likewise **fleet** (`GET /api/fleet/roster` — see every Orb at once and pick
which to operate) has **no MCP tool**; curl the roster. See
the `aeon-power` skill and the `aeon-fleet` skill.

It also exposes stored macros + prompts as MCP **resources**
(`aeon://macros/<name>`, `aeon://prompts/<name>`) and prompts as MCP
**prompts** (`prompts/list` + `prompts/get`) — so a UI like Claude Desktop's
resource picker can browse them with no filesystem mount.

## REST-only capabilities (no MCP tool — use curl)

A handful of capabilities have **no tool in the 67-tool catalog** and must be
driven over HTTP even from an MCP session:

| Capability | Endpoint | Reference |
|---|---|---|
| Capture-source switching | `GET/PUT /api/streamer/config {source}` | `vision.md` |
| Webcam passthrough (UVC to target) | `GET/PUT /api/webcam` | `webcam.md` |
| UPS / battery state | `GET /api/ups` | `power.md` |
| Fleet roster (every Orb) | `GET /api/fleet/roster` | `fleet.md` |
| AirVPN config generator | `POST /api/network/vpn/providers/airvpn/generate` | `network.md` |
| Tor identity rotation (NEWNYM) | `POST /api/network/vpn/rotate` | `network.md` |

If MCP parity matters for any of these, the server needs new tools in
`mcp.rs`; until then, the curl recipe in the linked reference is the way.

## Not exposed to agents

Some capabilities are **human-admin-only** and require the web-admin session.
This is now **enforced** by the MCP handler's per-tool scope, not merely a
convention: no provisioned agent token — `read` or `full` — can see these in
`tools/list` or invoke them via `tools/call`.

The admin-only tools are:

- **Token management:** `issue_token`, `revoke_token`, `list_tokens`.
- **Target / Pi power:** `target_power_tap`, `target_power_hold`,
  `target_wake`, `target_reboot`, `pi_reboot`.

These sit in a **third** scope tier (Admin) that provisioned tokens never get —
distinct from the read/full split above. SSH key management is likewise
operator-only and has no agent tool. All of the above are done from the web UI
or an admin session. Don't look for them here, and don't tell an agent it can
call target power or `pi_reboot` — it can't.
