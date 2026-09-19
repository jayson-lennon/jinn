//! Session lifecycle and persistence actor (trouper port) - owns session state from input to streaming.
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

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::{BusService, jinn_domain_topic};
use crate::common::state::State;
use crate::feat::chat_input::protocol::command::{
    EnqueueResumeTurn, EnqueueUserMessage, SubmitSteeringMessage,
};
use crate::feat::context::protocol::command::LoadPersonaPickerEntries;
use crate::feat::context::protocol::event::PersonasLoaded;
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
use jinn_session_history_msg::{ChatEntryPinChanged, PinChatEntry, PushChatEntry, UnpinChatEntry};
use jinn_tools_msg::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted, ToolsRegistered, ToolsUnregistered,
};

/// The session actor's static trouper path.
pub const SESSION_PATH: &str = "session";

/// The session actor's mailbox capacity.
///
/// The session actor is the single sink for every streaming event
/// (`StreamToken`, `StreamCompleted`, tool events) from a provider burst.
/// A small mailbox could fill at the `[DONE]` peak of a large reasoning
/// turn and stall the pipeline; Block policy backpressures publishers
/// rather than dropping, so the terminal `StreamCompleted` is never lost.
pub const SESSION_MAILBOX_CAPACITY: usize = 65_536;

/// Session lifecycle and persistence actor.
///
/// Handles session-related commands and events, mutates [`State`],
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
    token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
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
    pub token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
    pub builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry,
    pub shell: String,
    pub image_converter: crate::feat::image_convert::ImageConverterService,
}

impl ServiceActor for SessionPersistenceActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `start_with`
        // (Deps carries typed handles that cannot ride JSON args).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("SessionPersistenceActor is spawned via start_with"),
        )
    }
}

impl SessionPersistenceActor {
    /// Spawns the actor at its static trouper path and subscribes it to the
    /// shared `jinn.domain` topic. Live on return: the subscription is the
    /// readiness point, so publishes after this call resolves cannot be
    /// missed (B4: the orchestrator's builtin registration in its own
    /// `start` lands in a running session actor).
    #[expect(
        clippy::expect_used,
        reason = "a failed topic subscription is a wiring bug that must abort launch"
    )]
    pub fn spawn(system: &ActorSystem, deps: SessionPersistenceActorDeps) -> ActorPath {
        let path = ActorPath::new(SESSION_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state: deps.state,
                            cap: deps.cap,
                            frontend_cap: deps.frontend_cap,
                            services: deps.deps.services,
                            counter: deps.counter,
                            token_cache: deps.token_cache,
                            builtin_registry: deps.builtin_registry,
                            shell: deps.shell,
                            lifecycle_child: None,
                            image_converter: deps.image_converter,
                        })
                    })
                }
            })
            // Persistence + picker.
            .handles::<SessionLoadRequested>()
            .handles::<LoadSessionPickerEntries>()
            .handles::<SessionForkRequested>()
            // Input & dispatch.
            .handles::<EnqueueUserMessage>()
            .handles::<SubmitSteeringMessage>()
            .handles::<EnqueueResumeTurn>()
            .handles::<PushChatEntry>()
            .handles::<SendMessage>()
            // Lifecycle commands.
            .handles::<RunSessionSetup>()
            .handles::<RunSessionTeardown>()
            .handles::<FinishSessionTeardown>()
            .handles::<FinishSessionSetup>()
            .handles::<CancelLifecycleCommand>()
            .handles::<SetSessionCwd>()
            .handles::<PersistSession>()
            .handles::<CloseSession>()
            .handles::<ArchiveSession>()
            .handles::<ArchiveSessionTree>()
            .handles::<TeardownSessionTree>()
            .handles::<SubmitHistoryMutations>()
            .handles::<MarkSessionInteracted>()
            .handles::<RetryStalledSession>()
            // The actor arms the in-flight-stream guard on dispatch receipt —
            // the single write point covering every `SendToLlmProvider`
            // publisher (user, queued/steered, direct, tool-loop, stall-retry).
            .handles::<SendToLlmProvider>()
            // Context-related.
            .handles::<PinChatEntry>()
            .handles::<UnpinChatEntry>()
            .handles::<LoadPersonaPickerEntries>()
            // Events.
            .handles::<StreamToken>()
            .handles::<StreamCompleted>()
            .handles::<ToolUseStarted>()
            .handles::<ToolCallReceived>()
            .handles::<ToolCallStreaming>()
            .handles::<ToolExecutionCompleted>()
            .handles::<ToolBatchCompleted>()
            .handles::<ToolExecutionStarted>()
            .handles::<ToolExecutionOutput>()
            .handles::<CitationsReceived>()
            .handles::<ChatEntryPinChanged>()
            .handles::<TaskListUpdated>()
            .handles::<ModelsRefreshed>()
            .handles::<SkillsLoaded>()
            .handles::<EnvironmentLoaded>()
            .handles::<ToolsRegistered>()
            .handles::<ToolsUnregistered>()
            .handles::<SessionClosed>()
            .handles::<PromptTemplatesLoaded>()
            .handles::<PersonasLoaded>()
            // Deep mailbox with Block: this actor is the single sink for
            // every streaming token burst. Block backpressures rather than
            // drops, so the terminal `StreamCompleted` can never be lost.
            .mailbox(SESSION_MAILBOX_CAPACITY, trouper::inbox::OverloadPolicy::Block)
            .start();
        // One topic suffices: trouper dispatches by schema id at the typed
        // adapter, and every publisher reaches this actor through the
        // `jinn.domain` default (BusService publishes and bridge closures).
        system
            .subscribe(&path, &jinn_domain_topic(), None)
            .expect("session actor subscribes the jinn.domain topic");
        path
    }
}

// ---------------------------------------------------------------------------
// Message handlers — direct handler calls
// ---------------------------------------------------------------------------

impl MsgHandler<SessionLoadRequested> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SessionLoadRequested, _ctx: &mut MsgCtx<'_>) {
        self.on_load_requested(&msg).await;
    }
}

impl MsgHandler<LoadSessionPickerEntries> for SessionPersistenceActor {
    async fn handle(&mut self, msg: LoadSessionPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_session_picker_entries(&msg).await;
    }
}

impl MsgHandler<SessionForkRequested> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SessionForkRequested, _ctx: &mut MsgCtx<'_>) {
        self.on_session_fork_requested(&msg).await;
    }
}

impl MsgHandler<EnqueueUserMessage> for SessionPersistenceActor {
    async fn handle(&mut self, msg: EnqueueUserMessage, _ctx: &mut MsgCtx<'_>) {
        self.handle_enqueue_user_message(&msg).await;
    }
}

impl MsgHandler<SubmitSteeringMessage> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SubmitSteeringMessage, _ctx: &mut MsgCtx<'_>) {
        self.handle_submit_steering_message(&msg);
    }
}

impl MsgHandler<EnqueueResumeTurn> for SessionPersistenceActor {
    async fn handle(&mut self, msg: EnqueueResumeTurn, _ctx: &mut MsgCtx<'_>) {
        self.handle_enqueue_resume_turn(&msg).await;
    }
}

impl MsgHandler<PushChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: PushChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_push_chat_entry(&msg).await;
    }
}

impl MsgHandler<SendMessage> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SendMessage, _ctx: &mut MsgCtx<'_>) {
        self.handle_send_message(&msg).await;
    }
}

impl MsgHandler<RunSessionSetup> for SessionPersistenceActor {
    async fn handle(&mut self, msg: RunSessionSetup, _ctx: &mut MsgCtx<'_>) {
        self.handle_run_session_setup(&msg).await;
    }
}

impl MsgHandler<RunSessionTeardown> for SessionPersistenceActor {
    async fn handle(&mut self, msg: RunSessionTeardown, _ctx: &mut MsgCtx<'_>) {
        self.handle_run_session_teardown(&msg).await;
    }
}

impl MsgHandler<FinishSessionTeardown> for SessionPersistenceActor {
    async fn handle(&mut self, msg: FinishSessionTeardown, _ctx: &mut MsgCtx<'_>) {
        self.handle_finish_session_teardown(&msg).await;
    }
}

impl MsgHandler<FinishSessionSetup> for SessionPersistenceActor {
    async fn handle(&mut self, msg: FinishSessionSetup, _ctx: &mut MsgCtx<'_>) {
        self.handle_finish_session_setup(&msg).await;
    }
}

impl MsgHandler<CancelLifecycleCommand> for SessionPersistenceActor {
    async fn handle(&mut self, msg: CancelLifecycleCommand, _ctx: &mut MsgCtx<'_>) {
        self.handle_cancel_lifecycle_command(&msg);
    }
}

impl MsgHandler<SetSessionCwd> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SetSessionCwd, _ctx: &mut MsgCtx<'_>) {
        self.handle_set_session_cwd(&msg).await;
    }
}

impl MsgHandler<PersistSession> for SessionPersistenceActor {
    async fn handle(&mut self, msg: PersistSession, _ctx: &mut MsgCtx<'_>) {
        self.handle_persist_session(&msg).await;
    }
}

impl MsgHandler<CloseSession> for SessionPersistenceActor {
    async fn handle(&mut self, msg: CloseSession, _ctx: &mut MsgCtx<'_>) {
        self.handle_close_session(&msg).await;
    }
}

impl MsgHandler<ArchiveSession> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ArchiveSession, _ctx: &mut MsgCtx<'_>) {
        self.handle_archive_session(&msg).await;
    }
}

impl MsgHandler<ArchiveSessionTree> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ArchiveSessionTree, _ctx: &mut MsgCtx<'_>) {
        self.handle_archive_session_tree(&msg).await;
    }
}

impl MsgHandler<TeardownSessionTree> for SessionPersistenceActor {
    async fn handle(&mut self, msg: TeardownSessionTree, _ctx: &mut MsgCtx<'_>) {
        self.handle_teardown_session_tree(&msg).await;
    }
}

impl MsgHandler<PinChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: PinChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_pin_chat_entry(&msg).await;
    }
}

impl MsgHandler<UnpinChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: UnpinChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_unpin_chat_entry(&msg).await;
    }
}

impl MsgHandler<LoadPersonaPickerEntries> for SessionPersistenceActor {
    async fn handle(&mut self, msg: LoadPersonaPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_persona_picker_entries(&msg);
    }
}

impl MsgHandler<MarkSessionInteracted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: MarkSessionInteracted, _ctx: &mut MsgCtx<'_>) {
        self.handle_mark_session_interacted(&msg).await;
    }
}

impl MsgHandler<SubmitHistoryMutations> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SubmitHistoryMutations, _ctx: &mut MsgCtx<'_>) {
        self.handle_submit_history_mutations(&msg).await;
    }
}

impl MsgHandler<RetryStalledSession> for SessionPersistenceActor {
    async fn handle(&mut self, msg: RetryStalledSession, _ctx: &mut MsgCtx<'_>) {
        self.on_retry_stalled_session(&msg).await;
    }
}

impl MsgHandler<SendToLlmProvider> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.on_send_to_llm_provider(&msg);
    }
}

// Event handlers

impl MsgHandler<StreamToken> for SessionPersistenceActor {
    async fn handle(&mut self, msg: StreamToken, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_token(&msg);
    }
}

impl MsgHandler<StreamCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_completed(&msg).await;
    }
}

impl MsgHandler<ToolUseStarted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolUseStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_use_started(&msg);
    }
}

impl MsgHandler<ToolCallReceived> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolCallReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_received(&msg);
    }
}

impl MsgHandler<ToolCallStreaming> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolCallStreaming, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_streaming(&msg);
    }
}

impl MsgHandler<ToolExecutionCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_completed(&msg).await;
    }
}

impl MsgHandler<ToolBatchCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolBatchCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_batch_completed(&msg).await;
    }
}

impl MsgHandler<ToolExecutionStarted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolExecutionStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_started(&msg);
    }
}

impl MsgHandler<ToolExecutionOutput> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolExecutionOutput, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_output(&msg);
    }
}

impl MsgHandler<CitationsReceived> for SessionPersistenceActor {
    async fn handle(&mut self, msg: CitationsReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_citations_received(&msg).await;
    }
}

impl MsgHandler<ModelsRefreshed> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ModelsRefreshed, _ctx: &mut MsgCtx<'_>) {
        self.on_models_refreshed(&msg);
    }
}

impl MsgHandler<SkillsLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SkillsLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_skills_loaded(&msg);
    }
}

impl MsgHandler<EnvironmentLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: EnvironmentLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_environment_loaded(&msg.config).await;
    }
}

impl MsgHandler<ChatEntryPinChanged> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ChatEntryPinChanged, _ctx: &mut MsgCtx<'_>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl MsgHandler<TaskListUpdated> for SessionPersistenceActor {
    async fn handle(&mut self, msg: TaskListUpdated, _ctx: &mut MsgCtx<'_>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl MsgHandler<ToolsRegistered> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolsRegistered, _ctx: &mut MsgCtx<'_>) {
        self.on_tools_registered(&msg);
    }
}

impl MsgHandler<ToolsUnregistered> for SessionPersistenceActor {
    async fn handle(&mut self, msg: ToolsUnregistered, _ctx: &mut MsgCtx<'_>) {
        self.on_tools_unregistered(&msg);
    }
}

/// Cleans the closed session's entry from the context tool cache — the map
/// the orchestrator's own cleanup does not reach (it prunes its routing map,
/// not the LLM-facing definitions cache).
impl MsgHandler<SessionClosed> for SessionPersistenceActor {
    async fn handle(&mut self, msg: SessionClosed, _ctx: &mut MsgCtx<'_>) {
        self.on_session_closed_cleanup(&msg.session_id);
    }
}

impl MsgHandler<PromptTemplatesLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: PromptTemplatesLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_prompt_templates_loaded(&msg);
    }
}

impl MsgHandler<PersonasLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: PersonasLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_personas_loaded(&msg);
    }
}
