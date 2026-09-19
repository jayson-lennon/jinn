//! Event types for context management.

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Emitted when personas have been scanned and loaded from disk.
///
/// The context actor receives this event and stores the loaded personas
/// in `AppState`. If no active persona is set, the first one becomes default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonasLoaded {
    /// The loaded persona files.
    pub personas: Vec<crate::feat::persona::Persona>,
    /// Error message if scanning failed, `None` on success.
    pub error: Option<String>,
}

impl BusMessage for PersonasLoaded {}

/// Emitted when a chat entry's context override is toggled (e.g. via the `x` keybind).
///
/// The intent handler emits this after toggling an entry's inclusion in
/// the LLM context. The `ContextSizeActor` subscribes to this event to
/// recalculate the context size for the status bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOverrideChanged {
    /// The session whose entry was toggled.
    pub session_id: SessionId,
    /// The entry whose context override changed.
    pub entry_id: crate::protocol::ChatEntryId,
}

impl BusMessage for ContextOverrideChanged {}

jinn_slices::crossing_schema!(ContextOverrideChanged, "ContextOverrideChanged",
trouper::schema::SchemaKind::Event,
description: "A chat entry's context override changed.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,
"entry_id" => trouper::schema::FieldTy::Uuid]);

/// Emitted when project context files (AGENTS.md/CLAUDE.md) have been scanned
/// and loaded for a session.
///
/// The context-files scan actor emits this after walking the bounded ancestor
/// chain for the session's cwd and reading the first existing candidate per dir.
/// Downstream handlers store the result in the session's ephemeral discovered set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFilesLoaded {
    /// The session whose cwd drove the scan.
    pub session_id: SessionId,
    /// The discovered context files (AGENTS.md / CLAUDE.md), ordered
    /// least-local (root-most ancestor) to most-local (cwd).
    pub files: Vec<crate::feat::context::env_context::ContextFile>,
    /// Error message if scanning failed, `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BusMessage for ContextFilesLoaded {}

jinn_slices::crossing_schema!(ContextFilesLoaded, "ContextFilesLoaded",
trouper::schema::SchemaKind::Event,
description: "Project context files scanned and loaded for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "files" => trouper::schema::FieldTy::List(Box::new(trouper::schema::FieldTy::Json)),
]);
