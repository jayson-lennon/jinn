//! The SSE fan-out actor — bus events in, HTML fragments out.
//!
//! Subscribes to the stream-adjacent **Events** (never commands: a
//! command routes to one declarant, an event broadcasts) and pushes
//! rendered fragments onto a [`tokio::sync::broadcast`] channel. Each
//! connected SSE endpoint holds a receiver; connect = subscribe,
//! disconnect = drop. The channel is the whole of the actor's state
//! beyond the per-session token batch buffers — transport state only:
//! it owns nothing authoritative, never replays, and a crash loses
//! nothing. The pull endpoint is the correctness dependency.
//!
//! Token batching: `StreamToken`s arrive per-delta while pages want
//! low-frequency fragments, so tokens accumulate per session in an
//! HTML buffer and flush on a self-addressed 150 ms tick as one `chat`
//! fragment. When a session settles to `Idle` the partial buffer
//! flushes **first**, then the regions' settle fragments follow in
//! registry order — the ordering that keeps the last tokens from
//! appearing twice (once appended, once in the full render) and the
//! convergence push that heals any drift between an incremental page
//! and the real history (rewinds, cancels, partial-entry edge cases).
//! The fan-out names no region: settle pushes resolve through
//! [`crate::region`]'s registry, so a new region contributes pushes by
//! declaring settle kinds, never by editing this module.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_inference_msg::StreamToken;
use jinn_kernel::State;
use jinn_provider_selection_msg::ModelCacheStored;
use jinn_session_msg::SessionOrigin;
use jinn_session_msg::{SessionArchived, SessionPhaseChanged};
use jinn_session_store_msg::{SessionLibraryMembershipChanged, SessionLoadCompleted};
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use jinn_slices::cell::TypedCell;
use jinn_webui_msg::{SseEnvelope, SseKind, UnreadSessionMarks};
use tokio::sync::broadcast;
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The actor's registered path segment.
pub const WEBUI_FANOUT_NAME: &str = "webui-fanout";

/// How long tokens accumulate before one flush, per session.
const FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// The broadcast capacity. A slow client that falls further behind than
/// this skips fragments and re-syncs at the next `transcript-full` —
/// the pull endpoint remains the truth, so a skipped fragment is never
/// a correctness event.
const BROADCAST_CAPACITY: usize = 256;

/// The fan-out's self-addressed flush heartbeat: drains every session's
/// buffered tokens as `chat` fragments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Flush buffered stream-token HTML to connected pages.")]
pub struct FlushTick;

impl jinn_slices::BusMessage for FlushTick {}

/// The fan-out's shared handle: what an SSE endpoint subscribes to.
#[derive(Clone)]
pub struct FanoutHandle {
    tx: Arc<broadcast::Sender<SseEnvelope>>,
}

impl FanoutHandle {
    /// Subscribes a new SSE connection. Dropping the receiver
    /// unsubscribes it.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SseEnvelope> {
        self.tx.subscribe()
    }

    /// Pushes a refreshed sessions-list fragment to every page,
    /// addressed to the named session — from **outside** the actor.
    ///
    /// A synchronous HTTP handler (the rename POST) must push without
    /// publishing anything the fan-out subscribes to: the only command
    /// it could publish, `PersistSession`, routes to the store actor,
    /// so there is no command whose arrival means "refresh the list".
    /// The caller renders the fragment — the **merged** list shape the
    /// GET pull serves, never the actor's live-only refresh — and
    /// hands the rendered markup here; a push whose shape differs from
    /// the surrounding pulls reads as rows vanishing then returning.
    ///
    /// Send failures are the no-receiver case — no page is connected —
    /// and are not errors.
    pub(crate) fn push_sessions_refresh_from(&self, session_id: &SessionId, html: String) {
        let _ = self.tx.send(SseEnvelope {
            kind: SseKind::Sessions,
            session_id: session_id.clone(),
            cursor: 0,
            html,
        });
    }

    /// Broadcasts the post-cache-write models signal from **outside**
    /// the actor — the serve-tier wire test's seam, addressed like the
    /// actor's own push (per live session). The data line carries the
    /// write timestamp: an SSE frame whose data buffer is empty is
    /// discarded by the browser (never dispatched), so the signal must
    /// hold bytes to land.
    #[cfg(test)]
    pub(crate) fn push_models_changed_from(
        &self,
        session_id: &SessionId,
        stored_at: jiff::Timestamp,
    ) {
        let _ = self.tx.send(SseEnvelope {
            kind: SseKind::Models,
            session_id: session_id.clone(),
            cursor: 0,
            html: stored_at.to_string(),
        });
    }

    /// Broadcasts the post-status-write MCP signal from **outside**
    /// the actor — the serve-tier wire test's seam, addressed like the
    /// actor's own push (per live session). The data line carries the
    /// write timestamp: an SSE frame whose data buffer is empty is
    /// discarded by the browser (never dispatched), so the signal must
    /// hold bytes to land.
    #[cfg(test)]
    pub(crate) fn push_mcp_status_from(&self, session_id: &SessionId, stored_at: jiff::Timestamp) {
        let _ = self.tx.send(SseEnvelope {
            kind: SseKind::McpStatus,
            session_id: session_id.clone(),
            cursor: 0,
            html: stored_at.to_string(),
        });
    }
}

/// The fan-out actor: events to fragments.
pub struct WebUiFanoutActor {
    /// The shared application state — fragment renders read it under
    /// one read guard and drop the guard before any await.
    state: State,
    /// The broadcast end every SSE connection subscribes through.
    tx: Arc<broadcast::Sender<SseEnvelope>>,
    /// One partially-accumulated token buffer per session: the HTML
    /// appended since the last flush.
    pending: HashMap<SessionId, String>,
    /// The unread-mark cell's write handle — the fan-out is the sole
    /// writer of the mark (set on an unseen settle, cleared on
    /// activation of the marked session). Resolved once in the webui's
    /// `activate` and handed nowhere else.
    unread_marks: TypedCell<UnreadSessionMarks>,
}

impl ServiceActor for WebUiFanoutActor {
    // Never called: spawned via `init_with`. The `async` signature is
    // the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        Err(error_stack::Report::new(RegistryError::InvalidSpec)
            .attach("WebUiFanoutActor spawns via init_with"))
    }
}

impl WebUiFanoutActor {
    /// Spawns the fan-out at `webui-fanout`.
    ///
    /// The `.handles` list **is** the subscription; an event missing
    /// from it would be dropped silently at runtime, so every event the
    /// `MsgHandler` impls handle must appear here. Events plus the
    /// self-addressed flush tick — never a domain command.
    ///
    /// `unread_marks` is the sole write handle to the unread-mark
    /// cell: minted by the cell catalog, resolved once in the webui's
    /// `activate`, and handed only here. Render paths resolve their own
    /// read handles.
    #[must_use]
    pub fn spawn(
        system: &ActorSystem,
        state: State,
        _bus: BusService,
        unread_marks: TypedCell<UnreadSessionMarks>,
    ) -> FanoutHandle {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        let tx = Arc::new(tx);
        let actor_tx = tx.clone();

        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(WEBUI_FANOUT_NAME))
            .init_with(move || {
                let tx = actor_tx.clone();
                let state = state.clone();
                Box::pin(async move {
                    Ok(Self {
                        state,
                        tx,
                        pending: HashMap::new(),
                        unread_marks: unread_marks.clone(),
                    })
                })
            })
            .handles::<StreamToken>()
            .handles::<SessionPhaseChanged>()
            .handles::<SessionLoadCompleted>()
            .handles::<SessionArchived>()
            .handles::<SessionLibraryMembershipChanged>()
            .handles::<ModelCacheStored>()
            // The MCP runtime cell changed: the coordinator emits this
            // stored event strictly after each status write (never the
            // raw status events — two publishers against one cell
            // writer). A broadcast-legal Event; the handler pushes a
            // signal only, the pull remains the renderer.
            .handles::<jinn_mcp_msg::McpRuntimeStored>()
            // Skills discovery landed (the scan actor emits this after
            // writing the session's ephemeral set). A broadcast-legal
            // Event; the handler pushes the `skillsTick` signal only —
            // open skills modals re-pull their rows from the GET route,
            // which remains the renderer.
            .handles::<jinn_skills_msg::SkillsLoaded>()
            // A browser-created session must reach every connected
            // page's sidebar without a reload (the creation POST's
            // response reaches only the creating page), and the async
            // setup's outcome must reach that creating page's
            // transcript. Both are Events — broadcast is legal for any
            // number of declarants — and neither carries creation
            // logic here: the handlers below render existing region
            // fragments only.
            .handles::<jinn_session_lifecycle_msg::SessionCreated>()
            .handles::<jinn_session_lifecycle_msg::SessionSetupCompleted>()
            // An entry landed in history (idle-direct dispatch, queue
            // drain, or an explicit push): the transcript-full
            // convergence push carries it to the page within one batch
            // window — the visibility path for a message queued while
            // busy, whose optimistic entry the settle's convergence
            // render wiped. A broadcast-legal Event; the handler pushes
            // the region's convergence fragment only.
            .handles::<jinn_chat_input_msg::ChatEntrySubmitted>()
            .handles::<FlushTick>()
            // Arms the first flush ~1 tick after spawn; the chain
            // self-perpetuates from the handler.
            .kickoff(FlushTick, FLUSH_INTERVAL)
            .start();

        FanoutHandle { tx }
    }

    /// Builds an actor over the given state with its own broadcast
    /// channel, returning it and a subscribed receiver. Test-only: the
    /// `MsgHandler` impls delegate to the plain methods these tests
    /// drive, and `MsgCtx` is trouper-internal.
    #[cfg(test)]
    pub(crate) fn test_with_receiver(
        state: State,
        unread_marks: TypedCell<UnreadSessionMarks>,
    ) -> (Self, broadcast::Receiver<SseEnvelope>) {
        let (tx, rx) = broadcast::channel(BROADCAST_CAPACITY);
        (
            Self {
                state,
                tx: Arc::new(tx),
                pending: HashMap::new(),
                unread_marks,
            },
            rx,
        )
    }

    /// The shared handle over the actor's own broadcast channel.
    /// Test-only: production builds the handle in `spawn`, where the
    /// channel is created; the tests drive the out-of-actor push
    /// through the same handle type production handlers hold.
    #[cfg(test)]
    #[expect(
        clippy::same_name_method,
        reason = "the test accessor names the type it hands back (handle -> FanoutHandle); the MsgHandler::handle collisions are trait-scoped and never ambiguous at a call site"
    )]
    pub(crate) fn handle(&self) -> FanoutHandle {
        FanoutHandle {
            tx: self.tx.clone(),
        }
    }

    /// Spawns the actor the way production does and hands back a
    /// receiver subscribed to its broadcast channel — the pair a test
    /// needs to observe what an HTTP handler pushed through the
    /// shared handle. Test-only.
    #[cfg(test)]
    pub(crate) fn test_pair(
        state: &State,
        bus: BusService,
        unread_marks: TypedCell<UnreadSessionMarks>,
    ) -> (FanoutHandle, broadcast::Receiver<SseEnvelope>) {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        let handle = Self::spawn(&system, state.clone(), bus, unread_marks);
        (handle.clone(), handle.subscribe())
    }

    /// Flushes one session's buffer with a cursor of zero. Test-only:
    /// production flushes stamp the session's real entry count via
    /// `flush_all`.
    #[cfg(test)]
    pub(crate) fn flush_for_test(&mut self, session_id: &SessionId) {
        self.flush_session(session_id, 0);
    }

    /// Pushes one envelope to every connected page. Send failures are
    /// the no-receiver case — a page disconnected between flushes —
    /// and are not errors.
    fn broadcast(&self, envelope: SseEnvelope) {
        let _ = self.tx.send(envelope);
    }

    /// Appends rendered token HTML to a session's buffer.
    pub(crate) fn append_token(&mut self, token: &StreamToken) {
        use std::fmt::Write as _;
        let span_class = if token.is_thinking {
            "tok-thinking"
        } else {
            "tok-text"
        };
        // The token text is escaped verbatim: token deltas are raw text
        // fragments, not markup.
        let escaped = crate::transcript::escape_html(&token.token);
        let entry = self.pending.entry(token.session_id.clone()).or_default();
        let _ = write!(entry, "<span class=\"{span_class}\">{escaped}</span>");
    }

    /// Flushes one session's buffered tokens as a `chat` fragment
    /// stamped with the session's current rendered-entry count.
    pub(crate) fn flush_session(&mut self, session_id: &SessionId, cursor: usize) {
        if let Some(html) = self.pending.remove(session_id) {
            // An empty buffer flushes nothing: there is no fragment to
            // append, and an empty `beforeend` swap would be noise.
            if !html.is_empty() {
                self.broadcast(SseEnvelope {
                    kind: SseKind::Chat,
                    session_id: session_id.clone(),
                    cursor,
                    html,
                });
            }
        }
    }

    /// Flushes every buffered session, reading each one's entry count
    /// for the cursor stamp. One state read per buffered session; the
    /// guard drops before the broadcast (no await under it). The stamp
    /// is the RENDERED entry count — the cursor's coordinate space is
    /// the filtered transcript, so raw history length (which counts
    /// transients the transcript drops) would drift clients' cursors
    /// and corrupt every reconnect after one.
    fn flush_all(&mut self) {
        let ids: Vec<SessionId> = self.pending.keys().cloned().collect();
        for session_id in ids {
            let cursor = {
                let guard = self.state.read();
                guard.session.get(&session_id).map_or(0, |session| {
                    session
                        .history()
                        .iter()
                        .filter(|e| crate::transcript::is_renderable(e))
                        .count()
                })
            };
            self.flush_session(&session_id, cursor);
        }
    }

    /// Marks a session unread — the unseen-settle write.
    ///
    /// Returns whether the mark is **new**: a re-mark of an
    /// already-marked session writes nothing and reports `false`. The
    /// caller decides the refresh — the settle path may have a second
    /// reason (the working flip) to share one fan with.
    fn mark_unread(&mut self, session_id: &SessionId) -> bool {
        self.unread_marks
            .update(|marks| marks.mark(session_id.clone()))
    }

    /// Clears a session's unread mark — activation landed.
    ///
    /// Returns whether a mark was actually removed; the caller decides
    /// the refresh.
    fn clear_unread(&mut self, session_id: &SessionId) -> bool {
        self.unread_marks.update(|marks| marks.clear(session_id))
    }

    /// Settles a session to idle: flush the partial token buffer FIRST,
    /// then the regions' settle fragments in registry order — the
    /// transcript's convergence push (cursor 0 — the client re-bases on
    /// it) then the composer's idle controls. The ordering keeps the
    /// final tokens from appearing twice (once appended, once in the
    /// full render), and the full push is the convergence backstop that
    /// heals drift after rewinds, cancels, or partial-entry edges.
    #[expect(
        clippy::expect_used,
        reason = "bootstrap assertion: the conformance gate guarantees every settle kind one owning region; a None here is a registry bug that must abort, not skip a push"
    )]
    pub(crate) fn settle_to_idle(&mut self, session_id: &SessionId) {
        self.flush_session(session_id, 0);
        for kind in settle_push_kinds() {
            let html = {
                let guard = self.state.read();
                let region = crate::region::owner_of(kind)
                    .expect("a settle kind has exactly one owning region (conformance gate)");
                region.render(session_id, &guard)
            };
            self.broadcast(SseEnvelope {
                kind,
                session_id: session_id.clone(),
                cursor: 0,
                html,
            });
        }
    }

    /// Pushes the busy-state controls fragment for a session — the
    /// composer region's guard render.
    pub(crate) fn push_busy_controls(&mut self, session_id: &SessionId) {
        let html = {
            let guard = self.state.read();
            crate::regions::composer::controls_fragment(session_id, &guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Controls,
            session_id: session_id.clone(),
            cursor: 0,
            html,
        });
    }

    /// Pushes a refreshed sessions-list fragment to every page — the
    /// sessions region's guard render.
    pub(crate) fn push_sessions_refresh(&mut self, session_id: &SessionId) {
        let html = {
            let guard = self.state.read();
            crate::regions::sessions::sessions_live_fragment(&guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Sessions,
            session_id: session_id.clone(),
            cursor: 0,
            html,
        });
    }

    /// Pushes a refreshed sessions-list fragment to **every** live
    /// session's stream — one addressed `sessions` envelope per live
    /// session id, the `push_models_changed` shape.
    ///
    /// The sessions fragment is page-global (one list, every viewer),
    /// but the SSE endpoint filters per stream: an addressed push
    /// reaches only the page viewing that session, and the
    /// interesting mark transitions happen on pages viewing *other*
    /// sessions. So a set-or-clear transition loops every live
    /// session id with one render feeding every envelope — the list
    /// does not vary by viewer.
    fn refresh_all_sessions_lists(&mut self) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        let html = {
            let guard = self.state.read();
            crate::regions::sessions::sessions_live_fragment(&guard)
        };
        for session_id in ids {
            self.broadcast(SseEnvelope {
                kind: SseKind::Sessions,
                session_id,
                cursor: 0,
                html: html.clone(),
            });
        }
    }

    /// Signals every open model modal that the discovered-model cache
    /// changed — one `models` push per live session, addressed so each
    /// session-filtered stream receives it — and pushes the
    /// transcript's full fragment so the refresh summary transient
    /// (written by the provider actor's shared `ModelsRefreshed`
    /// handler before this event fires) reaches the web transcript
    /// without a reload.
    ///
    /// The `models` envelope is a **signal, not rows**: the fan-out
    /// holds only `State` (no provider registry or api keys), so it
    /// cannot render TUI-parity rows; the client answers the push by
    /// re-pulling the picker's GET route with its current filter. Rows
    /// always arrive by pull — the pull endpoint remains the
    /// correctness dependency. The transcript envelope is the region's
    /// guard render (cursor 0, full replace — the convergence shape).
    #[expect(
        clippy::expect_used,
        reason = "bootstrap assertion: the conformance gate guarantees the transcript kind \
                  exactly one owning region; a None here is a registry bug that must abort"
    )]
    pub(crate) fn push_models_changed(&mut self, stored_at: jiff::Timestamp) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        for session_id in ids {
            let transcript_html = {
                let guard = self.state.read();
                crate::region::owner_of(SseKind::TranscriptFull)
                    .expect(
                        "a transcript-full kind has exactly one owning region (conformance gate)",
                    )
                    .render(&session_id, &guard)
            };
            // The `models` event is a signal, but the data line must
            // carry bytes: the browser discards an SSE event whose data
            // buffer is empty at dispatch (WHATWG stream spec — the
            // frame never fires its listener), which is exactly the
            // never-stops-spinner bug. The cache-write timestamp is
            // the signal's provenance; the payload's meaning stays
            // "re-pull your rows" and no rows ride the push.
            self.broadcast(SseEnvelope {
                kind: SseKind::Models,
                session_id: session_id.clone(),
                cursor: 0,
                html: stored_at.to_string(),
            });
            self.broadcast(SseEnvelope {
                kind: SseKind::TranscriptFull,
                session_id,
                cursor: 0,
                html: transcript_html,
            });
        }
    }

    /// Signals every open MCP modal that the runtime cell's status
    /// projection changed — one `mcp-status` push per live session,
    /// addressed so each session-filtered stream receives it (the
    /// `push_models_changed` shape, without the transcript envelope:
    /// a status transition touches no transcript entry).
    ///
    /// The `mcp-status` envelope is a **signal, not rows**: the
    /// fan-out holds no MCP cell or config handle, so it cannot render
    /// TUI-parity rows; the client answers the push by re-pulling the
    /// modal's GET route. Rows always arrive by pull — the pull
    /// endpoint remains the correctness dependency. The data line
    /// carries the write timestamp for the same dispatch rule the
    /// models push documents (an empty data buffer never fires).
    pub(crate) fn push_mcp_status_changed(&mut self, stored_at: jiff::Timestamp) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        for session_id in ids {
            self.broadcast(SseEnvelope {
                kind: SseKind::McpStatus,
                session_id,
                cursor: 0,
                html: stored_at.to_string(),
            });
        }
    }

    /// Signals every open skills modal that a discovery scan landed —
    /// one `skills` push per live session, the `push_mcp_status_changed`
    /// shape verbatim.
    ///
    /// The `skills` envelope is a **signal, not rows**: discovery
    /// writes the session's ephemeral set, and the pull endpoint
    /// renders it — the fan-out renders no rows here, the same
    /// pull-remains-the-renderer rule every modal follows. The data
    /// line carries the scan's landing timestamp: an SSE frame whose
    /// data buffer is empty is discarded by the browser (never
    /// dispatched), so the signal must hold bytes to land.
    pub(crate) fn push_skills_changed(&mut self, loaded_at: jiff::Timestamp) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        for session_id in ids {
            self.broadcast(SseEnvelope {
                kind: SseKind::Skills,
                session_id,
                cursor: 0,
                html: loaded_at.to_string(),
            });
        }
    }

    /// Signals every open tools modal that MCP connections changed —
    /// one `tools` push per live session, the `push_skills_changed`
    /// shape verbatim.
    ///
    /// The `tools` envelope is a **signal, not rows**: the MCP
    /// coordinator wrote its runtime cell, and the tools rows pull
    /// renders it — the fan-out renders no rows here, the same
    /// pull-remains-the-renderer rule every modal follows. The client
    /// answers the push by re-pulling the **rows fragment** only (the
    /// revert stamp lives in the modal skeleton the re-pull never
    /// touches, so the open-time snapshot survives mid-open ticks).
    /// The data line carries the write timestamp: a signal with an
    /// empty data line never dispatches in the browser.
    pub(crate) fn push_tools_changed(&mut self, stored_at: jiff::Timestamp) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        for session_id in ids {
            self.broadcast(SseEnvelope {
                kind: SseKind::Tools,
                session_id,
                cursor: 0,
                html: stored_at.to_string(),
            });
        }
    }

    /// A browser-created session landed: refresh the sessions list.
    /// Synchronous work only; `async` is the `MsgHandler::handle`
    /// signature's shape, dropped here.
    pub(crate) fn on_session_created(&mut self, msg: &jinn_session_lifecycle_msg::SessionCreated) {
        // Every page's sidebar refreshes so it lists the new session —
        // the same live-only refresh shape the other "the list
        // changed" events push. The creating page also receives this
        // (its POST response carries the new page, whose sidebar is
        // already fresh; the double render is the same harmless
        // overlap a rename POST causes).
        self.push_sessions_refresh(&msg.session_id);
    }

    /// The setup script's async outcome: push the transcript-full
    /// convergence for that session. Synchronous work only; `async`
    /// is the `MsgHandler::handle` signature's shape, dropped here.
    pub(crate) fn on_session_setup_completed(
        &mut self,
        msg: &jinn_session_lifecycle_msg::SessionSetupCompleted,
    ) {
        // Success notes and error entries alike land in the session's
        // history before this event fires (the lifecycle actor's own
        // handling), so the transcript-full convergence push is the
        // whole visibility path: cursor 0, full replace, exactly the
        // shape a settle's convergence uses. This is NOT
        // `settle_to_idle`: the session is not settling phases here,
        // and flipping the composer controls would lie about the
        // session's state.
        let session_id = &msg.session_id;
        let transcript_html = {
            let guard = self.state.read();
            crate::region::owner_of(SseKind::TranscriptFull)
                .map_or_default(|region| region.render(session_id, &guard))
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: session_id.clone(),
            cursor: 0,
            html: transcript_html,
        });
    }

    /// A chat entry landed in history: flush the session's pending
    /// token buffer first, then push the transcript-full convergence.
    /// Synchronous work only; `async` is the `MsgHandler::handle`
    /// signature's shape, dropped here.
    ///
    /// The flush runs **before** the replace for the same reason a
    /// settle flushes first: the dispatch that published this event
    /// follows a convergence render (a settle push or this push) with
    /// an inner replace that would otherwise re-append any tokens still
    /// buffered when a later `FlushTick` lands it. The convergence push
    /// itself is the settle's transcript shape — cursor 0, the region's
    /// guard render, an idempotent full replace — so it restores a
    /// queued message's wiped optimistic entry and cannot double an
    /// entry the send POST's optimistic append already added.
    ///
    /// This fires on ordinary sends too (the idle-direct dispatch
    /// publishes the same event); the convergence render makes that a
    /// harmless overlap, exactly like the setup-completed path above.
    pub(crate) fn on_chat_entry_submitted(
        &mut self,
        msg: &jinn_chat_input_msg::ChatEntrySubmitted,
    ) {
        self.flush_session(&msg.session_id, 0);
        let transcript_html = {
            let guard = self.state.read();
            crate::region::owner_of(SseKind::TranscriptFull)
                .map_or_default(|region| region.render(&msg.session_id, &guard))
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: msg.session_id.clone(),
            cursor: 0,
            html: transcript_html,
        });
    }
}

/// The settle push order: every region's `settle_kinds` flattened in
/// registry order. The partial token flush (above) runs before these;
/// the transcript's convergence push precedes the composer's idle
/// controls. A new region contributes its own settle kinds by
/// declaring them — no edit here.
fn settle_push_kinds() -> Vec<SseKind> {
    crate::region::regions()
        .iter()
        .flat_map(|region| region.settle_kinds().iter().copied())
        .collect()
}

impl MsgHandler<FlushTick> for WebUiFanoutActor {
    async fn handle(&mut self, _msg: &FlushTick, ctx: &mut MsgCtx<'_>) {
        self.flush_all();
        ctx.schedule_self(FlushTick, FLUSH_INTERVAL);
    }
}

impl MsgHandler<StreamToken> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &StreamToken, _ctx: &mut MsgCtx<'_>) {
        self.append_token(msg);
    }
}

impl MsgHandler<SessionPhaseChanged> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionPhaseChanged, _ctx: &mut MsgCtx<'_>) {
        self.on_phase_changed(msg);
    }
}

impl WebUiFanoutActor {
    /// The phase-change body, split out of the `MsgHandler` impl so
    /// tests can drive it without fabricating trouper-internal
    /// `MsgCtx` values (the same seam `settle_to_idle` exposes).
    ///
    /// The sessions list carries the session's working state as the
    /// row spinner, so the list must refresh exactly when that state
    /// **flips** — a `Streaming→Working` turn boundary changes no
    /// rendered fact and fans nothing, while every turn start and
    /// settle does. The flip is decided from the event's phases before
    /// the busy/idle branch (the busy branch early-returns), and the
    /// settle's refresh is shared with the unread-mark transition
    /// below so one settling event fans at most one wave.
    pub(crate) fn on_phase_changed(&mut self, msg: &SessionPhaseChanged) {
        let was_working = msg.old_phase.is_working();
        let now_working = msg.new_phase.is_working();
        let working_flipped = was_working != now_working;

        // Busy: swap the composer control to cancel immediately.
        if msg.new_phase != PhaseView::Idle {
            self.push_busy_controls(&msg.session_id);
            if working_flipped {
                self.refresh_all_sessions_lists();
            }
            return;
        }
        self.settle_to_idle(&msg.session_id);
        // The unseen-settle mark, decided at set time: only a settle
        // FROM a working phase (a cancel drain or error settle delivers
        // entries the user has not seen, so it marks like any other)
        // of a session that is not the instance's active one right now
        // and is not subagent-origin (subagent rows render only inside
        // their tree, under a session the user is interacting with;
        // `SessionTreeEntry`/`SessionSummary` carry no origin, so the
        // render path could not filter them). Re-marking is a no-op;
        // marking the active session is refused — a session the user
        // is watching is viewed by definition.
        let idle_settle = msg.old_phase != PhaseView::Idle;
        let origin = {
            let guard = self.state.read();
            guard
                .session
                .get(&msg.session_id)
                .map(jinn_kernel::ChatSessionState::origin)
        };
        let is_active = active_session_id_of(&self.state) == msg.session_id;
        // The mark write runs BEFORE the single refresh decision: a
        // settling unseen session has two reasons to refresh (the
        // spinner cleared, the mark landed) and fans one wave carrying
        // both facts.
        let marked = idle_settle
            && !is_active
            && origin != Some(SessionOrigin::Subagent)
            && self.mark_unread(&msg.session_id);
        if working_flipped || marked {
            self.refresh_all_sessions_lists();
        }
    }
}

/// The instance's active session id, under one short-lived read guard.
#[must_use]
fn active_session_id_of(state: &State) -> SessionId {
    state.read().session.active_session_id().clone()
}

impl MsgHandler<SessionLoadCompleted> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionLoadCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_load_completed(msg);
    }
}

impl WebUiFanoutActor {
    /// The load-completed body, split out for the same test seam.
    pub(crate) fn on_load_completed(&mut self, msg: &SessionLoadCompleted) {
        // A session activation landed (from the web or the TUI — both
        // ride the same command): refresh every page's sessions list.
        self.push_sessions_refresh(&msg.session_id);
        // The "the user has now viewed it" clear, keyed to the active
        // id at handling time: a tree load publishes one completed
        // event per background member restored under a different
        // active id, and those members are still unviewed — clearing
        // them would erase the exact signal the mark exists for. Only
        // the session that IS the active one at this moment was viewed.
        if active_session_id_of(&self.state) == msg.session_id && self.clear_unread(&msg.session_id)
        {
            // The addressed refresh above reaches only the activated
            // session's stream — every OTHER page's sidebar still
            // carries the yellow, and only a per-live-session fan
            // clears it. The transition gate keeps a re-clear quiet.
            self.refresh_all_sessions_lists();
        }
    }
}

impl MsgHandler<SessionArchived> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionArchived, _ctx: &mut MsgCtx<'_>) {
        self.push_sessions_refresh(&msg.session_id);
    }
}

impl MsgHandler<SessionLibraryMembershipChanged> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionLibraryMembershipChanged, _ctx: &mut MsgCtx<'_>) {
        self.push_sessions_refresh(&msg.session_id);
    }
}

impl MsgHandler<ModelCacheStored> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &ModelCacheStored, _ctx: &mut MsgCtx<'_>) {
        // The provider cell now holds the merged cache (the event is
        // emitted strictly after the write). Signal every page; open
        // modals re-pull their rows with their current filter. The
        // event's timestamp rides the push so the frame dispatches
        // (a signal with an empty data line never fires).
        self.push_models_changed(msg.stored_at);
    }
}

impl MsgHandler<jinn_mcp_msg::McpRuntimeStored> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &jinn_mcp_msg::McpRuntimeStored, _ctx: &mut MsgCtx<'_>) {
        // The coordinator emitted this strictly after its runtime-cell
        // write, so the cell now holds the new status. Signal every
        // page; open MCP modals re-pull their rows. The event's
        // timestamp rides the push so the frame dispatches (a signal
        // with an empty data line never fires).
        self.push_mcp_status_changed(msg.stored_at);
        // The same write moved the session's connection set, which is
        // the only thing that moves a tools list (the registry,
        // provider, and bot gates are static per session) — so the
        // open tools modals re-pull their rows too. One handler, two
        // signal kinds; both remain signals, the pull routes remain
        // the renderers. The tools re-pull swaps the rows fragment
        // only, so the revert stamp's data-init never re-runs mid-open.
        self.push_tools_changed(msg.stored_at);
    }
}

impl MsgHandler<jinn_skills_msg::SkillsLoaded> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &jinn_skills_msg::SkillsLoaded, _ctx: &mut MsgCtx<'_>) {
        // Discovery wrote the session's ephemeral discovered-skills
        // set (the scan actor emits this strictly after the write), so
        // a pull now renders the fresh rows. Signal every page; open
        // skills modals re-pull their rows — discovery lands async
        // after the modal can be opened, and the pull route remains
        // the correctness dependency. A failed scan still bumps the
        // tick (empty rows are a legal render); the timestamp rides
        // the push so the frame dispatches.
        let loaded_at = jiff::Timestamp::now();
        let _ = &msg.error;
        self.push_skills_changed(loaded_at);
    }
}

impl MsgHandler<jinn_session_lifecycle_msg::SessionCreated> for WebUiFanoutActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_lifecycle_msg::SessionCreated,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_session_created(msg);
    }
}

impl MsgHandler<jinn_session_lifecycle_msg::SessionSetupCompleted> for WebUiFanoutActor {
    async fn handle(
        &mut self,
        msg: &jinn_session_lifecycle_msg::SessionSetupCompleted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_session_setup_completed(msg);
    }
}

impl MsgHandler<jinn_chat_input_msg::ChatEntrySubmitted> for WebUiFanoutActor {
    async fn handle(
        &mut self,
        msg: &jinn_chat_input_msg::ChatEntrySubmitted,
        _ctx: &mut MsgCtx<'_>,
    ) {
        self.on_chat_entry_submitted(msg);
    }
}
