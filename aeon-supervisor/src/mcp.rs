//! MCP (Model Context Protocol) server for the aeon-magick device.
//!
//! Transport: Streamable HTTP. Single POST endpoint at `/api/mcp` accepts
//! JSON-RPC 2.0 messages and replies with JSON-RPC responses inline
//! (Content-Type: application/json). No persistent SSE connection is opened
//! for the request/response tools we expose here — every tool call is one
//! HTTP round trip. This is the simplest spec-compliant subset and works
//! with Claude Desktop's "URL" MCP server config and with `@modelcontextprotocol/sdk`
//! `StreamableHTTPClientTransport`.
//!
//! What we expose:
//!   initialize       handshake
//!   tools/list       enumerate available tools
//!   tools/call       invoke a tool by name
//!   prompts/list     enumerate stored prompt templates
//!   prompts/get      fetch a stored prompt template
//!   resources/list   list macros and prompts as MCP resources
//!   resources/read   read a macro TOML or prompt text
//!
//! Tools wrap the existing REST/HID surface: state, snapshot, type_text,
//! key_chord, click, move_cursor, scroll, set_persona, release_all,
//! run_macro, list_macros.

use crate::api::AppState;
use crate::macros;
use crate::proxy;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "aeon-magick";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── JSON-RPC envelopes ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ── Tool catalog ────────────────────────────────────────────────────────

fn tools_catalog() -> Value {
    json!({
        "tools": [
            tool("state", "Get the current device state — streamer online, capture resolution, FPS, HID persona, keyboard/mouse online flags.", json!({"type":"object","properties":{}})),
            tool("snapshot", "Capture one JPEG frame of the target host's screen. Returns the frame as an MCP image content block (base64 JPEG, ~50–150ms latency).", json!({"type":"object","properties":{}})),
            tool("screen_text", "Read the on-device vision/OCR of the current capture feed (text + detected objects from the aeon-vision daemon — CPU OCR or the Hailo AI HAT+). Each detection has text/label, confidence, and a box in FRACTIONS 0..1 of the frame — feed a box centre straight into click(x,y). Returns {present:false} if vision is disabled or no frame is available.", json!({"type":"object","properties":{}})),
            tool("screen_find", "Find on-screen TEXT matching a query and get its clickable location — the fast path for 'click the X button/link/field'. Searches the live OCR (aeon-vision) for `query` and returns ranked matches, each with a `center` {x,y} in FRACTIONS 0..1 of the frame to feed straight into click(x,y). Returns ok:false if vision is off or nothing matches. Prefer this over reading all of screen_text when you know the label you want.", json!({"type":"object","properties":{"query":{"type":"string","description":"text to find, e.g. 'Save', 'Sign in', 'Settings'"}},"required":["query"]})),
            tool("describe_screen", "Get a natural-language description of what's on the target screen RIGHT NOW, from the on-device Qwen2-VL vision model (Hailo AI HAT+). Optional `prompt` asks a specific visual question (e.g. 'which dialog is open?', 'is the upload finished?'). Use for SEMANTIC understanding of the screen; use screen_text/screen_find for exact text + coordinates. ~3s warm / ~13s cold. NOTE: shares the NPU's single GenAI slot with the local LLM, so it's available when the local hailo-ollama LLM isn't resident (the normal online case); returns ok:false with guidance otherwise.", json!({"type":"object","properties":{"prompt":{"type":"string","description":"optional visual question; omit for a general description"},"max_tokens":{"type":"integer","description":"max generated tokens (default 96)"}}})),
            tool("record_start", "Record the target's screen to an MP4. Optional `duration_s` auto-stops after N seconds (default 30; 0 = open-ended / manual stop, capped at 1h). One recording at a time; requires H.264 stream mode. Returns the recording id — download later with list_recordings + GET /api/streamer/recordings/<id>.", json!({"type":"object","properties":{"duration_s":{"type":"integer","description":"auto-stop after N seconds; omit for the 30s default, 0 for manual-stop-only"}}})),
            tool("record_stop", "Stop the in-progress screen recording and finalize the MP4.", json!({"type":"object","properties":{}})),
            tool("recording_state", "Screen-recording status: the active recording (id, elapsed) if any, plus finished recordings with sizes.", json!({"type":"object","properties":{}})),
            tool("list_recordings", "List finished screen recordings (id, size, timestamp), newest first.", json!({"type":"object","properties":{}})),
            tool("type_text",
                 "Type a string on the target as the emulated USB keyboard. Each character is press→release, paced on-device.",
                 json!({"type":"object","required":["text"],
                        "properties":{"text":{"type":"string","description":"The text to type."}}})),
            tool("key_chord",
                 "Fire a key combo. Names: CTRL ALT SHIFT GUI/CMD/WIN ENTER TAB ESC SPACE BACKSPACE DELETE HOME END PAGEUP PAGEDOWN UP DOWN LEFT RIGHT F1..F12, plus single ASCII characters.",
                 json!({"type":"object","required":["keys"],
                        "properties":{
                            "keys":{"type":"array","items":{"type":"string"},"description":"Key names to press together."},
                            "hold_ms":{"type":"integer","default":30,"description":"How long to hold the chord (ms) before releasing."}}})),
            tool("click",
                 "Click a mouse button at the current cursor position. Press→release is atomic on the device.",
                 json!({"type":"object",
                        "properties":{
                            "button":{"type":"string","enum":["left","right","middle"],"default":"left"},
                            "count":{"type":"integer","default":1,"description":"1 for single, 2 for double, 3 for triple."}}})),
            tool("move_cursor",
                 "Move the mouse cursor by a relative delta. Boot-mouse semantics: deltas are clamped to int8 (−127..127). For larger moves, split client-side.",
                 json!({"type":"object","required":["dx","dy"],
                        "properties":{
                            "dx":{"type":"integer"},
                            "dy":{"type":"integer"}}})),
            tool("scroll",
                 "Scroll the mouse wheel. Positive dy = standard wheel up; macOS 'natural scrolling' inverts.",
                 json!({"type":"object","required":["dy"],
                        "properties":{"dy":{"type":"integer"}}})),
            tool("click_at",
                 "Click at an EXACT screen position — the precise, reliable way to hit a specific on-screen element. x and y are FRACTIONS of the screen: top-left = (0,0), bottom-right = (1,1), center = (0.5,0.5). From a `snapshot`, compute x = pixel_x / image_width and y = pixel_y / image_height. REQUIRES the `generic-absolute` persona — call set_persona(\"generic-absolute\") once first. Strongly preferred over move_cursor + click for clicking UI: move_cursor is relative and pointer acceleration makes precise targeting unreliable.",
                 json!({"type":"object","required":["x","y"],
                        "properties":{
                            "x":{"type":"number","description":"Horizontal, 0.0 (left) .. 1.0 (right)."},
                            "y":{"type":"number","description":"Vertical, 0.0 (top) .. 1.0 (bottom)."},
                            "button":{"type":"string","enum":["left","right","middle"],"default":"left"},
                            "double":{"type":"boolean","default":false,"description":"true = double-click."}}})),
            tool("move_pointer",
                 "Move the ABSOLUTE pointer to a screen position without clicking (hover). x,y are screen fractions 0.0..1.0 (see click_at). Requires the generic-absolute persona. Use for hover-triggered UI or to position before inspecting.",
                 json!({"type":"object","required":["x","y"],
                        "properties":{
                            "x":{"type":"number","description":"0.0 (left) .. 1.0 (right)."},
                            "y":{"type":"number","description":"0.0 (top) .. 1.0 (bottom)."}}})),
            tool("drag",
                 "Click-and-drag with the ABSOLUTE pointer: press at (x1,y1), move to (x2,y2) with the button held, then release. All coordinates are screen fractions 0.0..1.0 (see click_at). Requires the generic-absolute persona.",
                 json!({"type":"object","required":["x1","y1","x2","y2"],
                        "properties":{
                            "x1":{"type":"number"},"y1":{"type":"number"},
                            "x2":{"type":"number"},"y2":{"type":"number"},
                            "button":{"type":"string","enum":["left","right","middle"],"default":"left"}}})),
            tool("set_persona",
                 "Hot-swap the USB HID persona the target sees (USB re-enumerates, ~1 s blip). Use `generic-absolute` to enable click_at / move_pointer / drag — absolute positioning is the most reliable way for an agent to target on-screen elements (Linux/Windows). `generic-composite` is the relative-mouse default; logitech-mx / apple-magic[-stable] mimic those vendors (logitech-mx can wedge on Linux — prefer generic-* there).",
                 json!({"type":"object","required":["persona"],
                        "properties":{"persona":{"type":"string","enum":["generic-composite","generic-absolute","logitech-mx","apple-magic-stable","apple-magic"]}}})),
            tool("release_all",
                 "Panic button. Releases every modifier and mouse button and issues a HID reset. Rare with atomic-op design, but available.",
                 json!({"type":"object","properties":{}})),
            tool("list_macros",
                 "List names of stored macros available to run via run_macro.",
                 json!({"type":"object","properties":{}})),
            tool("run_macro",
                 "Run a stored macro by name. Optional params object substitutes {{var}} placeholders inside the macro's string fields.",
                 json!({"type":"object","required":["name"],
                        "properties":{
                            "name":{"type":"string"},
                            "params":{"type":"object","additionalProperties":{"type":"string"}}}})),
            // ── Network / security / config tools ──
            tool("network_status",
                 "Read-only snapshot of all network-layer state: USB ethernet config, DNSCrypt config + provider, VPN provider + bootstrap status, current public IP/country if a tunnel is up. Use this to confirm the device's outbound posture before triggering sensitive ops.",
                 json!({"type":"object","properties":{}})),
            tool("security_metrics",
                 "Read-only throughput + blocked-packet counters + top-clients list. Cheap to call (Pi-friendly). Use this to verify traffic is flowing through the expected interface (VPN vs WAN) or to spot a spike.",
                 json!({"type":"object","properties":{}})),
            tool("firewall_rules",
                 "List all user-defined firewall/NAT/port-forward rules with hit counters and any redundancy flags.",
                 json!({"type":"object","properties":{}})),
            tool("dns_blacklist",
                 "Return the current DNS blacklist (domains + regex patterns) plus the most recent query log (when logging is enabled).",
                 json!({"type":"object","properties":{}})),
            tool("dns_sources",
                 "List subscription blacklist sources (StevenBlack / OISD / Ultimate Hosts / etc.) and the curated presets the user can subscribe to. Includes per-source fetch status + entry count.",
                 json!({"type":"object","properties":{}})),
            // ── Hidden services (Tor v3 onions) ──
            tool("hidden_service_list",
                 "Read-only: list the Tor v3 hidden services this Orb hosts (nickname, .onion address, virtual + local port).",
                 json!({"type":"object","properties":{}})),
            tool("hidden_service_create",
                 "Host a Tor v3 hidden service: mints a fresh .onion that forwards <virt_port> (default 80) to 127.0.0.1:<local_port>, so a site/app you run locally becomes reachable at the returned .onion. Returns the address. Auto-enables the feature.",
                 json!({"type":"object","required":["local_port"],
                        "properties":{
                            "nickname":{"type":"string","description":"A friendly label for this onion."},
                            "local_port":{"type":"integer","description":"The local 127.0.0.1 port the app/site listens on."},
                            "virt_port":{"type":"integer","default":80,"description":"The port visitors use on the .onion (default 80)."}}})),
            tool("hidden_service_remove",
                 "Retire a hosted hidden service by its id (from hidden_service_list), dropping its .onion key + mapping.",
                 json!({"type":"object","required":["id"],
                        "properties":{"id":{"type":"string"}}})),
            // ── IPFS (decentralized hosting) ──
            tool("ipfs_status",
                 "Read-only: IPFS node + gateway state (installed, daemon, peers, repo size, storage cap, gateway port).",
                 json!({"type":"object","properties":{}})),
            tool("ipfs_pin",
                 "Pin an IPFS CID so this Orb hosts + keeps it available on the decentralized web.",
                 json!({"type":"object","required":["cid"],"properties":{"cid":{"type":"string"}}})),
            tool("ipfs_add",
                 "Add a local file or directory (by absolute path on the Orb) to IPFS and return its root CID — e.g. to publish a site/app you built here. Reachable at the local gateway and any public IPFS gateway.",
                 json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Absolute path on the Orb to a file or directory."}}})),
            // ── Model management: pull shared models down, push to connected systems ──
            tool("connected_systems",
                 "List the connected systems (DGX Sparks, agent gateways) this Orb manages over SSH — id, label, address, roles, status. Use the `id` as `system_id` for model_push / model_push_status.",
                 json!({"type":"object","properties":{}})),
            tool("model_list",
                 "List AI models available to pull/push: `catalog` = shareable from this Orb, `network` = discovered over IPFS gossip, `library` = already pulled down to this Orb (ready to push). Each has cid + name + size.",
                 json!({"type":"object","properties":{}})),
            tool("model_pull",
                 "Pull a shared model DOWN to this Orb's library — materializes its files locally (fetching from the IPFS network if needed) so it can be pushed to systems. Identify it by `cid`, by `name`, or both.",
                 json!({"type":"object","properties":{"cid":{"type":"string","description":"model directory CID"},"name":{"type":"string","description":"model display name (used as the library folder)"}}})),
            tool("model_push",
                 "Push a model to a connected system, materializing it on the Orb first if needed, then rsyncing over the established SSH key. Runs in the BACKGROUND — poll model_push_status. Identify the model by cid and/or name (see model_list) and the target by system_id (see connected_systems).",
                 json!({"type":"object","required":["system_id"],"properties":{
                            "system_id":{"type":"string","description":"target system id from connected_systems"},
                            "cid":{"type":"string","description":"model directory CID"},
                            "name":{"type":"string","description":"model display name"},
                            "dest":{"type":"string","description":"destination dir on the target; relative to the SSH user's home. Default aeon-models/<slug>."}}})),
            tool("model_push_status",
                 "Progress of model pushes to a connected system (keyed by model slug): phase, percent, and any error. Poll after model_push.",
                 json!({"type":"object","required":["system_id"],"properties":{"system_id":{"type":"string"}}})),
            tool("model_import",
                 "Import a model into the Model Share network from an external registry — HuggingFace, Ollama, or Civitai — verifying it (SHA-256 / layer digest) and publishing it as a shareable IPFS model. Runs in the BACKGROUND; poll model_list (it appears under `library`/`catalog`). `source` = huggingface | ollama | civitai; `reference` = the model id / URL / tag.",
                 json!({"type":"object","required":["source","reference"],"properties":{
                            "source":{"type":"string","enum":["huggingface","ollama","civitai"],"description":"the registry to import from"},
                            "reference":{"type":"string","description":"HF \"org/model\" (or full URL), an Ollama tag like \"llama3.2:3b\", or a Civitai model URL/id"}}})),
            tool("model_purge",
                 "Recover from a FAILED or stuck model download: clears its error, kills any hung fetch of the CID, unpins partial data, and garbage-collects the orphaned IPFS blocks to reclaim the space — so you can retry from scratch. Refuses a CID that's a fully-shared model (use the remove/unshare path for those). Returns freed bytes.",
                 json!({"type":"object","required":["cid"],"properties":{"cid":{"type":"string","description":"the model directory CID whose failed download to purge"}}})),
            // ── Aeon Bench: deploy pod + verified remote endpoint path ──
            tool("bench_model_info",
                 "Preview the serve recipe the bench pod will derive for a HuggingFace model before you deploy: quantization, params, native + rope-scaled context, dtype, architecture, gated flag, and warnings (gated needs a token; context < 64k limits the Hermes agentic harness; GGUF caveat). `hf_link` = \"org/model\".",
                 json!({"type":"object","required":["hf_link"],"properties":{"hf_link":{"type":"string","description":"HuggingFace model id, e.g. \"org/Model\""}}})),
            tool("bench_deploy",
                 "Deploy the Aeon Bench Pod (`ghcr.io/aeon-7/aeon-pod`) on `target` (`local` = this Orb, or a connected system id). Pulls the image and docker-runs it. Set `gpu:true` (default) for co-located serve/bench on NVIDIA; set `gpu:false` for a GPU-less control plane used only for verified remote endpoint benches. `hf_link` is OPTIONAL. Background — poll bench_status.",
                 json!({"type":"object","required":["target"],"properties":{
                            "target":{"type":"string","description":"\"local\" or connected system id from connected_systems"},
                            "hf_link":{"type":"string","description":"OPTIONAL HuggingFace model id to pre-load"},
                            "hf_token":{"type":"string","description":"optional HuggingFace token for gated weights"},
                            "gpu":{"type":"boolean","description":"pass --gpus all (default true); false for remote-endpoint-only pod"}}})),
            tool("bench_status",
                 "Progress of the bench pod on a target: phase (pulling → starting → running / failed), log tail, running-state, dashboard host+port (default 8091). Poll after bench_deploy.",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})),
            tool("bench_stop",
                 "Stop the bench pod on a target (docker rm -f aeon-pod).",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})),
            tool("bench_updates",
                 "Is a newer pod IMAGE published than the one pulled on a target? Compares digests against GHCR. Read-only.",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})),
            tool("bench_update",
                 "Hot-update the bench pod: pull newest image + recreate from persisted run config. Background — poll bench_status.",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})),
            tool("bench_ssh_key",
                 "Fetch the running pod's SSH PUBLIC key (GET /api/pod/ssh_key on the pod). Authorize it on a serve host so the pod can probe hardware / docker over SSH for remote verified benches.",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string","description":"pod host: local or system id"}}})),
            tool("bench_authorize",
                 "Install the pod's SSH public key on a connected serve system (via agent-connect). Required once per serve host before remote verified endpoint benches.",
                 json!({"type":"object","required":["target","serve_system"],"properties":{
                            "target":{"type":"string","description":"pod host"},
                            "serve_system":{"type":"string","description":"connected system id that is serving the model"}}})),
            tool("bench_scan_endpoints",
                 "Scan for live OpenAI-compatible endpoints via the pod (and optional SSH remote on a connected system). Returns urls + hf_guess when autodetected. Then pass url + hf_guess into bench_run_verified.",
                 json!({"type":"object","required":["target"],"properties":{
                            "target":{"type":"string","description":"pod host"},
                            "remote":{"type":"string","description":"optional connected system id or user@host to scan over SSH"}}})),
            tool("bench_run_verified",
                 "Launch a VALIDATED benchmark on the pod (POST /api/pod/run/verified). Prefer the remote verified path: hf_link (exact quant repo) + serve_url + remote_host (connected system id) + verify_endpoint=true. Defaults preset=comprehensive (ranks on the board). Returns job_id.",
                 json!({"type":"object","required":["target","hf_link"],"properties":{
                            "target":{"type":"string","description":"pod host"},
                            "hf_link":{"type":"string","description":"exact HF quant repo being served"},
                            "serve_url":{"type":"string","description":"live OpenAI base URL e.g. http://host:8000/v1"},
                            "remote_host":{"type":"string","description":"connected system id or user@host of the serving machine"},
                            "endpoint_model":{"type":"string","description":"served model id when the endpoint has several"},
                            "verify_endpoint":{"type":"boolean","description":"bind live serve to verified weights (default true)"},
                            "deep_verify":{"type":"boolean","description":"force container weight sha256 over SSH"},
                            "preset":{"type":"string","enum":["comprehensive","hard-bench","god-mode"]}}})),
            tool("bench_jobs",
                 "List jobs on the running pod (progress of verified runs). Read-only.",
                 json!({"type":"object","required":["target"],"properties":{"target":{"type":"string"}}})),
            tool("system_image_updates",
                 "Is a newer Aeon Orb IMAGE published for this Orb's hardware track? Compares the flashed image version (stamped on device) against the published manifest on GitHub. Returns {track, installed, latest, latest_name, update_available, notes, patreon_url}. Notify-only — flashing a new image is a manual Patreon download. Read-only.",
                 json!({"type":"object","properties":{}})),
            tool("os_update_check",
                 "Refresh the OS package lists and count how many system packages can be upgraded (how many are security). Returns {count, security, packages}. Read-only.",
                 json!({"type":"object","properties":{}})),
            tool("os_update_apply",
                 "Apply all pending OS package upgrades on the Pi (apt update && apt upgrade). Runs in the BACKGROUND — poll os_update_status through refreshing → upgrading → cleaning → done. Sets reboot_required when a kernel/firmware package changed.",
                 json!({"type":"object","properties":{}})),
            tool("os_update_status",
                 "Progress of an in-flight (or the last) OS update: phase, percent, done, ok, a log tail, and reboot_required. Poll after os_update_apply. Read-only.",
                 json!({"type":"object","properties":{}})),
            tool("os_auto_updates",
                 "Automatic OS security patching (unattended-upgrades). Call with no args to READ the current state {enabled, installed}; pass {\"enable\": true|false} to turn it on (installs unattended-upgrades, security-only, never auto-reboots) or off.",
                 json!({"type":"object","properties":{"enable":{"type":"boolean","description":"omit to read state; true/false to set it"}}})),
            // NOTE: SSH key management is intentionally NOT exposed over MCP.
            // Granting/listing SSH access to the device is a human-admin-only
            // action (web UI + admin session). Agents must never manage SSH.
            tool("audit_log",
                 "Recent audit-log entries (logins, logouts, token CRUD, network/firewall/dns/ssh/wifi/storage/persona mutations). Returns newest-first, capped at the supplied limit.",
                 json!({"type":"object","properties":{
                    "limit":{"type":"integer","default":200,"description":"Max entries to return (1..2000)"},
                    "actor":{"type":"string","description":"Filter by exact actor string."},
                    "action":{"type":"string","description":"Filter by action prefix (e.g. \"login\" matches login_ok + login_fail)."}
                 }})),

            // ── Target machine power (v60+) ──
            tool("target_info",
                 "Get current state of the USB-connected target machine's power-control bindings: known MAC, network interface, available modes. Read-only.",
                 json!({"type":"object","properties":{}})),
            tool("target_power_tap",
                 "Short press the target's HID power button (200ms). On most systems this triggers a graceful OS shutdown dialog. Use when you want OS-managed shutdown.",
                 json!({"type":"object","properties":{}})),
            tool("target_power_hold",
                 "Long press the target's HID power button (8s). Forces hardware-level shutdown bypassing the OS. Use when target is wedged.",
                 json!({"type":"object","properties":{}})),
            tool("target_wake",
                 "Send a Wake-on-LAN magic packet to the target. Requires target's MAC to be known (auto-discovered from ARP table after target DHCPs from usb0, or set manually).",
                 json!({"type":"object","properties":{}})),
            tool("target_reboot",
                 "Force-off then wake the target: 8-second HID power hold, 5-second wait, WoL packet. The canonical 'reboot the target now' for AI agents doing OS installs.",
                 json!({"type":"object","properties":{}})),

            // ── Shared clipboard (v48+) ──
            tool("get_clipboard",
                 "Read the shared text clipboard buffer (persists across reboots, 64KB max). Useful for AI agents passing data to subsequent runs or to a human.",
                 json!({"type":"object","properties":{}})),
            tool("set_clipboard",
                 "Write to the shared text clipboard. Replaces existing content. Limited to 64KB.",
                 json!({"type":"object","required":["text"],
                        "properties":{"text":{"type":"string"}}})),
            tool("type_clipboard",
                 "Type the current clipboard contents on the target via HID keyboard. Skips unmappable characters (emoji, smart quotes) and reports counts. The canonical 'paste long credential without the OS clipboard' for AI agents.",
                 json!({"type":"object","properties":{}})),

            // ── File transfer (v48+) ──
            tool("list_files",
                 "List files in /var/lib/aeon/files/. These can be served to the USB-connected target via the optional HTTP server. Returns name, size_bytes, modified_ms per file.",
                 json!({"type":"object","properties":{}})),
            tool("read_file",
                 "Read the contents of a specific file from /var/lib/aeon/files/. Returns base64 data + size. Use list_files first to see what's available.",
                 json!({"type":"object","required":["name"],
                        "properties":{"name":{"type":"string","description":"Filename (no path)"}}})),
            tool("delete_file",
                 "Remove a file from /var/lib/aeon/files/. Idempotent — succeeds if file already absent.",
                 json!({"type":"object","required":["name"],
                        "properties":{"name":{"type":"string"}}})),

            // ── DNSCrypt + relays (v50-v56) ──
            tool("dnscrypt_state",
                 "Get full DNSCrypt configuration: enabled, provider, custom stamp, anonymized relay state + criteria, server-selection mode (specific/auto), currently picked servers + relays. Includes resolver catalog and relay catalog.",
                 json!({"type":"object","properties":{}})),
            tool("set_dnscrypt_criteria",
                 "Update the criteria for the DNSCrypt server auto-picker. Mode 'auto' means lb_strategy=p2 routes per-query to lowest-latency match; mode 'specific' pins to vpn.provider.",
                 json!({"type":"object","properties":{
                    "enabled":{"type":"boolean"},
                    "mode":{"type":"string","enum":["auto","specific"]},
                    "no_logs":{"type":"boolean"},
                    "dnssec":{"type":"boolean"},
                    "no_filter":{"type":"boolean"},
                    "outside_five_eyes":{"type":"boolean"},
                    "outside_fourteen_eyes":{"type":"boolean"},
                    "min_trust_score":{"type":"integer","minimum":0,"maximum":5}
                 }})),
            tool("set_anonymized_relays",
                 "Update the anonymized DNSCrypt relay configuration. When enabled, queries ride through one of several relays so the resolver never sees client IP. Mode 'auto' picks by criteria; 'specific' uses provided relay names.",
                 json!({"type":"object","properties":{
                    "enabled":{"type":"boolean"},
                    "mode":{"type":"string","enum":["auto","specific"]},
                    "no_logs":{"type":"boolean"},
                    "outside_five_eyes":{"type":"boolean"},
                    "outside_fourteen_eyes":{"type":"boolean"},
                    "dnssec":{"type":"boolean"},
                    "specific_relays":{"type":"array","items":{"type":"string"}}
                 }})),

            // ── Tor (v58/v58.1) ──
            tool("set_tor",
                 "Toggle Tor + set routing mode + over-VPN nesting. mode='split_tunnel' only routes .onion via Tor; mode='transparent' routes ALL TCP via Tor (except i2pd). over_vpn requires a clearnet VPN active — Tor's entry-guard connections then exit via the VPN tunnel.",
                 json!({"type":"object","properties":{
                    "enabled":{"type":"boolean"},
                    "mode":{"type":"string","enum":["split_tunnel","transparent"]},
                    "over_vpn":{"type":"boolean"}
                 }})),

            // ── I2P (v57/v58.1) ──
            tool("set_i2p",
                 "Toggle I2P + set over-VPN nesting. I2P is always proxy-based (no transparent mode); browsers must point at the HTTP proxy explicitly. over_vpn marks i2pd's outbound TCP to route via VPN tunnel.",
                 json!({"type":"object","properties":{
                    "enabled":{"type":"boolean"},
                    "outproxy":{"type":"string","description":"Optional clearnet outproxy (e.g. exit.stormycloud.i2p)"},
                    "over_vpn":{"type":"boolean"}
                 }})),
            tool("i2p_status",
                 "Read-only i2pd daemon state: installed/running flags, bound HTTP/SOCKS/console addresses, current outproxy, browser-hint URLs.",
                 json!({"type":"object","properties":{}})),

            // ── Pi system (v54) ──
            tool("pi_system_info",
                 "Pi-side health: uptime, CPU temperature in °C, load average (1/5/15m), CPU count, memory total + available. Polls cheap, safe to call often.",
                 json!({"type":"object","properties":{}})),
            tool("pi_reboot",
                 "Reboot the RASPBERRY PI ITSELF (not the target). Disconnects web UI for ~30-60s. Use sparingly — agents almost never want this; they want target_reboot.",
                 json!({"type":"object","properties":{}})),

            // ── WiFi management ──
            tool("wifi_state",
                 "Current WiFi state: connected SSID, radio on/off, AP-fallback mode active.",
                 json!({"type":"object","properties":{}})),
            tool("wifi_scan",
                 "Scan for nearby WiFi networks. Returns array of SSIDs with signal strength + security types.",
                 json!({"type":"object","properties":{}})),
            tool("wifi_connect",
                 "Connect to a WiFi network. Persisted across reboots.",
                 json!({"type":"object","required":["ssid"],
                        "properties":{
                            "ssid":{"type":"string"},
                            "psk":{"type":"string","description":"Pre-shared key, omit for open networks"}
                        }})),

            // ── Storage (USB-CDROM ISO library) ──
            tool("list_isos",
                 "List ISO files in /var/lib/aeon/iso/. The active ISO (if any) is exposed as a USB-CDROM to the connected target — used by AI agents for OS installation workflows.",
                 json!({"type":"object","properties":{}})),
            tool("set_active_iso",
                 "Set which ISO is exposed as the USB-CDROM. Pass empty string to unmount. The target sees the change on its next USB enumeration (eject + insert).",
                 json!({"type":"object","required":["slug"],
                        "properties":{"slug":{"type":"string","description":"ISO filename, or empty to unmount"}}})),

            // ── VPN clearnet routing ──
            tool("vpn_state",
                 "Current VPN clearnet routing state: provider, enabled, kill-switch, lan_bypass, provider-specific config flags. Independent of Tor + I2P which live elsewhere.",
                 json!({"type":"object","properties":{}})),
            tool("set_vpn_provider",
                 "Switch the active clearnet VPN provider (none / tailscale / wireguard / openvpn / mullvad / ivpn / azirevpn / airvpn). For mullvad/ivpn/azirevpn/airvpn you must run the setup wizard via REST first. AirVPN additionally supports stealth modes (OpenVPN-over-SSL / over-SSH) chosen during its setup.",
                 json!({"type":"object","required":["provider"],
                        "properties":{
                            "provider":{"type":"string","enum":["none","tailscale","wireguard","openvpn","mullvad","ivpn","azirevpn","airvpn"]},
                            "enabled":{"type":"boolean"},
                            "kill_switch":{"type":"boolean"}
                        }})),

            // ── VPN provider wizards (v59) ──
            tool("vpn_providers_catalog",
                 "Static catalog of supported VPN providers with privacy + trust metadata: HQ country, Eyes tier, audit history, anonymous-signup flags, trust score.",
                 json!({"type":"object","properties":{}})),
            tool("vpn_provider_state",
                 "Get setup state of a specific provider — whether account is configured, cached server list with privacy badges, current selected server.",
                 json!({"type":"object","required":["provider"],
                        "properties":{"provider":{"type":"string","enum":["mullvad","ivpn","azirevpn","airvpn"]}}})),
            tool("vpn_provider_select",
                 "Pick a specific server for an already-configured provider. server_id is the hostname from vpn_provider_state's server list.",
                 json!({"type":"object","required":["provider","server_id"],
                        "properties":{
                            "provider":{"type":"string","enum":["mullvad","ivpn","azirevpn","airvpn"]},
                            "server_id":{"type":"string"},
                            "mode":{"type":"string","enum":["manual","auto"]}
                        }})),
            tool("vpn_provider_pick_fastest",
                 "ICMP-probe cached servers for the given provider, return ranking by RTT (each entry includes its eyes tier). Set no_eyes=true to rank ONLY servers in countries outside the 5/9/14-Eyes alliances. The supervisor doesn't auto-apply — agent inspects the result and calls vpn_provider_select on the winner.",
                 json!({"type":"object","required":["provider"],
                        "properties":{
                            "provider":{"type":"string","enum":["mullvad","ivpn","azirevpn","airvpn"]},
                            "no_eyes":{"type":"boolean","description":"Restrict to No-Eyes jurisdictions (outside 5/9/14-Eyes)"}}})),

            // ── API token management ──
            tool("list_tokens",
                 "List active API tokens (id + name + scope + created_at). Plaintext is NEVER echoed; only hashes are stored.",
                 json!({"type":"object","properties":{}})),
            tool("issue_token",
                 "Create a new API token. Returns the plaintext ONCE. Scope choices: admin (everything), full (everything except token CRUD + reboot), macros (run macros + reads), read (reads only).",
                 json!({"type":"object","required":["name","scope"],
                        "properties":{
                            "name":{"type":"string"},
                            "scope":{"type":"string","enum":["admin","full","macros","read"]}
                        }})),
            tool("revoke_token",
                 "Revoke a token by id. Subsequent requests using that token will 401.",
                 json!({"type":"object","required":["id"],
                        "properties":{"id":{"type":"string"}}})),

            // ── Security console ──
            tool("blocked_log",
                 "Recent kernel firewall drops/rejects parsed from the AEON-DROP iptables LOG prefix. Each entry has src, dst, proto, ports, cause_tag (which firewall rule blocked it).",
                 json!({"type":"object","properties":{}})),
        ]
    })
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

// ── Tool dispatch ───────────────────────────────────────────────────────

async fn dispatch_tool(state: &AppState, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "state" => {
            let v = proxy::fetch_supervisor_state(state).await;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "snapshot" => {
            let bytes = proxy::fetch_snapshot_bytes(state).await?;
            let b64 = B64.encode(&bytes);
            Ok(json!({
                "content": [{
                    "type": "image",
                    "data": b64,
                    "mimeType": "image/jpeg",
                }],
                "isError": false,
            }))
        }
        "screen_text" => {
            let v = tokio::fs::read_to_string("/run/aeon/vision.json")
                .await
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or_else(|| json!({ "present": false }));
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "screen_find" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or("screen_find needs a `query` string argument")?;
            let v = crate::vision::screen_find_value(query).await;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "describe_screen" => {
            let prompt = args.get("prompt").and_then(|v| v.as_str());
            let max_tokens = args.get("max_tokens").and_then(|v| v.as_u64());
            let v = crate::vision::describe_screen_value(prompt, max_tokens).await;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "record_start" => {
            let dur = args.get("duration_s").and_then(|v| v.as_u64());
            let body = serde_json::to_vec(&json!({ "duration_s": dur })).unwrap();
            let v = proxy::post_streamer_json(state, "/record/start", body).await?;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "record_stop" => {
            let v = proxy::post_streamer_json(state, "/record/stop", Vec::new()).await?;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "recording_state" => {
            let v = proxy::fetch_streamer_json(state, "/record/state").await?;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "list_recordings" => {
            let v = proxy::fetch_streamer_json(state, "/recordings").await?;
            Ok(text_result(&serde_json::to_string_pretty(&v).unwrap_or_default()))
        }
        "type_text" => {
            let text = args.get("text").and_then(|v| v.as_str())
                .ok_or("type_text needs a `text` string argument")?;
            proxy::post_hid(state, "/type", serde_json::to_vec(&json!({"text": text})).unwrap()).await?;
            Ok(text_result("ok"))
        }
        "key_chord" => {
            let keys = args.get("keys").and_then(|v| v.as_array())
                .ok_or("key_chord needs a `keys` array argument")?;
            let hold_ms = args.get("hold_ms").and_then(|v| v.as_u64()).unwrap_or(30) as u32;
            let body = serde_json::to_vec(&json!({ "keys": keys, "hold_ms": hold_ms })).unwrap();
            proxy::post_hid(state, "/key", body).await?;
            Ok(text_result("ok"))
        }
        "click" => {
            let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            let body = serde_json::to_vec(&json!({ "button": button, "count": count })).unwrap();
            proxy::post_hid(state, "/click", body).await?;
            Ok(text_result("ok"))
        }
        "move_cursor" => {
            let dx = args.get("dx").and_then(|v| v.as_i64()).ok_or("dx required")? as i32;
            let dy = args.get("dy").and_then(|v| v.as_i64()).ok_or("dy required")? as i32;
            let body = serde_json::to_vec(&json!({ "dx": dx, "dy": dy })).unwrap();
            proxy::post_hid(state, "/move", body).await?;
            Ok(text_result("ok"))
        }
        "scroll" => {
            let dy = args.get("dy").and_then(|v| v.as_i64()).ok_or("dy required")? as i32;
            let body = serde_json::to_vec(&json!({ "dy": dy })).unwrap();
            proxy::post_hid(state, "/scroll", body).await?;
            Ok(text_result("ok"))
        }
        "click_at" => {
            let x = args.get("x").and_then(|v| v.as_f64()).ok_or("click_at needs `x` (0..1)")?;
            let y = args.get("y").and_then(|v| v.as_f64()).ok_or("click_at needs `y` (0..1)")?;
            let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let mask: i64 = match button { "right" => 2, "middle" => 4, _ => 1 };
            let reps = if args.get("double").and_then(|v| v.as_bool()).unwrap_or(false) { 2 } else { 1 };
            for _ in 0..reps {
                proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x,"y":y,"buttons":mask})).unwrap()).await?;
                proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x,"y":y,"buttons":0})).unwrap()).await?;
            }
            Ok(text_result(&format!("clicked {button} at ({x:.3}, {y:.3})")))
        }
        "move_pointer" => {
            let x = args.get("x").and_then(|v| v.as_f64()).ok_or("move_pointer needs `x` (0..1)")?;
            let y = args.get("y").and_then(|v| v.as_f64()).ok_or("move_pointer needs `y` (0..1)")?;
            proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x,"y":y,"buttons":0})).unwrap()).await?;
            Ok(text_result(&format!("pointer at ({x:.3}, {y:.3})")))
        }
        "drag" => {
            let x1 = args.get("x1").and_then(|v| v.as_f64()).ok_or("drag needs x1 (0..1)")?;
            let y1 = args.get("y1").and_then(|v| v.as_f64()).ok_or("drag needs y1 (0..1)")?;
            let x2 = args.get("x2").and_then(|v| v.as_f64()).ok_or("drag needs x2 (0..1)")?;
            let y2 = args.get("y2").and_then(|v| v.as_f64()).ok_or("drag needs y2 (0..1)")?;
            let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left");
            let mask: i64 = match button { "right" => 2, "middle" => 4, _ => 1 };
            // press at start → move to end with button held → release.
            proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x1,"y":y1,"buttons":0})).unwrap()).await?;
            proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x1,"y":y1,"buttons":mask})).unwrap()).await?;
            proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x2,"y":y2,"buttons":mask})).unwrap()).await?;
            proxy::post_hid(state, "/move_abs", serde_json::to_vec(&json!({"x":x2,"y":y2,"buttons":0})).unwrap()).await?;
            Ok(text_result(&format!("dragged {button} ({x1:.3},{y1:.3}) -> ({x2:.3},{y2:.3})")))
        }
        "set_persona" => {
            let persona = args.get("persona").and_then(|v| v.as_str())
                .ok_or("persona required")?;
            let body = serde_json::to_vec(&json!({ "persona": persona })).unwrap();
            proxy::post_hid(state, "/persona", body).await?;
            Ok(text_result(&format!("persona set to {persona}")))
        }
        "release_all" => {
            proxy::post_hid(state, "/release_all", vec![]).await?;
            Ok(text_result("released"))
        }
        "list_macros" => {
            let names = state.stores.list_macros();
            Ok(text_result(&serde_json::to_string_pretty(&names).unwrap_or_default()))
        }
        "run_macro" => {
            let name = args.get("name").and_then(|v| v.as_str()).ok_or("name required")?;
            let params: HashMap<String, String> = match args.get("params") {
                Some(Value::Object(m)) => m
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect(),
                _ => HashMap::new(),
            };
            let Json(result) = macros::run(state, name, &params).await;
            Ok(text_result(&serde_json::to_string_pretty(&result).unwrap_or_default()))
        }
        "network_status" => {
            // Hit each network handler — they only read /etc/aeon/network.toml
            // + system tools, no shared state needed beyond AppState.
            // vpn_status (live tunnel state) intentionally NOT bundled here:
            // it shells out to aeon-vpn-status which is expensive; agents
            // should call it explicitly via REST when they need it.
            let usb = crate::network::get_state(axum::extract::State(state.clone())).await;
            // Status overview — omit the ~141 KB resolver catalog (agents
            // pick resolvers by criteria via set_dnscrypt_criteria, not by
            // browsing the raw list; dnscrypt_state returns it if needed).
            let dns = crate::network::get_dnscrypt(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::network::DnscryptQuery { catalog: false }),
            ).await;
            let vpn = crate::network::get_vpn(axum::extract::State(state.clone())).await;
            let combined = json!({
                "usb_ethernet": usb.0,
                "dnscrypt": dns.0,
                "vpn": vpn.0,
            });
            Ok(text_result(&serde_json::to_string_pretty(&combined).unwrap_or_default()))
        }
        "hidden_service_list" => {
            let v = crate::onions::status(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "hidden_service_create" => {
            let nickname = args.get("nickname").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let local_port = args.get("local_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            let virt_port = args.get("virt_port").and_then(|v| v.as_u64()).map(|n| n as u16);
            let v = crate::onions::create(
                axum::extract::State(state.clone()),
                axum::Json(crate::onions::CreateReq { nickname, local_port, virt_port }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "hidden_service_remove" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let v = crate::onions::remove(
                axum::extract::State(state.clone()),
                axum::Json(crate::onions::RemoveReq { id }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "ipfs_status" => {
            let v = crate::ipfs::status(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "ipfs_pin" => {
            let cid = args.get("cid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let v = crate::ipfs::pin(
                axum::extract::State(state.clone()),
                axum::Json(crate::ipfs::CidReq { cid }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "ipfs_add" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let v = crate::ipfs::add_path(
                axum::extract::State(state.clone()),
                axum::Json(crate::ipfs::AddReq { path }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "connected_systems" => {
            Ok(text_result(&serde_json::to_string_pretty(&crate::agent_connect::systems_value()).unwrap_or_default()))
        }
        "model_list" => {
            Ok(text_result(&serde_json::to_string_pretty(&crate::ipfs::model_list_value()).unwrap_or_default()))
        }
        "model_pull" => {
            let cid = args.get("cid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let v = crate::ipfs::pull_model(
                axum::extract::State(state.clone()),
                axum::Json(crate::ipfs::PullReq { cid, name }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "model_push" => {
            let system_id = args
                .get("system_id")
                .and_then(|v| v.as_str())
                .ok_or("model_push needs a `system_id` — call connected_systems to list them")?
                .to_string();
            let cid = args.get("cid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let dest = args.get("dest").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let v = crate::agent_connect::push_model(
                axum::extract::Path(system_id),
                axum::Json(crate::agent_connect::PushModelReq { cid, name, dest }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "model_push_status" => {
            let system_id = args
                .get("system_id")
                .and_then(|v| v.as_str())
                .ok_or("model_push_status needs a `system_id`")?
                .to_string();
            let v = crate::agent_connect::push_status(axum::extract::Path(system_id)).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "model_import" => {
            let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let reference = args
                .get("reference")
                .and_then(|v| v.as_str())
                .ok_or("model_import needs a `reference` (an org/model, Ollama tag, or Civitai URL)")?
                .to_string();
            let st = axum::extract::State(state.clone());
            let v = match source.as_str() {
                "huggingface" | "hf" => {
                    crate::ipfs::import_hf(st, axum::Json(crate::ipfs::ImportHfReq { url: reference })).await
                }
                "ollama" => {
                    crate::ipfs::import_ollama(st, axum::Json(crate::ipfs::ImportOllamaReq { reference })).await
                }
                "civitai" => {
                    crate::ipfs::import_civitai(st, axum::Json(crate::ipfs::ImportCivitaiReq { reference })).await
                }
                _ => axum::Json(json!({"ok": false, "err": "source must be huggingface | ollama | civitai"})),
            };
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "model_purge" => {
            let cid = args
                .get("cid")
                .and_then(|v| v.as_str())
                .ok_or("model_purge needs a `cid`")?
                .to_string();
            let v = crate::ipfs::purge_download(
                axum::extract::State(state.clone()),
                axum::Json(crate::ipfs::CidReq { cid }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_model_info" => {
            let hf_link = args
                .get("hf_link")
                .and_then(|v| v.as_str())
                .ok_or("bench_model_info needs `hf_link` (\"org/model\")")?
                .to_string();
            let v = crate::bench::model_info(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::ModelInfoQuery { hf_link }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_deploy" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_deploy needs a `target` (\"local\" or connected system id)")?
                .to_string();
            let hf_link = args
                .get("hf_link")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let hf_token = args.get("hf_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let gpu = args.get("gpu").and_then(|v| v.as_bool()).unwrap_or(true);
            let v = crate::bench::deploy(
                axum::extract::State(state.clone()),
                axum::Json(crate::bench::DeployReq {
                    target,
                    hf_link,
                    hf_token,
                    env: Default::default(),
                    gpu,
                }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_status" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_status needs a `target`")?
                .to_string();
            let v = crate::bench::status(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::TargetQuery { target }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_stop" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_stop needs a `target`")?
                .to_string();
            let v = crate::bench::stop(
                axum::extract::State(state.clone()),
                axum::Json(crate::bench::TargetQuery { target }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_updates" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_updates needs a `target`")?
                .to_string();
            let v = crate::bench::updates(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::TargetQuery { target }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_update" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_update needs a `target`")?
                .to_string();
            let v = crate::bench::update(
                axum::extract::State(state.clone()),
                axum::Json(crate::bench::TargetQuery { target }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_ssh_key" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_ssh_key needs a `target`")?
                .to_string();
            let v = crate::bench::ssh_key(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::SshKeyQuery { target, port: None }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_authorize" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_authorize needs `target` (pod host)")?
                .to_string();
            let serve_system = args
                .get("serve_system")
                .and_then(|v| v.as_str())
                .ok_or("bench_authorize needs `serve_system`")?
                .to_string();
            let v = crate::bench::authorize(
                axum::extract::State(state.clone()),
                axum::Json(crate::bench::AuthorizeReq {
                    target,
                    serve_system,
                    port: None,
                }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_scan_endpoints" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_scan_endpoints needs `target`")?
                .to_string();
            let remote = args
                .get("remote")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let v = crate::bench::scan_endpoints(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::ScanQuery {
                    target,
                    remote,
                    port: None,
                }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_run_verified" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_run_verified needs `target`")?
                .to_string();
            let hf_link = args
                .get("hf_link")
                .and_then(|v| v.as_str())
                .ok_or("bench_run_verified needs `hf_link`")?
                .to_string();
            let serve_url = args
                .get("serve_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let remote_host = args
                .get("remote_host")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let endpoint_model = args
                .get("endpoint_model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let verify_endpoint = args
                .get("verify_endpoint")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let deep_verify = args
                .get("deep_verify")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let preset = args
                .get("preset")
                .and_then(|v| v.as_str())
                .unwrap_or("comprehensive")
                .to_string();
            let v = crate::bench::run_verified(
                axum::extract::State(state.clone()),
                axum::Json(crate::bench::VerifiedRunReq {
                    target,
                    hf_link,
                    serve_url,
                    remote_host,
                    endpoint_model,
                    verify_endpoint,
                    deep_verify,
                    preset,
                    port: None,
                }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "bench_jobs" => {
            let target = args
                .get("target")
                .and_then(|v| v.as_str())
                .ok_or("bench_jobs needs a `target`")?
                .to_string();
            let v = crate::bench::jobs(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::bench::JobsQuery { target, port: None }),
            )
            .await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "system_image_updates" => {
            let v = crate::image_update::image_updates(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "os_update_check" => {
            let v = crate::update::check(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "os_update_apply" => {
            // Audit actor comes from HeaderMap; MCP dispatch has none in scope, so
            // pass an empty one (logs as "anonymous") — same as reboot/create_token.
            let v = crate::update::apply(axum::extract::State(state.clone()), axum::http::HeaderMap::new()).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "os_update_status" => {
            let v = crate::update::status(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "os_auto_updates" => {
            // No `enable` arg → read state; with it → set it.
            let v = match args.get("enable").and_then(|v| v.as_bool()) {
                None => crate::update::auto_status(axum::extract::State(state.clone())).await,
                Some(enable) => {
                    crate::update::auto_set(
                        axum::extract::State(state.clone()),
                        axum::http::HeaderMap::new(),
                        axum::Json(crate::update::AutoReq { enable }),
                    )
                    .await
                }
            };
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "security_metrics" => {
            let m = crate::security_metrics::get_metrics(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&m.0).unwrap_or_default()))
        }
        "firewall_rules" => {
            let r = crate::firewall::list_rules(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&r.0).unwrap_or_default()))
        }
        "dns_blacklist" => {
            let b = crate::dns_log::get_blacklist(axum::extract::State(state.clone())).await;
            let log = crate::dns_log::get_log(axum::extract::State(state.clone())).await;
            let combined = json!({
                "blacklist": b.0,
                "log": log.0,
            });
            Ok(text_result(&serde_json::to_string_pretty(&combined).unwrap_or_default()))
        }
        "dns_sources" => {
            let s = crate::dns_log::list_sources(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&s.0).unwrap_or_default()))
        }
        // "ssh_keys" deliberately removed — SSH management is human-admin only.
        "audit_log" => {
            let q = crate::audit::AuditQuery {
                limit: args.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize),
                actor: args.get("actor").and_then(|v| v.as_str()).map(|s| s.to_string()),
                action: args.get("action").and_then(|v| v.as_str()).map(|s| s.to_string()),
            };
            let resp = crate::audit::list(
                axum::extract::State(state.clone()),
                axum::extract::Query(q),
            ).await.into_response();
            // resp is a 200 with JSON body; serialize body bytes back to text.
            // Simpler: re-call the list helper but it doesn't expose the
            // pre-IntoResponse value. We collect the body here.
            use http_body_util::BodyExt;
            let (parts, body) = resp.into_parts();
            let _ = parts; // status is always 200 in success path
            let bytes = body.collect().await
                .map_err(|e| format!("collect body: {e}"))?
                .to_bytes();
            let s = String::from_utf8_lossy(&bytes).to_string();
            Ok(text_result(&s))
        }

        // ── Target machine power (v60) ──
        "target_info" => {
            let v = crate::target::get_info(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "target_power_tap" => {
            let resp = crate::target::power_tap(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "target_power_hold" => {
            let resp = crate::target::power_hold(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "target_wake" => {
            let resp = crate::target::wake(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "target_reboot" => {
            let resp = crate::target::reboot(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Shared clipboard ──
        "get_clipboard" => {
            let resp = crate::clipboard::get_clipboard(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "set_clipboard" => {
            let text = args.get("text").and_then(|v| v.as_str())
                .ok_or("set_clipboard needs a `text` string")?;
            let req: crate::clipboard::ClipboardPut = serde_json::from_value(
                json!({"text": text})
            ).map_err(|e| format!("parse: {e}"))?;
            let resp = crate::clipboard::put_clipboard(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "type_clipboard" => {
            let resp = crate::clipboard::type_on_target(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Files ──
        "list_files" => {
            let resp = crate::file_xfer::list_files(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&resp.0).unwrap_or_default()))
        }
        "read_file" => {
            let name = args.get("name").and_then(|v| v.as_str()).ok_or("name required")?;
            let resp = crate::file_xfer::download(
                axum::extract::State(state.clone()),
                axum::extract::Path(name.to_string()),
            ).await.into_response();
            use http_body_util::BodyExt;
            let (parts, body) = resp.into_parts();
            if !parts.status.is_success() {
                return Err(format!("read_file failed: HTTP {}", parts.status));
            }
            let bytes = body.collect().await
                .map_err(|e| format!("collect body: {e}"))?
                .to_bytes();
            let b64 = B64.encode(&bytes);
            Ok(text_result(&serde_json::to_string(&json!({
                "name": name,
                "size_bytes": bytes.len(),
                "base64": b64,
            })).unwrap_or_default()))
        }
        "delete_file" => {
            let name = args.get("name").and_then(|v| v.as_str()).ok_or("name required")?;
            let resp = crate::file_xfer::delete_file(
                axum::extract::State(state.clone()),
                axum::extract::Path(name.to_string()),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── DNSCrypt criteria + state ──
        "dnscrypt_state" => {
            // Dedicated DNSCrypt detail tool — include the full catalog so
            // an agent inspecting state can see the available resolvers.
            let v = crate::network::get_dnscrypt(
                axum::extract::State(state.clone()),
                axum::extract::Query(crate::network::DnscryptQuery { catalog: true }),
            ).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "set_dnscrypt_criteria" => {
            // Translate flat MCP args into the nested PUT shape the HTTP
            // handler expects (servers.{mode, auto_criteria}).
            let mut servers = json!({});
            if let Some(m) = args.get("mode") { servers["mode"] = m.clone(); }
            let mut crit = json!({});
            for k in ["no_logs","dnssec","no_filter","outside_five_eyes","outside_fourteen_eyes","min_trust_score"] {
                if let Some(v) = args.get(k) { crit[k] = v.clone(); }
            }
            if crit.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                servers["auto_criteria"] = crit;
            }
            let mut body = json!({"servers": servers});
            if let Some(en) = args.get("enabled") { body["enabled"] = en.clone(); }
            let req: crate::network::DnscryptPutReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::network::put_dnscrypt(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "set_anonymized_relays" => {
            let mut anon = json!({});
            for k in ["enabled","mode"] {
                if let Some(v) = args.get(k) { anon[k] = v.clone(); }
            }
            let mut crit = json!({});
            for k in ["no_logs","outside_five_eyes","outside_fourteen_eyes","dnssec"] {
                if let Some(v) = args.get(k) { crit[k] = v.clone(); }
            }
            if crit.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                anon["criteria"] = crit;
            }
            if let Some(r) = args.get("specific_relays") {
                anon["specific_relays"] = r.clone();
            }
            let body = json!({"anonymized": anon});
            let req: crate::network::DnscryptPutReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::network::put_dnscrypt(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Tor ──
        "set_tor" => {
            let mut tor = json!({});
            for k in ["enabled","mode","over_vpn"] {
                if let Some(v) = args.get(k) { tor[k] = v.clone(); }
            }
            let body = json!({"tor": tor});
            let req: crate::network::VpnPutReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::network::put_vpn(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── I2P ──
        "set_i2p" => {
            let mut i2p = json!({});
            for k in ["enabled","outproxy","over_vpn"] {
                if let Some(v) = args.get(k) { i2p[k] = v.clone(); }
            }
            let body = json!({"i2p": i2p});
            let req: crate::network::VpnPutReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::network::put_vpn(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "i2p_status" => {
            let v = crate::i2p::get_status(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }

        // ── Pi system ──
        "pi_system_info" => {
            let v = crate::system::info(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "pi_reboot" => {
            // system::reboot wants HeaderMap for audit-actor identification.
            let v = crate::system::reboot(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
            ).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }

        // ── WiFi ──
        "wifi_state" => {
            let resp = crate::wifi::state(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "wifi_scan" => {
            let resp = crate::wifi::scan(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "wifi_connect" => {
            let ssid = args.get("ssid").and_then(|v| v.as_str()).ok_or("ssid required")?;
            let psk = args.get("psk").and_then(|v| v.as_str()).unwrap_or("");
            let body = json!({"ssid": ssid, "psk": psk});
            let req: crate::wifi::ConnectReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::wifi::connect(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Storage (ISO library) ──
        "list_isos" => {
            let v = crate::storage::list(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "set_active_iso" => {
            let slug = args.get("slug").and_then(|v| v.as_str()).ok_or("slug required")?;
            let req: crate::storage::ActivateReq = serde_json::from_value(
                json!({"slug": slug})
            ).map_err(|e| format!("parse: {e}"))?;
            let resp = crate::storage::put_active(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── VPN clearnet ──
        "vpn_state" => {
            let v = crate::network::get_vpn(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "set_vpn_provider" => {
            let mut body = json!({});
            for k in ["provider","enabled","kill_switch"] {
                if let Some(v) = args.get(k) { body[k] = v.clone(); }
            }
            let req: crate::network::VpnPutReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::network::put_vpn(
                axum::extract::State(state.clone()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── VPN provider wizards (v59) ──
        "vpn_providers_catalog" => {
            let v = crate::vpn_providers::api::get_catalog(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&v.0).unwrap_or_default()))
        }
        "vpn_provider_state" => {
            let p = args.get("provider").and_then(|v| v.as_str()).ok_or("provider required")?;
            let resp = crate::vpn_providers::api::get_state(
                axum::extract::State(state.clone()),
                axum::extract::Path(p.to_string()),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "vpn_provider_select" => {
            let p = args.get("provider").and_then(|v| v.as_str()).ok_or("provider required")?;
            let body = json!({
                "server_id": args.get("server_id"),
                "mode": args.get("mode").cloned().unwrap_or_else(|| json!("manual")),
            });
            let req: crate::vpn_providers::api::SelectReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            let resp = crate::vpn_providers::api::select(
                axum::extract::State(state.clone()),
                axum::extract::Path(p.to_string()),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "vpn_provider_pick_fastest" => {
            let p = args.get("provider").and_then(|v| v.as_str()).ok_or("provider required")?;
            // v76: optional no_eyes — rank only servers outside the 5/9/14-Eyes alliances.
            let no_eyes = args.get("no_eyes").and_then(|v| v.as_bool()).unwrap_or(false);
            let params = crate::vpn_providers::api::PickFastestParams {
                eyes: if no_eyes { Some("none".to_string()) } else { None },
            };
            let resp = crate::vpn_providers::api::pick_fastest(
                axum::extract::State(state.clone()),
                axum::extract::Path(p.to_string()),
                axum::extract::Query(params),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Tokens ──
        "list_tokens" => {
            let resp = crate::auth::list_tokens(axum::extract::State(state.clone())).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "issue_token" => {
            let name = args.get("name").and_then(|v| v.as_str()).ok_or("name required")?;
            let scope = args.get("scope").and_then(|v| v.as_str()).ok_or("scope required")?;
            let body = json!({"name": name, "scope": scope});
            let req: crate::auth::CreateTokenReq = serde_json::from_value(body)
                .map_err(|e| format!("parse: {e}"))?;
            // create_token wants HeaderMap for audit-logging the actor.
            // From MCP we don't have one — pass an empty map; the
            // identify() helper falls back to "anonymous".
            let resp = crate::auth::create_token(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::Json(req),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }
        "revoke_token" => {
            let id = args.get("id").and_then(|v| v.as_str()).ok_or("id required")?;
            let resp = crate::auth::revoke_token(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::extract::Path(id.to_string()),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        // ── Blocked log ──
        "blocked_log" => {
            let q = crate::blocked_log::BlockedQuery::default();
            let resp = crate::blocked_log::list(
                axum::extract::State(state.clone()),
                axum::extract::Query(q),
            ).await.into_response();
            Ok(text_result(&body_to_string(resp).await))
        }

        other => Err(format!("unknown tool: {other}")),
    }
}

/// Drain an axum Response body into a string for MCP text_result.
async fn body_to_string(resp: axum::response::Response) -> String {
    use http_body_util::BodyExt;
    let (_, body) = resp.into_parts();
    match body.collect().await {
        Ok(c) => String::from_utf8_lossy(&c.to_bytes()).to_string(),
        Err(e) => format!("(body collect error: {e})"),
    }
}

fn text_result(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

// ── Prompts / resources ────────────────────────────────────────────────

fn list_prompts_for_mcp(state: &AppState) -> Value {
    let names = state.stores.list_prompts();
    let prompts: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({
                "name": n,
                "description": format!("Stored prompt: {n}"),
            })
        })
        .collect();
    json!({ "prompts": prompts })
}

fn get_prompt_for_mcp(state: &AppState, name: &str) -> Result<Value, String> {
    let Some(path) = state.stores.find_prompt_path(name) else {
        return Err("prompt not found".into());
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
    Ok(json!({
        "description": format!("Stored prompt: {name}"),
        "messages": [
            {
                "role": "user",
                "content": { "type": "text", "text": text }
            }
        ],
    }))
}

fn list_resources_for_mcp(state: &AppState) -> Value {
    let mut resources: Vec<Value> = Vec::new();
    for name in state.stores.list_macros() {
        resources.push(json!({
            "uri": format!("aeon://macros/{name}"),
            "name": format!("macro: {name}"),
            "mimeType": "application/toml",
        }));
    }
    for name in state.stores.list_prompts() {
        resources.push(json!({
            "uri": format!("aeon://prompts/{name}"),
            "name": format!("prompt: {name}"),
            "mimeType": "text/markdown",
        }));
    }
    json!({ "resources": resources })
}

fn read_resource_for_mcp(state: &AppState, uri: &str) -> Result<Value, String> {
    if let Some(name) = uri.strip_prefix("aeon://macros/") {
        let Some(path) = state.stores.find_macro_path(name) else {
            return Err("macro not found".into());
        };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/toml",
                "text": text,
            }]
        }));
    }
    if let Some(name) = uri.strip_prefix("aeon://prompts/") {
        let Some(path) = state.stores.find_prompt_path(name) else {
            return Err("prompt not found".into());
        };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("read: {e}"))?;
        return Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/markdown",
                "text": text,
            }]
        }));
    }
    Err(format!("unknown resource uri: {uri}"))
}

// ── HTTP handler ────────────────────────────────────────────────────────

/// Minimum token scope required to call each MCP tool — mirrors the REST policy
/// in `auth::scope_allows` so an MCP token can't be used to bypass it. Default is
/// Full (the interactive-control surface); reads are Read, macro-run is Macros,
/// and the human-only line (token management + target/Pi power) is Admin — never
/// reachable by a provisioned agent token (agents are granted read or full).
fn tool_min_scope(name: &str) -> crate::auth::TokenScope {
    use crate::auth::TokenScope::{Admin, Full, Macros, Read};
    match name {
        "issue_token" | "revoke_token" | "list_tokens" | "target_power_tap"
        | "target_power_hold" | "target_wake" | "target_reboot" | "pi_reboot" => Admin,
        "run_macro" => Macros,
        "state" | "snapshot" | "screen_text" | "recording_state" | "list_recordings" | "list_macros"
        | "network_status" | "security_metrics" | "firewall_rules" | "dns_blacklist"
        | "dns_sources" | "audit_log" | "target_info" | "get_clipboard" | "list_files"
        | "read_file" | "dnscrypt_state" | "i2p_status" | "pi_system_info" | "wifi_state"
        | "wifi_scan" | "list_isos" | "vpn_state" | "vpn_providers_catalog"
        | "vpn_provider_state" | "blocked_log" | "hidden_service_list" | "ipfs_status"
        | "bench_status" | "bench_model_info" | "bench_updates"
        | "bench_ssh_key" | "bench_scan_endpoints" | "bench_jobs"
        | "system_image_updates" | "os_update_check" | "os_update_status" => Read,
        _ => Full,
    }
}

fn scope_rank(s: &crate::auth::TokenScope) -> u8 {
    use crate::auth::TokenScope::*;
    match s {
        Read => 0,
        Macros => 1,
        Full => 2,
        Admin => 3,
    }
}

pub async fn handle(State(state): State<AppState>, headers: HeaderMap, body: String) -> impl IntoResponse {
    // Reject obviously empty bodies with a JSON-RPC parse error.
    if body.trim().is_empty() {
        return (StatusCode::OK, Json(json!({
            "jsonrpc": "2.0", "id": null,
            "error": { "code": -32700, "message": "empty body" }
        }))).into_response();
    }

    let req: RpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::OK, Json(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse: {e}") }
            }))).into_response();
        }
    };
    let id = req.id.clone().unwrap_or(Value::Null);

    let response = match req.method.as_str() {
        "initialize" => RpcResponse::ok(id, json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false },
                "prompts": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION,
            },
            "instructions":
                "AEON Magick AI Computer Control. \
                 Drive a target host's keyboard, mouse, trackpad, and screen capture \
                 through atomic HTTPS-backed HID ops. Call `state` first to confirm the \
                 target USB-C is connected (keyboard_online + mouse_online true), then \
                 `snapshot` to see, then act with `type_text`, `key_chord`, `click`, \
                 `move_cursor`, `scroll`. Use the USER'S OWN devices only."
        })),
        // Notification — no response expected.
        "notifications/initialized" | "initialized" => {
            return (StatusCode::ACCEPTED, "").into_response();
        }
        "ping" => RpcResponse::ok(id, json!({})),
        "tools/list" => {
            // Only advertise tools the caller's scope can actually call.
            let crank = crate::auth::identify(&state.auth, &headers)
                .map(|i| scope_rank(&i.scope))
                .unwrap_or(0);
            let mut cat = tools_catalog();
            if let Some(arr) = cat.get_mut("tools").and_then(|t| t.as_array_mut()) {
                arr.retain(|t| {
                    t.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| scope_rank(&tool_min_scope(n)) <= crank)
                        .unwrap_or(false)
                });
            }
            RpcResponse::ok(id, cat)
        }
        "tools/call" => {
            let name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let empty = Value::Null;
            let args = req.params.get("arguments").unwrap_or(&empty);
            // Per-tool scope gate (mirrors REST auth::scope_allows). The /api/mcp
            // route only checks that a token is Macros+, so without this a
            // macros/full token could invoke admin-only tools (issue_token,
            // target/Pi power) and escalate.
            let caller = crate::auth::identify(&state.auth, &headers).map(|i| i.scope);
            let need = tool_min_scope(name);
            if caller.as_ref().map(scope_rank).unwrap_or(0) < scope_rank(&need) {
                let have = caller.as_ref().map(|s| s.as_str()).unwrap_or("none");
                RpcResponse::ok(id, json!({
                    "content": [{ "type": "text", "text": format!(
                        "error: tool '{name}' requires '{}' scope but this token is '{}'. \
                         Token management and target/Pi power are human-admin only.",
                        need.as_str(), have) }],
                    "isError": true,
                }))
            } else {
                match dispatch_tool(&state, name, args).await {
                    Ok(result) => RpcResponse::ok(id, result),
                    Err(e) => RpcResponse::ok(id.clone(), json!({
                        "content": [{ "type": "text", "text": format!("error: {e}") }],
                        "isError": true,
                    })),
                }
            }
        }
        "prompts/list" => RpcResponse::ok(id, list_prompts_for_mcp(&state)),
        "prompts/get" => {
            let name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match get_prompt_for_mcp(&state, name) {
                Ok(v) => RpcResponse::ok(id, v),
                Err(e) => RpcResponse::err(id, -32602, e),
            }
        }
        "resources/list" => RpcResponse::ok(id, list_resources_for_mcp(&state)),
        "resources/read" => {
            let uri = req.params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            match read_resource_for_mcp(&state, uri) {
                Ok(v) => RpcResponse::ok(id, v),
                Err(e) => RpcResponse::err(id, -32602, e),
            }
        }
        other => RpcResponse::err(id, -32601, format!("method not found: {other}")),
    };

    (StatusCode::OK, Json(response)).into_response()
}
