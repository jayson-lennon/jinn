//! Session closed and removed from the sessions map.
//!
//! Emitted by the session-persistence actor after closing a session (via
//! [`CloseSession`] command). Sidebar actors subscribe to this event to
//! adjust cursor state.
//!
//! [`CloseSession`]: super::close_session::CloseSession

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

/// Session closed and removed from the sessions map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClosed {
    /// The session that was closed.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for SessionClosed {}

jinn_slices::crossing_schema!(SessionClosed, "SessionClosed",
trouper::schema::SchemaKind::Event,
description: "A session was closed and removed from the sessions map.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);
