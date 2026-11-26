//! The outbound actor: subscribes to the events a page shows, pushes frames.
//!
//! One `MsgHandler` per subscribed event, each a one-line delegation to a
//! handler in `handlers.rs`. Streamed text is not pushed per token: a
//! handler queues a chunk and the [`OutboundFlushTick`] — which the actor
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

use jinn_core_types::SessionId;
use jinn_kernel::{State, common::actor_deps::BusPublish};
use jinn_slices::bus::BusService;
use jinn_webui2_msg::ServerMsg;
use parking_lot::Mutex;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::system::ActorSystem;

use crate::{connections::ConnectionRegistry, revision::RevisionTracker};

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
#[schema(
    description = "Flush queued stream chunks from the /web2 outbound actor to its connected pages."
)]
pub struct OutboundFlushTick;

impl jinn_slices::BusMessage for OutboundFlushTick {}

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
    /// The provider registry and API keys the model picker's rows are built from.
    pub services: jinn_kernel::Services,
    /// The provider slice's cell: the last known model cache.
    pub provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
    /// The session store, for the stored-session picker's rows.
    pub session_store: jinn_session_state::SessionStoreService,
    /// The persona slice's cell: the scanned persona catalog.
    pub personas_cell: jinn_slices::TypedCell<jinn_persona_msg::Personas>,
    /// The MCP slice's runtime cell: the live connection status projection.
    pub mcp_runtime_cell: jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState>,
    /// The live configuration layer, for the `[mcp.<name>]` catalog.
    pub config: jinn_config::ConfigLayer,
}

/// Which modal a remembered filter belongs to.
///
/// One session can have one modal open at a time in practice, but the
/// server does not know which; remembering per kind means a push for one
/// modal can never answer another's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RowsKind {
    Skills,
    Tools,
    Mcp,
    Persona,
    Model,
}

/// What the actor, the protocol dispatch and the listener share.
///
/// Every field is a cheap clone of an `Arc`. The dispatch and the socket
/// tasks reach the connections and the revisions through it directly, so
/// a page's replies never queue behind a busy actor.
#[derive(Debug, Clone)]
pub struct OutboundHandle {
    deps: OutboundActorDeps,
    /// What each entry looked like when it last reached a page. Shared by
    /// the actor (which streams) and the dispatch (which answers a fresh
    /// page with a snapshot), so neither re-sends what the other delivered.
    sent: Arc<Mutex<handlers::SentLengths>>,
    /// The filter each session's modal of each kind last asked for, so a
    /// discovery push can answer with the view the page is holding. The
    /// dispatch records it; the actor reads it.
    row_filters: Arc<Mutex<std::collections::HashMap<(SessionId, RowsKind), String>>>,
    /// The queue depth each session's pages were last told, so the tick
    /// pushes a frame only when the depth has actually changed.
    told_queue_lens: Arc<Mutex<std::collections::HashMap<SessionId, usize>>>,
}

impl OutboundHandle {
    /// The per-session revision counters, read when a fresh page's
    /// snapshot is built so its revision matches what streaming will
    /// carry forward.
    #[must_use]
    pub fn revisions(&self) -> &crate::revision::RevisionTracker {
        &self.deps.revisions
    }

    /// Records the filter a session's modal of one kind last asked for.
    pub(crate) fn remember_filter(&self, session_id: &SessionId, kind: RowsKind, filter: &str) {
        self.row_filters
            .lock()
            .insert((session_id.clone(), kind), filter.to_owned());
    }

    /// The filter a session's modal of one kind last asked for — the empty
    /// filter when it never asked.
    #[must_use]
    pub(crate) fn last_filter(&self, session_id: &SessionId, kind: RowsKind) -> String {
        self.row_filters
            .lock()
            .get(&(session_id.clone(), kind))
            .cloned()
            .unwrap_or_default()
    }

    /// Every `(session, filter)` a modal of one kind last asked for.
    ///
    /// What a push iterates: one frame per session that has a modal of that
    /// kind open, in the filter it is holding.
    pub(crate) fn filters_for_kind(&self, kind: RowsKind) -> Vec<(SessionId, String)> {
        self.row_filters
            .lock()
            .iter()
            .filter(|((_, remembered), _)| *remembered == kind)
            .map(|((session_id, _), filter)| (session_id.clone(), filter.clone()))
            .collect()
    }
}

mod handlers;

#[cfg(test)]
pub(crate) use handlers::{
    flush, follow_active_session, library_membership_changed, mcp_runtime_stored,
    model_cache_stored, reconcile, skills_loaded,
};

/// The outbound actor.
#[derive(Debug)]
pub struct OutboundActor {
    handle: OutboundHandle,
    bus: BusService,
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

impl MsgHandler<OutboundFlushTick> for OutboundActor {
    async fn handle(&mut self, _msg: &OutboundFlushTick, ctx: &mut MsgCtx<'_>) {
        self.flush_streaming();
        ctx.schedule_self(OutboundFlushTick, FLUSH_INTERVAL);
    }
}

impl OutboundHandle {
    /// A handle over the given deps, with empty streaming bookkeeping.
    ///
    /// The one constructor: the spawned actor and a test harness build the
    /// same shape, so neither can leave a field unset.
    #[must_use]
    pub fn new(deps: OutboundActorDeps) -> Self {
        Self {
            deps,
            sent: Arc::new(Mutex::new(handlers::SentLengths::default())),
            row_filters: Arc::new(Mutex::new(std::collections::HashMap::new())),
            told_queue_lens: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Records that every entry of a session reached a page as it stands.
    ///
    /// Called by the snapshot path, whose reply replaces the page's view
    /// wholesale: what it carried must not be streamed or reconciled again.
    pub(crate) fn mark_delivered(&self, session_id: &SessionId) {
        let history = crate::frames::history(&self.deps.state, session_id);
        self.sent
            .lock()
            .remember_all(history.iter().map(|(_, entry)| entry));
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
    pub fn broadcast(&self, msg: &ServerMsg) {
        self.deps.connections.broadcast(msg);
    }

    /// Whether no page is connected — the flush tick then has no reader.
    #[must_use]
    pub fn no_connections(&self) -> bool {
        self.deps.connections.is_empty()
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

    /// The provider registry and API keys the model picker reads.
    #[must_use]
    pub fn services(&self) -> &jinn_kernel::Services {
        &self.deps.services
    }

    /// The session store, for the stored-session picker's rows.
    #[must_use]
    pub fn session_store(&self) -> &jinn_session_state::SessionStoreService {
        &self.deps.session_store
    }

    /// The provider slice's cell (the last known model cache).
    #[must_use]
    pub fn provider_cell(
        &self,
    ) -> &jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell> {
        &self.deps.provider_cell
    }

    /// The persona slice's cell (the scanned persona catalog).
    #[must_use]
    pub fn personas_cell(&self) -> &jinn_slices::TypedCell<jinn_persona_msg::Personas> {
        &self.deps.personas_cell
    }

    /// The MCP slice's runtime cell (the live connection status projection).
    #[must_use]
    pub fn mcp_runtime_cell(&self) -> &jinn_slices::TypedCell<jinn_mcp_msg::McpRuntimeState> {
        &self.deps.mcp_runtime_cell
    }

    /// The live configuration layer (the `[mcp.<name>]` catalog).
    #[must_use]
    pub fn config(&self) -> &jinn_config::ConfigLayer {
        &self.deps.config
    }
}

/// Spawns the outbound actor and arms its flush tick.
#[must_use]
pub fn spawn(system: &ActorSystem, deps: OutboundActorDeps) -> OutboundHandle {
    let bus = deps.bus.clone();
    let handle = OutboundHandle::new(deps);
    let returned = handle.clone();

    trouper::builder::spawn_service_builder::<OutboundActor>(system)
        .at(ActorPath::new(OUTBOUND_ACTOR_NAME))
        .init_with(move || {
            let handle = handle.clone();
            let bus = bus.clone();
            Box::pin(async move { Ok(OutboundActor { handle, bus }) })
        })
        .handles::<OutboundFlushTick>()
        .handles::<jinn_session_msg::SessionPhaseChanged>()
        .handles::<jinn_tools_msg::ToolUseStarted>()
        .handles::<jinn_tools_msg::ToolCallReceived>()
        .handles::<jinn_tools_msg::ToolExecutionCompleted>()
        .handles::<jinn_session_history_msg::HistoryAppended>()
        .handles::<jinn_chat_input_msg::ChatEntrySubmitted>()
        .handles::<jinn_provider_selection_msg::ProviderSwitched>()
        .handles::<jinn_session_store_msg::SessionLoadCompleted>()
        .handles::<jinn_skills_msg::SkillsLoaded>()
        // The coordinator emits this strictly after every runtime-cell
        // status write, so a handler reading the cell now sees the
        // transition. Never the raw `McpServerStatus` facts: those have
        // two publishers and can land before the write.
        .handles::<jinn_mcp_msg::McpRuntimeStored>()
        // The merged model cache landed after a refresh: an open model
        // picker receives fresh rows. Read the cell, never the event.
        .handles::<jinn_provider_selection_msg::ModelCacheStored>()
        // The store actor wrote a membership: both sidebar lists re-read.
        .handles::<jinn_session_store_msg::SessionLibraryMembershipChanged>()
        // The instance's active session moved — from this page, another
        // page, the TUI, or a subagent. Every page follows with a
        // snapshot. Never `SessionCreated`: that has several publishers,
        // and a bot session's creation must not move the page.
        .handles::<jinn_kernel::protocol::system::ActiveSessionChanged>()
        // Arms the first flush; the chain self-perpetuates from there (the
        // tick's handler schedules the next one). Without this the tick
        // never starts and no delta ever leaves the actor.
        .kickoff(OutboundFlushTick, Duration::ZERO)
        .start();

    returned
}
