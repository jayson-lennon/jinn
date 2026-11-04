//! The axum control surface — three routes over loopback HTTP.
//!
//! [`router`] builds the application; [`ServerShared`] is the state the
//! handlers reach through axum's `State` extractor: the live session
//! state, the bus, and this instance's identity for `/health`.
//!
//! Delivery logic lives in [`crate::delivery`]; the handlers here are
//! the HTTP layer only — parse the request, call it, map the outcome.

use axum::Json;
use axum::Router;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::routing::get;
use axum::routing::post;
use jinn_kernel::State;
use jinn_remote_msg::ErrorBody;
use jinn_remote_msg::HealthResponse;
use jinn_remote_msg::SendAck;
use jinn_remote_msg::SendRequest;
use jinn_remote_msg::SessionInfo;
use jinn_remote_msg::SessionsResponse;
use jinn_slices::bus::BusService;

/// The state every handler shares.
#[derive(Clone)]
pub struct ServerShared {
    /// The live session registry and this process's app state.
    state: State,
    /// The message fabric, for delivery publications.
    bus: BusService,
    /// This instance's pid (reported by `/health`).
    pid: u32,
    /// The bound port (reported by `/health`).
    port: u16,
    /// The serving binary's version (reported by `/health`).
    version: String,
}

impl ServerShared {
    #[must_use]
    pub fn new(state: State, bus: BusService, pid: u32, port: u16, version: String) -> Self {
        Self {
            state,
            bus,
            pid,
            port,
            version,
        }
    }
}

/// Builds the control surface's routes.
pub fn router(shared: ServerShared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(sessions))
        .route("/send", post(send))
        .with_state(shared)
}

/// `GET /health` — instance identity.
async fn health(AxumState(shared): AxumState<ServerShared>) -> Json<HealthResponse> {
    Json(HealthResponse {
        pid: shared.pid,
        version: shared.version,
        port: shared.port,
    })
}

/// `GET /sessions` — the live in-memory sessions.
async fn sessions(AxumState(shared): AxumState<ServerShared>) -> Json<SessionsResponse> {
    let guard = shared.state.read();
    let sessions = guard
        .session
        .iter()
        .map(|(id, session)| SessionInfo {
            id: id.to_string(),
            title: session.title().map(ToOwned::to_owned),
            phase: session.phase().as_str().to_owned(),
            bot: session.is_bot(),
        })
        .collect();
    Json(SessionsResponse { sessions })
}

/// `POST /send` — deliver text into a live session.
///
/// Body parse failures and domain refusals both answer with the JSON
/// error body; a refusal's status comes from the error's own mapping.
async fn send(
    AxumState(shared): AxumState<ServerShared>,
    request: Result<Json<SendRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<SendAck>, (StatusCode, Json<ErrorBody>)> {
    let Json(request) = request.map_err(|rejection| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: format!("malformed request body: {rejection}"),
            }),
        )
    })?;
    match crate::delivery::handle_send(&shared.state, &shared.bus, &request).await {
        Ok(ack) => Ok(Json(ack)),
        Err(error) => {
            let status =
                StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            Err((
                status,
                Json(ErrorBody {
                    error: error.sentence(),
                }),
            ))
        }
    }
}
