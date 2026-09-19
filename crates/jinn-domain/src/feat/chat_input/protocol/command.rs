//! Commands that mutate the chat input buffer.
//!
//! Insertion, deletion, submission, and clearing of the text
//! the user is composing.

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::ChatEntry;
use crate::protocol::SessionId;

/// Enqueue a user message for processing by the message queue.
///
/// Submitted instead of directly pushing a chat entry when the queue is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueUserMessage {
    /// The session this message belongs to.
    pub session_id: SessionId,
    /// The fully constructed user chat entry (with display/expanded text).
    pub entry: ChatEntry,
}

impl BusMessage for EnqueueUserMessage {}

/// Enqueue a manual resume for a session: re-assemble current history and
/// re-send to the provider. Adds no user message.
///
/// Submitted instead of pushing a fresh user entry when the user wants to
/// resume after an error or after restarting the app mid-stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnqueueResumeTurn {
    /// The session to resume.
    pub session_id: SessionId,
}

impl BusMessage for EnqueueResumeTurn {}

/// Append a fragment to a session's steering buffer.
///
/// Submitted when the user picks STEER mode and the LLM is currently
/// mid-turn (phase != Idle). Fragments accumulate FIFO and are drained
/// into a single `User` entry at the next prompt-assembly boundary.
///
/// If submitted while phase == Idle, the chat-input layer is responsible
/// for routing to [`EnqueueUserMessage`] instead.
///
/// See [`crate::feat::session::steering_buffer::SteeringBuffer`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitSteeringMessage {
    /// The session whose steering buffer to append to.
    pub session_id: SessionId,
    /// The raw user-typed text to buffer.
    pub text: String,
}

impl crate::common::bus::BusMessage for SubmitSteeringMessage {}
