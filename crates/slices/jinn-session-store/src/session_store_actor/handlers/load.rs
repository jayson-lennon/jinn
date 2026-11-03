//! Session loading, restoration, tree hydration, and forking.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jinn_chat_log_view::kernel_element::layout_supervisor::{
    LAYOUT_DEADLINE, LAYOUT_SUPERVISOR_PATH,
};
use jinn_chat_log_view_msg::{ArmLayoutDeadline, DEFAULT_MIN_COLLAPSE_COUNT, LayoutChatSession};
use jinn_core_types::{ChatEntry, ChatEntryId, SessionId};
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::protocol::system::ActiveSessionChanged;
use jinn_session_state::SessionStoreService;
use jinn_session_state::{ChatSessionState, SessionSnapshot, snapshot_frozen_node_from_snapshot};
use jinn_session_store_msg::SessionForkRequested;
use jinn_session_store_msg::{LoadSessionTree, SessionLoadRequested};
use jinn_session_store_msg::{SessionLoadCompleted, SessionState};
use jinn_work_time_msg::RestoreWorkingTime;
use trouper::actor::{ActorPath, MsgHandler};
use trouper::context::MsgCtx;
use trouper::envelope::Address;

/// Default lines before a tool call or result is truncated, matching the
/// chat log renderer's own fallback.
const DEFAULT_TOOL_ENTRY_MAX_LINES: u16 = 6;

use jinn_preferences_config::schemas::ChatLogConfig;

use crate::hydrate::{HydrateCompleted, HydrateSession};
use crate::session_store_actor::SessionStoreActor;
use marked_path::{Absolute, MarkedPathBuf};

impl MsgHandler<HydrateCompleted> for SessionStoreActor {
    async fn handle(&mut self, msg: &HydrateCompleted, ctx: &mut MsgCtx<'_>) {
        self.on_hydrate_completed(msg, ctx).await;
    }
}

impl SessionStoreActor {
    /// Rebuilds a live session whose capture numbering outranks storage's record.
    ///
    /// The store refuses any write whose revision it has already accepted and
    /// keeps that record for the whole process run, but a rebuilt session's
    /// counter starts at zero. Without this floor the first save after a load
    /// is silently skipped and the first archive is rejected — a session that
    /// can be read but never written again.
    async fn restore_seeded(&self, snapshot: SessionSnapshot) -> ChatSessionState {
        let floor = match self
            .services
            .session_store
            .last_accepted_revision(&snapshot.metadata.session_id)
            .await
        {
            Ok(floor) => floor,
            Err(error) => {
                // The floor is a best-effort safety net, not the read the load
                // depends on: a session restored without it behaves exactly as
                // it did before, so a failure here must not block the load.
                tracing::warn!(
                    ?error,
                    session_id = %snapshot.metadata.session_id,
                    "could not read the store's last accepted revision; \
                     restoring with a fresh capture counter"
                );
                jinn_session_state::SessionRevision::new(0)
            }
        };
        snapshot.restore_live_above(floor)
    }

    /// Rewrites a loaded session's persona against the persona catalog.
    ///
    /// A session whose persona was deleted from disk while jinn was closed
    /// comes back carrying the default persona rather than a name nothing
    /// resolves. A catalog that is not there (the persona slice not
    /// activated) degrades to leaving the session unchanged.
    fn normalize_loaded_persona(&self, session: &mut ChatSessionState) {
        let Some(cell) = self
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        else {
            return;
        };
        let normalized = cell.read().normalize_name(
            session.persona_name(),
            jinn_core_types::DEFAULT_PERSONA_NAME,
        );
        session.set_persona_name(normalized);
    }

    /// Inserts a loaded session and returns its ID.
    ///
    /// The stored persona is normalized first — see
    /// [`Self::normalize_loaded_persona`].
    pub(crate) fn insert_loaded_session(&self, mut session: ChatSessionState) -> SessionId {
        self.normalize_loaded_persona(&mut session);
        let session_id = session.session_id().clone();
        self.state.with_session(|view| {
            view.session.map().insert(session);
            view.session.map().remove_frozen_node(&session_id);
        });
        session_id
    }

    /// Applies one finished hydration job.
    ///
    /// This is the body the startup handler used to run inline, one session at a
    /// time, while holding the mailbox. It now runs once per completion, so a
    /// session appears the moment its own read lands rather than after every
    /// other session has been read too.
    pub(crate) async fn on_hydrate_completed(
        &mut self,
        msg: &HydrateCompleted,
        ctx: &mut MsgCtx<'_>,
    ) {
        if msg.frozen {
            self.insert_hydrated_frozen_node(msg);
            self.note_frozen_hydration_completion();
            return;
        }
        if let Some(snapshot) = msg.snapshot.clone() {
            let session_id = self.insert_loaded_session({
                let mut session = self.restore_seeded(snapshot.clone()).await;
                session.mark_interacted();
                session
            });
            // Hand the recorded working intervals to the work-time monitor,
            // which owns them. Publishing rather than writing the cell keeps
            // the monitor the only writer, and carries the open interval a
            // killed session left behind so the monitor can close it at the
            // snapshot's own last-update time rather than at load.
            self.publish(RestoreWorkingTime {
                session_id: session_id.clone(),
                intervals: snapshot.metadata.working_intervals.clone(),
                last_active_at: snapshot.metadata.updated_at,
            })
            .await;
            self.publish(SessionLoadCompleted { session_id }).await;
        }
        if self.note_hydration_completion() {
            // Tree membership is resolved from the live session map, so the
            // frozen sweep has to wait until every unarchived session has
            // landed. The summary read is one query, not a loop of history
            // reads, so it is safe to hold the mailbox for.
            self.hydrate_all_tree_frozen_nodes(ctx).await;
        }
    }

    /// Stores a tree member's frozen snapshot, unless the session is now live.
    ///
    /// A live session supersedes its own frozen node — `insert_loaded_session`
    /// removes it — so re-freezing one here would resurrect a stale tree
    /// snapshot for a session the user can already open.
    fn insert_hydrated_frozen_node(&self, msg: &HydrateCompleted) {
        let Some(snapshot) = msg.snapshot.as_ref() else {
            return;
        };
        let already_live = self.state.read().session.contains(&msg.session_id);
        if already_live {
            tracing::debug!(
                session_id = %msg.session_id,
                "tree member already hydrated as a live session, skipping frozen node"
            );
            return;
        }
        let node = snapshot_frozen_node_from_snapshot(snapshot);
        self.state.with_session(|view| {
            view.session.map().insert_frozen_node(node);
        });
    }

    /// Records one finished frozen tree node read.
    ///
    /// Zero-guarded for the same reason the unarchived counter is: the
    /// load/fork path's single frozen read dispatches no batch, so its
    /// completion arrives with nothing outstanding.
    fn note_frozen_hydration_completion(&mut self) {
        self.pending_frozen_hydrations = self.pending_frozen_hydrations.saturating_sub(1);
    }

    /// Completes initialization of an explicitly loaded session, then publishes its ID.
    ///
    /// The load guard is deliberately *not* cleared here. The chat log's
    /// loading indication is driven by that guard, and clearing it the moment
    /// the session is in memory is what made the next frame run the whole
    /// layout pass — the freeze this hand-off exists to avoid. Instead the
    /// chat log is handed to the layout workers, and the completion actor
    /// clears the guard once the line counts are measured.
    pub(crate) async fn restore_loaded_session(
        &self,
        ctx: &mut MsgCtx<'_>,
        snapshot: SessionSnapshot,
    ) {
        let session_id = snapshot.metadata.session_id.clone();
        // Read out before `restore_seeded` consumes the snapshot: these are
        // both the restore publish's payload and the reactivation save's
        // stamp — see the save below.
        let working_intervals = snapshot.metadata.working_intervals.clone();
        let last_active_at = snapshot.metadata.updated_at;
        let model = if snapshot.metadata.profile.model.is_no_provider() {
            self.state
                .read()
                .frontend
                .app_state
                .last_model
                .clone()
                .unwrap_or_default()
        } else {
            snapshot.metadata.profile.model.clone()
        };
        let mut session = self.restore_seeded(snapshot).await;
        session.set_model(model);
        session.mark_interacted();
        // The snapshot may have been taken while the session was archived. Loading it
        // makes it live again, and the sidebar lists only `Loaded` sessions.
        session.set_session_state(SessionState::Loaded);
        // The persona is normalized before the map insert, exactly as a
        // startup-hydrated session's is — both are load-from-disk
        // insertion points, and neither may resurrect a dead persona name.
        self.normalize_loaded_persona(&mut session);
        let original_cwd = session.cwd().clone();

        // Everything the measurement needs, taken while the session is still
        // owned here. Once it is moved into the map only a borrow of its
        // history is reachable, and a worker thread cannot hold that — so the
        // deep clone has to happen now, or not at all.
        let layout_inputs = self.collect_layout_inputs(&session, &session_id);

        self.state.with_frontend_state(|ops| {
            ops.frontend().update_sections(|sections| {
                sections
                    .sessions
                    .visual_parents
                    .retain(|_id, parent| parent != &session_id);
            });
        });
        self.state.with_session(|view| {
            let map = view.session.map();
            map.remove_frozen_node(&session_id);
            map.insert(session);
            map.set_active(session_id.clone());
        });

        // Dispatched only now that the session is active, so the completion
        // actor's active-session check sees it and the workers measure the
        // session that is actually on screen.
        Self::dispatch_layout(ctx, &session_id, layout_inputs);

        let cwd_exists = tokio::fs::try_exists(&original_cwd).await.unwrap_or(false);
        if !cwd_exists {
            self.restore_missing_cwd(&session_id, &original_cwd);
        }

        // Hand the recorded working intervals to the work-time monitor before
        // the reactivation save below — which would otherwise snapshot the
        // still-empty cell — so the status bar reads the session's recorded
        // time the moment the session is on screen. Publishing rather than
        // writing the cell keeps the monitor the only writer. An interval left
        // open by a killed session closes at the snapshot's own last-update
        // time, so the hours the app spent closed are not billed.
        self.publish(RestoreWorkingTime {
            session_id: session_id.clone(),
            intervals: working_intervals.clone(),
            last_active_at,
        })
        .await;
        self.publish(ActiveSessionChanged {
            session_id: session_id.clone(),
        })
        .await;
        // The stamp override, not the cell read: the restore publish above
        // reaches the monitor asynchronously, so the cell may still be empty
        // here, and a stamp from it would rewrite the stored blob with no
        // intervals — erasing exactly what this load restored.
        self.save_active_session_stamping(&session_id, Some(working_intervals))
            .await;
        self.publish(SessionLoadCompleted { session_id }).await;
    }

    /// Measures an in-memory session's chat log, without reading it again.
    ///
    /// A session switched to from the sidebar is already hydrated, so it needs
    /// only the measurement — not the disk read a [`SessionLoadRequested`]
    /// would perform. The layout hand-off is otherwise identical to a freshly
    /// loaded session's, so it goes through the same dispatch.
    pub(crate) fn on_activate_in_memory(
        &self,
        ctx: &mut MsgCtx<'_>,
        payload: &SessionLoadRequested,
    ) {
        let session_id = payload.session_id.clone();
        // The session is expected to be in the map: the caller asked to activate
        // something it believes is loaded. If it is not — a race with an
        // eviction, say — the load guard this measurement was meant to clear
        // would stay up forever, so it is cleared here rather than left for a
        // worker that will never run.
        //
        // Only the two values the measurement needs are read out. Cloning the
        // session itself would deep-copy its entire history — tens of
        // thousands of entries on a long session — to reach them, and the
        // session stays in the map afterwards regardless.
        // The session's own measurement inputs, read out under a brief lock.
        // The read guard is released by this block ending, which must happen
        // before `clear_load` below — that takes a write lock on the same map.
        let session_inputs = {
            let state = self.state.read();
            state.session.get(&session_id).map(|session| {
                (
                    Arc::from(session.history().to_vec()),
                    session.shown_ignored_blocks_snapshot(),
                )
            })
        };
        let Some((history, shown_ignored_blocks)) = session_inputs else {
            tracing::warn!(
                session_id = %session_id,
                "measure requested for a session that is not in memory"
            );
            self.clear_load(&session_id);
            return;
        };

        // The caller's width, not one re-derived from state: the frontend has
        // already switched to this session, so its own width is the
        // never-rendered zero. Measuring there would publish counts no frame
        // can use and have the completion actor discard them as stale.
        //
        // A caller with no width to offer (Discord, loading a session it has
        // never seen) falls back to the width the chat log would render at.
        let content_width = payload
            .content_width
            .unwrap_or_else(|| self.active_content_width());
        let layout_inputs = self.collect_layout_inputs_at(
            history,
            shown_ignored_blocks,
            &session_id,
            content_width,
        );
        self.state.with_session(|view| {
            view.session.map().set_active(session_id.clone());
        });
        Self::dispatch_layout(ctx, &session_id, layout_inputs);
    }

    /// Hands a session's chat log to the layout workers and arms its deadline.
    ///
    /// The deadline is what stops a pathological job from stranding the user on
    /// a spinner: when it expires the supervisor clears the load guard and the
    /// next frame measures inline instead.
    fn dispatch_layout(
        ctx: &mut MsgCtx<'_>,
        session_id: &SessionId,
        layout_inputs: LayoutChatSession,
    ) {
        // Typed sends: the history travels as a live value and is never
        // serialized, and the route table hands the job to one worker of the
        // pool.
        ctx.send_to_any(LayoutChatSession {
            session_id: session_id.clone(),
            ..layout_inputs
        });
        ctx.send(
            Address::Path(ActorPath::new(LAYOUT_SUPERVISOR_PATH)),
            ArmLayoutDeadline {
                session_id: session_id.clone(),
                after: LAYOUT_DEADLINE,
            },
            None,
        );
    }

    /// Everything the layout workers need to measure a freshly loaded session.
    ///
    /// The content width is the one the chat log last rendered at, which is the
    /// width the next frame will use. Measuring at a guess instead would yield
    /// counts that first frame could not use, throwing the whole measurement
    /// away and leaving the frame to do exactly the work this hand-off exists
    /// to avoid.
    fn collect_layout_inputs(
        &self,
        session: &ChatSessionState,
        session_id: &SessionId,
    ) -> LayoutChatSession {
        let content_width = self.active_content_width();
        // The one unavoidable copy of the history: a worker thread cannot hold
        // a borrow into the session, so the entries are copied out once here
        // and shared with the worker from then on. This session is genuinely
        // owned by the caller and about to be moved into the map, so there is
        // no shorter path.
        //
        // The ignored-block set is read from the incoming session rather than
        // the active one: the session was not active when it was still owned
        // here, and its own view state is the one that will be measured.
        self.collect_layout_inputs_at(
            Arc::from(session.history().to_vec()),
            session.shown_ignored_blocks_snapshot(),
            session_id,
            content_width,
        )
    }

    /// The content width a session would render at, derived from state.
    ///
    /// The fallback for a caller that has no width of its own to offer. It is
    /// read from whichever session is on screen, which is the frame that will
    /// render whatever comes next.
    fn active_content_width(&self) -> u16 {
        let state = self.state.read();
        state
            .session
            .get(state.session.active_session_id())
            .map_or(0, ChatSessionState::content_width)
    }

    /// The same inputs, at a width the caller has already resolved.
    ///
    /// Split out so a caller that knows the width — because it read it before
    /// changing the active session, and can no longer read it after — does not
    /// have to re-derive it from state that has since moved on.
    fn collect_layout_inputs_at(
        &self,
        entries: Arc<[ChatEntry]>,
        shown_ignored_blocks: HashSet<ChatEntryId>,
        session_id: &SessionId,
        content_width: u16,
    ) -> LayoutChatSession {
        // Resolved through the configuration layer, the same handle every
        // other consumer reads: a frame that straddles a `reload` sees the new
        // value rather than a stale copy.
        let preferences = self.services.config.read::<ChatLogConfig>();
        LayoutChatSession {
            session_id: session_id.clone(),
            content_width,
            entries,
            shown_ignored_blocks,
            min_collapse_count: preferences
                .min_collapse_count
                .unwrap_or(DEFAULT_MIN_COLLAPSE_COUNT),
            tool_entry_max_lines: preferences
                .tool_entry_max_lines
                .unwrap_or(DEFAULT_TOOL_ENTRY_MAX_LINES),
        }
    }

    /// Replaces a missing working directory with the application default.
    fn restore_missing_cwd(&self, session_id: &SessionId, original_cwd: &MarkedPathBuf<Absolute>) {
        let default_cwd = self.state.read().session.default_cwd().clone();
        self.state.with_session(|view| {
            let Some(session) = view.session.map().get_mut(session_id) else {
                return;
            };
            session.push_entry(ChatEntry::system(format!(
                "Warning: working directory '{original_cwd}' not found, falling back to '{default_cwd}'"
            )));
            session.set_cwd(default_cwd);
        });
    }

    /// Loads a full session from storage and restores it into active state.
    pub(crate) async fn on_load_requested(
        &self,
        ctx: &mut MsgCtx<'_>,
        payload: &SessionLoadRequested,
    ) {
        // Already in memory: no disk read. It still needs its chat log measured,
        // which is the same hand-off a freshly-loaded session gets — and
        // without it the next frame lays the whole history out inline.
        //
        // Only the store actor can make this call: it is the single writer of
        // the session map, so it is the only place that knows what is loaded.
        if self.state.read().session.get(&payload.session_id).is_some() {
            self.on_activate_in_memory(ctx, payload);
            return;
        }

        let store = self.services.session_store.clone();
        match store.load_session(&payload.session_id).await {
            Ok(Some(session)) => {
                if let Err(error) = store.set_archived(&payload.session_id, false).await {
                    tracing::warn!(?error, "failed to unarchive session on load");
                }
                self.restore_loaded_session(ctx, session).await;
                self.hydrate_tree_frozen_nodes(&store, &payload.session_id)
                    .await;
                // Unarchiving changes what a library member's row shows
                // (archived → loaded), so the section refreshes.
                self.refresh_library_cell().await;
            }
            Ok(None) => {
                tracing::warn!(
                    session_id = ?payload.session_id,
                    "session load returned None"
                );
                self.clear_load(&payload.session_id);
            }
            Err(error) => {
                tracing::warn!(?error, "failed to load session");
                self.clear_load(&payload.session_id);
            }
        }
    }

    /// Loads the named session's whole tree live: ancestors and descendants.
    ///
    /// The picker's plain activation restores one node; this restores the
    /// tree *around* it, so the user can move to its parents and spawned
    /// children without loading each node by hand. Which member the request
    /// names does not matter — the tree is resolved from the store's
    /// summaries, where archived and never-hydrated members are still
    /// visible, and every member missing from the live map is restored as a
    /// background session: marked interacted, forced `Loaded`, unarchived on
    /// disk, its working time handed back to the monitor, and a
    /// [`SessionLoadCompleted`] published per member. Members already live
    /// are skipped untouched — a live session can hold history the store has
    /// not seen, so re-reading one would regress it.
    ///
    /// The named member ends as the active, measured session, whatever the
    /// rest of the tree did: if it was absent it goes through
    /// [`Self::restore_loaded_session`] (active, layout hand-off, events),
    /// and if it was already live a `set_active` re-assertion plus the
    /// normal measurement path covers it — the two arrivals of a
    /// `SessionLoadRequested` racing this command from the same keypress
    /// both converge here.
    ///
    /// A member id missing from the store is skipped: a gap in the store
    /// must not fail the rest of the load.
    pub(crate) async fn on_load_tree_requested(
        &self,
        ctx: &mut MsgCtx<'_>,
        payload: &LoadSessionTree,
    ) {
        let store = self.services.session_store.clone();
        // Resolved from the store, not from memory: the summary is the only
        // place a member that was archived or never hydrated is still named.
        let Some(summary_map) = self.summary_parent_map(&store).await else {
            return;
        };
        let tree_ids = collect_tree_ids(&payload.session_id, &summary_map);

        // Background members first, so the picked member's restore — which
        // reads and writes the active pointer — runs last and wins.
        for member_id in &tree_ids {
            if member_id == &payload.session_id {
                continue;
            }
            // Already live: skip untouched. In-memory history can be newer
            // than the store; this is not an optimization but the point.
            if self.state.read().session.contains(member_id) {
                continue;
            }
            match store.load_session(member_id).await {
                Ok(Some(snapshot)) => {
                    if let Err(error) = store.set_archived(member_id, false).await {
                        tracing::warn!(?error, session_id = %member_id, "failed to unarchive tree member");
                    }
                    self.restore_background_member(snapshot).await;
                }
                Ok(None) => {
                    tracing::debug!(
                        session_id = %member_id,
                        "session in tree not found in store, skipping tree member"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        session_id = %member_id,
                        "failed to load tree member; loading the rest of the tree"
                    );
                }
            }
        }

        // The picked member lands last: active, measured, and on screen.
        if self.state.read().session.get(&payload.session_id).is_some() {
            // Already live — the racing `SessionLoadRequested` (or an earlier
            // load) did the work. Re-assert active in case a background
            // restore moved it, and let the normal measure path run: the
            // guard `activate_session` armed is cleared by the completion
            // actor against whichever measurement lands.
            self.state.with_session(|view| {
                view.session.map().set_active(payload.session_id.clone());
            });
            self.on_activate_in_memory(
                ctx,
                &SessionLoadRequested {
                    session_id: payload.session_id.clone(),
                    content_width: None,
                },
            );
            return;
        }
        match store.load_session(&payload.session_id).await {
            Ok(Some(snapshot)) => {
                if let Err(error) = store.set_archived(&payload.session_id, false).await {
                    tracing::warn!(?error, "failed to unarchive session on tree load");
                }
                self.restore_loaded_session(ctx, snapshot).await;
                self.hydrate_tree_frozen_nodes(&store, &payload.session_id)
                    .await;
            }
            Ok(None) => {
                tracing::warn!(
                    session_id = ?payload.session_id,
                    "tree load named a session absent from the store"
                );
                self.clear_load(&payload.session_id);
            }
            Err(error) => {
                tracing::warn!(?error, "failed to load the picked tree member");
                self.clear_load(&payload.session_id);
            }
        }
    }

    /// Restores one non-picked tree member as a live background session.
    ///
    /// This is the hydrate-completion branch of
    /// [`Self::on_hydrate_completed`] plus the unarchive fixups of
    /// [`Self::restore_loaded_session`], minus everything an *activated*
    /// session gets: no model fallback (the snapshot's own model stands),
    /// no active-pointer move, no layout dispatch (the completion actor
    /// discards results for non-active sessions, so eager measurement would
    /// burn a full-history clone per member for nothing — background
    /// members measure lazily on first activation, exactly like
    /// startup-hydrated sessions), and no cwd replacement.
    async fn restore_background_member(&self, snapshot: SessionSnapshot) {
        let session_id = snapshot.metadata.session_id.clone();
        let working_intervals = snapshot.metadata.working_intervals.clone();
        let updated_at = snapshot.metadata.updated_at;
        let session = {
            let mut session = self.restore_seeded(snapshot).await;
            session.mark_interacted();
            // Loading a member makes it live again, and the sidebar lists
            // only `Loaded` sessions.
            session.set_session_state(SessionState::Loaded);
            session
        };
        self.state.with_frontend_state(|ops| {
            ops.frontend().update_sections(|sections| {
                sections
                    .sessions
                    .visual_parents
                    .retain(|_id, parent| parent != &session_id);
            });
        });
        self.insert_loaded_session(session);
        self.publish(RestoreWorkingTime {
            session_id: session_id.clone(),
            intervals: working_intervals,
            last_active_at: updated_at,
        })
        .await;
        self.publish(SessionLoadCompleted { session_id }).await;
    }

    /// Forks the source and restores the child as the active session.
    ///
    /// The child is derived from the source's *live* history, not from a
    /// second read of its stored snapshot. The source is on screen and in
    /// memory by definition — the user forked from an entry they were looking
    /// at — so an entry it has not written yet is still history the fork owes
    /// the child, and only the live session knows about it.
    ///
    /// The guard is re-pointed at the child before its measurement is
    /// dispatched, and released once the child is live. The chat log's loading
    /// indication is driven by that guard, and a guard naming the source would
    /// name a session nothing measures: the completion actor releases by id, so
    /// it would find nothing to release and the indication would outlive the
    /// fork. Releasing here rather than leaving it to the worker is what makes
    /// the end of a fork independent of a layout worker existing; the next
    /// frame falls back to measuring inline, which the child's inherited
    /// entries make cheap — they carry the source's entry ids and content, so
    /// the line cache the source warmed still answers for them.
    pub(crate) async fn on_session_fork_requested(
        &self,
        ctx: &mut MsgCtx<'_>,
        payload: &SessionForkRequested,
    ) {
        self.state.with_session(|view| {
            if let Some(session) = view.session.map().get_mut(&payload.source_session_id) {
                session.mark_interacted();
            }
        });
        self.save_active_session(&payload.source_session_id).await;

        let child = {
            let Some(source) = self.source_snapshot(&payload.source_session_id) else {
                tracing::warn!(
                    source_session_id = %payload.source_session_id,
                    "cannot fork a session that is not in memory"
                );
                // The guard was armed for the session the user acted on, which
                // is the fork's source — not a child that was never created.
                self.clear_load(&payload.source_session_id);
                return;
            };
            source.forked_from(SessionId::new(), payload.at_ordinal)
        };
        let child_id = child.session_id().clone();

        if let Err(error) = self.services.session_store.save(&child).await {
            tracing::warn!(?error, "failed to persist forked session");
            self.clear_load(&payload.source_session_id);
            return;
        }

        self.begin_load(&child_id);
        self.restore_loaded_session(ctx, child).await;
        self.clear_load(&child_id);
    }

    /// Captures the source's snapshot from the live session map.
    ///
    /// Read under the state's own lock and returned by value: the snapshot
    /// outlives this closure, and it cannot be cloned back out of a session
    /// still borrowed by a caller on another thread.
    fn source_snapshot(&self, source_session_id: &SessionId) -> Option<SessionSnapshot> {
        self.state
            .read()
            .session
            .get(source_session_id)
            .map(ChatSessionState::capture_snapshot)
    }

    /// Arms the loading session's guard.
    ///
    /// One slot, last writer wins — a guard is only ever up for the session the
    /// user is looking at, so re-pointing it at the session a hand-off is
    /// about to measure is what keeps the indication attached to the work.
    fn begin_load(&self, session_id: &SessionId) {
        self.state
            .with_session(|view| view.session.map().begin_load(session_id.clone()));
    }

    /// Releases the loading session's guard, if that session still holds it.
    ///
    /// Addressed by id: the guard is one shared slot, and a failure or
    /// timeout for a session the user has since left must not free the session
    /// that is loading now — that would drop its spinner and send the next
    /// frame back to measuring the whole history inline.
    fn clear_load(&self, session_id: &SessionId) {
        self.state
            .with_session(|view| view.session.map().clear_load_for(session_id));
    }

    /// Hydrates frozen nodes for the loaded session's whole tree.
    pub(crate) async fn hydrate_tree_frozen_nodes(
        &self,
        store: &SessionStoreService,
        loaded_session_id: &SessionId,
    ) {
        let Some(summary_map) = self.summary_parent_map(store).await else {
            return;
        };
        let tree_ids = collect_tree_ids(loaded_session_id, &summary_map);
        let missing = self.missing_tree_members(&tree_ids);
        self.load_frozen_members(store, missing, Some(loaded_session_id))
            .await;
    }

    /// Hydrates frozen nodes for every live session's tree at startup.
    ///
    /// Tree membership is resolved first — one summary query, then in-memory
    /// parent-link walking — and the resulting reads are dispatched to the
    /// worker pool rather than awaited, so the mailbox is free for the rest of
    /// each session's history to land.
    pub(crate) async fn hydrate_all_tree_frozen_nodes(&mut self, ctx: &mut MsgCtx<'_>) {
        let store = self.services.session_store.clone();
        let Some(summary_map) = self.summary_parent_map(&store).await else {
            return;
        };
        let all_tree_ids = self
            .state
            .read()
            .session
            .iter()
            .flat_map(|(id, _)| collect_tree_ids(id, &summary_map))
            .collect::<HashSet<_>>();
        let missing = self.missing_tree_members(&all_tree_ids);
        if missing.is_empty() {
            return;
        }
        tracing::warn!(count = missing.len(), "dispatching frozen tree node loads");
        self.pending_frozen_hydrations = missing.len();
        for session_id in missing {
            ctx.send_to_any(HydrateSession {
                session_id,
                frozen: true,
            });
        }
    }

    /// Loads the parent-link map used to resolve session trees.
    async fn summary_parent_map(
        &self,
        store: &SessionStoreService,
    ) -> Option<HashMap<SessionId, Option<SessionId>>> {
        match store.load_summaries().await {
            Ok(summaries) => Some(
                summaries
                    .into_iter()
                    .map(|summary| (summary.session_id, summary.parent_session))
                    .collect(),
            ),
            Err(error) => {
                tracing::warn!(?error, "failed to load summaries for tree hydration");
                None
            }
        }
    }

    /// Selects tree members that are neither live nor already frozen.
    fn missing_tree_members(&self, tree_ids: &HashSet<SessionId>) -> Vec<SessionId> {
        let state = self.state.read();
        tree_ids
            .iter()
            .filter(|id| {
                !state.session.contains(id) && !state.session.frozen_nodes().contains_key(*id)
            })
            .cloned()
            .collect()
    }

    /// Loads missing full sessions and inserts their frozen snapshots.
    async fn load_frozen_members(
        &self,
        store: &SessionStoreService,
        missing: Vec<SessionId>,
        loaded_session_id: Option<&SessionId>,
    ) {
        if missing.is_empty() {
            return;
        }
        tracing::warn!(
            loaded_session = loaded_session_id.map(ToString::to_string).as_deref(),
            need_frozen = missing.len(),
            "hydrating frozen nodes for tree members"
        );
        let frozen = self.load_frozen_nodes(store, &missing).await;
        if frozen.is_empty() {
            return;
        }
        self.state.with_session(|view| {
            for node in frozen {
                view.session.insert_frozen_node(node);
            }
        });
    }

    /// Loads each member that still exists and converts it to a tree snapshot.
    async fn load_frozen_nodes(
        &self,
        store: &SessionStoreService,
        session_ids: &[SessionId],
    ) -> Vec<jinn_session_store_msg::FrozenTreeNode> {
        let mut nodes = Vec::new();
        for id in session_ids {
            match store.load_session(id).await {
                Ok(Some(snapshot)) => nodes.push(snapshot_frozen_node_from_snapshot(&snapshot)),
                Ok(None) => {
                    tracing::debug!(session_id = %id, "session in tree not found in store, skipping frozen node");
                }
                Err(error) => {
                    tracing::warn!(session_id = %id, ?error, "failed to load session for frozen node");
                }
            }
        }
        nodes
    }
}

/// Walks to the root and returns the complete tree in breadth-first order.
fn collect_tree_ids(
    start: &SessionId,
    summary_map: &HashMap<SessionId, Option<SessionId>>,
) -> HashSet<SessionId> {
    let mut visited = HashSet::new();
    let mut current = start.clone();
    loop {
        if !visited.insert(current.clone()) {
            break;
        }
        let Some(Some(parent)) = summary_map.get(&current) else {
            break;
        };
        if !summary_map.contains_key(parent) {
            break;
        }
        current = parent.clone();
    }

    let mut tree = HashSet::new();
    let mut queue = vec![current];
    while let Some(id) = queue.pop() {
        if !tree.insert(id.clone()) {
            continue;
        }
        let children = summary_map
            .iter()
            .filter(|(child_id, parent_id)| {
                parent_id.as_ref() == Some(&id) && !tree.contains(*child_id)
            })
            .map(|(child_id, _)| child_id.clone());
        queue.extend(children);
    }
    tree
}
