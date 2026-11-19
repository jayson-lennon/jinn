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
use jinn_session_msg::{SessionArchived, SessionPhaseChanged};
use jinn_session_store_msg::{SessionLibraryMembershipChanged, SessionLoadCompleted};
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use jinn_webui_msg::{SseEnvelope, SseKind};
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
    #[must_use]
    pub fn spawn(system: &ActorSystem, state: State, _bus: BusService) -> FanoutHandle {
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
                    })
                })
            })
            .handles::<StreamToken>()
            .handles::<SessionPhaseChanged>()
            .handles::<SessionLoadCompleted>()
            .handles::<SessionArchived>()
            .handles::<SessionLibraryMembershipChanged>()
            .handles::<ModelCacheStored>()
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
    pub(crate) fn test_with_receiver(state: State) -> (Self, broadcast::Receiver<SseEnvelope>) {
        let (tx, rx) = broadcast::channel(BROADCAST_CAPACITY);
        (
            Self {
                state,
                tx: Arc::new(tx),
                pending: HashMap::new(),
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
    ) -> (FanoutHandle, broadcast::Receiver<SseEnvelope>) {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        let handle = Self::spawn(&system, state.clone(), bus);
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
        // Busy: swap the composer control to cancel immediately.
        if msg.new_phase != PhaseView::Idle {
            self.push_busy_controls(&msg.session_id);
            return;
        }
        self.settle_to_idle(&msg.session_id);
    }
}

impl MsgHandler<SessionLoadCompleted> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionLoadCompleted, _ctx: &mut MsgCtx<'_>) {
        // A session activation landed (from the web or the TUI — both
        // ride the same command): refresh every page's sessions list.
        self.push_sessions_refresh(&msg.session_id);
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
