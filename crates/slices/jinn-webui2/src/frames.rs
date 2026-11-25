//! Translating bus events into `ServerMsg`s.
//!
//! Pure functions from an event plus a read of `State` to the frames a
//! page needs. Kept free of actor plumbing so each translation is a plain
//! unit test: give it state, give it an event, assert the frames.
//!
//! Every frame carries data read from the live entry at the moment of the
//! call — never a copy accumulated from the event — so what the page
//! receives is exactly what the session holds.

use std::collections::HashMap;

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

/// The session-list frame, re-read from state.
#[must_use]
pub fn sessions(state: &State) -> ServerMsg {
    ServerMsg::Sessions(jinn_webui2_msg::SessionsBody {
        rows: crate::snapshot::session_rows(state),
    })
}

/// The upsert for the entry at a history position.
#[must_use]
pub fn upsert_at(session_id: &SessionId, index: usize, entry: &ChatEntry) -> ServerMsg {
    ServerMsg::EntryUpsert(EntryUpsertBody {
        session_id: session_id.clone(),
        index,
        entry: EntryEnvelope::new(entry.clone()),
    })
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

/// Reads the live assistant (or thinking) streaming entry of a session.
///
/// The session creates the streaming entry lazily on the first token and
/// tracks its history position; the text already in it is everything the
/// stream has produced so far.
#[must_use]
pub fn streaming_entry(
    state: &State,
    session_id: &SessionId,
    thinking: bool,
) -> Option<(usize, ChatEntry)> {
    let guard = state.read();
    let session = guard.session.get(session_id)?;
    let index = if thinking {
        session.streaming_thinking_entry_index()?
    } else {
        session.streaming_entry_index()?
    };
    let entry = session.history().get(index)?.clone();
    Some((index, entry))
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

/// Merges queued chunks into one frame per entry and field, in queue order.
///
/// Several tokens for one entry within a tick become a single delta, so a
/// fast stream costs a page one frame per tick rather than one per token.
#[must_use]
pub fn coalesce(pending: Vec<crate::outbound::PendingDelta>) -> Vec<ServerMsg> {
    let mut order: Vec<(SessionId, ChatEntryId, DeltaField)> = Vec::new();
    let mut chunks: HashMap<(SessionId, ChatEntryId, DeltaField), String> = HashMap::new();

    for chunk in pending {
        let key = (chunk.session_id, chunk.entry_id, chunk.field);
        match chunks.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                slot.get_mut().push_str(&chunk.chunk);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                order.push(slot.key().clone());
                slot.insert(chunk.chunk);
            }
        }
    }

    order
        .into_iter()
        .filter_map(|(session_id, entry_id, field)| {
            let chunk = chunks.remove(&(session_id.clone(), entry_id.clone(), field))?;
            Some(delta(&session_id, &entry_id, field, chunk))
        })
        .collect()
}

/// Tracks the live turn's tool-call ids by stream index.
#[derive(Debug, Default)]
pub struct ToolCallIndex {
    by_session: HashMap<SessionId, HashMap<usize, ToolCallId>>,
}

impl ToolCallIndex {
    /// Records that stream `index` carries the tool call `id`.
    pub fn begin(&mut self, session_id: &SessionId, index: usize, id: ToolCallId) {
        self.by_session
            .entry(session_id.clone())
            .or_default()
            .insert(index, id);
    }

    /// The tool-call id a stream index refers to, if one was begun.
    #[must_use]
    pub fn get(&self, session_id: &SessionId, index: usize) -> Option<ToolCallId> {
        self.by_session.get(session_id)?.get(&index).cloned()
    }

    /// Forgets the live turn's tool calls for a session.
    pub fn clear(&mut self, session_id: &SessionId) {
        self.by_session.remove(session_id);
    }
}
