//! Lifecycle event structs - completion callbacks for setup/teardown.
//!
//! [`SessionSetupCompleted`] and [`SessionTeardownFinished`] live in
//! `jinn-session-msg` (the crossing-contract crate) and are re-exported
//! here so the kernel path stays stable.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

pub use jinn_session_msg::SessionSetupCompleted;
pub use jinn_session_msg::SessionTeardownFinished;

/// A new chat session was created.
///
/// Emitted by the intent handler when `handle_session_lifecycle_setup()` inserts
/// a new session into the sessions map. Other actors subscribe to this event
/// to run side effects (e.g., lifecycle scripts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreated {
    /// The newly created session's ID.
    pub session_id: SessionId,
}

/// A session's working directory changed.
///
/// Emitted by the session-persistence actor when it applies a `SetSessionCwd`
/// command. The discovery scan actors subscribe to this to re-scan skills,
/// prompts, and context files for the session's new cwd.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCwdChanged {
    /// The session whose cwd changed.
    pub session_id: SessionId,
    /// The new working directory.
    pub cwd: PathBuf,
}

impl crate::common::bus::BusMessage for SessionCwdChanged {}

impl crate::common::bus::BusMessage for SessionCreated {}

jinn_slices::crossing_schema!(SessionCreated, "SessionCreated",
trouper::schema::SchemaKind::Event,
description: "A new chat session was created.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);

jinn_slices::crossing_schema!(SessionCwdChanged, "SessionCwdChanged",
trouper::schema::SchemaKind::Event,
description: "A session's working directory changed.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "cwd" => trouper::schema::FieldTy::Str,
]);
