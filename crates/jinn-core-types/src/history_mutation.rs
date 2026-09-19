//! History mutation types - declarative mutations for background workers.
//!
//! Workers produce [`Vec<HistoryMutation>`] batches. The session actor resolves
//! [`ChatEntryId`](crate::ChatEntryId) → current
//! position at application time. Mutations targeting nonexistent entries are
//! silently skipped.

use serde::{Deserialize, Serialize};

use crate::{ChangeSource, ChatEntry, ChatEntryId, ContextOverride, PinPosition};

/// A declarative mutation to apply to a session's history.
///
/// Workers produce `Vec<HistoryMutation>` batches. The session actor
/// resolves `ChatEntryId` → current position at application time.
/// Mutations targeting nonexistent entries are silently skipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "InsertEntry owns a full ChatEntry by design; variants are produced in Vec batches and matched by value"
)]
pub enum HistoryMutation {
    /// Set the context override on an entry (include/exclude from LLM context).
    SetContextOverride {
        entry_id: ChatEntryId,
        value: ContextOverride,
        source: ChangeSource,
    },
    /// Insert a new entry into history after the specified entry.
    /// `after_entry_id: None` means insert at the beginning (index 0).
    InsertEntry {
        after_entry_id: Option<ChatEntryId>,
        entry: ChatEntry,
    },
    /// Pin an entry to a specific position in the assembled prompt.
    PinEntry {
        entry_id: ChatEntryId,
        position: PinPosition,
    },
    /// Remove the pin from an entry.
    UnpinEntry { entry_id: ChatEntryId },
}
