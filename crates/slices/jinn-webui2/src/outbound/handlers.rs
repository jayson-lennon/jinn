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

/// What each streaming field looked like when it was last sent to pages.
///
/// Keyed by entry id: an entry streams one field at a time.
#[derive(Debug, Default)]
pub(crate) struct SentLengths(pub(crate) HashMap<ChatEntryId, usize>);

impl SentLengths {
    /// The part of `text` not yet sent for this entry, and remembers it.
    fn suffix(&mut self, entry_id: &ChatEntryId, text: &str) -> String {
        let sent = self.0.entry(entry_id.clone()).or_insert(0);
        let fresh = text.get(*sent..).unwrap_or("").to_owned();
        *sent = text.len();
        fresh
    }

    /// Whether the page has been sent anything for this entry yet.
    pub(crate) fn knows(&self, entry_id: &ChatEntryId) -> bool {
        self.0.contains_key(entry_id)
    }

    /// Forgets an entry (it settled).
    fn forget(&mut self, entry_id: &ChatEntryId) {
        self.0.remove(entry_id);
    }

    /// Forgets everything (a session was replaced wholesale).
    fn clear(&mut self) {
        self.0.clear();
    }
}

impl OutboundActor {
    fn push(&self, frame: jinn_webui2_msg::ServerMsg) {
        self.handle.broadcast(frame);
    }

    /// The tick: one flush.
    pub(super) fn flush_streaming(&mut self) {
        flush(&self.handle, &mut self.sent);
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
        self.push(crate::frames::phase(&msg.session_id, msg.new_phase));
        self.push(crate::frames::sessions(&self.handle.deps.state));
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
            self.push(crate::frames::upsert_at(&msg.session_id, index, &entry));
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
            self.sent.forget(&entry.id);
            self.push(crate::frames::upsert_at(&msg.session_id, index, &entry));
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
            self.sent.forget(&entry.id);
            self.push(crate::frames::upsert_at(&msg.session_id, index, &entry));
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
        self.sent.clear();
        self.handle.bump(&msg.session_id);
        self.push(crate::frames::sessions(&self.handle.deps.state));
    }
}

/// One flush: reads every streaming field and emits its unsent suffix.
///
/// A free function over the handle so the tick's behaviour is driven
/// directly in a test, with nothing spawned: the actor's tick calls it, and
/// so does a test that has put a session into its streaming phase by hand.
pub(crate) fn flush(handle: &super::OutboundHandle, sent: &mut SentLengths) {
    if handle.no_connections() {
        return;
    }
    let growing = growing_fields(handle);
    for (session_id, index, entry, field, current) in growing {
        send_suffix(handle, sent, &session_id, index, &entry, field, &current);
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
