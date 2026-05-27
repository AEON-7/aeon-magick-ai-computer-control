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
            tool("ssh_keys",
                 "List trusted SSH public keys (fingerprint + comment).",
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
        "ssh_keys" => {
            let k = crate::ssh_keys::list_keys(axum::extract::State(state.clone())).await;
            Ok(text_result(&serde_json::to_string_pretty(&k.0).unwrap_or_default()))
        }
        other => Err(format!("unknown tool: {other}")),
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
