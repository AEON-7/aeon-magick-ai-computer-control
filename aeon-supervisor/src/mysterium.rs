//! Mysterium — run a Mysterium Network node: share idle bandwidth to help
//! decentralize internet access and earn MYST. The `aeon-mysterium` script
//! installs + runs the official myst node; this module reads live stats from the
//! node's local TequilAPI (127.0.0.1:4050) for the dashboard. Account + wallet/
//! payout are set up non-custodially at mystnodes.co. Admin-gated; OFF by default.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Command;

use crate::api::AppState;

const CONFIG_TOML: &str = "/etc/aeon/mysterium.toml";
const SCRIPT: &str = "/usr/local/bin/aeon-mysterium";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MysteriumConfig {
    #[serde(default)]
    pub enabled: bool,
}

fn read_config() -> MysteriumConfig {
    std::fs::read_to_string(CONFIG_TOML)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_config(c: &MysteriumConfig) -> std::io::Result<()> {
    let text = toml::to_string_pretty(c)
        .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
    if let Some(p) = std::path::Path::new(CONFIG_TOML).parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = format!("{CONFIG_TOML}.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, CONFIG_TOML)
}

fn run_script(args: &[&str]) -> Result<String, String> {
    let out = Command::new(SCRIPT)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {SCRIPT}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{SCRIPT} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn script_status() -> Value {
    run_script(&["status"])
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"installed": false, "daemon": "inactive"}))
}

/// Query the node's local TequilAPI (default Basic auth myst:mystberry).
fn tq(path: &str) -> Option<Value> {
    let url = format!("http://127.0.0.1:4050{path}");
    let out = Command::new("curl")
        .args(["-fsS", "-u", "myst:mystberry", "--max-time", "6", "--", &url])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Send a POST/DELETE to the TequilAPI. The (optional) JSON body is streamed via
/// the child's stdin — NEVER argv — so a secret (e.g. the MystNodes API key)
/// never lands in the process table or any log. Returns (http_code, body).
fn tq_send(method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
    use std::io::Write;
    use std::process::Stdio;
    let url = format!("http://127.0.0.1:4050{path}");
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sS", "-u", "myst:mystberry", "--max-time", "20", "-X", method, "-w", "\n%{http_code}",
    ]);
    if body.is_some() {
        cmd.args(["-H", "Content-Type: application/json", "--data", "@-"]);
    }
    cmd.arg("--").arg(&url);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn curl: {e}"))?;
    match body {
        Some(b) => child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(b.as_bytes())
            .map_err(|e| format!("write body: {e}"))?,
        None => drop(child.stdin.take()),
    }
    let out = child.wait_with_output().map_err(|e| format!("wait curl: {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout).to_string();
    let (b, code) = s.rsplit_once('\n').unwrap_or(("", s.as_str()));
    Ok((code.trim().parse().unwrap_or(0), b.to_string()))
}

fn ptr_str(o: &Option<Value>, p: &str) -> String {
    o.as_ref()
        .and_then(|v| v.pointer(p))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn ptr_u64(o: &Option<Value>, p: &str) -> u64 {
    o.as_ref()
        .and_then(|v| v.pointer(p))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// GET /api/mysterium/status — node lifecycle + registration + earnings +
/// sessions + bandwidth + region, aggregated from the TequilAPI. Admin-gated.
pub async fn status(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = read_config();
        let st = script_status();
        let installed = st.get("installed").and_then(|v| v.as_bool()).unwrap_or(false);
        let daemon = st.get("daemon").and_then(|v| v.as_str()).unwrap_or("inactive").to_string();
        if daemon != "active" {
            return json!({"ok": true, "enabled": cfg.enabled, "installed": installed, "daemon": daemon});
        }
        let hc = tq("/healthcheck");
        let id = tq("/identities")
            .and_then(|v| v.pointer("/identities/0/id").and_then(|x| x.as_str()).map(String::from));
        let info = id.as_ref().and_then(|i| tq(&format!("/identities/{i}")));
        let bene = id.as_ref().and_then(|i| tq(&format!("/identities/{i}/beneficiary")));
        let loc = tq("/location");
        let data = tq("/node/provider/transferred-data?range=30d");
        let sess = tq("/node/provider/sessions-count?range=30d");
        let cons = tq("/node/provider/consumers-count?range=30d");
        let mmn_linked = tq("/mmn/api-key")
            .as_ref()
            .and_then(|v| v.pointer("/api_key"))
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        json!({
            "ok": true,
            "enabled": cfg.enabled,
            "installed": true,
            "daemon": daemon,
            "version": ptr_str(&hc, "/version"),
            "uptime": ptr_str(&hc, "/uptime"),
            "identity": id.clone().unwrap_or_default(),
            "mmn_linked": mmn_linked,
            "registration": ptr_str(&info, "/registration_status"),
            "earnings_myst": ptr_str(&info, "/earnings_tokens/human"),
            "earnings_total_myst": ptr_str(&info, "/earnings_total_tokens/human"),
            "balance_myst": ptr_str(&info, "/balance_tokens/human"),
            "beneficiary": ptr_str(&bene, "/beneficiary"),
            "country": ptr_str(&loc, "/country"),
            "region": ptr_str(&loc, "/region"),
            "city": ptr_str(&loc, "/city"),
            "ip": ptr_str(&loc, "/ip"),
            "data_bytes_30d": ptr_u64(&data, "/transferred_data_bytes"),
            "sessions_30d": ptr_u64(&sess, "/count"),
            "consumers_30d": ptr_u64(&cons, "/count"),
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "status task failed"}));
    Json(v)
}

/// POST /api/mysterium/enable — install (first run downloads ~19 MB) + start the
/// node. Non-blocking: persists config + brings it up in the background; the
/// dashboard polls /status. Admin-gated.
pub async fn enable(State(_s): State<AppState>) -> Json<Value> {
    let saved = tokio::task::spawn_blocking(|| -> bool {
        let mut cfg = read_config();
        cfg.enabled = true;
        write_config(&cfg).is_ok()
    })
    .await
    .unwrap_or(false);
    if saved {
        tokio::task::spawn_blocking(|| {
            let _ = run_script(&["up"]);
        });
        Json(json!({"ok": true, "starting": true}))
    } else {
        Json(json!({"ok": false, "err": "write config failed"}))
    }
}

/// POST /api/mysterium/disable — stop the node (keeps it installed + your claim).
/// Admin-gated.
pub async fn disable(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let mut cfg = read_config();
        cfg.enabled = false;
        let _ = write_config(&cfg);
        let _ = run_script(&["down"]);
        json!({"ok": true})
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "disable task failed"}));
    Json(v)
}

#[derive(Debug, Deserialize)]
pub struct ClaimReq {
    pub api_key: String,
}

/// POST /api/mysterium/claim — link this node to an existing MystNodes account
/// using the account API key from https://my.mystnodes.com/me. The key is
/// streamed to the node's TequilAPI (POST /mmn/api-key) via stdin and persisted
/// in the node's own config (that's how Mysterium keeps the link); the supervisor
/// never stores or logs it. This is an account-management token, NOT a wallet key
/// — funds/payout stay non-custodial on mystnodes.co. Admin-gated.
pub async fn claim(State(_s): State<AppState>, Json(req): Json<ClaimReq>) -> Json<Value> {
    let key = req.api_key.trim().to_string();
    if key.len() < 40 {
        return Json(json!({
            "ok": false,
            "err": "That key looks too short — copy the full API key (40+ chars) from my.mystnodes.com/me."
        }));
    }
    let v = tokio::task::spawn_blocking(move || -> Value {
        let body = json!({ "api_key": key }).to_string();
        match tq_send("POST", "/mmn/api-key", Some(&body)) {
            Ok((code, _)) if (200..300).contains(&code) => json!({"ok": true, "linked": true}),
            Ok((code, resp)) => {
                // The node persists the key even when MMN registration fails (a bad
                // key still gets written), which would leave the node looking
                // "linked". Roll it back so key-presence stays an accurate signal.
                let _ = tq_send("DELETE", "/mmn/api-key", None);
                let parsed = serde_json::from_str::<Value>(&resp).ok();
                let err_code = parsed
                    .as_ref()
                    .and_then(|v| v.pointer("/error/code"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let msg = if err_code == "err_mmn_registration" {
                    "Couldn't link with that key — double-check you copied the correct \
                     API key from my.mystnodes.com/me."
                        .to_string()
                } else {
                    parsed
                        .as_ref()
                        .and_then(|v| {
                            v.pointer("/error/detail")
                                .or_else(|| v.pointer("/error/message"))
                                .and_then(|x| x.as_str())
                                .map(String::from)
                        })
                        .unwrap_or_else(|| format!("node rejected the key (HTTP {code})"))
                };
                json!({"ok": false, "status": code, "err": msg})
            }
            Err(e) => {
                let _ = tq_send("DELETE", "/mmn/api-key", None);
                json!({"ok": false, "err": e})
            }
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "claim task failed"}));
    Json(v)
}

/// POST /api/mysterium/unclaim — clear the stored MystNodes API key from the node
/// (DELETE /mmn/api-key). Unlinks the node from the account; does not touch the
/// wallet/payout. Admin-gated.
pub async fn unclaim(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        match tq_send("DELETE", "/mmn/api-key", None) {
            Ok((code, _)) if (200..300).contains(&code) => json!({"ok": true, "linked": false}),
            Ok((code, _)) => json!({"ok": false, "status": code}),
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "unclaim task failed"}));
    Json(v)
}

#[derive(Debug, Deserialize)]
pub struct ServicesReq {
    #[serde(default)]
    pub vpn: bool,
    #[serde(default)]
    pub scraping: bool,
    #[serde(default)]
    pub data_transfer: bool,
    #[serde(default)]
    pub public: bool,
}

/// GET /api/mysterium/services — the four traffic toggles, read from the node's
/// active-services (service types) + access-policy.list (B2B allowlist vs open).
/// Admin-gated.
pub async fn services_get(State(_s): State<AppState>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(|| -> Value {
        let cfg = tq("/config/user");
        let active = cfg
            .as_ref()
            .and_then(|v| v.pointer("/data/active-services"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let has = |t: &str| active.split(',').any(|s| s.trim() == t);
        // Empty access-policy list = no allowlist = open to the whole network
        // (Public). An unset list defaults to NOT public (conservative).
        let public = cfg
            .as_ref()
            .and_then(|v| v.pointer("/data/access-policy/list"))
            .and_then(|v| v.as_str())
            .map(|s| s.is_empty())
            .unwrap_or(false);
        json!({
            "ok": true,
            "vpn": has("dvpn"),
            "scraping": has("scraping"),
            "data_transfer": has("data_transfer"),
            "public": public,
        })
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "services task failed"}));
    Json(v)
}

/// POST /api/mysterium/services — set the traffic toggles. Writes active-services
/// (the enabled types + always-on `monitoring`) and access-policy.list ("mysterium"
/// B2B allowlist unless Public is on), then restarts the node so the change takes
/// effect. The WAN split-tunnel survives the restart (it's iptables/ip-rule state).
/// Admin-gated.
pub async fn services_set(State(_s): State<AppState>, Json(req): Json<ServicesReq>) -> Json<Value> {
    let v = tokio::task::spawn_blocking(move || -> Value {
        let mut types = vec!["monitoring".to_string()];
        if req.vpn {
            types.push("dvpn".into());
        }
        if req.scraping {
            types.push("scraping".into());
        }
        if req.data_transfer {
            types.push("data_transfer".into());
        }
        let active = types.join(",");
        let list = if req.public { "" } else { "mysterium" };
        let body = json!({"data": {"active-services": active, "access-policy": {"list": list}}})
            .to_string();
        match tq_send("POST", "/config/user", Some(&body)) {
            Ok((code, _)) if (200..300).contains(&code) => {
                let _ = Command::new("systemctl")
                    .args(["restart", "mysterium-node.service"])
                    .status();
                json!({"ok": true, "restarting": true})
            }
            Ok((code, resp)) => {
                json!({"ok": false, "status": code, "err": resp.chars().take(160).collect::<String>()})
            }
            Err(e) => json!({"ok": false, "err": e}),
        }
    })
    .await
    .unwrap_or_else(|_| json!({"ok": false, "err": "services task failed"}));
    Json(v)
}
