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

//! Retry a session whose in-flight LLM stream went silent.
//!
//! Emitted by the plugin coordinator when the first-party `stall-watchdog`
//! plugin detects silence on a session's in-flight provider stream (mirrored
//! from the plugin's `RestartStalledStream` wire message). The handler
//! discards any partial streaming entries, pushes a system marker, and
//! re-dispatches the turn — mirroring the server-error retry path. A hung
//! stream is treated identically to a hard provider error.

use serde::{Deserialize, Serialize};

use crate::protocol::SessionId;

/// Command to retry a session whose in-flight stream stalled.
///
/// Published by the plugin coordinator on the `stall-watchdog` plugin's
/// request. The session-actor handler restarts only when the phase is active
/// *and* an LLM stream is genuinely in flight — the session actor arms the
/// `stream_dispatched_at` guard when it receives the generation's own
/// `SendToLlmProvider` dispatch (single write point, so any dispatch path
/// counts) and `StreamCompleted` clears it — making a stale or bogus request
/// a no-op by construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryStalledSession {
    /// The session whose turn has stalled.
    pub session_id: SessionId,
    /// The 1-based restart attempt within the current stall lineage, as
    /// reported by the watchdog plugin. Surfaced in the chat retry marker
    /// so the user can see each attempt.
    pub attempt: u32,
    /// The restart budget the watchdog plugin enforces, rendered in the
    /// chat retry marker as "attempt N of M".
    pub max_restarts: u32,
}

impl crate::common::bus::BusMessage for RetryStalledSession {}

jinn_slices::crossing_schema!(RetryStalledSession, "RetryStalledSession",
trouper::schema::SchemaKind::Command,
description: "Re-dispatch a turn whose stream stalled.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid,]);
