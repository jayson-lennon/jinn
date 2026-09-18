//! Session lifecycle and persistence actor - owns session state from input to streaming.
//!
//! This actor is the **sole owner** of session-related state: chat history, input
//! buffers, session phase transitions, tool call state, and streaming tokens. It
//! also handles persisting sessions to disk and restoring them on load.
//!
//! # State ownership
//!
//! This actor **owns** the following `AppState` fields:
//! - session history (entries, tool calls, streaming state)
//! - session input buffers
//! - session phase (idle → sending → streaming → idle)
//! - `active_session`, `session_load_guard`
//!
//! # Lock discipline
//!
//! All handlers follow the same pattern: acquire state lock → mutate → release →
//! then emit. Never hold the lock during emission.

mod handlers;
mod helpers;

pub use handlers::lifecycle::setup_running_msg;

pub use handlers::multimodal_gate::evaluate_attachment_gate;

use kameo::prelude::{Actor, ActorRef, Context, Message};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::feat::chat_input::protocol::command::{
    EnqueueResumeTurn, EnqueueUserMessage, PushChatEntry, SubmitSteeringMessage,
};
use crate::feat::context::protocol::command::{
    LoadPersonaPickerEntries, PinChatEntry, UnpinChatEntry,
};
use crate::feat::context::protocol::event::{ChatEntryPinChanged, PersonasLoaded};
use crate::feat::context::strategy::token_estimator::TiktokenCounter;
use crate::feat::provider::protocol::command::SendMessage;
use crate::feat::provider::protocol::event::{ModelsRefreshed, PromptTemplatesLoaded};
use crate::feat::session::protocol::archive_session::ArchiveSession;
use crate::feat::session::protocol::archive_session_tree::ArchiveSessionTree;
use crate::feat::session::protocol::citations_received::CitationsReceived;
use crate::feat::session::protocol::close_session::CloseSession;
use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;
use crate::feat::session::protocol::retry_stalled_session::RetryStalledSession;
use crate::feat::session::protocol::session_closed::SessionClosed;
use crate::feat::session::protocol::session_fork_requested::SessionForkRequested;
use crate::feat::session::protocol::session_load_requested::SessionLoadRequested;
use crate::feat::session::protocol::submit_history_mutations::SubmitHistoryMutations;
use crate::feat::session::protocol::task_list_updated::TaskListUpdated;
use crate::feat::session::protocol::teardown_session_tree::TeardownSessionTree;
use crate::feat::session_lifecycle::protocol::command::PersistSession;
use crate::feat::session_lifecycle::protocol::command::{
    CancelLifecycleCommand, FinishSessionSetup, FinishSessionTeardown, RunSessionSetup,
    RunSessionTeardown, SetSessionCwd,
};
use crate::feat::skills::SkillsLoaded;
use crate::init::EnvironmentLoaded;
use jinn_inference_msg::{SendToLlmProvider, StreamCompleted, StreamToken};
use jinn_tools_msg::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted, ToolsRegistered, ToolsUnregistered,
};

/// Session lifecycle and persistence actor.
///
/// Subscribes to session-related commands and events, mutates [`State`],
/// and emits new commands and events via the message bus.
/// Also persists session snapshots to disk when session state changes.
pub struct SessionPersistenceActor {
    state: State,
    cap: crate::common::tcaps::session::SessionCap,
    frontend_cap: crate::common::tcaps::frontend::FrontendCap,
    /// Runtime services (user preferences storage for startup config loading).
    services: crate::common::services::Services,
    /// Token counter for recording token usage in the session ledger.
    counter: TiktokenCounter,
    /// Auto-pruner entry token cache, shared with the prune workers. Used by the
    /// accumulation gate's token-cost resolver (cache hit is the common path).
    token_cache: crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache,
    /// Registry of builtin lifecycle handlers.
    builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry,
    /// Shell captured at startup for running lifecycle commands.
    shell: String,
    /// Handle for cancelling a currently running lifecycle shell process.
    /// `None` when no lifecycle command is in flight. Carries the process-group
    /// PID (for kill) and the inner reader's `AbortHandle` (so aborting it
    /// surfaces the existing "... was cancelled" branch in the outer wrapper).
    lifecycle_child: Option<crate::feat::session_lifecycle::command_runner::LifecycleCancelHandle>,
    /// Image converter (ImageMagick) for transcoding non-native image
    /// attachments. Wraps a trait object so tests inject fakes.
    image_converter: crate::feat::image_convert::ImageConverterService,
}

impl BusPublish for SessionPersistenceActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

#[derive(Clone)]
pub struct SessionPersistenceActorDeps {
    pub deps: ActorDeps,
    pub state: State,
    pub cap: crate::common::tcaps::session::SessionCap,
    pub frontend_cap: crate::common::tcaps::frontend::FrontendCap,
    pub counter: TiktokenCounter,
    /// Auto-pruner entry token cache for the accumulation gate.
    pub token_cache: crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache,
    pub builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry,
    pub shell: String,
    pub image_converter: crate::feat::image_convert::ImageConverterService,
}

impl Actor for SessionPersistenceActor {
    type Args = SessionPersistenceActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        let bus = &args.deps.services.bus;

        // Persistence subscriptions.
        bus.subscribe::<SessionLoadRequested, _>(&actor_ref).await;
        bus.subscribe::<LoadSessionPickerEntries, _>(&actor_ref)
            .await;
        bus.subscribe::<SessionForkRequested, _>(&actor_ref).await;

        // Session lifecycle subscriptions.
        bus.subscribe::<EnqueueUserMessage, _>(&actor_ref).await;
        bus.subscribe::<SubmitSteeringMessage, _>(&actor_ref).await;
        bus.subscribe::<EnqueueResumeTurn, _>(&actor_ref).await;
        bus.subscribe::<PushChatEntry, _>(&actor_ref).await;
        bus.subscribe::<SendMessage, _>(&actor_ref).await;

        // Lifecycle command subscriptions.
        bus.subscribe::<RunSessionSetup, _>(&actor_ref).await;
        bus.subscribe::<RunSessionTeardown, _>(&actor_ref).await;
        bus.subscribe::<FinishSessionTeardown, _>(&actor_ref).await;
        bus.subscribe::<FinishSessionSetup, _>(&actor_ref).await;
        bus.subscribe::<CancelLifecycleCommand, _>(&actor_ref).await;
        bus.subscribe::<SetSessionCwd, _>(&actor_ref).await;

        bus.subscribe::<PersistSession, _>(&actor_ref).await;
        bus.subscribe::<CloseSession, _>(&actor_ref).await;
        bus.subscribe::<ArchiveSession, _>(&actor_ref).await;
        bus.subscribe::<ArchiveSessionTree, _>(&actor_ref).await;
        bus.subscribe::<TeardownSessionTree, _>(&actor_ref).await;
        bus.subscribe::<SubmitHistoryMutations, _>(&actor_ref).await;
        bus.subscribe::<MarkSessionInteracted, _>(&actor_ref).await;
        bus.subscribe::<RetryStalledSession, _>(&actor_ref).await;

        // The actor arms the in-flight-stream guard on dispatch receipt —
        // the single write point covering every `SendToLlmProvider`
        // publisher (user, queued/steered, direct, tool-loop, stall-retry).
        bus.subscribe::<SendToLlmProvider, _>(&actor_ref).await;

        // Context-related subscriptions.
        bus.subscribe::<PinChatEntry, _>(&actor_ref).await;
        bus.subscribe::<UnpinChatEntry, _>(&actor_ref).await;
        bus.subscribe::<LoadPersonaPickerEntries, _>(&actor_ref)
            .await;

        // Event subscriptions.
        bus.subscribe::<StreamToken, _>(&actor_ref).await;
        bus.subscribe::<StreamCompleted, _>(&actor_ref).await;
        bus.subscribe::<ToolUseStarted, _>(&actor_ref).await;
        bus.subscribe::<ToolCallReceived, _>(&actor_ref).await;
        bus.subscribe::<ToolCallStreaming, _>(&actor_ref).await;
        bus.subscribe::<ToolExecutionCompleted, _>(&actor_ref).await;
        bus.subscribe::<ToolBatchCompleted, _>(&actor_ref).await;
        bus.subscribe::<ToolExecutionStarted, _>(&actor_ref).await;
        bus.subscribe::<ToolExecutionOutput, _>(&actor_ref).await;
        bus.subscribe::<CitationsReceived, _>(&actor_ref).await;
        bus.subscribe::<ChatEntryPinChanged, _>(&actor_ref).await;
        bus.subscribe::<TaskListUpdated, _>(&actor_ref).await;
        bus.subscribe::<ModelsRefreshed, _>(&actor_ref).await;
        bus.subscribe::<SkillsLoaded, _>(&actor_ref).await;
        bus.subscribe::<EnvironmentLoaded, _>(&actor_ref).await;
        bus.subscribe::<ToolsRegistered, _>(&actor_ref).await;
        bus.subscribe::<ToolsUnregistered, _>(&actor_ref).await;
        bus.subscribe::<SessionClosed, _>(&actor_ref).await;
        bus.subscribe::<PromptTemplatesLoaded, _>(&actor_ref).await;
        bus.subscribe::<PersonasLoaded, _>(&actor_ref).await;

        Ok(Self {
            state: args.state,
            cap: args.cap,
            frontend_cap: args.frontend_cap,
            services: args.deps.services,
            counter: args.counter,
            token_cache: args.token_cache,
            builtin_registry: args.builtin_registry,
            shell: args.shell,
            lifecycle_child: None,
            image_converter: args.image_converter,
        })
    }
}

// ---------------------------------------------------------------------------
// Message handlers — direct handler calls
// ---------------------------------------------------------------------------

impl Message<SessionLoadRequested> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SessionLoadRequested, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_load_requested(&msg).await;
    }
}

impl Message<LoadSessionPickerEntries> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: LoadSessionPickerEntries,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        self.handle_load_session_picker_entries(&msg).await;
    }
}

impl Message<SessionForkRequested> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SessionForkRequested, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_session_fork_requested(&msg).await;
    }
}

impl Message<EnqueueUserMessage> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: EnqueueUserMessage, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_enqueue_user_message(&msg).await;
    }
}

impl Message<SubmitSteeringMessage> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SubmitSteeringMessage, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_submit_steering_message(&msg);
    }
}

impl Message<EnqueueResumeTurn> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: EnqueueResumeTurn, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_enqueue_resume_turn(&msg).await;
    }
}

impl Message<PushChatEntry> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: PushChatEntry, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_push_chat_entry(&msg).await;
    }
}

impl Message<SendMessage> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SendMessage, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_send_message(&msg).await;
    }
}

impl Message<RunSessionSetup> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: RunSessionSetup, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_run_session_setup(&msg).await;
    }
}

impl Message<RunSessionTeardown> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: RunSessionTeardown, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_run_session_teardown(&msg).await;
    }
}

impl Message<FinishSessionTeardown> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: FinishSessionTeardown, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_finish_session_teardown(&msg).await;
    }
}

impl Message<FinishSessionSetup> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: FinishSessionSetup, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_finish_session_setup(&msg).await;
    }
}

impl Message<CancelLifecycleCommand> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: CancelLifecycleCommand, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_cancel_lifecycle_command(&msg);
    }
}

impl Message<SetSessionCwd> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SetSessionCwd, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_set_session_cwd(&msg).await;
    }
}

impl Message<PersistSession> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: PersistSession, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_persist_session(&msg).await;
    }
}

impl Message<CloseSession> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: CloseSession, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_close_session(&msg).await;
    }
}

impl Message<ArchiveSession> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ArchiveSession, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_archive_session(&msg).await;
    }
}

impl Message<ArchiveSessionTree> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ArchiveSessionTree, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_archive_session_tree(&msg).await;
    }
}

impl Message<TeardownSessionTree> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: TeardownSessionTree, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_teardown_session_tree(&msg).await;
    }
}

impl Message<PinChatEntry> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: PinChatEntry, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_pin_chat_entry(&msg).await;
    }
}

impl Message<UnpinChatEntry> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: UnpinChatEntry, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_unpin_chat_entry(&msg).await;
    }
}

impl Message<LoadPersonaPickerEntries> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: LoadPersonaPickerEntries,
        _ctx: &mut Context<Self, Self::Reply>,
    ) {
        self.handle_load_persona_picker_entries(&msg);
    }
}

impl Message<MarkSessionInteracted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: MarkSessionInteracted, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_mark_session_interacted(&msg).await;
    }
}

impl Message<SubmitHistoryMutations> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SubmitHistoryMutations, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_submit_history_mutations(&msg).await;
    }
}

impl Message<RetryStalledSession> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: RetryStalledSession, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_retry_stalled_session(&msg).await;
    }
}

impl Message<SendToLlmProvider> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SendToLlmProvider, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_send_to_llm_provider(&msg);
    }
}

// Event handlers

impl Message<StreamToken> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: StreamToken, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_stream_token(&msg);
    }
}

impl Message<StreamCompleted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: StreamCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_stream_completed(&msg).await;
    }
}

impl Message<ToolUseStarted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolUseStarted, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_use_started(&msg);
    }
}

impl Message<ToolCallReceived> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolCallReceived, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_call_received(&msg);
    }
}

impl Message<ToolCallStreaming> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolCallStreaming, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_call_streaming(&msg);
    }
}

impl Message<ToolExecutionCompleted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolExecutionCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_execution_completed(&msg).await;
    }
}

impl Message<ToolBatchCompleted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolBatchCompleted, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_batch_completed(&msg).await;
    }
}

impl Message<ToolExecutionStarted> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolExecutionStarted, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_execution_started(&msg);
    }
}

impl Message<ToolExecutionOutput> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolExecutionOutput, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tool_execution_output(&msg);
    }
}

impl Message<CitationsReceived> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: CitationsReceived, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_citations_received(&msg).await;
    }
}

impl Message<ModelsRefreshed> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ModelsRefreshed, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_models_refreshed(&msg);
    }
}

impl Message<SkillsLoaded> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SkillsLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_skills_loaded(&msg);
    }
}

impl Message<EnvironmentLoaded> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: EnvironmentLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_environment_loaded(&msg.config).await;
    }
}

impl Message<ChatEntryPinChanged> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ChatEntryPinChanged, _ctx: &mut Context<Self, Self::Reply>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl Message<TaskListUpdated> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: TaskListUpdated, _ctx: &mut Context<Self, Self::Reply>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl Message<ToolsRegistered> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolsRegistered, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tools_registered(&msg);
    }
}

impl Message<ToolsUnregistered> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: ToolsUnregistered, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_tools_unregistered(&msg);
    }
}

/// Cleans the closed session's entry from the context tool cache — the map
/// the orchestrator's own cleanup does not reach (it prunes its routing map,
/// not the LLM-facing definitions cache).
impl Message<SessionClosed> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_session_closed_cleanup(&msg.session_id);
    }
}

impl Message<PromptTemplatesLoaded> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: PromptTemplatesLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_prompt_templates_loaded(&msg);
    }
}

impl Message<PersonasLoaded> for SessionPersistenceActor {
    type Reply = ();
    async fn handle(&mut self, msg: PersonasLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_personas_loaded(&msg);
    }
}
