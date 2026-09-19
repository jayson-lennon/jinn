//! Archive a session without running teardown.
//!
//! Sent by the intent handler when the user archives a session from the sidebar.
//! The session-persistence actor handles the archive: marks the session as
//! archived in SQLite, removes it from the sessions map, and emits
//! [`SessionArchived`] + [`SessionClosed`].
//!
//! [`SessionArchived`]: super::session_archived::SessionArchived
//! [`SessionClosed`]: super::session_closed::SessionClosed

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Archive a session without running teardown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSession {
    /// The session to archive.
    pub session_id: SessionId,
}

impl BusMessage for ArchiveSession {}

jinn_slices::crossing_schema!(ArchiveSession, "ArchiveSession",
trouper::schema::SchemaKind::Command,
description: "Archive a session without running teardown.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
