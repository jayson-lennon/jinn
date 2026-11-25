//! The outbound actor's event handlers.
//!
//! Each `MsgHandler` impl is a one-line delegation to the named handler
//! beside it. The handlers turn one bus event into zero or more frames and
//! push them to every connection through the actor's handle.
//!
//! A streamed chunk is read as the **suffix** of the live field rather than
//! copied from the event: the session actor has already appended the
//! event's text by the time a handler runs, so comparing what was last sent
//! against what the entry now holds yields exactly the new text — and can
//! never disagree with the history.

use std::collections::HashMap;

use jinn_core_types::{ChatEntryId, ChatEntryKind};
use jinn_webui2_msg::DeltaField;
use trouper::actor::MsgHandler;
use trouper::context::MsgCtx;

use super::{OutboundActor, PendingDelta};

/// What each streaming field looked like when it was last sent to pages.
///
/// Keyed by entry id: an entry streams one field at a time.
#[derive(Debug, Default)]
pub(super) struct SentLengths(HashMap<ChatEntryId, usize>);

impl SentLengths {
    /// The part of `text` not yet sent for this entry, and remembers it.
    fn suffix(&mut self, entry_id: &ChatEntryId, text: &str) -> String {
        let sent = self.0.entry(entry_id.clone()).or_insert(0);
        let fresh = text.get(*sent..).unwrap_or("").to_owned();
        *sent = text.len();
        fresh
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

    /// Queues the not-yet-sent suffix of one streaming field.
    fn queue_suffix(
        &mut self,
        session_id: &jinn_core_types::SessionId,
        entry_id: &ChatEntryId,
        field: DeltaField,
        current: &str,
    ) {
        let chunk = self.sent.suffix(entry_id, current);
        if chunk.is_empty() {
            return;
        }
        self.handle.queue_delta(PendingDelta {
            session_id: session_id.clone(),
            entry_id: entry_id.clone(),
            field,
            chunk,
        });
    }
}

impl MsgHandler<jinn_session_msg::SessionPhaseChanged> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_session_msg::SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.on_phase_changed(msg);
    }
}

impl OutboundActor {
    fn on_phase_changed(&mut self, msg: &jinn_session_msg::SessionPhaseChanged) {
        use jinn_slices::PhaseView;
        self.handle.bump(&msg.session_id);
        if msg.new_phase == PhaseView::Idle {
            self.handle.tool_calls.lock().clear(&msg.session_id);
        }
        self.push(crate::frames::phase(&msg.session_id, msg.new_phase));
        self.push(crate::frames::sessions(&self.handle.deps.state));
    }
}

impl MsgHandler<jinn_inference_msg::StreamToken> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_inference_msg::StreamToken, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_token(msg);
    }
}

impl OutboundActor {
    fn on_stream_token(&mut self, msg: &jinn_inference_msg::StreamToken) {
        let Some((_index, entry)) = crate::frames::streaming_entry(
            &self.handle.deps.state,
            &msg.session_id,
            msg.is_thinking,
        ) else {
            return;
        };
        let field = if msg.is_thinking {
            DeltaField::Thinking
        } else {
            DeltaField::Text
        };
        let current = match &entry.kind {
            ChatEntryKind::Assistant(text) | ChatEntryKind::Thinking(text) => text.clone(),
            _ => return,
        };
        self.queue_suffix(&msg.session_id, &entry.id, field, &current);
    }
}

impl MsgHandler<jinn_tools_msg::ToolUseStarted> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_tools_msg::ToolUseStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_use_started(msg);
    }
}

impl OutboundActor {
    fn on_tool_use_started(&mut self, msg: &jinn_tools_msg::ToolUseStarted) {
        self.handle
            .tool_calls
            .lock()
            .begin(&msg.session_id, msg.index, msg.id.clone());
        self.handle.bump(&msg.session_id);
        let found =
            crate::frames::tool_call_entry(&self.handle.deps.state, &msg.session_id, &msg.id);
        if let Some((index, entry)) = found {
            self.push(crate::frames::upsert_at(&msg.session_id, index, &entry));
        }
    }
}

impl MsgHandler<jinn_tools_msg::ToolCallStreaming> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_tools_msg::ToolCallStreaming, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_streaming(msg);
    }
}

impl OutboundActor {
    fn on_tool_call_streaming(&mut self, msg: &jinn_tools_msg::ToolCallStreaming) {
        let Some(id) = self
            .handle
            .tool_calls
            .lock()
            .get(&msg.session_id, msg.index)
        else {
            return;
        };
        let Some((_index, entry)) =
            crate::frames::tool_call_entry(&self.handle.deps.state, &msg.session_id, &id)
        else {
            return;
        };
        let ChatEntryKind::ToolCall { arguments, .. } = &entry.kind else {
            return;
        };
        self.queue_suffix(&msg.session_id, &entry.id, DeltaField::Args, arguments);
    }
}

impl MsgHandler<jinn_tools_msg::ToolExecutionOutput> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_tools_msg::ToolExecutionOutput, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_output(msg);
    }
}

impl OutboundActor {
    fn on_tool_execution_output(&mut self, msg: &jinn_tools_msg::ToolExecutionOutput) {
        let found = crate::frames::tool_result_entry(
            &self.handle.deps.state,
            &msg.session_id,
            &msg.tool_call_id,
        );
        let Some((_index, entry)) = found else {
            return;
        };
        let ChatEntryKind::ToolResult { content, .. } = &entry.kind else {
            return;
        };
        self.queue_suffix(&msg.session_id, &entry.id, DeltaField::Output, content);
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
