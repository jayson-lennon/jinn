//! Close a session.
//!
//! Sent by the intent handler when closing a session (with or without teardown).
//! The session-persistence actor handles the close: archives the session in SQLite,
//! removes it from the sessions map, creates a new session if the map is empty,
//! switches the active session if needed, and emits [`SessionClosed`].
//!
//! [`SessionClosed`]: super::session_closed::SessionClosed

use serde::{Deserialize, Serialize};

use crate::{BusMessage, protocol::SessionId};

/// Close a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseSession {
    /// The session to close.
    pub session_id: SessionId,
}

impl BusMessage for CloseSession {}

jinn_slices::crossing_schema!(CloseSession, "CloseSession",
trouper::schema::SchemaKind::Command,
description: "Close a session (running teardown when due).",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
