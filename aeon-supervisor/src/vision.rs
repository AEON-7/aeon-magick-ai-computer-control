//! GET /api/vision/detections — on-device live OCR / object detection.
//!
//! Relays /run/aeon/vision.json, written by the `aeon-vision` daemon, which
//! pulls frames from the streamer and runs OCR (CPU tesseract) or a model on
//! the Hailo AI HAT+. Detection boxes are fractions 0..1 — the same convention
//! the MCP `click` tool uses — so an agent can act on a result directly.
//! `{"present": false}` when vision is disabled, the daemon isn't running, or
//! there's no frame yet. Mirrors `ups.rs`.
use crate::api::AppState;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

const VISION_JSON: &str = "/run/aeon/vision.json";
const VLM_SOCK: &str = "/run/aeon/vision-vlm.sock";

pub async fn get_detections(State(_state): State<AppState>) -> impl IntoResponse {
    match tokio::fs::read_to_string(VISION_JSON).await {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => Json(v).into_response(),
            Err(e) => Json(json!({
                "present": false, "ok": false,
                "err": format!("parse {VISION_JSON}: {e}")
            }))
            .into_response(),
        },
        Err(_) => Json(json!({ "present": false })).into_response(),
    }
}

// ── screen_find: locate on-screen TEXT for the agent to click ───────────────
// Searches the live OCR detections (aeon-vision) for `query` and returns ranked
// matches, each with a `center` {x,y} in fractions 0..1 — feed straight into
// click(x,y). Pure read of /run/aeon/vision.json; no NPU needed (the OCR daemon
// already published the boxes), so it works alongside the resident LLM.
pub async fn screen_find_value(query: &str) -> Value {
    let v = tokio::fs::read_to_string(VISION_JSON)
        .await
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({ "present": false }));
    if v.get("present").and_then(|p| p.as_bool()) != Some(true) {
        return json!({
            "ok": false, "present": false,
            "err": "vision/OCR not available — enable aeon-vision (backend=hailo or tesseract) to use screen_find",
        });
    }
    find_matches(&v, query)
}

/// Pure matcher over a parsed vision.json — ranks detections against `query`
/// and emits click-ready centres. Split out so it's unit-testable.
fn find_matches(v: &Value, query: &str) -> Value {
    let q = query.trim().to_lowercase();
    let empty: Vec<Value> = Vec::new();
    let dets = v.get("detections").and_then(|d| d.as_array()).unwrap_or(&empty);
    let mut matches: Vec<Value> = Vec::new();
    for d in dets {
        let text = d.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let tl = text.to_lowercase();
        if tl.is_empty() {
            continue;
        }
        // Rank: exact > detection-contains-query > query-contains-detection >
        // word-overlap. Keeps "Save" matching a "Save As…" button, etc.
        let score = if tl == q {
            1.0
        } else if tl.contains(&q) {
            0.85
        } else if q.contains(&tl) {
            0.7
        } else {
            let hits = q.split_whitespace().filter(|w| tl.contains(w)).count();
            if hits > 0 {
                (0.4 + 0.1 * hits as f64).min(0.65)
            } else {
                0.0
            }
        };
        if score <= 0.0 {
            continue;
        }
        let b = d.get("box").cloned().unwrap_or_else(|| json!({}));
        let g = |k: &str| b.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let cx = g("x") + g("w") / 2.0;
        let cy = g("y") + g("h") / 2.0;
        let r4 = |f: f64| (f * 10000.0).round() / 10000.0;
        matches.push(json!({
            "text": text,
            "match_score": (score * 100.0).round() / 100.0,
            "ocr_conf": d.get("conf"),
            "center": { "x": r4(cx), "y": r4(cy) },
            "box": b,
        }));
    }
    matches.sort_by(|a, b| {
        let s = |x: &Value| x.get("match_score").and_then(|s| s.as_f64()).unwrap_or(0.0);
        s(b).partial_cmp(&s(a)).unwrap_or(std::cmp::Ordering::Equal)
    });
    matches.truncate(8);
    json!({
        "ok": true,
        "query": query,
        "count": matches.len(),
        "matches": matches,
        "hint": "feed a match `center` {x,y} (fractions 0..1 of the frame) straight into click(x,y)",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_find_ranks_and_centres() {
        // Two detections; query "save" should rank the exact "Save" first and
        // return its box centre (0.10+0.06/2, 0.20+0.04/2) = (0.13, 0.22).
        let v = json!({
            "present": true,
            "detections": [
                {"text": "Cancel", "conf": 0.9, "box": {"x": 0.5, "y": 0.2, "w": 0.08, "h": 0.04}},
                {"text": "Save",   "conf": 0.95, "box": {"x": 0.10, "y": 0.20, "w": 0.06, "h": 0.04}},
                {"text": "Save As…", "conf": 0.8, "box": {"x": 0.3, "y": 0.2, "w": 0.12, "h": 0.04}}
            ]
        });
        let r = find_matches(&v, "Save");
        assert_eq!(r["count"], 2, "should match 'Save' and 'Save As…', not 'Cancel'");
        let top = &r["matches"][0];
        assert_eq!(top["text"], "Save");
        assert_eq!(top["match_score"], 1.0);
        assert_eq!(top["center"]["x"], 0.13);
        assert_eq!(top["center"]["y"], 0.22);
        // word-overlap / no-match
        assert_eq!(find_matches(&v, "nonexistent")["count"], 0);
    }
}

// ── describe_screen: natural-language read of the screen via Qwen2-VL ────────
// Proxies to the on-demand aeon-vision-vlm service over its unix socket. That
// service lazily loads the VLM (holds the NPU's single GenAI slot while resident,
// then idle-unloads) — so describe_screen needs the slot free (i.e. the local LLM
// not resident). ~3s warm, ~13s cold; allow a generous timeout for the cold load.
pub async fn describe_screen_value(prompt: Option<&str>, max_tokens: Option<u64>) -> Value {
    let body = json!({
        "prompt": prompt,
        "max_tokens": max_tokens.unwrap_or(96),
    })
    .to_string();
    let out = tokio::process::Command::new("curl")
        .args([
            "-s", "--max-time", "60", "--unix-socket", VLM_SOCK,
            "-X", "POST", "-H", "Content-Type: application/json",
            "-d", &body, "http://localhost/describe",
        ])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            serde_json::from_slice::<Value>(&o.stdout).unwrap_or_else(|_| {
                json!({ "ok": false, "err": "vlm service returned non-JSON" })
            })
        }
        _ => json!({
            "ok": false,
            "err": "describe_screen unavailable — the aeon-vision-vlm service isn't running or \
                    the VLM isn't deployed (deploy 'qwen2-vl-2b-instruct' from the Hailo app, \
                    and ensure the NPU GenAI slot isn't held by the local LLM)",
        }),
    }
}

#[derive(Deserialize)]
pub struct FindParams {
    pub query: String,
}

/// GET /api/vision/find?query=… — OCR-anchored grounding for the web/agent.
pub async fn get_find(State(_s): State<AppState>, Query(p): Query<FindParams>) -> impl IntoResponse {
    Json(screen_find_value(&p.query).await)
}

#[derive(Deserialize)]
pub struct DescribeReq {
    pub prompt: Option<String>,
    pub max_tokens: Option<u64>,
}

/// POST /api/vision/describe {prompt?, max_tokens?} — VLM describe-screen.
pub async fn post_describe(
    State(_s): State<AppState>,
    Json(req): Json<DescribeReq>,
) -> impl IntoResponse {
    Json(describe_screen_value(req.prompt.as_deref(), req.max_tokens).await)
}
