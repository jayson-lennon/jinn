//! Command types for context management.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::SessionId;

/// Load entries for the persona picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadPersonaPickerEntries;

impl BusMessage for LoadPersonaPickerEntries {}

/// Scan project context files (AGENTS.md/CLAUDE.md) for a specific session.
///
/// Carries the session's cwd: the worker walks the bounded ancestor chain
/// (stopping at an exclusive `$HOME` or inclusive VCS root, whichever comes
/// first), reads the first existing candidate per walked dir, and writes the
/// result into that session's ephemeral discovered-context-files set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanContextFiles {
    /// The session whose scan this is.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    #[serde(default)]
    pub cwd: PathBuf,
}

impl BusMessage for ScanContextFiles {}

jinn_slices::crossing_schema!(ScanContextFiles, "ScanContextFiles",
trouper::schema::SchemaKind::Command,
description: "Scan project context files for a session.",
fields: [
    "session_id" => trouper::schema::FieldTy::Uuid,
    "cwd" => trouper::schema::FieldTy::Str,
]);
