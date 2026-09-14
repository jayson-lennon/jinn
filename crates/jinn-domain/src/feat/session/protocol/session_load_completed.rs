//! Signals that a session has been inserted into state after loading.
//!
//! Emitted by the session-persistence actor after loading a session from disk
//! **and** inserting it into `state.session`. Subscribers can look up the
//! session by ID immediately upon receiving this event.
//!
//! # Guarantees
//!
//! When a subscriber receives this event, `state.session.get(&session_id)`
//! is guaranteed to return `Some(session)`.
//!
//! The full [`ChatSessionState`] is carried as a convenience so the
//! session-actor's `handle_session_load_completed` can perform the heavy
//! restore flow (system message, model, CWD, context size) without an
//! extra state lookup.

use serde::{Deserialize, Serialize};

use crate::feat::session::chat_session::ChatSessionState;
use crate::{common::bus::BusMessage, protocol::SessionId};
/// Emitted after a session has been loaded and inserted into state.
///
/// Carries the fully loaded session. The session-actor's
/// `handle_session_load_completed` uses this to perform the restore flow
/// (system message, model, CWD, context size). External subscribers
/// (token-count actor, sidebar) use `session_id()` to look up the session
/// from state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLoadCompleted {
    /// The fully loaded session from disk.
    pub session: ChatSessionState,
}

impl SessionLoadCompleted {
    /// Returns the session ID.
    pub fn session_id(&self) -> &SessionId {
        self.session.session_id()
    }
}

impl BusMessage for SessionLoadCompleted {}

jinn_slices::crossing_schema!(SessionLoadCompleted, "SessionLoadCompleted",
trouper::schema::SchemaKind::Event,
description: "A session was loaded from disk and inserted into state.",
fields: ["session" => trouper::schema::FieldTy::Json]);
