//! Request to load a full session from disk by session ID.
//!
//! Emitted when the user confirms a session selection in the session picker.
//! The persistence actor receives this command, loads the session from
//! SQLite, and sends back a [`SessionLoadCompleted`] command.
//!
//! [`SessionLoadCompleted`]: crate::feat::session::SessionLoadCompleted

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Request to load a full session from disk by session ID.
///
/// Carries the session ID so the actor can load it directly from SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLoadRequested {
    /// The session to load.
    pub session_id: SessionId,
}

impl BusMessage for SessionLoadRequested {}

jinn_slices::crossing_schema!(SessionLoadRequested, "SessionLoadRequested",
trouper::schema::SchemaKind::Command,
description: "Load a session from the store by id.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
