//! The SSE fan-out actor — bus events in, HTML fragments out.
//!
//! Subscribes to the stream-adjacent **Events** (never commands: a
//! command routes to one declarant, an event broadcasts) and pushes
//! rendered fragments onto a [`tokio::sync::broadcast`] channel. Each
//! connected SSE endpoint holds a receiver; connect = subscribe,
//! disconnect = drop. The channel plus the per-session streaming state
//! are the whole of the actor's non-authoritative state — transport
//! and display state only: it owns nothing authoritative, never
//! replays, and a crash loses nothing. The pull endpoint is the
//! correctness dependency.
//!
//! Streaming: `StreamToken`s arrive per-delta while pages want
//! low-frequency fragments. The actor holds each session's
//! **settled boundary** — the rendered-entry count captured when the
//! session's phase flipped Idle→working — and, on the self-addressed
//! 150 ms tick, renders the turn's in-flight tail (`boundary..end` of
//! the history) through the same transcript projection the settled
//! transcript uses, pushing it as one `streaming` envelope that the
//! wire seam wholesale-replaces into the page's `#streaming`
//! container. A flush whose rendered tail is unchanged from the last
//! pushed tail pushes nothing (the quiet-stretch skip). When a session
//! settles to `Idle` the boundary is cleared **before** the regions'
//! settle fragments push in registry order — the transcript's
//! convergence push then carries the whole turn (rendered through the
//! boundary-free projection) and structurally empties the streaming
//! container, so no entry renders twice and nothing is lost. Thinking
//! entries render open in the tail (the reasoning is what is being
//! watched) and collapsed in the settled render; the same projection
//! serves both through [`crate::transcript::render_entry_streaming`].
//!
//! An untracked session renders as if idle — full transcript, empty
//! streaming container — the safe degrade direction: convergence at
//! the settle heals it, never duplication. The fan-out names no
//! region: settle pushes resolve through [`crate::region`]'s registry,
//! so a new region contributes pushes by declaring settle kinds, never
//! by editing this module.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_kernel::protocol::system::ActiveSessionChanged;
use jinn_provider_selection_msg::ModelCacheStored;
use jinn_session_msg::SessionOrigin;
use jinn_session_msg::{SessionArchived, SessionPhaseChanged};
use jinn_session_store_msg::{SessionLibraryMembershipChanged, SessionLoadCompleted};
use jinn_slices::PhaseView;
use jinn_slices::bus::BusService;
use jinn_slices::cell::TypedCell;
use jinn_webui_msg::{SessionPageOpened, SseEnvelope, SseKind, UnreadSessionMarks};
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

/// The per-session settled-boundary map: for each session with a live
/// turn whose start the fan-out observed, the rendered-entry count the
/// settled transcript already carried. Display state behind a mutex —
/// its loss degrades to a full render (never duplication), and the
/// settle clears the entry as the settle's convergence push lands.
///
/// **Single writer:** the fan-out actor (`capture_boundary`,
/// `clear_boundary`). The render paths read only, through the shared
/// handle.
#[derive(Clone, Default)]
pub struct StreamingBoundaries {
    inner: Arc<parking_lot::Mutex<HashMap<SessionId, usize>>>,
}

impl StreamingBoundaries {
    /// The session's boundary, `None` when untracked.
    #[must_use]
    pub fn get(&self, session_id: &SessionId) -> Option<usize> {
        self.inner.lock().get(session_id).copied()
    }

    /// Captures the boundary as the session's current rendered-entry
    /// count. Actor-only (the sole writer).
    fn set(&self, session_id: &SessionId, boundary: usize) {
        self.inner.lock().insert(session_id.clone(), boundary);
    }

    /// Clears the session's boundary. Actor-only (the sole writer).
    fn clear(&self, session_id: &SessionId) {
        self.inner.lock().remove(session_id);
    }
}

/// One session's snapshot for the tail render: the in-flight entries
/// (`boundary..end`) and the boundary itself — taken under one short
/// read guard.
struct TailSnapshot {
    entries: Vec<crate::transcript::RenderedEntry>,
    boundary: usize,
}

/// The fan-out's shared handle: what an SSE endpoint subscribes to and
/// what the HTTP render paths read the streaming boundaries through.
#[derive(Clone)]
pub struct FanoutHandle {
    tx: Arc<broadcast::Sender<SseEnvelope>>,
    /// The per-session settled-boundary map, shared with the actor.
    /// The actor is the sole writer; the HTTP paths (page fill,
    /// catch-up, the pull) read it to split settled from in-flight
    /// entries. Losing it degrades to full-render — never to
    /// duplication.
    boundaries: StreamingBoundaries,
}

impl FanoutHandle {
    /// Subscribes a new SSE connection. Dropping the receiver
    /// unsubscribes it.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<SseEnvelope> {
        self.tx.subscribe()
    }

    /// Pushes the composer's controls fragment for a session, rendered
    /// from the phase the state holds — busy shows cancel/queue, idle
    /// shows send. Named for the fragment, not a phase: the ordinary
    /// busy push fires on the phase flip and the settle wave covers
    /// the ordinary idle swap — but this render is also the
    /// no-live-turn cancel's settle path (see
    /// `crate::delivery::handle_cancel`): a cancel landing on a turn
    /// the ledger no longer holds publishes a fact nothing answers, so
    /// the handler re-broadcasts the controls state the session
    /// actually holds through this handle. The POST response stays
    /// empty — the controls push remains the busy flag's single
    /// writer. The handle holds no state (a bare sender seam), so the
    /// caller reads the flag under its own guard and hands it in.
    pub(crate) fn push_controls_from(&self, session_id: &SessionId, busy: bool) {
        let _ = self.tx.send(SseEnvelope {
            kind: SseKind::Controls,
            session_id: session_id.clone(),
            cursor: 0,
            html: busy.to_string(),
        });
    }

    /// The session's settled boundary: the rendered-entry count the
    /// settled transcript already carried when the live turn began.
    /// `None` when the session is not mid-turn (or its boundary was
    /// never observed) — the caller renders the whole history and an
    /// empty streaming container.
    #[must_use]
    pub fn settled_boundary(&self, session_id: &SessionId) -> Option<usize> {
        self.boundaries.get(session_id)
    }

    /// Pushes a refreshed sessions signal to every page,
    /// addressed to the named session — from **outside** the actor.
    ///
    /// A synchronous HTTP handler (the rename POST) must push without
    /// publishing anything the fan-out subscribes to: the only command
    /// it could publish, `PersistSession`, routes to the store actor,
    /// so there is no command whose arrival means "refresh the list".
    /// The handle holds no state (it is a bare sender seam), so the
    /// caller reads the current unread-mark count under its own guard
    /// and hands it in — the push's payload (the addressed Sessions
    /// kind is signal-only at the wire; the receiving pages re-pull
    /// the list).
    ///
    /// Send failures are the no-receiver case — no page is connected —
    /// and are not errors.
    pub(crate) fn push_sessions_refresh_from(&self, session_id: &SessionId, attention: usize) {
        let _ = self.tx.send(SseEnvelope {
            kind: SseKind::Sessions,
            session_id: session_id.clone(),
            cursor: 0,
            html: attention.to_string(),
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
    /// The per-session settled-boundary map, shared with the handle.
    /// This actor is the map's sole writer.
    boundaries: StreamingBoundaries,
    /// The last tail HTML each session was pushed, kept beside the
    /// boundary so a flush tick whose rendered tail is unchanged
    /// pushes nothing (the quiet-stretch skip). Cleared with the
    /// boundary at settle.
    last_tail: HashMap<SessionId, String>,
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
        let handle_boundaries = StreamingBoundaries::default();
        let actor_boundaries = handle_boundaries.clone();

        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(WEBUI_FANOUT_NAME))
            .init_with(move || {
                let tx = actor_tx.clone();
                let state = state.clone();
                let boundaries = actor_boundaries.clone();
                Box::pin(async move {
                    Ok(Self {
                        state,
                        tx,
                        boundaries,
                        last_tail: HashMap::new(),
                        unread_marks: unread_marks.clone(),
                    })
                })
            })
            // No `.handles::<StreamToken>()`: the tokens are already in
            // the session's history when the next flush tick reads it,
            // and the boundary map (populated by the phase-change
            // handler below) decides which sessions have a tail to
            // render. Buffering the token deltas was the old transport's
            // job; the tail re-render needs only the tick.
            .handles::<SessionPhaseChanged>()
            .handles::<SessionLoadCompleted>()
            // The activation fact — the web click, the TUI switch, and
            // now the store actor's in-memory branch all announce it.
            // The handler clears that session's unread mark: activation
            // anywhere in the instance IS "the user looked".
            .handles::<ActiveSessionChanged>()
            // A page arrived at a session (GET, reload, deep link —
            // archived sessions included, no activation behind it).
            // The handler clears the same mark: opening the page is
            // "the user looked" too.
            .handles::<SessionPageOpened>()
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

        FanoutHandle {
            tx,
            boundaries: handle_boundaries,
        }
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
                boundaries: StreamingBoundaries::default(),
                last_tail: HashMap::new(),
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
            boundaries: self.boundaries.clone(),
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

    /// Flushes every tracked session's tail. Test-only: the same body
    /// the flush tick handler runs, without the re-arm.
    #[cfg(test)]
    pub(crate) fn flush_for_test(&mut self) {
        self.flush_all();
    }

    /// Flushes ONE session's tail. Test-only: the per-session seam the
    /// scoping tests drive.
    #[cfg(test)]
    pub(crate) fn flush_session_tail_for_test(&mut self, session_id: &SessionId) {
        self.flush_session_tail(session_id);
    }

    /// Pushes one envelope to every connected page. Send failures are
    /// the no-receiver case — a page disconnected between flushes —
    /// and are not errors.
    fn broadcast(&self, envelope: SseEnvelope) {
        let _ = self.tx.send(envelope);
    }

    /// Reads the tail snapshot for one session: the in-flight entries
    /// rendered with thinking disclosures open (the streaming
    /// disclosure rule), everything else collapsed. `None` when the
    /// session is untracked (no live turn observed) — the caller
    /// pushes nothing for it. One short read guard; the cursor
    /// coordinate space is the rendered count (`is_renderable`-
    /// filtered), the same space every cursor uses.
    fn tail_snapshot(&self, session_id: &SessionId) -> Option<TailSnapshot> {
        let boundary = self.boundaries.get(session_id)?;
        let guard = self.state.read();
        let session = guard.session.get(session_id)?;
        let entries: Vec<_> = session
            .history()
            .iter()
            .filter(|e| crate::transcript::is_renderable(e))
            .skip(boundary)
            .map(|e| crate::transcript::render_entry_streaming(e, session_id, true))
            .collect();
        drop(guard);
        Some(TailSnapshot { entries, boundary })
    }

    /// Flushes one session's streaming tail: renders `boundary..end`
    /// through the real projection (one [`EntryTemplate`][tmpl] per
    /// entry, the catch-up shape) and pushes it as one `streaming`
    /// envelope whose cursor parks at the boundary. A tail identical
    /// to the last pushed one pushes nothing (the quiet-stretch
    /// skip), and an untracked session pushes nothing.
    ///
    /// [tmpl]: crate::templates::EntryTemplate
    fn flush_session_tail(&mut self, session_id: &SessionId) {
        let Some(snapshot) = self.tail_snapshot(session_id) else {
            // Untracked: nothing in flight was observed for this
            // session — degrade to silence (the settle's convergence
            // push is the correctness backstop).
            return;
        };
        let mut html = String::new();
        for (offset, entry) in snapshot.entries.iter().enumerate() {
            let template = crate::templates::EntryTemplate {
                entry: entry.clone(),
                index: snapshot.boundary + offset,
            };
            match crate::templates::render(&template) {
                Ok(rendered) => html.push_str(&rendered),
                Err(error) => {
                    tracing::warn!(%error, what = "streaming-tail", "webui template render failed; \
                         skipping this flush");
                    return;
                }
            }
        }
        if self
            .last_tail
            .get(session_id)
            .is_some_and(|last| *last == html)
        {
            // Unchanged tail — nothing new rendered since the last
            // push; keep the 150 ms tick from churning the DOM during
            // long quiet stretches.
            return;
        }
        self.last_tail.insert(session_id.clone(), html.clone());
        self.broadcast(SseEnvelope {
            kind: SseKind::Streaming,
            session_id: session_id.clone(),
            cursor: snapshot.boundary,
            html,
        });
    }

    /// Flushes every tracked session's tail. One state read per
    /// tracked session; the guard drops before the broadcast (no
    /// await under it).
    fn flush_all(&mut self) {
        let ids: Vec<SessionId> = self.boundaries.inner.lock().keys().cloned().collect();
        for session_id in ids {
            self.flush_session_tail(&session_id);
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

    /// The boundary-aware full-transcript fragment for one session,
    /// under one short read guard: the settled prefix when a turn is
    /// in flight (these pushes land mid-turn, where a full render
    /// would double the tail the streaming container shows), the whole
    /// history otherwise. The settle path does NOT use this — its
    /// boundary is already cleared when it renders, so
    /// [`Self::settle_to_idle`]'s registry render carries everything.
    fn settled_transcript_fragment(&self, session_id: &SessionId) -> String {
        let boundary = self.boundaries.get(session_id);
        let guard = self.state.read();
        let Some(session) = guard.session.get(session_id) else {
            return String::new();
        };
        let transcript = crate::transcript::TranscriptModel::from_live(
            session.history(),
            session.title(),
            &crate::regions::transcript::model_label(session.model_selection()),
            session.phase().as_str(),
            session.is_bot(),
            session_id,
        );
        drop(guard);
        let entries = match boundary {
            Some(boundary) => transcript
                .entries
                .get(..boundary.min(transcript.len()))
                .map_or_else(
                    || transcript.entries.clone(),
                    <[crate::transcript::RenderedEntry]>::to_vec,
                ),
            None => transcript.entries,
        };
        crate::templates::render(&crate::templates::TranscriptTemplate::new(entries, 0))
            .unwrap_or_default()
    }

    /// Settles a session to idle: clear the settled boundary FIRST,
    /// then the regions' settle fragments in registry order — the
    /// transcript's convergence push (cursor 0 — the client re-bases
    /// on it) then the composer's idle controls. The convergence push
    /// is the whole turn's visibility path: rendered through the
    /// boundary-free projection it carries every entry once, and its
    /// trailing empty `#streaming` container empties the streamed tail
    /// structurally — the ordering keeps the in-flight entries from
    /// appearing twice (once in the tail container, once in the full
    /// render), and the full push is the convergence backstop that
    /// heals drift after rewinds, cancels, or partial-entry edges.
    ///
    /// A Sessions envelope (when the sessions region declares a settle
    /// kind) carries the unread-mark count in its payload, not a
    /// fragment — the kind is signal-only at the wire. The count read
    /// shares the settle's guard so the stamped number and the
    /// rendered fragments agree.
    #[expect(
        clippy::expect_used,
        reason = "bootstrap assertion: the conformance gate guarantees every settle kind one owning region; a None here is a registry bug that must abort, not skip a push"
    )]
    pub(crate) fn settle_to_idle(&mut self, session_id: &SessionId) {
        // The boundary dies before the settle renders: the convergence
        // push must render the whole history (not stop at the old
        // boundary), and the tail-skip state must not survive into the
        // next turn.
        self.boundaries.clear(session_id);
        self.last_tail.remove(session_id);
        for kind in settle_push_kinds() {
            let html = {
                let guard = self.state.read();
                if kind == SseKind::Sessions {
                    crate::regions::sessions::unread_count_of(&guard).to_string()
                } else {
                    let region = crate::region::owner_of(kind)
                        .expect("a settle kind has exactly one owning region (conformance gate)");
                    region.render(session_id, &guard)
                }
            };
            self.broadcast(SseEnvelope {
                kind,
                session_id: session_id.clone(),
                cursor: 0,
                html,
            });
        }
    }

    /// Pushes a refreshed sessions-list fragment to every page — the
    /// sessions region's guard render. The Sessions kind is
    /// signal-only at the wire (the client re-pulls the list with its
    /// active filter), so the envelope's payload field carries the
    /// current unread-mark count — the titlebar sessions button's
    /// live update — and no fragment is rendered here.
    pub(crate) fn push_sessions_refresh(&mut self, session_id: &SessionId) {
        let attention = {
            let guard = self.state.read();
            crate::regions::sessions::unread_count_of(&guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Sessions,
            session_id: session_id.clone(),
            cursor: 0,
            html: attention.to_string(),
        });
    }

    /// Pushes a refreshed sessions-list signal to **every** live
    /// session's stream — one addressed `sessions` envelope per live
    /// session id, the `push_models_changed` shape.
    ///
    /// The sessions signal is page-global (one list, every viewer),
    /// but the SSE endpoint filters per stream: an addressed push
    /// reaches only the page viewing that session, and the
    /// interesting mark transitions happen on pages viewing *other*
    /// sessions. So a set-or-clear transition loops every live
    /// session id — the payload is the unread-mark count, which does
    /// not vary by viewer.
    fn refresh_all_sessions_lists(&mut self) {
        let ids: Vec<SessionId> = self
            .state
            .read()
            .session
            .sessions()
            .keys()
            .cloned()
            .collect();
        let attention = {
            let guard = self.state.read();
            crate::regions::sessions::unread_count_of(&guard)
        };
        for session_id in ids {
            self.broadcast(SseEnvelope {
                kind: SseKind::Sessions,
                session_id,
                cursor: 0,
                html: attention.to_string(),
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
    /// correctness dependency. The transcript envelope is the
    /// boundary-aware render (cursor 0, full replace — the convergence
    /// shape): mid-turn it carries the settled prefix, so the swap
    /// never doubles the tail the streaming container shows.
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
            let transcript_html = self.settled_transcript_fragment(&session_id);
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
        // session's state. The render is boundary-aware — a setup
        // completing while a turn is in flight must not double the
        // tail.
        let session_id = &msg.session_id;
        let transcript_html = self.settled_transcript_fragment(session_id);
        self.broadcast(SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: session_id.clone(),
            cursor: 0,
            html: transcript_html,
        });
    }

    /// A chat entry landed in history: push the transcript-full
    /// convergence for the session. Synchronous work only; `async` is
    /// the `MsgHandler::handle` signature's shape, dropped here.
    ///
    /// This is the visibility path for a message queued while busy:
    /// its optimistic entry is wiped by the settle's convergence
    /// render before the real entry exists in history, and this push
    /// restores it at the dispatch that drains the queue — within one
    /// batch window, no reload.
    ///
    /// The render is boundary-aware (the settled fragment, not the
    /// whole history): a drain dispatching while the current turn's
    /// tail is still streaming must not swallow the in-flight entries
    /// the `streaming` pushes are showing, and must not re-render them
    /// into the settled region — that is the settle's job, one render,
    /// ordered after the boundary clear.
    ///
    /// It fires on ordinary idle-direct sends too (the same event);
    /// the boundary-aware fragment makes that a harmless overlap —
    /// exactly the settled region's current content — like the
    /// setup-completed path above.
    pub(crate) fn on_chat_entry_submitted(
        &mut self,
        msg: &jinn_chat_input_msg::ChatEntrySubmitted,
    ) {
        let transcript_html = self.settled_transcript_fragment(&msg.session_id);
        self.broadcast(SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: msg.session_id.clone(),
            cursor: 0,
            html: transcript_html,
        });
    }
}

/// The settle push order: every region's `settle_kinds` flattened in
/// registry order. The boundary clear (above) runs before these; the
/// transcript's convergence push precedes the composer's idle
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
    /// The settled boundary is captured at the Idle→working flip: the
    /// rendered-entry count of the history the projection write
    /// (which lands before the event, the production order) has not
    /// yet been joined by the turn's entries. Every subsequent flush
    /// tick renders only entries past it.
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
            // Turn start (Idle→working): capture the settled boundary
            // — the rendered count the history holds right now. The
            // boundary lives from here until the settle clears it.
            if working_flipped {
                let boundary = {
                    let guard = self.state.read();
                    guard.session.get(&msg.session_id).map_or(0, |session| {
                        session
                            .history()
                            .iter()
                            .filter(|e| crate::transcript::is_renderable(e))
                            .count()
                    })
                };
                self.boundaries.set(&msg.session_id, boundary);
                self.last_tail.remove(&msg.session_id);
            }
            self.push_controls_from_state(&msg.session_id);
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

    /// Pushes the composer's busy flag for a session, read from the
    /// phase the state holds — busy shows cancel/queue, idle shows
    /// send (signal-bound client-side; the fragment itself never
    /// re-renders). The phase-flip push and the no-live-turn cancel's
    /// settle path (via [`FanoutHandle::push_controls_from`], handed
    /// the same read) share one body, so the flag can never disagree
    /// between the two writers of the same kind.
    pub(crate) fn push_controls_from_state(&mut self, session_id: &SessionId) {
        let busy = {
            let guard = self.state.read();
            crate::regions::composer::busy_of(session_id, &guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Controls,
            session_id: session_id.clone(),
            cursor: 0,
            html: busy.to_string(),
        });
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
    ///
    /// Refresh duty only: the event fires once per restored session —
    /// background tree members included — so the list every page shows
    /// must re-render with the new row set. The unread **clear** lives
    /// on the two "the user looked" events below ([`MsgHandler for
    /// ActiveSessionChanged`] and [`MsgHandler for SessionPageOpened`]):
    /// keying it here would wipe the marks of tree members nobody
    /// viewed, the exact signal the mark exists to carry.
    pub(crate) fn on_load_completed(&mut self, msg: &SessionLoadCompleted) {
        // A session landed in the live map (a web click's disk load, a
        // TUI load, a tree hydration): refresh every page's list.
        self.push_sessions_refresh(&msg.session_id);
    }
}

impl MsgHandler<ActiveSessionChanged> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &ActiveSessionChanged, _ctx: &mut MsgCtx<'_>) {
        self.on_active_session_changed(msg);
    }
}

impl WebUiFanoutActor {
    /// The activation body, split out for the same test seam.
    ///
    /// Activation anywhere in the instance — the web click, the TUI
    /// switch, the store actor's in-memory branch — is "the user
    /// looked", so the event's session is exactly the session whose
    /// mark clears. No instance-pointer read: the event carries the
    /// viewed id. The clear runs BEFORE any fan so the wave carries
    /// the post-clear attention count — the swap seed and the
    /// stream-connect convergence patch both read it, and a wave
    /// stamped pre-clear would leave the navigating client a stale
    /// badge no later wave revisits. The transition gate keeps a
    /// re-clear quiet, and the all-sessions fan is what clears every
    /// OTHER page's stale yellow.
    pub(crate) fn on_active_session_changed(&mut self, msg: &ActiveSessionChanged) {
        let session_id = &msg.session_id;
        if self.clear_unread(session_id) {
            self.refresh_all_sessions_lists();
        } else {
            self.push_sessions_refresh(session_id);
        }
    }
}

impl MsgHandler<SessionPageOpened> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionPageOpened, _ctx: &mut MsgCtx<'_>) {
        self.on_session_page_opened(msg);
    }
}

impl WebUiFanoutActor {
    /// The arrival body, split out for the same test seam.
    ///
    /// A page opened the session (GET, reload, deep link — an archived
    /// session's page included, which activates nothing). The same
    /// clear-and-refresh shape the activation handler runs: opening
    /// the page IS the viewing.
    pub(crate) fn on_session_page_opened(&mut self, msg: &SessionPageOpened) {
        let session_id = &msg.session_id;
        if self.clear_unread(session_id) {
            self.refresh_all_sessions_lists();
        } else {
            self.push_sessions_refresh(session_id);
        }
    }
}

impl MsgHandler<SessionArchived> for WebUiFanoutActor {
    async fn handle(&mut self, msg: &SessionArchived, _ctx: &mut MsgCtx<'_>) {
        self.push_sessions_refresh(&msg.session_id);
    }
}

impl MsgHandler<SessionLibraryMembershipChanged> for WebUiFanoutActor {
    async fn handle(&mut self, _msg: &SessionLibraryMembershipChanged, _ctx: &mut MsgCtx<'_>) {
        // The membership change moves a row between views on every
        // surface viewing the list — not only the page viewing that
        // session — so the refresh is page-global like the mark
        // transitions, not addressed like the archive path.
        self.refresh_all_sessions_lists();
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
