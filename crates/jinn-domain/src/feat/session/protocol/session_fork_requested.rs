//! Request to fork a session at a specific entry ordinal.
//!
//! Emitted when the user confirms a selection in the fork picker.
//! The persistence actor receives this command, calls `store.fork()`,
//! loads the new session, and sends back a [`SessionLoadCompleted`] command.
//!
//! [`SessionLoadCompleted`]: crate::feat::session::protocol::session_load_completed::SessionLoadCompleted

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Request to fork a session at a specific entry ordinal.
///
/// Creates a new session with all entries from the source session up to and
/// including `at_ordinal`. Entry data is shared (not duplicated) via the
/// SQLite junction table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionForkRequested {
    /// The session to fork from.
    pub source_session_id: SessionId,
    /// Include entries with ordinal <= this value in the forked session.
    pub at_ordinal: usize,
}

impl BusMessage for SessionForkRequested {}

jinn_slices::crossing_schema!(SessionForkRequested, "SessionForkRequested",
trouper::schema::SchemaKind::Command,
description: "Fork a session at an entry ordinal.",
fields: ["source_session_id" => trouper::schema::FieldTy::Uuid,]);
