//! Delivery — the one composition site for an external message.
//!
//! Validates the request against live session state, routes it to the
//! existing chat-input schemas, and publishes. The rules mirror the
//! keyboard-side path exactly:
//!
//! - `queue` → [`EnqueueUserMessage`] (a fully built `ChatEntry::user`).
//! - `steer` on a busy session → [`SubmitSteeringMessage`] (raw text;
//!   fragments accumulate FIFO at the steering buffer).
//! - `steer` on an idle session falls back to queue delivery — the same
//!   rule `tell_session` applies — and the ack reports `fell_back`.
//!
//! Text is delivered verbatim: no prefix, no transformation. An
//! external caller that wants attribution writes it into the message.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_chat_input_msg::SubmitSteeringMessage;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_remote_msg::SendAck;
use jinn_remote_msg::SendMode;
use jinn_remote_msg::SendRequest;
use jinn_remote_msg::SendStatus;
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use wherror::Error;

/// Why a send request was refused.
#[derive(Debug, Error)]
#[error(debug)]
pub enum SendError {
    /// The request's session id is not a UUID.
    BadSessionId,
    /// No live session carries the requested id.
    UnknownSession(SessionId),
    /// The target session is a bot run; external delivery does not fold
    /// text into bot templates.
    BotTarget(SessionId),
    /// The request carried no message text.
    EmptyText,
}

impl SendError {
    /// The HTTP status this refusal maps to.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            // Malformed request bodies are client errors: 400.
            Self::BadSessionId | Self::EmptyText | Self::BotTarget(_) => 400,
            // A well-formed request naming a session that does not
            // exist: 404.
            Self::UnknownSession(_) => 404,
        }
    }

    /// The error sentence the JSON body carries.
    #[must_use]
    pub fn sentence(&self) -> String {
        match self {
            Self::BadSessionId => "session_id is not a valid session id".to_owned(),
            Self::EmptyText => "text must not be empty".to_owned(),
            Self::BotTarget(id) => {
                format!("session {id} is a bot run; external delivery does not target bot sessions")
            }
            Self::UnknownSession(id) => format!("no live session with id {id}"),
        }
    }
}

/// Handles one `POST /send`.
///
/// The validation and routing decision happen under one state write
/// guard: the phase check, the `mark_interacted` write, and the entry
/// construction observe one consistent snapshot, so the queue-or-steer
/// decision cannot race a phase change between check and publish. The
/// guard drops before the publish awaits, so the bus is never reached
/// while state is locked.
///
/// # Errors
///
/// Returns [`SendError`] for every refusal; the caller maps it to the
/// HTTP error response. Publication happens only on `Ok`.
pub async fn handle_send(
    state: &State,
    bus: &BusService,
    request: &SendRequest,
) -> Result<SendAck, SendError> {
    let ack = {
        let session_id =
            SessionId::try_from_string(&request.session_id).ok_or(SendError::BadSessionId)?;
        if request.text.is_empty() {
            return Err(SendError::EmptyText);
        }

        // One write guard covers the routing decision, the interaction
        // mark, and the entry build.
        let mut guard = state.write();
        let Some(session) = guard.session.get_mut(&session_id) else {
            return Err(SendError::UnknownSession(session_id));
        };

        if session.is_bot() {
            return Err(SendError::BotTarget(session_id));
        }

        // A delivered message marks the target interacted — without the
        // mark `is_persistable()` gates every save and the turn would
        // turn but never reach disk (the tell_session invariant).
        session.mark_interacted();

        let mode = request.mode.unwrap_or(SendMode::Queue);
        match mode {
            SendMode::Steer if session.phase() != PhaseView::Idle => {
                let message = SubmitSteeringMessage {
                    session_id,
                    text: request.text.clone(),
                };
                (SendStatus::Steered, false, BusMessage::Steer(message))
            }
            // Queue mode, or a steer aimed at an idle session: the
            // fallback keeps scripts correct regardless of which side
            // observed the phase.
            _ => {
                let message = EnqueueUserMessage {
                    session_id,
                    entry: jinn_core_types::ChatEntry::user(request.text.clone()),
                };
                (
                    SendStatus::Queued,
                    mode == SendMode::Steer,
                    BusMessage::Queue(message),
                )
            }
        }
    };
    let (status, fell_back, delivery) = ack;
    match delivery {
        BusMessage::Steer(message) => bus.publish(message).await,
        BusMessage::Queue(message) => bus.publish(message).await,
    }
    Ok(SendAck {
        status,
        session_id: request.session_id.clone(),
        fell_back,
    })
}

/// The message a send decision settled on, published after the state
/// guard drops. The size skew is deliberate and inert: the enum exists
/// only to carry one message across the guard boundary, and `publish`
/// takes the schema by value, so boxing would break its bounds.
#[expect(
    clippy::large_enum_variant,
    reason = "the enum is a short-lived transport across the guard boundary; publish takes the schema by value"
)]
enum BusMessage {
    Queue(EnqueueUserMessage),
    Steer(SubmitSteeringMessage),
}
