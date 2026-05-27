//! Macros, scripts, and prompts — small file-backed automation library.
//!
//! Storage layout:
//!     /etc/aeon/macros/         user-editable TOML macros (priority)
//!     /etc/aeon/scripts/        same schema, semantically "longer recipes"
//!     /etc/aeon/prompts/        plain text playbooks an agent fetches
//!     /usr/share/aeon/macros/   shipped, read-only fallback (lower priority)
//!     /usr/share/aeon/prompts/  shipped, read-only fallback
//!
//! Names: filename minus `.toml` (or `.txt`/`.md` for prompts). One namespace
//! across user-editable + shipped; user-editable wins. PUT into a shipped name
//! shadows the shipped version with a user-editable copy.
//!
//! Macros run sequentially. Each step is one atomic HID call (or `wait` /
//! `snapshot`). Parameter substitution: `{{var}}` placeholders are replaced in
//! every string field before the step is dispatched.
//!
//! Run response shape:
//!     { ok: bool, steps_run: u32, snapshots: [path,...], error?: "..." }

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

// ── Schema ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<Param>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default = "yes")]
    pub required: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Type {
        text: String,
    },
    Key {
        keys: Vec<String>,
        #[serde(default = "default_hold_ms")]
        hold_ms: u32,
    },
    Click {
        #[serde(default = "default_button")]
        button: String,
        #[serde(default = "default_count")]
        count: u32,
    },
    Move {
        dx: i32,
        dy: i32,
    },
    Scroll {
        dy: i32,
    },
    Wait {
        ms: u64,
    },
    Persona {
        persona: String,
    },
    Snapshot,
    ReleaseAll,
}

fn default_hold_ms() -> u32 {
    30
}
fn default_button() -> String {
    "left".into()
}
fn default_count() -> u32 {
    1
}

// ── Discovery & I/O ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Stores {
    pub user_macros: PathBuf,
    pub user_prompts: PathBuf,
    pub shipped_macros: PathBuf,
    pub shipped_prompts: PathBuf,
    pub snapshot_dir: PathBuf,
}

impl Default for Stores {
    fn default() -> Self {
        Self {
            user_macros: PathBuf::from("/etc/aeon/macros"),
            user_prompts: PathBuf::from("/etc/aeon/prompts"),
            shipped_macros: PathBuf::from("/usr/share/aeon/macros"),
            shipped_prompts: PathBuf::from("/usr/share/aeon/prompts"),
            snapshot_dir: PathBuf::from("/run/aeon/snapshots"),
        }
    }
}

impl Stores {
    pub fn ensure_dirs(&self) {
        for d in [
            &self.user_macros,
            &self.user_prompts,
            &self.snapshot_dir,
        ] {
            if let Err(e) = std::fs::create_dir_all(d) {
                warn!(dir=%d.display(), err=%e, "could not create dir");
            }
        }
    }

    fn list_dir(&self, dir: &StdPath, ext: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return vec![];
        };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) == Some(ext) {
                    p.file_stem().and_then(|s| s.to_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn list_macros(&self) -> Vec<String> {
        let mut seen: HashMap<String, ()> = HashMap::new();
        for name in self
            .list_dir(&self.user_macros, "toml")
            .into_iter()
            .chain(self.list_dir(&self.shipped_macros, "toml"))
        {
            seen.entry(name).or_insert(());
        }
        let mut names: Vec<String> = seen.into_keys().collect();
        names.sort();
        names
    }

    pub fn list_prompts(&self) -> Vec<String> {
        let mut seen: HashMap<String, ()> = HashMap::new();
        for dir in [&self.user_prompts, &self.shipped_prompts] {
            for ext in ["txt", "md"] {
                for name in self.list_dir(dir, ext) {
                    seen.entry(name).or_insert(());
                }
            }
        }
        let mut names: Vec<String> = seen.into_keys().collect();
        names.sort();
        names
    }

    pub fn find_macro_path(&self, name: &str) -> Option<PathBuf> {
        for dir in [&self.user_macros, &self.shipped_macros] {
            let p = dir.join(format!("{name}.toml"));
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    pub fn find_prompt_path(&self, name: &str) -> Option<PathBuf> {
        for dir in [&self.user_prompts, &self.shipped_prompts] {
            for ext in ["txt", "md"] {
                let p = dir.join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }
        None
    }
}

fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ── Parameter substitution ──────────────────────────────────────────────

fn substitute(s: &str, params: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            let key = after[..end].trim();
            if let Some(v) = params.get(key) {
                out.push_str(v);
            } else {
                // Unknown var: leave the literal placeholder, let the user see
                // their typo when they look at the result rather than silently
                // emitting an empty string.
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
            rest = &after[end + 2..];
        } else {
            // Unmatched `{{` — emit literally
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

// ── Runner ──────────────────────────────────────────────────────────────

/// Execute one macro by name with optional parameter overrides.
pub async fn run(
    state: &AppState,
    name: &str,
    given_params: &HashMap<String, String>,
) -> Json<Value> {
    let stores = &state.stores;
    let Some(path) = stores.find_macro_path(name) else {
        return Json(json!({"ok": false, "error": "macro not found", "steps_run": 0}));
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return Json(
                json!({"ok": false, "error": format!("read: {e}"), "steps_run": 0}),
            )
        }
    };
    let m: Macro = match toml::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            return Json(
                json!({"ok": false, "error": format!("parse: {e}"), "steps_run": 0}),
            )
        }
    };

    // Merge defaults into params, then verify required ones present.
    let mut effective: HashMap<String, String> = HashMap::new();
    for p in &m.params {
        if let Some(v) = given_params.get(&p.name) {
            effective.insert(p.name.clone(), v.clone());
        } else if let Some(d) = &p.default {
            effective.insert(p.name.clone(), d.clone());
        } else if p.required {
            return Json(json!({
                "ok": false,
                "error": format!("missing required param: {}", p.name),
                "steps_run": 0,
            }));
        }
    }
    // Allow extra params (caller may pass things the macro doesn't reference)
    for (k, v) in given_params {
        effective.entry(k.clone()).or_insert_with(|| v.clone());
    }

    let mut snapshots: Vec<String> = Vec::new();
    let mut steps_run: u32 = 0;

    for step in &m.steps {
        if let Err(e) = exec_step(state, step, &effective, &mut snapshots).await {
            return Json(json!({
                "ok": false,
                "steps_run": steps_run,
                "snapshots": snapshots,
                "error": e,
            }));
        }
        steps_run += 1;
    }

    Json(json!({
        "ok": true,
        "steps_run": steps_run,
        "snapshots": snapshots,
    }))
}

async fn exec_step(
    state: &AppState,
    step: &Step,
    params: &HashMap<String, String>,
    snapshots: &mut Vec<String>,
) -> Result<(), String> {
    use crate::proxy;
    match step {
        Step::Type { text } => {
            let body = serde_json::to_vec(&json!({ "text": substitute(text, params) }))
                .map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/type", body).await
        }
        Step::Key { keys, hold_ms } => {
            let resolved: Vec<String> =
                keys.iter().map(|k| substitute(k, params)).collect();
            let body =
                serde_json::to_vec(&json!({ "keys": resolved, "hold_ms": *hold_ms }))
                    .map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/key", body).await
        }
        Step::Click { button, count } => {
            let body = serde_json::to_vec(&json!({
                "button": substitute(button, params),
                "count": *count,
            }))
            .map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/click", body).await
        }
        Step::Move { dx, dy } => {
            let body = serde_json::to_vec(&json!({ "dx": *dx, "dy": *dy }))
                .map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/move", body).await
        }
        Step::Scroll { dy } => {
            let body =
                serde_json::to_vec(&json!({ "dy": *dy })).map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/scroll", body).await
        }
        Step::Wait { ms } => {
            tokio::time::sleep(std::time::Duration::from_millis(*ms)).await;
            Ok(())
        }
        Step::Persona { persona } => {
            let body =
                serde_json::to_vec(&json!({ "persona": substitute(persona, params) }))
                    .map_err(|e| e.to_string())?;
            proxy::post_hid(state, "/persona", body).await
        }
        Step::Snapshot => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let path = state
                .stores
                .snapshot_dir
                .join(format!("snap_{ts}_{}.jpg", snapshots.len()));
            proxy::write_snapshot(state, &path).await?;
            snapshots.push(path.to_string_lossy().into_owned());
            Ok(())
        }
        Step::ReleaseAll => proxy::post_hid(state, "/release_all", vec![]).await,
    }
}

// ── REST handlers ───────────────────────────────────────────────────────

pub async fn list_macros(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "macros": state.stores.list_macros() }))
}

pub async fn get_macro(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    let Some(path) = state.stores.find_macro_path(&name) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match std::fs::read_to_string(&path) {
        Ok(t) => ([("content-type", "text/plain; charset=utf-8")], t).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}")).into_response(),
    }
}

pub async fn put_macro(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: String,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    // Validate as Macro before persisting so bad TOML never lands on disk.
    if let Err(e) = toml::from_str::<Macro>(&body) {
        return (StatusCode::BAD_REQUEST, format!("invalid macro TOML: {e}"))
            .into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&state.stores.user_macros) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}"))
            .into_response();
    }
    let path = state.stores.user_macros.join(format!("{name}.toml"));
    if let Err(e) = std::fs::write(&path, body) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}"))
            .into_response();
    }
    (StatusCode::OK, format!("saved {}", path.display())).into_response()
}

pub async fn delete_macro(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    let path = state.stores.user_macros.join(format!("{name}.toml"));
    if !path.exists() {
        return (StatusCode::NOT_FOUND, "no user macro by that name").into_response();
    }
    match std::fs::remove_file(&path) {
        Ok(()) => (StatusCode::OK, "deleted").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("delete: {e}"))
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
pub struct RunBody {
    #[serde(default)]
    pub params: HashMap<String, String>,
}

pub async fn run_macro(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RunBody>>,
) -> Json<Value> {
    let params = body.map(|Json(b)| b.params).unwrap_or_default();
    run(&state, &name, &params).await
}

// ── Prompt handlers ─────────────────────────────────────────────────────

pub async fn list_prompts(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "prompts": state.stores.list_prompts() }))
}

pub async fn get_prompt(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    let Some(path) = state.stores.find_prompt_path(&name) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match std::fs::read_to_string(&path) {
        Ok(t) => ([("content-type", "text/plain; charset=utf-8")], t).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("read: {e}")).into_response(),
    }
}

pub async fn put_prompt(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: String,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&state.stores.user_prompts) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}"))
            .into_response();
    }
    let path = state.stores.user_prompts.join(format!("{name}.md"));
    if let Err(e) = std::fs::write(&path, body) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}"))
            .into_response();
    }
    (StatusCode::OK, format!("saved {}", path.display())).into_response()
}

pub async fn delete_prompt(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !safe_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid name").into_response();
    }
    for ext in ["md", "txt"] {
        let p = state.stores.user_prompts.join(format!("{name}.{ext}"));
        if p.exists() {
            return match std::fs::remove_file(&p) {
                Ok(()) => (StatusCode::OK, "deleted").into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("delete: {e}"))
                    .into_response(),
            };
        }
    }
    (StatusCode::NOT_FOUND, "no user prompt by that name").into_response()
}
