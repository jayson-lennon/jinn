//! The outbound actor's event handlers.
//!
//! Each `MsgHandler` impl is a one-line delegation to the named handler
//! beside it. The handlers turn one bus event into zero or more frames and
//! push them to every connection through the actor's handle.
//!
//! Streamed text is **not** handled per event. The token event is published
//! before the session actor writes the token into history, so a handler
//! that reads the entry when the event arrives can miss it or read a stale
//! length. The flush tick is the one reader instead: it reads the committed
//! history and emits the suffix of each streaming field the page has not
//! seen — so any number of tokens become one frame per entry per tick, and
//! the frames can never disagree with the history.

use std::collections::HashMap;

use jinn_core_types::{ChatEntry, ChatEntryId, ChatEntryKind, SessionId};
use jinn_webui2_msg::DeltaField;
use trouper::actor::MsgHandler;
use trouper::context::MsgCtx;

use super::OutboundActor;

/// What each entry looked like when it was last sent to pages.
///
/// Keyed by entry id. The length is what a streaming field's suffix is cut
/// against; the signature is the whole serialized entry, compared when a
/// turn settles to find entries that changed while no tick was watching.
#[derive(Debug, Default)]
pub(crate) struct SentLengths {
    lengths: HashMap<ChatEntryId, usize>,
    signatures: HashMap<ChatEntryId, String>,
}

impl SentLengths {
    /// The part of `text` not yet sent for this entry, and remembers it.
    fn suffix(&mut self, entry_id: &ChatEntryId, text: &str) -> String {
        let sent = self.lengths.entry(entry_id.clone()).or_insert(0);
        let fresh = text.get(*sent..).unwrap_or("").to_owned();
        *sent = text.len();
        fresh
    }

    /// Whether the page has been sent anything for this entry yet.
    pub(crate) fn knows(&self, entry_id: &ChatEntryId) -> bool {
        self.lengths.contains_key(entry_id)
    }

    /// Forgets an entry's streaming length: its next text starts over.
    fn forget(&mut self, entry_id: &ChatEntryId) {
        self.lengths.remove(entry_id);
    }

    /// Records every entry as sent exactly as it stands.
    pub(crate) fn remember_all<'a>(&mut self, entries: impl Iterator<Item = &'a ChatEntry>) {
        for entry in entries {
            let _ = self.changed(entry);
        }
    }

    /// Whether `entry` differs from the last one sent for its id, and
    /// remembers it either way.
    fn changed(&mut self, entry: &ChatEntry) -> bool {
        let signature = serde_json::to_string(entry).unwrap_or_default();
        match self.signatures.entry(entry.id.clone()) {
            std::collections::hash_map::Entry::Occupied(slot) if *slot.get() == signature => {
                false
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                slot.insert(signature);
                true
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(signature);
                true
            }
        }
    }
}

impl OutboundActor {
    fn push(&self, frame: jinn_webui2_msg::ServerMsg) {
        self.handle.broadcast(frame);
    }

    /// Sends one entry as an upsert and records it as delivered, so a
    /// later reconcile does not send the same entry again.
    fn push_upsert(&self, session_id: &SessionId, index: usize, entry: &ChatEntry) {
        self.handle.sent.lock().changed(entry);
        self.push(crate::frames::upsert_at(session_id, index, entry));
    }

    /// The tick: one flush.
    pub(super) fn flush_streaming(&mut self) {
        flush(&self.handle);
    }
}

impl MsgHandler<jinn_session_msg::SessionPhaseChanged> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_session_msg::SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.on_phase_changed(msg);
    }
}

impl OutboundActor {
    fn on_phase_changed(&mut self, msg: &jinn_session_msg::SessionPhaseChanged) {
        self.handle.bump(&msg.session_id);
        if msg.new_phase == jinn_slices::PhaseView::Idle {
            self.reconcile(&msg.session_id);
        }
        self.push(crate::frames::phase(&msg.session_id, msg.new_phase));
        self.push(crate::frames::sessions(&self.handle.deps.state));
    }

    /// On a settle or a load, sends every entry the page does not hold.
    fn reconcile(&mut self, session_id: &SessionId) {
        reconcile(&self.handle, session_id);
    }
}

impl MsgHandler<jinn_tools_msg::ToolUseStarted> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_tools_msg::ToolUseStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_use_started(msg);
    }
}

impl OutboundActor {
    fn on_tool_use_started(&mut self, msg: &jinn_tools_msg::ToolUseStarted) {
        self.handle.bump(&msg.session_id);
        let found =
            crate::frames::tool_call_entry(&self.handle.deps.state, &msg.session_id, &msg.id);
        if let Some((index, entry)) = found {
            self.push_upsert(&msg.session_id, index, &entry);
        }
    }
}

impl MsgHandler<jinn_tools_msg::ToolCallReceived> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_tools_msg::ToolCallReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_received(msg);
    }
}

impl OutboundActor {
    fn on_tool_call_received(&mut self, msg: &jinn_tools_msg::ToolCallReceived) {
        self.handle.bump(&msg.session_id);
        let id = msg.tool_call.id.clone();
        let found = crate::frames::tool_call_entry(&self.handle.deps.state, &msg.session_id, &id);
        if let Some((index, entry)) = found {
            self.handle.sent.lock().forget(&entry.id);
            self.push_upsert(&msg.session_id, index, &entry);
        }
    }
}

impl MsgHandler<jinn_tools_msg::ToolExecutionCompleted> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_tools_msg::ToolExecutionCompleted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_tool_execution_completed(msg);
    }
}

impl OutboundActor {
    fn on_tool_execution_completed(&mut self, msg: &jinn_tools_msg::ToolExecutionCompleted) {
        self.handle.bump(&msg.session_id);
        let id = msg.result.tool_call_id.clone();
        let found = crate::frames::tool_result_entry(&self.handle.deps.state, &msg.session_id, &id);
        if let Some((index, entry)) = found {
            self.handle.sent.lock().forget(&entry.id);
            self.push_upsert(&msg.session_id, index, &entry);
        }
    }
}

impl MsgHandler<jinn_session_history_msg::HistoryAppended> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_history_msg::HistoryAppended,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_history_appended(msg);
    }
}

impl OutboundActor {
    fn on_history_appended(&mut self, msg: &jinn_session_history_msg::HistoryAppended) {
        self.handle.bump(&msg.session_id);
        self.push(crate::frames::sessions(&self.handle.deps.state));
    }
}

impl MsgHandler<jinn_chat_input_msg::ChatEntrySubmitted> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_chat_input_msg::ChatEntrySubmitted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_entry_submitted(msg);
    }
}

impl OutboundActor {
    fn on_entry_submitted(&mut self, msg: &jinn_chat_input_msg::ChatEntrySubmitted) {
        self.handle.bump(&msg.session_id);
        let found =
            crate::frames::upsert_by_id(&self.handle.deps.state, &msg.session_id, &msg.entry.id);
        if let Some(frame) = found {
            self.handle.sent.lock().changed(&msg.entry);
            self.push(frame);
        }
    }
}

impl MsgHandler<jinn_provider_selection_msg::ProviderSwitched> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_provider_selection_msg::ProviderSwitched,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_provider_switched(msg);
    }
}

impl OutboundActor {
    fn on_provider_switched(&mut self, msg: &jinn_provider_selection_msg::ProviderSwitched) {
        self.push(jinn_webui2_msg::ServerMsg::Model(
            jinn_webui2_msg::ModelBody {
                session_id: msg.session_id.clone(),
                label: msg.provider_name.clone(),
            },
        ));
    }
}

impl MsgHandler<jinn_session_store_msg::SessionLoadCompleted> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_store_msg::SessionLoadCompleted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_session_load_completed(msg);
    }
}

impl OutboundActor {
    fn on_session_load_completed(&mut self, msg: &jinn_session_store_msg::SessionLoadCompleted) {
        self.handle.bump(&msg.session_id);
        // A load replaces the session's history in memory. The page's copy
        // may now be stale or may be exactly right; reconciling sends what
        // differs and nothing else, where clearing would re-send all of it.
        self.reconcile(&msg.session_id);
        self.push(crate::frames::sessions(&self.handle.deps.state));
    }
}

/// One flush: reads every streaming field and emits its unsent suffix.
///
/// A free function over the handle so the tick's behaviour is driven
/// directly in a test, with nothing spawned: the actor's tick calls it, and
/// so does a test that has put a session into its streaming phase by hand.
pub(crate) fn flush(handle: &super::OutboundHandle) {
    if handle.no_connections() {
        return;
    }
    let growing = growing_fields(handle);
    let mut sent = handle.sent.lock();
    for (session_id, index, entry, field, current) in growing {
        send_suffix(handle, &mut sent, &session_id, index, &entry, field, &current);
    }
}

/// Sends every entry of `session_id` that a page does not hold as it now
/// stands.
///
/// A turn can begin and end between two ticks — an instant reply, a
/// cancelled one — and its entry then settles before any tick read it. The
/// phase change to idle arrives after the history is committed, so
/// comparing every entry against what was last sent catches whatever the
/// ticks missed. An entry the page already holds costs one serialization
/// and no frame.
///
/// A free function over the handle so a test drives the real reconcile.
pub(crate) fn reconcile(handle: &super::OutboundHandle, session_id: &SessionId) {
    let history = crate::frames::history(&handle.deps.state, session_id);
    let mut sent = handle.sent.lock();
    for (index, entry) in history {
        if sent.changed(&entry) {
            handle.broadcast(crate::frames::upsert_at(session_id, index, &entry));
        }
    }
}

/// Emits the unsent suffix of one streaming field — after making sure the
/// page holds the entry it extends: an entry the stream creates lazily must
/// reach the page as an upsert before its first delta.
fn send_suffix(
    handle: &super::OutboundHandle,
    sent: &mut SentLengths,
    session_id: &SessionId,
    index: usize,
    entry: &ChatEntry,
    field: DeltaField,
    current: &str,
) {
    if !sent.knows(&entry.id) {
        handle.broadcast(crate::frames::upsert_at(session_id, index, entry));
    }
    let chunk = sent.suffix(&entry.id, current);
    if chunk.is_empty() {
        return;
    }
    handle.broadcast(crate::frames::delta(session_id, &entry.id, field, chunk));
    sent.changed(entry);
}

/// One read of the state: every streaming field, with its entry.
///
/// The history is read once, under one lock, into a snapshot of the fields
/// that are still growing; the frames are pushed after the lock is released,
/// so a slow connection never holds the state.
fn growing_fields(
    handle: &super::OutboundHandle,
) -> Vec<(SessionId, usize, ChatEntry, DeltaField, String)> {
    let guard = handle.deps.state.read();
    let mut rows = Vec::new();
    for (session_id, session) in guard.session.iter() {
        let history = session.history();
        if let Some(index) = session.streaming_entry_index() {
            push_row(&mut rows, session_id, history, index, DeltaField::Text);
        }
        if let Some(index) = session.streaming_thinking_entry_index() {
            push_row(&mut rows, session_id, history, index, DeltaField::Thinking);
        }
        for entry_id in session.streaming_tool_call_ids() {
            if let Some(index) = history.iter().position(|e| e.id == entry_id) {
                push_row(&mut rows, session_id, history, index, DeltaField::Args);
            }
        }
        for (_id, index, _content) in session.streaming_tool_result_fragments() {
            push_row(&mut rows, session_id, history, index, DeltaField::Output);
        }
    }
    rows
}

/// Adds one row when the entry at `index` is of the kind `field` streams.
fn push_row(
    rows: &mut Vec<(SessionId, usize, ChatEntry, DeltaField, String)>,
    session_id: &SessionId,
    history: &[ChatEntry],
    index: usize,
    field: DeltaField,
) {
    let Some(entry) = history.get(index) else {
        return;
    };
    let text = match (&entry.kind, field) {
        (ChatEntryKind::Assistant(text), DeltaField::Text)
        | (ChatEntryKind::Thinking(text), DeltaField::Thinking) => text.clone(),
        (ChatEntryKind::ToolCall { arguments, .. }, DeltaField::Args) => arguments.clone(),
        (ChatEntryKind::ToolResult { content, .. }, DeltaField::Output) => content.clone(),
        _ => return,
    };
    rows.push((session_id.clone(), index, entry.clone(), field, text));
}
