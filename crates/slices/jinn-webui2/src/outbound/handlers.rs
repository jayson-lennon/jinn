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
use jinn_kernel::State;

/// What each entry looked like when it was last sent to pages.
///
/// Keyed by entry id. The length is what a streaming field's suffix is cut
/// against; the signature is the whole serialized entry, compared when a
/// turn settles to find entries that changed while no tick was watching.
///
/// `held` is the set of entries some frame has carried to pages. The
/// transcript a page receives is a bounded tail plus pages it asks for, so
/// most of a long history was never sent: a reconcile must not push those
/// as "changed". The set only ever grows — a snapshot's tail adds its
/// entries, a page adds its own, and a streamed entry joins when it is
/// first upserted. It is shared by every connection, so "held" means "some
/// page has been sent this", which is the same single-bookkeeping model
/// the lengths and signatures already use.
#[derive(Debug, Default)]
pub(crate) struct SentLengths {
    lengths: HashMap<ChatEntryId, usize>,
    signatures: HashMap<ChatEntryId, String>,
    held: std::collections::HashSet<ChatEntryId>,
    /// Per session, how many leading entries of the history any frame
    /// has carried — the newest held entry's index plus one. A reconcile
    /// treats an unheld entry at or after it as new (an append the ticks
    /// missed), and one before it as history a page never asked for.
    held_through: HashMap<SessionId, usize>,
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

    /// Whether some frame has carried this entry to pages.
    pub(crate) fn held(&self, entry_id: &ChatEntryId) -> bool {
        self.held.contains(entry_id)
    }

    /// Whether `entry` is new to pages: never carried, and appended after
    /// everything any frame carried for its session.
    ///
    /// The second half is what keeps a reconcile from pushing a long
    /// history's older half: those entries were never carried either, but
    /// they lie before the watermark and are reached by paging, not by a
    /// reconcile.
    pub(crate) fn is_new(
        &self,
        session_id: &SessionId,
        history_index: usize,
        entry: &ChatEntry,
    ) -> bool {
        !self.held(&entry.id)
            && history_index >= self.held_through.get(session_id).copied().unwrap_or(0)
    }

    /// Moves the session's watermark forward to just past `entry`, when
    /// that is forward. An older page never moves it.
    fn advance_watermark(&mut self, session_id: &SessionId, history_index: usize) {
        let through = self.held_through.entry(session_id.clone()).or_insert(0);
        *through = (*through).max(history_index + 1);
    }

    /// Records an entry as held, when a frame carried it to pages.
    pub(crate) fn hold(&mut self, entry_id: &ChatEntryId) {
        self.held.insert(entry_id.clone());
    }

    /// Records that the entry at `history_index` of `session_id` reached
    /// pages exactly as it stands, and moves the watermark past it when
    /// that is forward.
    pub(crate) fn remember_at(
        &mut self,
        session_id: &SessionId,
        history_index: usize,
        entry: &ChatEntry,
    ) {
        self.hold(&entry.id);
        self.advance_watermark(session_id, history_index);
        let _ = self.changed(entry);
    }

    /// Whether `entry` differs from the last one sent for its id, and
    /// remembers it either way.
    fn changed(&mut self, entry: &ChatEntry) -> bool {
        let signature = serde_json::to_string(entry).unwrap_or_default();
        match self.signatures.entry(entry.id.clone()) {
            std::collections::hash_map::Entry::Occupied(slot) if *slot.get() == signature => false,
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
    fn push(&self, frame: &jinn_webui2_msg::ServerMsg) {
        self.handle.broadcast(frame);
    }

    /// Sends one entry as an upsert and records it as delivered, so a
    /// later reconcile does not send the same entry again.
    fn push_upsert(&self, session_id: &SessionId, index: usize, entry: &ChatEntry) {
        let mut sent = self.handle.sent.lock();
        sent.remember_at(session_id, index, entry);
        drop(sent);
        self.push(&crate::frames::upsert_at(session_id, index, entry));
    }

    /// The tick: one flush.
    pub(super) fn flush_streaming(&mut self) {
        flush(&self.handle);
    }
}

impl MsgHandler<jinn_session_msg::SessionPhaseChanged> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_session_msg::SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        phase_changed(&self.handle, msg);
    }
}

/// Turns one `SessionPhaseChanged` into the frames a page needs.
///
/// A landing on Idle is the end of a turn — the one moment that asks for
/// the user's attention — so it is recorded first, and a subagent session
/// is not: its rows are children of a session the user is already
/// watching. The record runs before the session list is built so the
/// frame that announces the settle carries the time it happened.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn phase_changed(
    handle: &super::OutboundHandle,
    msg: &jinn_session_msg::SessionPhaseChanged,
) {
    handle.bump(&msg.session_id);
    if msg.new_phase == jinn_slices::PhaseView::Idle {
        if !is_subagent(&handle.deps.state, &msg.session_id) {
            handle.settles().record(&msg.session_id, msg.at);
        }
        reconcile(handle, &msg.session_id);
    }
    let queue_len = live_queue_len(&handle.deps.state, &msg.session_id);
    handle.broadcast(&crate::frames::phase(
        &msg.session_id,
        msg.new_phase,
        queue_len,
    ));
    handle.broadcast(&crate::frames::sessions(
        &handle.deps.state,
        handle.settles(),
        &handle.tearing_down(),
    ));
}

/// Whether the session was spawned as a child by another session.
fn is_subagent(state: &jinn_kernel::State, session_id: &SessionId) -> bool {
    let guard = state.read();
    guard
        .session
        .get(session_id)
        .is_some_and(|session| session.origin() == jinn_session_msg::SessionOrigin::Subagent)
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
    /// On any history append, sends every entry the page does not hold.
    ///
    /// `HistoryAppended` is the one event every committed entry gets, from
    /// fourteen sites. Most of them are followed by another event that
    /// carries the entry itself — a dispatch's `ChatEntrySubmitted`, a
    /// phase change — but a few commit and stop: the attachment gate
    /// blocks with a user entry and an error and never dispatches, a
    /// provider error settles the turn without one. Without this the page
    /// holds those entries only after the next unrelated send, and the
    /// error a user should see now shows up a message late. An entry the
    /// page already holds costs one serialization and no frame.
    fn on_history_appended(&mut self, msg: &jinn_session_history_msg::HistoryAppended) {
        self.handle.bump(&msg.session_id);
        reconcile(&self.handle, &msg.session_id);
        self.push(&crate::frames::sessions(
            &self.handle.deps.state,
            self.handle.settles(),
            &self.handle.tearing_down(),
        ));
    }
}

impl MsgHandler<jinn_session_history_msg::ChatEntryPinChanged> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_history_msg::ChatEntryPinChanged,
        _ctx: &mut MsgCtx<'_>,
    ) {
        pin_changed(&self.handle, msg);
    }
}

/// On a pin or unpin, sends every entry the page holds differently.
///
/// A pin rewrites `pin_position` in place — nothing is appended — so the
/// event carries only the session, and the reconcile is what finds the
/// changed entries: each one's serialized form no longer matches what the
/// page was last sent. A pin takes a whole tool loop, so one gesture
/// commonly upserts several entries at once.
pub(crate) fn pin_changed(
    handle: &super::OutboundHandle,
    msg: &jinn_session_history_msg::ChatEntryPinChanged,
) {
    handle.bump(&msg.session_id);
    reconcile(handle, &msg.session_id);
}

impl MsgHandler<jinn_context_assembly_msg::ContextOverrideChanged> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_context_assembly_msg::ContextOverrideChanged,
        _ctx: &mut MsgCtx<'_>,
    ) {
        context_override_changed(&self.handle, msg);
    }
}

/// On a context override change, sends every entry the page holds
/// differently.
///
/// A toggle rewrites `context_override` in place — nothing is appended —
/// and it takes a whole tool loop, while the event names only the entry
/// the user acted on. So the event carries the session and one id, and the
/// reconcile is what finds every changed member: each one's serialized
/// form no longer matches what the page was last sent. Without this a
/// toggle made in the terminal would reach the page only at the next
/// turn's settle.
pub(crate) fn context_override_changed(
    handle: &super::OutboundHandle,
    msg: &jinn_context_assembly_msg::ContextOverrideChanged,
) {
    handle.bump(&msg.session_id);
    reconcile(handle, &msg.session_id);
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
            let mut sent = self.handle.sent.lock();
            sent.hold(&msg.entry.id);
            sent.changed(&msg.entry);
            drop(sent);
            self.push(&frame);
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
        provider_switched(&self.handle, msg);
    }
}

/// Turns one provider switch into the frames a page needs: the new model's
/// label, and the context figure recomputed against the new model's window.
///
/// The figure's own push is not left to the tick: the label changes this
/// instant, so the figure beside it changes this instant too.
pub(crate) fn provider_switched(
    handle: &super::OutboundHandle,
    msg: &jinn_provider_selection_msg::ProviderSwitched,
) {
    handle.broadcast(&jinn_webui2_msg::ServerMsg::Model(
        jinn_webui2_msg::ModelBody {
            session_id: msg.session_id.clone(),
            label: msg.provider_name.clone(),
        },
    ));
    push_status(handle);
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
        reconcile(&self.handle, &msg.session_id);
        self.push(&crate::frames::sessions(
            &self.handle.deps.state,
            self.handle.settles(),
            &self.handle.tearing_down(),
        ));
    }
}

impl MsgHandler<jinn_skills_msg::SkillsLoaded> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_skills_msg::SkillsLoaded, _ctx: &mut MsgCtx<'_>) {
        skills_loaded(&self.handle, msg);
    }
}

/// Turns one `SkillsLoaded` event into the frame a page needs.
///
/// Discovery wrote the session's discovered set before it published the
/// event, so rows read now are the fresh ones. An open modal receives
/// them without asking: discovery lands asynchronously, and can finish
/// after the modal is already open. The rows answer the filter that
/// session's modal last asked for, so they replace the view the page is
/// holding.
///
/// A failed scan publishes the same event with an empty set; the frame
/// then carries no rows, which is a legal render.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn skills_loaded(handle: &super::OutboundHandle, msg: &jinn_skills_msg::SkillsLoaded) {
    let filter = handle.last_filter(&msg.session_id, super::RowsKind::Skills);
    let rows = {
        let guard = handle.deps.state.read();
        guard
            .session
            .get(&msg.session_id)
            .map_or_else(Vec::new, |session| {
                crate::actions::skill_row_bodies(session, &filter)
            })
    };
    handle.broadcast(&jinn_webui2_msg::ServerMsg::SkillRows(
        jinn_webui2_msg::SkillRowsBody {
            session_id: msg.session_id.clone(),
            filter,
            rows,
        },
    ));
}

impl MsgHandler<jinn_provider_selection_msg::ModelCacheStored> for OutboundActor {
    async fn handle(
        &mut self,
        _msg: &jinn_provider_selection_msg::ModelCacheStored,
        _ctx: &mut MsgCtx<'_>,
    ) {
        model_cache_stored(&self.handle);
    }
}

/// Turns one `ModelCacheStored` into the frames the page needs.
///
/// The event carries no session and no rows — it is only the "the cache was
/// written" fact, emitted strictly after the write. Two things follow it:
///
/// - Every session whose model picker last asked for rows gets a fresh
///   frame, built from the cell now, in the filter that picker is holding.
///   The frame is what tells the page its refresh is done.
/// - Every loaded session is reconciled. The provider actor writes the
///   refresh's summary table into history as a transient entry and publishes
///   no history event for it — the terminal re-reads history on every tick,
///   so it shows; a page only shows what it is told. Without this the table
///   the TUI shows would never reach `/web2`.
pub(crate) fn model_cache_stored(handle: &super::OutboundHandle) {
    for (session_id, filter) in handle.filters_for_kind(super::RowsKind::Model) {
        handle.broadcast(&jinn_webui2_msg::ServerMsg::ModelRows(
            jinn_webui2_msg::ModelRowsBody {
                session_id,
                rows: crate::actions::model_row_bodies(handle, &filter),
                filter,
            },
        ));
    }
    for session_id in loaded_session_ids(&handle.deps.state) {
        reconcile(handle, &session_id);
    }
}

/// Every session currently in memory, in the map's order.
fn loaded_session_ids(state: &State) -> Vec<SessionId> {
    state
        .read()
        .session
        .iter()
        .map(|(id, _)| id.clone())
        .collect()
}

impl MsgHandler<jinn_session_store_msg::SessionLibraryMembershipChanged> for OutboundActor {
    async fn handle(
        &mut self,
        _msg: &jinn_session_store_msg::SessionLibraryMembershipChanged,
        _ctx: &mut MsgCtx<'_>,
    ) {
        library_membership_changed(&self.handle);
    }
}

/// Turns one `SessionLibraryMembershipChanged` into the frame both sidebar
/// lists need: the loaded non-members and the loaded members, re-read from
/// live state now that the store actor has mirrored the flag into the
/// in-memory session.
pub(crate) fn library_membership_changed(handle: &super::OutboundHandle) {
    handle.broadcast(&crate::frames::sessions(
        &handle.deps.state,
        handle.settles(),
        &handle.tearing_down(),
    ));
}

impl MsgHandler<jinn_session_msg::SessionArchived> for OutboundActor {
    async fn handle(&mut self, _msg: &jinn_session_msg::SessionArchived, _ctx: &mut MsgCtx<'_>) {
        sessions_changed(&self.handle);
    }
}

/// Turns one `SessionArchived` into the frame the rail needs: both sidebar
/// lists re-read from live state now that the store actor has removed the
/// session. The page's move to the successor, when the archived session was
/// the one in view, is `follow_active_session`'s job.
pub(crate) fn sessions_changed(handle: &super::OutboundHandle) {
    handle.broadcast(&crate::frames::sessions(
        &handle.deps.state,
        handle.settles(),
        &handle.tearing_down(),
    ));
}

impl MsgHandler<jinn_session_store_msg::StartupHydrationFinished> for OutboundActor {
    async fn handle(
        &mut self,
        _msg: &jinn_session_store_msg::StartupHydrationFinished,
        _ctx: &mut MsgCtx<'_>,
    ) {
        startup_hydration_finished(&self.handle);
    }
}

/// Turns the end of startup hydration into the frame the rail needs: both
/// sidebar lists re-read from live state, which now carries every landed
/// session and the hydration flag down.
///
/// The store publishes the event strictly after clearing that flag, so the
/// state read here is the final one. The event itself carries nothing — it
/// is the signal, and the lists are the fact. No revision is bumped: no
/// history changed. No snapshot is sent: the viewed session did not move.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn startup_hydration_finished(handle: &super::OutboundHandle) {
    handle.broadcast(&crate::frames::sessions(
        &handle.deps.state,
        handle.settles(),
        &handle.tearing_down(),
    ));
}

impl MsgHandler<jinn_session_msg::SessionArchiveFailed> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_msg::SessionArchiveFailed,
        _ctx: &mut MsgCtx<'_>,
    ) {
        archive_failed(&self.handle, msg);
    }
}

/// Turns one `SessionArchiveFailed` into the frame that tells every page
/// why the row they tried to archive is still there.
pub(crate) fn archive_failed(
    handle: &super::OutboundHandle,
    msg: &jinn_session_msg::SessionArchiveFailed,
) {
    handle.broadcast(&jinn_webui2_msg::ServerMsg::Error(
        jinn_webui2_msg::ErrorBody {
            message: format!(
                "could not archive session {}: {}",
                msg.session_id, msg.error
            ),
        },
    ));
}

impl MsgHandler<jinn_session_lifecycle_msg::SessionTeardownStarted> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_lifecycle_msg::SessionTeardownStarted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        teardown_started(&self.handle, msg);
    }
}

/// Turns one `SessionTeardownStarted` into the frame the rail needs: the
/// row's busy dot lights while the teardown command runs.
///
/// A running teardown does not change the session's phase, so `is_busy`
/// stays false and the dot needs this flag of its own. The id joins the
/// handle's tearing-down set first, then the session list is pushed — so a
/// frame built at any later instant, including a fresh page's snapshot,
/// carries the flag.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn teardown_started(
    handle: &super::OutboundHandle,
    msg: &jinn_session_lifecycle_msg::SessionTeardownStarted,
) {
    handle.mark_tearing_down(&msg.session_id);
    sessions_changed(handle);
}

impl MsgHandler<jinn_session_lifecycle_msg::SessionTeardownFinished> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_lifecycle_msg::SessionTeardownFinished,
        _ctx: &mut MsgCtx<'_>,
    ) {
        teardown_finished(&self.handle, msg);
    }
}

/// Turns one `SessionTeardownFinished` into the frame the rail needs: the
/// busy dot drops.
///
/// The lifecycle actor publishes this on every outcome — success, failure,
/// a refusal before anything spawned, a cancel — so the id is removed
/// whether or not it was ever marked. A successful teardown-and-archive
/// clears the dot here a moment before the archive removes the row; that
/// is the actor's own publish order.
pub(crate) fn teardown_finished(
    handle: &super::OutboundHandle,
    msg: &jinn_session_lifecycle_msg::SessionTeardownFinished,
) {
    handle.clear_tearing_down(&msg.session_id);
    sessions_changed(handle);
}

impl MsgHandler<jinn_kernel::protocol::system::ActiveSessionChanged> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_kernel::protocol::system::ActiveSessionChanged,
        _ctx: &mut MsgCtx<'_>,
    ) {
        follow_active_session(&self.handle, msg).await;
    }
}

/// Turns one `ActiveSessionChanged` into the frame every page needs to
/// follow the instance's active session: a whole snapshot of that session.
///
/// The snapshot replaces a page's view wholesale — which session it views,
/// its title, model, phase, and transcript from the start — in the one arm
/// that already applies one. It is recorded as delivered exactly as the
/// dispatch's snapshot reply is, so the streaming flush does not re-send
/// what it carried. A page holding an unsent draft declines the frame on
/// its side; the server does not know which pages those are and sends to
/// all.
///
/// The activation may still be crossing the store actor's own turn for a
/// session read from disk, so the pointer is waited for (bounded) first —
/// the same wait a row click's reply takes.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) async fn follow_active_session(
    handle: &super::OutboundHandle,
    msg: &jinn_kernel::protocol::system::ActiveSessionChanged,
) {
    crate::actions::wait_for_activation(&handle.deps.state, &msg.session_id).await;
    let (frame, window) = crate::frames::snapshot(
        &handle.deps.state,
        &handle.deps.revisions,
        handle.settles(),
        &handle.tearing_down(),
    );
    handle.mark_delivered(&msg.session_id, &window);
    handle.broadcast(&frame);
}

impl MsgHandler<jinn_mcp_msg::McpRuntimeStored> for OutboundActor {
    async fn handle(&mut self, _msg: &jinn_mcp_msg::McpRuntimeStored, _ctx: &mut MsgCtx<'_>) {
        mcp_runtime_stored(&self.handle);
    }
}

/// Turns one `McpRuntimeStored` event into the frames a page needs.
///
/// The event carries no session: it is the coordinator's "the runtime cell
/// was written" fact, emitted strictly after the write. Every session whose
/// MCP modal last asked for rows gets a fresh frame, built from the live
/// cell now, in the filter that modal is holding — so an open modal's
/// badges follow the connection without the page asking.
///
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn mcp_runtime_stored(handle: &super::OutboundHandle) {
    for (session_id, filter) in handle.filters_for_kind(super::RowsKind::Mcp) {
        let rows = crate::actions::mcp_row_bodies(handle, &session_id, &filter);
        handle.broadcast(&jinn_webui2_msg::ServerMsg::McpRows(
            jinn_webui2_msg::McpRowsBody {
                session_id,
                filter,
                rows,
            },
        ));
    }
}

impl MsgHandler<jinn_applets_msg::AppletsRescanned> for OutboundActor {
    async fn handle(&mut self, msg: &jinn_applets_msg::AppletsRescanned, _ctx: &mut MsgCtx<'_>) {
        applets_rescanned(&self.handle, &msg.catalog);
    }
}

impl MsgHandler<jinn_applets_msg::AppletsServingChanged> for OutboundActor {
    async fn handle(
        &mut self,
        msg: &jinn_applets_msg::AppletsServingChanged,
        _ctx: &mut MsgCtx<'_>,
    ) {
        applets_serving_changed(&self.handle, msg.serving, &msg.bind);
    }
}

/// Turns one `AppletsRescanned` into the frame an open applets modal needs.
///
/// The list comes from the **event's own catalog**, never the cell. The
/// applets slice's republisher is a second subscriber to this same event and
/// is the one that writes the catalog cell — and delivery to two subscribers
/// is unordered, so a handler that read the cell could run first and push
/// the pre-rescan list. The event is the fact, in order. The serving phase
/// does ride the cell here: a rescan never changes it, so the cell is
/// current by construction.
///
/// With no applets modal open (no filter remembered) nothing is pushed.
/// A free function over the handle so a test drives the real push, with
/// nothing spawned.
pub(crate) fn applets_rescanned(
    handle: &super::OutboundHandle,
    catalog: &jinn_applets_msg::AppletsCatalog,
) {
    let Some(filter) = handle.applet_filter() else {
        return;
    };
    let (serving, bind) = serving_from_cell(handle);
    handle.broadcast(&jinn_webui2_msg::ServerMsg::AppletRows(
        crate::actions::applet_rows_body(catalog, serving, &bind, &filter),
    ));
}

/// Turns one `AppletsServingChanged` into the frame an open applets modal
/// needs: the phase from the **event**, the list from the catalog cell.
///
/// The mirror of [`applets_rescanned`]. The serving fact carries no catalog,
/// and a serving transition never changes the list, so the cell is the right
/// source for it — the same reasoning, read the other way round.
pub(crate) fn applets_serving_changed(handle: &super::OutboundHandle, serving: bool, bind: &str) {
    let Some(filter) = handle.applet_filter() else {
        return;
    };
    let catalog = handle.applets_catalog_cell().read().clone();
    handle.broadcast(&jinn_webui2_msg::ServerMsg::AppletRows(
        crate::actions::applet_rows_body(&catalog, serving, bind, &filter),
    ));
}

/// The applets server's serving phase as the picker cell holds it: the
/// resolved bind while serving, `(false, "")` while offline. The guard is
/// cloned and dropped inside the block.
fn serving_from_cell(handle: &super::OutboundHandle) -> (bool, String) {
    let picker = handle.applets_picker_cell().read();
    picker
        .serving
        .as_ref()
        .map_or((false, String::new()), |address| {
            (true, address.bind.clone())
        })
}

/// The status-bar facts for the active session: its persona and the context
/// figure, as the terminal's own shared function formats it.
fn status_facts(handle: &super::OutboundHandle) -> Option<(SessionId, String, String)> {
    // State guard first, then the provider cell — the order every other
    // path takes them in. Both drop before this returns.
    let guard = handle.state().read();
    let session_id = guard.session.active_session_id().clone();
    let session = guard.session.get(&session_id)?;
    let model_cache = handle.provider_cell().read().model_cache.clone();
    let figure = jinn_status_bar_msg::context_figure(session, model_cache.as_ref());
    Some((
        session_id,
        session.persona_name().to_owned(),
        jinn_status_bar_msg::format_context_figure(&figure),
    ))
}

/// Reads the active session's persona and context figure and pushes one
/// `ContextUsage` frame only when either differs from what pages were last
/// told.
///
/// The first read of a session records a baseline and pushes nothing, the
/// way the queue depth does: the tick is silent until something has actually
/// changed. An idle connection therefore never receives this frame.
pub(crate) fn push_status(handle: &super::OutboundHandle) {
    let Some((session_id, persona, label)) = status_facts(handle) else {
        return;
    };
    let status = (persona, label);
    let mut told = handle.told_status.lock();
    match told.get(&session_id) {
        Some(known) if *known == status => return,
        None => {
            told.insert(session_id, status);
            return;
        }
        Some(_) => {}
    }
    told.insert(session_id.clone(), status.clone());
    drop(told);
    handle.broadcast(&jinn_webui2_msg::ServerMsg::ContextUsage(
        jinn_webui2_msg::ContextUsageBody {
            session_id,
            persona: status.0,
            label: status.1,
        },
    ));
}

/// One flush: reads every streaming field and emits its unsent suffix, and
/// the status-bar facts if they have drifted.
///
/// A free function over the handle so the tick's behaviour is driven
/// directly in a test, with nothing spawned: the actor's tick calls it, and
/// so does a test that has put a session into its streaming phase by hand.
pub(crate) fn flush(handle: &super::OutboundHandle) {
    if handle.no_connections() {
        return;
    }
    push_drifted_queue_depths(handle);
    push_status(handle);
    let growing = growing_fields(handle);
    let mut sent = handle.sent.lock();
    for (session_id, index, entry, field, current) in growing {
        send_suffix(
            handle,
            &mut sent,
            &session_id,
            index,
            &entry,
            field,
            &current,
        );
    }
}

/// The depth of `session_id`'s turn queue, read from committed state.
fn live_queue_len(state: &State, session_id: &SessionId) -> usize {
    state
        .read()
        .session
        .get(session_id)
        .map_or(0, jinn_kernel::ChatSessionState::queue_len)
}

/// Pushes a phase frame for every session whose queue depth pages were
/// last told no longer matches its queue.
///
/// A message queued behind a running turn publishes nothing — not a
/// history event (the entry is not in the transcript yet) and not a phase
/// change — so without this a page would not learn its queue had grown
/// until the turn ended. The tick reads the committed depth, so the value
/// is never the pre-enqueue one a reply racing the session actor would read.
fn push_drifted_queue_depths(handle: &super::OutboundHandle) {
    let depths = {
        let guard = handle.deps.state.read();
        guard
            .session
            .iter()
            .map(|(session_id, session)| (session_id.clone(), session.phase(), session.queue_len()))
            .collect::<Vec<_>>()
    };
    let mut told = handle.told_queue_lens.lock();
    for (session_id, phase, queue_len) in depths {
        match told.get(&session_id) {
            // Unchanged: the page already holds this depth.
            Some(known) if *known == queue_len => continue,
            // The first tick for a session only records its baseline. A
            // page that connects late is sent its snapshot, and a depth
            // frame racing ahead of it would describe a session the page
            // has not been shown yet; the depth rides the snapshot anyway.
            None => {
                told.insert(session_id, queue_len);
                continue;
            }
            Some(_) => {}
        }
        told.insert(session_id.clone(), queue_len);
        handle.broadcast(&crate::frames::phase(&session_id, phase, queue_len));
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
        // An entry no frame ever carried is new only when it was appended
        // after everything carried; history older than the tail a page
        // received is reached by paging, never pushed as "changed".
        let deliverable = sent.held(&entry.id) || sent.is_new(session_id, index, &entry);
        if deliverable && sent.changed(&entry) {
            sent.remember_at(session_id, index, &entry);
            handle.broadcast(&crate::frames::upsert_at(session_id, index, &entry));
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
        sent.hold(&entry.id);
        handle.broadcast(&crate::frames::upsert_at(session_id, index, entry));
    }
    let chunk = sent.suffix(&entry.id, current);
    if chunk.is_empty() {
        return;
    }
    handle.broadcast(&crate::frames::delta(session_id, &entry.id, field, chunk));
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
