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
use axum::http::StatusCode;
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
            tool("set_persona",
                 "Hot-swap the USB HID persona the target sees. Triggers a USB re-enumeration (about 1 s blip).",
                 json!({"type":"object","required":["persona"],
                        "properties":{"persona":{"type":"string","enum":["generic-composite","logitech-mx","apple-magic"]}}})),
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
            tool("ssh_keys",
                 "List trusted SSH public keys (fingerprint + comment).",
                 json!({"type":"object","properties":{}})),
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
                 "Switch the active clearnet VPN provider (none / tailscale / wireguard / openvpn / mullvad / ivpn / azirevpn). For mullvad/ivpn/azirevpn you must run the setup wizard via REST first to register your account.",
                 json!({"type":"object","required":["provider"],
                        "properties":{
                            "provider":{"type":"string","enum":["none","tailscale","wireguard","openvpn","mullvad","ivpn","azirevpn"]},
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
                        "properties":{"provider":{"type":"string","enum":["mullvad","ivpn","azirevpn"]}}})),
            tool("vpn_provider_select",
                 "Pick a specific server for an already-configured provider. server_id is the hostname from vpn_provider_state's server list.",
                 json!({"type":"object","required":["provider","server_id"],
                        "properties":{
                            "provider":{"type":"string","enum":["mullvad","ivpn","azirevpn"]},
                            "server_id":{"type":"string"},
                            "mode":{"type":"string","enum":["manual","auto"]}
                        }})),
            tool("vpn_provider_pick_fastest",
                 "TCP-probe every cached server for the given provider, return ranking by RTT. The supervisor doesn't auto-apply — agent inspects the result and calls vpn_provider_select on the winner.",
                 json!({"type":"object","required":["provider"],
                        "properties":{"provider":{"type":"string","enum":["mullvad","ivpn","azirevpn"]}}})),

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
            let dns = crate::network::get_dnscrypt(axum::extract::State(state.clone())).await;
            let vpn = crate::network::get_vpn(axum::extract::State(state.clone())).await;
            let combined = json!({
                "usb_ethernet": usb.0,
                "dnscrypt": dns.0,
                "vpn": vpn.0,
            });
            Ok(text_result(&serde_json::to_string_pretty(&combined).unwrap_or_default()))
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
        "ssh_keys" => {
            let k = crate::ssh_keys::list_keys(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&k.0).unwrap_or_default()))
        }
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
            let v = crate::network::get_dnscrypt(axum::extract::State(state.clone())).await;
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
            let resp = crate::vpn_providers::api::pick_fastest(
                axum::extract::State(state.clone()),
                axum::extract::Path(p.to_string()),
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

pub async fn handle(State(state): State<AppState>, body: String) -> impl IntoResponse {
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
        "tools/list" => RpcResponse::ok(id, tools_catalog()),
        "tools/call" => {
            let name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let empty = Value::Null;
            let args = req.params.get("arguments").unwrap_or(&empty);
            match dispatch_tool(&state, name, args).await {
                Ok(result) => RpcResponse::ok(id, result),
                Err(e) => RpcResponse::ok(id.clone(), json!({
                    "content": [{ "type": "text", "text": format!("error: {e}") }],
                    "isError": true,
                })),
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
