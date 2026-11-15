//! The daemon's loopback HTTP control API.
//!
//! One JSON envelope for everything: `{"ok":true,"data":…}` or
//! `{"ok":false,"error":"…"}` — tool-level failures (unknown tab, stale
//! ref, dead browser) are envelope errors the caller can read, never
//! protocol-level HTTP errors, except a request for a tab that does not
//! exist, which is 404 so a mistyped id is visibly different from an
//! engine failure. Loopback only by construction: the daemon binds
//! `127.0.0.1:0` and nothing else.
//!
//! Endpoints (mirroring the tool surface exactly):
//! `POST /tabs` (open), `GET /tabs` (list), `POST /tabs/{tab}/navigate`
//! | `/snapshot` | `/click` | `/click_at` | `/type` | `/press` | `/evaluate` |
//! `/screenshot` | `/wait`, `DELETE /tabs/{tab}`, `GET /health`,
//! `POST /shutdown`.

use crate::backend::{BackendError, BrowserBackend, WaitCondition};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// The path that flips the daemon's shutdown watch.
pub const SHUTDOWN_PATH: &str = "/shutdown";

/// Shared handler state.
pub struct DaemonState {
    /// The supervised backend — the only Chrome access in the process.
    pub backend: Arc<dyn BrowserBackend>,
    /// The resolved browser executable (reported by `/health`).
    pub executable: std::path::PathBuf,
    /// How the binary was resolved: `config` or `auto-detected`.
    pub binary_source: String,
    /// Epoch millis at startup.
    pub started_at: u64,
    /// Where screenshots are written.
    pub screenshots_dir: std::path::PathBuf,
    /// The shutdown signal `/shutdown` flips.
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

/// The one response envelope.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Envelope {
    /// `{"ok":true,"data":…}`
    Ok { ok: bool, data: serde_json::Value },
    /// `{"ok":false,"error":"…"}`
    Err { ok: bool, error: String },
}

/// Wraps a successful value.
fn ok<T: Serialize>(data: T) -> (StatusCode, Json<Envelope>) {
    (
        StatusCode::OK,
        Json(Envelope::Ok {
            ok: true,
            data: serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }),
    )
}

/// Wraps a backend error as an envelope error. An unknown tab is 404 —
/// a mistyped or vanished id is visibly different from an engine
/// failure.
fn err(error: &BackendError) -> (StatusCode, Json<Envelope>) {
    let status = match error {
        BackendError::UnknownTab(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::OK,
    };
    (
        status,
        Json(Envelope::Err {
            ok: false,
            error: error.to_string(),
        }),
    )
}

/// `POST /tabs` body.
#[derive(Debug, Deserialize)]
pub struct OpenTabRequest {
    /// The URL to open; omitted opens `about:blank`.
    pub url: Option<String>,
}

/// `POST /tabs/{tab}/navigate` body.
#[derive(Debug, Deserialize)]
pub struct NavigateRequest {
    /// The URL to navigate to.
    pub url: String,
}

/// `POST /tabs/{tab}/type` body.
#[derive(Debug, Deserialize)]
pub struct TypeRequest {
    /// The element ref to focus (from the latest snapshot).
    pub reference: String,
    /// The text to type.
    pub text: String,
}

/// `POST /tabs/{tab}/click` body.
#[derive(Debug, Deserialize)]
pub struct ClickRequest {
    /// The element ref to click.
    pub reference: String,
}

/// `POST /tabs/{tab}/click_at` body.
#[derive(Debug, Deserialize)]
pub struct ClickAtRequest {
    /// Viewport CSS-pixel x (clamped to the live viewport).
    pub x: f64,
    /// Viewport CSS-pixel y (clamped to the live viewport).
    pub y: f64,
}

/// `POST /tabs/{tab}/press` body.
#[derive(Debug, Deserialize)]
pub struct PressRequest {
    /// The key (`Enter`, `Tab`, `ArrowDown`, …).
    pub key: String,
}

/// `POST /tabs/{tab}/evaluate` body.
#[derive(Debug, Deserialize)]
pub struct EvaluateRequest {
    /// The script (rides the stealth path only).
    pub script: String,
}

/// `POST /tabs/{tab}/wait` body.
#[derive(Debug, Deserialize)]
pub struct WaitRequest {
    /// The condition: `"navigation"`, `"network_idle"`, `{"text": "…"}`
    /// or `{"ms": n}` — the backend's `WaitCondition` untagged shape.
    pub condition: WaitCondition,
}

/// `GET /health` response body.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// `live` while a browser engine is up.
    pub browser: String,
    /// The resolved browser executable.
    pub binary: String,
    /// `config` or `auto-detected`.
    pub binary_source: String,
    /// Daemon uptime in milliseconds.
    pub uptime_ms: u64,
}

/// Builds the daemon's router.
#[must_use = "the router must be served to handle requests"]
pub fn router(state: DaemonState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/tabs", post(open_tab).get(list_tabs))
        .route("/tabs/{tab}/navigate", post(navigate))
        .route("/tabs/{tab}/snapshot", post(snapshot))
        .route("/tabs/{tab}/click", post(click))
        .route("/tabs/{tab}/click_at", post(click_at))
        .route("/tabs/{tab}/type", post(type_text))
        .route("/tabs/{tab}/press", post(press))
        .route("/tabs/{tab}/evaluate", post(evaluate))
        .route("/tabs/{tab}/screenshot", post(screenshot))
        .route("/tabs/{tab}/wait", post(wait))
        .route("/tabs/{tab}", delete(close_tab))
        .route("/health", get(health))
        .route(SHUTDOWN_PATH, post(shutdown))
        .with_state(state)
}

async fn open_tab(
    State(state): State<Arc<DaemonState>>,
    body: Option<Json<OpenTabRequest>>,
) -> (StatusCode, Json<Envelope>) {
    let url = body.and_then(|Json(req)| req.url);
    match state.backend.open_tab(url.as_deref()).await {
        Ok(tab) => ok(tab),
        Err(e) => err(&e),
    }
}

async fn list_tabs(State(state): State<Arc<DaemonState>>) -> (StatusCode, Json<Envelope>) {
    match state.backend.list_tabs().await {
        Ok(tabs) => ok(tabs),
        Err(e) => err(&e),
    }
}

async fn navigate(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<NavigateRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.navigate(&tab, &req.url).await {
        Ok(info) => ok(info),
        Err(e) => err(&e),
    }
}

async fn snapshot(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.snapshot(&tab).await {
        Ok(snap) => ok(snap),
        Err(e) => err(&e),
    }
}

async fn click(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<ClickRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.click(&tab, &req.reference).await {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn click_at(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<ClickAtRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.click_at(&tab, req.x, req.y).await {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn type_text(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<TypeRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state
        .backend
        .type_text(&tab, &req.reference, &req.text)
        .await
    {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn press(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<PressRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.press_key(&tab, &req.key).await {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn evaluate(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<EvaluateRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.evaluate(&tab, &req.script).await {
        Ok(value) => ok(value),
        Err(e) => err(&e),
    }
}

async fn screenshot(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
) -> (StatusCode, Json<Envelope>) {
    // Save the PNG under the jinn-managed screenshots dir and return the
    // path: jinn's MCP client flattens image blocks to a placeholder, so
    // bytes in the response would never reach the model — the path does,
    // via the agent's image-attachment built-in.
    let bytes = match state.backend.screenshot(&tab).await {
        Ok(bytes) => bytes,
        Err(e) => return err(&e),
    };
    let dir = &state.screenshots_dir;
    if let Err(e) = std::fs::create_dir_all(dir) {
        return (
            StatusCode::OK,
            Json(Envelope::Err {
                ok: false,
                error: format!("cannot create the screenshots dir: {e}"),
            }),
        );
    }
    let file_name = format!("{}-{tab}.png", now_millis());
    let path = dir.join(file_name);
    match std::fs::write(&path, &bytes) {
        Ok(()) => ok(serde_json::json!({"path": path.to_string_lossy()})),
        Err(e) => (
            StatusCode::OK,
            Json(Envelope::Err {
                ok: false,
                error: format!("cannot write the screenshot: {e}"),
            }),
        ),
    }
}

async fn wait(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
    Json(req): Json<WaitRequest>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.wait(&tab, &req.condition).await {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn close_tab(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Path(tab): axum::extract::Path<String>,
) -> (StatusCode, Json<Envelope>) {
    match state.backend.close_tab(&tab).await {
        Ok(()) => ok(serde_json::json!({"done": true})),
        Err(e) => err(&e),
    }
}

async fn health(State(state): State<Arc<DaemonState>>) -> (StatusCode, Json<Envelope>) {
    let browser = if state.backend.is_live().await {
        "live"
    } else {
        "dead"
    };
    ok(HealthResponse {
        browser: browser.to_owned(),
        binary: state.executable.to_string_lossy().into_owned(),
        binary_source: state.binary_source.clone(),
        uptime_ms: now_millis().saturating_sub(state.started_at),
    })
}

async fn shutdown(State(state): State<Arc<DaemonState>>) -> (StatusCode, Json<Envelope>) {
    let _ = state.shutdown.send(true);
    ok(serde_json::json!({"done": true}))
}

/// Epoch millis (shared with the daemon module's clock use).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
