// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Chat history wrapper - the sanctioned write path lives in the kernel.
//!
//! [`ChatHistory`] wraps a `Vec<ChatEntry>` and provides read access via
//! [`Deref<Target = [ChatEntry]>`](std::ops::Deref). The mutating methods are
//! public but not meant for general use: the sanctioned production write
//! path is the kernel `HistoryEditor` (reachable via
//! `ChatSessionState::edit_history`), and the history field on the session
//! state is feature-visibility restricted so only session code can reach
//! these. External code must use the `PushChatEntry` command.

use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};

use crate::ChatEntry;

/// Wrapper around chat history that restricts entry pushing to the session feature module.
///
/// Provides full read access via `Deref<Target = [ChatEntry]>`. The `push`
/// method is restricted so that only session feature code can add entries.
/// Other mutations (compaction inserts, streaming updates) use `pub(crate)`
/// methods that don't push complete entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatHistory {
    entries: Vec<ChatEntry>,
}

impl ChatHistory {
    /// Create a new empty chat history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a chat history from an existing vector of entries.
    pub fn from_vec(entries: Vec<ChatEntry>) -> Self {
        Self { entries }
    }

    /// Push an entry onto the history.
    ///
    /// Appends an entry. Sanctioned production callers go through the kernel
    /// `HistoryEditor`; external code must use the `PushChatEntry` command.
    pub fn push(&mut self, entry: ChatEntry) {
        self.entries.push(entry);
    }

    /// Insert an entry at a specific position.
    ///
    /// Used by compaction to place entries at boundary positions.
    /// Shifts all entries at or after the insertion point.
    pub fn insert(&mut self, index: usize, entry: ChatEntry) {
        self.entries.insert(index, entry);
    }

    /// Replace the entire history with a new set of entries.
    pub fn replace_all(&mut self, entries: Vec<ChatEntry>) {
        self.entries = entries;
    }

    /// Remove the entry at `index`, shifting subsequent entries down.
    ///
    /// Used by the stall-retry path to discard partial streaming entries.
    /// Panics if `index` is out of bounds.
    pub fn remove(&mut self, index: usize) -> ChatEntry {
        self.entries.remove(index)
    }
}

impl Deref for ChatHistory {
    type Target = [ChatEntry];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for ChatHistory {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

impl From<Vec<ChatEntry>> for ChatHistory {
    fn from(entries: Vec<ChatEntry>) -> Self {
        Self { entries }
    }
}

impl From<ChatHistory> for Vec<ChatEntry> {
    fn from(history: ChatHistory) -> Vec<ChatEntry> {
        history.entries
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    fn from_vec_creates_history() {
        // Given a vector of entries.
        let entries = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi")];

        // When converting to ChatHistory via From.
        let history = ChatHistory::from(entries);

        // Then the history contains the same entries.
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text(), "hello");
        assert_eq!(history[1].text(), "hi");
    }

    #[rstest::rstest]
    fn into_vec_roundtrips() {
        // Given a ChatHistory with entries.
        let mut history = ChatHistory::new();
        history.push(ChatEntry::user("hello"));
        history.push(ChatEntry::assistant("world"));

        // When converting to Vec via Into.
        let entries: Vec<ChatEntry> = history.into();

        // Then the vector contains the same entries.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text(), "hello");
        assert_eq!(entries[1].text(), "world");
    }

    #[rstest::rstest]
    fn from_vec_empty_history() {
        // Given an empty vector.
        let entries: Vec<ChatEntry> = vec![];

        // When converting to ChatHistory.
        let history = ChatHistory::from(entries);

        // Then it is empty.
        assert!(history.is_empty());
    }

    #[rstest::rstest]
    fn into_vec_empty_history() {
        // Given an empty ChatHistory.
        let history = ChatHistory::new();

        // When converting to Vec.
        let entries: Vec<ChatEntry> = history.into();

        // Then the vector is empty.
        assert!(entries.is_empty());
    }
}
