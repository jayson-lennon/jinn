//! Mark a session as having been interacted with by the user.
//!
//! Sent by the intent handler when the user submits a message or slash command.
//! The session-persistence actor handles the command: sets `has_interacted = true`
//! on the session and emits [`UserInteracted`].
//!
//! [`UserInteracted`]: super::user_interacted::UserInteracted

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

/// Mark a session as having been interacted with by the user.
///
/// Once handled, the session becomes eligible for persistence to disk.
/// Sessions that have never received this command are "scratch" sessions
/// that should not be persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkSessionInteracted {
    /// The session the user interacted with.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for MarkSessionInteracted {}

jinn_slices::crossing_schema!(MarkSessionInteracted, "MarkSessionInteracted",
trouper::schema::SchemaKind::Command,
description: "Mark a session as interacted by the user.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
