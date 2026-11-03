//! The HTTP wire payloads of the external control surface.
//!
//! These are the exact JSON shapes `POST /send`, `GET /sessions`, and
//! `GET /health` exchange. They are plain data so both clients — the
//! `jinn remote` CLI and a bare `curl` — can be written against the
//! same types the server deserializes.

use serde::{Deserialize, Serialize};

/// How an external message is delivered to its target session.
///
/// The caller decides explicitly; there is no phase-based auto routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    /// Queue behind any in-flight turn — the ordinary delivery path.
    Queue,
    /// Steer the running turn at its next prompt-assembly boundary.
    /// A steer aimed at an idle session falls back to queue delivery;
    /// the ack reports that with `fell_back: true`.
    Steer,
}

/// What happened to an accepted send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SendStatus {
    /// The text was queued onto the session's message queue.
    Queued,
    /// The text was appended to the session's steering buffer.
    Steered,
}

/// `POST /send` — deliver text into an existing live session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    /// The target session's id, as a UUID string.
    pub session_id: String,
    /// The text to deliver.
    pub text: String,
    /// Queue or steer. Absent reads as [`SendMode::Queue`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SendMode>,
}

/// The `POST /send` acknowledgement: the text was accepted onto the
/// delivery path. This is fire-and-forget — it does not wait for the
/// turn to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendAck {
    /// Whether the text queued or steered.
    pub status: SendStatus,
    /// The session the text was delivered to (echoes the request).
    pub session_id: String,
    /// True when a `steer` request found its target idle and was
    /// delivered as an ordinary queue message instead.
    pub fell_back: bool,
}

/// One live session as `GET /sessions` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The session's id, as a UUID string.
    pub id: String,
    /// The session's title, when one has been set.
    pub title: Option<String>,
    /// The session's phase: `idle`, `streaming`, `working`, or
    /// `cancelling`.
    pub phase: String,
    /// Whether the session is a bot run. Bot targets are refused by
    /// `POST /send` in v1.
    pub bot: bool,
}

/// `GET /sessions` — the live in-memory sessions of this instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsResponse {
    pub sessions: Vec<SessionInfo>,
}

/// `GET /health` — instance identity, so a client can confirm it is
/// talking to the instance it selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// The serving process's pid.
    pub pid: u32,
    /// The serving binary's version.
    pub version: String,
    /// The port the control surface actually bound (resolved from the
    /// configured port 0).
    pub port: u16,
}

/// The error body every non-2xx response carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}
