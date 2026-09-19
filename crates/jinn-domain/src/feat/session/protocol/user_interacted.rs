//! Session was interacted with by the user.
//!
//! Emitted by the session-persistence actor after handling
//! [`MarkSessionInteracted`]. Other actors can subscribe to this event
//! to react to session interaction (e.g., enabling persistence-related features).
//!
//! [`MarkSessionInteracted`]: super::mark_session_interacted::MarkSessionInteracted

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

/// Session was interacted with by the user.
///
/// Broadcast after the session's `has_interacted` flag is set to `true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInteracted {
    /// The session that was interacted with.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for UserInteracted {}

jinn_slices::crossing_schema!(UserInteracted, "UserInteracted",
trouper::schema::SchemaKind::Event,
description: "A session recorded its first user interaction.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);
