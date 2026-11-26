//! Translating bus events into `ServerMsg`s.
//!
//! Pure functions from an event plus a read of `State` to the frames a
//! page needs. Kept free of actor plumbing so each translation is a plain
//! unit test: give it state, give it an event, assert the frames.
//!
//! Every frame carries data read from the live entry at the moment of the
//! call — never a copy accumulated from the event — so what the page
//! receives is exactly what the session holds.

use jinn_core_types::{ChatEntry, ChatEntryId, ChatEntryKind, SessionId, ToolCallId};
use jinn_kernel::State;
use jinn_webui2_msg::{
    DeltaField, EntryDeltaBody, EntryEnvelope, EntryUpsertBody, PhaseBody, ServerMsg,
};

/// The frame for a phase change.
#[must_use]
pub fn phase(session_id: &SessionId, phase: jinn_slices::PhaseView) -> ServerMsg {
    ServerMsg::Phase(PhaseBody {
        session_id: session_id.clone(),
        phase,
    })
}

/// The session-list frame, re-read from state: both sidebar lists.
#[must_use]
pub fn sessions(state: &State) -> ServerMsg {
    let (rows, library) = crate::snapshot::session_rows(state);
    ServerMsg::Sessions(jinn_webui2_msg::SessionsBody { rows, library })
}

/// The upsert for the entry at a history position.
#[must_use]
pub fn upsert_at(session_id: &SessionId, index: usize, entry: &ChatEntry) -> ServerMsg {
    ServerMsg::EntryUpsert(Box::new(EntryUpsertBody {
        session_id: session_id.clone(),
        index,
        entry: EntryEnvelope::new(entry.clone()),
    }))
}

/// The upsert for the entry with this id, wherever it now sits in history.
///
/// `None` when the session is not in memory or no longer holds the entry
/// (it was removed by a compaction, or the turn belonged to another session).
#[must_use]
pub fn upsert_by_id(
    state: &State,
    session_id: &SessionId,
    entry_id: &ChatEntryId,
) -> Option<ServerMsg> {
    let guard = state.read();
    let session = guard.session.get(session_id)?;
    let index = session.find_entry_index_by_id(entry_id)?;
    let entry = session.history().get(index)?;
    Some(upsert_at(session_id, index, entry))
}

/// The whole-state frame for a page that must switch sessions: a
/// snapshot of the active session, replacing its view wholesale.
#[must_use]
pub fn snapshot(state: &State, revisions: &crate::revision::RevisionTracker) -> ServerMsg {
    ServerMsg::Snapshot(Box::new(crate::snapshot::snapshot(state, revisions)))
}

/// One session's whole history, as (index, entry) pairs.
///
/// `None` when the session is not in memory.
#[must_use]
pub fn history(state: &State, session_id: &SessionId) -> Vec<(usize, ChatEntry)> {
    let guard = state.read();
    guard
        .session
        .get(session_id)
        .map_or_else(Vec::new, |session| {
            session.history().iter().cloned().enumerate().collect()
        })
}

/// A delta appending `chunk` to one field of an entry.
#[must_use]
pub fn delta(
    session_id: &SessionId,
    entry_id: &ChatEntryId,
    field: DeltaField,
    chunk: String,
) -> ServerMsg {
    ServerMsg::EntryDelta(EntryDeltaBody {
        session_id: session_id.clone(),
        entry_id: entry_id.clone(),
        field,
        chunk,
    })
}

/// Finds the `ToolCall` entry for a tool-call id.
#[must_use]
pub fn tool_call_entry(
    state: &State,
    session_id: &SessionId,
    tool_call_id: &ToolCallId,
) -> Option<(usize, ChatEntry)> {
    find_by_tool_call_id(state, session_id, tool_call_id, false)
}

/// Finds the `ToolResult` entry for a tool call id.
#[must_use]
pub fn tool_result_entry(
    state: &State,
    session_id: &SessionId,
    tool_call_id: &ToolCallId,
) -> Option<(usize, ChatEntry)> {
    find_by_tool_call_id(state, session_id, tool_call_id, true)
}

fn find_by_tool_call_id(
    state: &State,
    session_id: &SessionId,
    tool_call_id: &ToolCallId,
    result: bool,
) -> Option<(usize, ChatEntry)> {
    let guard = state.read();
    let session = guard.session.get(session_id)?;
    session.history().iter().enumerate().find_map(|(i, entry)| {
        let matches = match &entry.kind {
            ChatEntryKind::ToolCall { id, .. } => !result && id == tool_call_id,
            ChatEntryKind::ToolResult { id, .. } => result && id == tool_call_id,
            _ => false,
        };
        matches.then(|| (i, entry.clone()))
    })
}
