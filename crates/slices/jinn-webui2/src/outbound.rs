//! The outbound actor: subscribes to the events a page shows, pushes frames.
//!
//! One `MsgHandler` per subscribed event, each a one-line delegation to a
//! handler in `handlers.rs`. Streamed text is not pushed per token: a
//! handler queues a chunk and the [`FlushTick`] — which the actor
//! schedules for itself — merges the queue into one `EntryDelta` per
//! entry and field. Everything else (an appended or rewritten entry, a
//! phase, the model, the session set) is pushed as it happens, read from
//! `State` at that moment.
//!
//! Every history-affecting event bumps the session's revision, which is
//! what lets a reconnecting page resume incrementally when — and only
//! when — nothing but appends happened since it last saw the transcript.

use std::sync::Arc;
use std::time::Duration;

use jinn_core_types::{ChatEntryId, SessionId};
use jinn_kernel::{State, common::actor_deps::BusPublish};
use jinn_slices::bus::BusService;
use jinn_webui2_msg::{DeltaField, ServerMsg};
use parking_lot::Mutex;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::system::ActorSystem;

use crate::{connections::ConnectionRegistry, frames::ToolCallIndex, revision::RevisionTracker};

/// The name the outbound actor is spawned under.
pub const OUTBOUND_ACTOR_NAME: &str = "webui2-outbound";

/// How often queued stream chunks are flushed to pages.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// The actor's self-addressed tick that flushes queued stream chunks.
///
/// A `Command`, never published on the bus: it is the actor scheduling a
/// message to itself, which is why it still carries the schema derive.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    trouper::schema::Command,
)]
#[schema(description = "Flush queued stream chunks to connected pages.")]
pub struct FlushTick;

impl jinn_slices::BusMessage for FlushTick {}

/// Everything the outbound actor reads or pushes through.
#[derive(Debug, Clone)]
pub struct OutboundActorDeps {
    /// The shared application state every frame is read from.
    pub state: State,
    /// The bus the actor is spawned on.
    pub bus: BusService,
    /// The open WebSocket connections.
    pub connections: ConnectionRegistry,
    /// The per-session history revisions.
    pub revisions: RevisionTracker,
}

/// One stream chunk waiting for the flush tick.
#[derive(Debug, Clone)]
pub struct PendingDelta {
    /// The session the entry belongs to.
    pub session_id: SessionId,
    /// The entry the chunk extends.
    pub entry_id: ChatEntryId,
    /// Which field the chunk extends.
    pub field: DeltaField,
    /// The text to append.
    pub chunk: String,
}

/// What the actor, the protocol dispatch and the listener share.
///
/// Every field is a cheap clone of an `Arc`. The dispatch and the socket
/// tasks reach the connections and the revisions through it directly, so
/// a page's replies never queue behind a busy actor.
#[derive(Debug, Clone)]
pub struct OutboundHandle {
    pub(crate) deps: OutboundActorDeps,
    /// The live turn's tool-call ids, by stream index.
    pub(crate) tool_calls: Arc<Mutex<ToolCallIndex>>,
    /// Chunks waiting for the next tick.
    pending: Arc<Mutex<Vec<PendingDelta>>>,
}

mod handlers;

/// The outbound actor.
#[derive(Debug)]
pub struct OutboundActor {
    handle: OutboundHandle,
    bus: BusService,
    /// What each streaming field looked like when last sent.
    sent: self::handlers::SentLengths,
}

impl ServiceActor for OutboundActor {
    // Never called: spawned via `init_with`. The `async` signature is the trait's.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the ServiceActor trait's init is async by definition; this actor spawns via init_with and init is a hard error"
    )]
    async fn init(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
        Err(
            error_stack::Report::new(trouper::registry::RegistryError::InvalidSpec)
                .attach("OutboundActor spawns via init_with"),
        )
    }
}

impl BusPublish for OutboundActor {
    fn bus(&self) -> &BusService {
        &self.bus
    }
}

impl MsgHandler<FlushTick> for OutboundActor {
    async fn handle(&mut self, _msg: &FlushTick, ctx: &mut MsgCtx<'_>) {
        self.handle.flush();
        ctx.schedule_self(FlushTick, FLUSH_INTERVAL);
    }
}

impl BusPublish for OutboundHandle {
    fn bus(&self) -> &BusService {
        &self.deps.bus
    }
}

impl OutboundHandle {
    /// Registers a fresh connection; returns the receiver its socket reads.
    pub fn register(&self) -> tokio::sync::mpsc::UnboundedReceiver<ServerMsg> {
        self.deps.connections.register()
    }

    /// Sends one frame to every open connection.
    pub fn broadcast(&self, msg: ServerMsg) {
        self.deps.connections.broadcast(msg);
    }

    /// The revision a session's history is at.
    #[must_use]
    pub fn revision(&self, session_id: &SessionId) -> u64 {
        self.deps.revisions.current(session_id)
    }

    /// Records that a session's history changed; returns the new revision.
    pub fn bump(&self, session_id: &SessionId) -> u64 {
        self.deps.revisions.bump(session_id)
    }

    /// The shared state, for the dispatch handlers' reads and writes.
    #[must_use]
    pub fn state(&self) -> &State {
        &self.deps.state
    }

    /// Queues a chunk for the next flush tick.
    pub(crate) fn queue_delta(&self, pending: PendingDelta) {
        self.pending.lock().push(pending);
    }

    /// Merges the queued chunks and pushes one frame per entry and field.
    pub(crate) fn flush(&self) {
        let queued = std::mem::take(&mut *self.pending.lock());
        for frame in crate::frames::coalesce(queued) {
            self.broadcast(frame);
        }
    }
}

/// Spawns the outbound actor and arms its flush tick.
#[must_use]
pub fn spawn(system: &ActorSystem, deps: OutboundActorDeps) -> OutboundHandle {
    let handle = OutboundHandle {
        deps,
        tool_calls: Arc::new(Mutex::new(ToolCallIndex::default())),
        pending: Arc::new(Mutex::new(Vec::new())),
    };
    let bus = handle.deps.bus.clone();
    let returned = handle.clone();

    trouper::builder::spawn_service_builder::<OutboundActor>(system)
        .at(ActorPath::new(OUTBOUND_ACTOR_NAME))
        .init_with(move || {
            let handle = handle.clone();
            let bus = bus.clone();
            Box::pin(async move {
                Ok(OutboundActor {
                    handle,
                    bus,
                    sent: self::handlers::SentLengths::default(),
                })
            })
        })
        .handles::<jinn_session_msg::SessionPhaseChanged>()
        .handles::<jinn_inference_msg::StreamToken>()
        .handles::<jinn_tools_msg::ToolUseStarted>()
        .handles::<jinn_tools_msg::ToolCallStreaming>()
        .handles::<jinn_tools_msg::ToolExecutionOutput>()
        .handles::<jinn_tools_msg::ToolCallReceived>()
        .handles::<jinn_tools_msg::ToolExecutionCompleted>()
        .handles::<jinn_session_history_msg::HistoryAppended>()
        .handles::<jinn_chat_input_msg::ChatEntrySubmitted>()
        .handles::<jinn_provider_selection_msg::ProviderSwitched>()
        .handles::<jinn_session_store_msg::SessionLoadCompleted>()
        .start();

    returned
}
