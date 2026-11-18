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
//! flushes **first**, then a full-transcript fragment follows — the
//! ordering that keeps the last tokens from appearing twice (once
//! appended, once in the full render) and the convergence push that
//! heals any drift between an incremental page and the real history
//! (rewinds, cancels, partial-entry edge cases).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jinn_core_types::SessionId;
use jinn_inference_msg::StreamToken;
use jinn_kernel::State;
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
    /// guard drops before the broadcast (no await under it).
    fn flush_all(&mut self) {
        let ids: Vec<SessionId> = self.pending.keys().cloned().collect();
        for session_id in ids {
            let cursor = {
                let guard = self.state.read();
                guard
                    .session
                    .get(&session_id)
                    .map_or(0, |session| session.history().len())
            };
            self.flush_session(&session_id, cursor);
        }
    }

    /// Settles a session to idle: flush the partial token buffer FIRST,
    /// then a full-transcript fragment (cursor 0 — the client re-bases
    /// on it), then the idle controls. The ordering keeps the final
    /// tokens from appearing twice (once appended, once in the full
    /// render), and the full push is the convergence backstop that
    /// heals drift after rewinds, cancels, or partial-entry edges.
    pub(crate) fn settle_to_idle(&mut self, session_id: &SessionId) {
        self.flush_session(session_id, 0);
        let transcript = {
            let guard = self.state.read();
            crate::server::transcript_full_fragment(session_id, &guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::TranscriptFull,
            session_id: session_id.clone(),
            cursor: 0,
            html: transcript,
        });
        let controls = {
            let guard = self.state.read();
            crate::server::controls_fragment(session_id, &guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Controls,
            session_id: session_id.clone(),
            cursor: 0,
            html: controls,
        });
    }

    /// Pushes the busy-state controls fragment for a session.
    pub(crate) fn push_busy_controls(&mut self, session_id: &SessionId) {
        let html = {
            let guard = self.state.read();
            crate::server::controls_fragment(session_id, &guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Controls,
            session_id: session_id.clone(),
            cursor: 0,
            html,
        });
    }

    /// Pushes a refreshed sessions-list fragment to every page.
    pub(crate) fn push_sessions_refresh(&mut self, session_id: &SessionId) {
        let html = {
            let guard = self.state.read();
            crate::server::sessions_live_fragment(&guard)
        };
        self.broadcast(SseEnvelope {
            kind: SseKind::Sessions,
            session_id: session_id.clone(),
            cursor: 0,
            html,
        });
    }
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
