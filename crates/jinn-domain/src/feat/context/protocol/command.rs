//! Command types for context management.

use serde::{Deserialize, Serialize};

use crate::BusMessage;
use crate::protocol::ChatEntryId;
use crate::protocol::PinPosition;
use crate::protocol::SessionId;

/// Pin a chat entry so it survives context management strategies.
///
/// The entry will be positioned according to `position` in the assembled prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinChatEntry {
    /// The session containing the entry.
    pub session_id: SessionId,
    /// The entry to pin.
    pub entry_id: ChatEntryId,
    /// Where the pinned entry should appear in the assembled prompt.
    pub position: PinPosition,
}

impl BusMessage for PinChatEntry {}

/// Remove the pin from a chat entry, allowing normal context management.
///
/// If the entry is not pinned, this is a no-op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpinChatEntry {
    /// The session containing the entry.
    pub session_id: SessionId,
    /// The entry to unpin.
    pub entry_id: ChatEntryId,
}

impl BusMessage for UnpinChatEntry {}

/// Load entries for the persona picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadPersonaPickerEntries;

impl BusMessage for LoadPersonaPickerEntries {}

/// Scan project context files (AGENTS.md/CLAUDE.md) for a specific session.
///
/// The actor reads the session's cwd, walks the bounded ancestor chain
/// (stopping at an exclusive `$HOME` or inclusive VCS root, whichever comes
/// first), reads the first existing candidate per walked dir, and writes the
/// result into that session's ephemeral discovered-context-files set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanContextFiles {
    /// The session whose cwd drives the scan.
    pub session_id: SessionId,
}

impl BusMessage for ScanContextFiles {}

jinn_slices::crossing_schema!(ScanContextFiles, "ScanContextFiles",
trouper::schema::SchemaKind::Command,
description: "Scan project context files for a session.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);
