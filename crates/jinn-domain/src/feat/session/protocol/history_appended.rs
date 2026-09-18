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

//! History appended event - emitted when a new entry is added to session history.
//!
//! The compaction actor subscribes to this event to evaluate whether
//! auto-compaction should be triggered. Unlike `StreamCompleted`, which
//! only fires at turn boundaries, `HistoryAppended` fires for every
//! history entry (user message, assistant message, tool call, tool result),
//! enabling mid-turn compaction triggers during agentic tool loops.

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

/// Emitted when a new entry is appended to the session history.
///
/// Carries no token count - the compaction actor reads `context_size()`
/// directly from session state, which uses the tiktoken-based count
/// from the last prompt assembly. This ensures the threshold check
/// and the status bar display use the same value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryAppended {
    /// The session whose history was appended to.
    pub session_id: SessionId,
}

impl crate::common::bus::BusMessage for HistoryAppended {}

jinn_slices::crossing_schema!(HistoryAppended, "HistoryAppended",
trouper::schema::SchemaKind::Event,
description: "A new entry was appended to a session's history.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);
