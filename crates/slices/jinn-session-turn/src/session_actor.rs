//! Session actor (trouper port) — owns turn progression and context state.
//!
//! This actor is the **sole owner** of the turn/context half of session
//! state: chat history, input buffers, session phase transitions, tool call
//! state, streaming tokens, and the context caches. Session persistence and
//! loading live in the `jinn-session-store` slice; scripted setup, teardown,
//! close, and working-directory changes live in the `jinn-session-lifecycle`
//! slice. This actor still persists on the turn path (enqueue, streaming,
//! tool calls) through the shared session-store service.
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

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_chat_input_msg::{EnqueueResumeTurn, EnqueueUserMessage, SubmitSteeringMessage};
use jinn_inference_msg::{CancelTurn, SendToLlmProvider, StreamCompleted, StreamToken};
use jinn_kernel::PromptTemplatesLoaded;
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::common::state::State;
use jinn_llm_support::token_estimator::TiktokenCounter;
use jinn_persona_msg::PersonasLoaded;
use jinn_session_history_msg::CitationsReceived;
use jinn_session_history_msg::SubmitHistoryMutations;
use jinn_session_history_msg::TaskListUpdated;
use jinn_session_history_msg::{ChatEntryPinChanged, PinChatEntry, PushChatEntry, UnpinChatEntry};
use jinn_session_msg::{MarkSessionInteracted, RetryStalledSession, TurnCompleted};
use jinn_skills_msg::SkillsLoaded;
use jinn_tools_msg::{
    ToolBatchCompleted, ToolCallReceived, ToolCallStreaming, ToolExecutionCompleted,
    ToolExecutionOutput, ToolExecutionStarted, ToolUseStarted,
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

/// Constructs a default auto-pruner entry token cache.
///
/// The cache type lives in a msg crate `jinn-tools` does not depend on;
/// this constructor spares cross-crate test wiring.
#[cfg(test)]
pub fn default_token_cache() -> jinn_token_count_msg::HistoryWorkerChatEntryTokenCache {
    jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default()
}

/// Session turn-and-context actor.
///
/// Handles the turn-progression and context commands/events, mutates [`State`],
/// and emits new commands and events via the message bus. Persists session
/// snapshots through the session-store service when turn state changes.
pub struct SessionPersistenceActor {
    state: State,
    /// Runtime services (the session store and the bus).
    services: jinn_kernel::common::services::Services,
    /// Token counter for recording token usage in the session ledger.
    counter: TiktokenCounter,
    /// Auto-pruner entry token cache, shared with the prune workers. Used by the
    /// accumulation gate's token-cost resolver (cache hit is the common path).
    token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
    /// Image converter (ImageMagick) for transcoding non-native image
    /// attachments. Wraps a trait object so tests inject fakes.
    image_converter: jinn_llm_support::image_convert::ImageConverterService,
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
    pub counter: TiktokenCounter,
    /// Auto-pruner entry token cache for the accumulation gate.
    pub token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
    pub image_converter: jinn_llm_support::image_convert::ImageConverterService,
}

impl ServiceActor for SessionPersistenceActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<RegistryError>> {
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
    /// # Panics
    ///
    /// Panics if the actor's path is already taken or its topic
    /// subscription fails — both mean a wiring bug at composition.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
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
                            services: deps.deps.services,
                            counter: deps.counter,
                            token_cache: deps.token_cache,
                            image_converter: deps.image_converter,
                        })
                    })
                }
            })
            // Input & dispatch.
            .handles::<EnqueueUserMessage>()
            .handles::<SubmitSteeringMessage>()
            .handles::<EnqueueResumeTurn>()
            .handles::<PushChatEntry>()
            .handles::<SubmitHistoryMutations>()
            .handles::<MarkSessionInteracted>()
            .handles::<RetryStalledSession>()
            // The actor arms the in-flight-stream guard on dispatch receipt —
            // the single write point covering every `SendToLlmProvider`
            // publisher (user, queued/steered, direct, tool-loop, stall-retry).
            .handles::<SendToLlmProvider>()
            // The cancel command is a broadcast: the inference actor stops the
            // stream on it and this actor reports the turn's end. Splitting
            // those two jobs across two commands is what let a partial cancel
            // leave a session wedged with no turn-end signal.
            .handles::<CancelTurn>()
            // Context-related.
            .handles::<PinChatEntry>()
            .handles::<UnpinChatEntry>()
            // Events (also broadcast targets — every publish of these
            // schemas reaches this actor, whatever slice emitted it).
            .handles::<StreamToken>()
            .handles::<StreamCompleted>()
            .handles::<TurnCompleted>()
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
            .handles::<SkillsLoaded>()
            .handles::<PromptTemplatesLoaded>()
            .handles::<PersonasLoaded>()
            // Deep mailbox with Block: this actor is the single sink for
            // every streaming token burst. Block backpressures rather than
            // drops, so the terminal `StreamCompleted` can never be lost.
            .mailbox(
                SESSION_MAILBOX_CAPACITY,
                trouper::inbox::OverloadPolicy::Block,
            )
            .start();
        path
    }
}

// ---------------------------------------------------------------------------
// Message handlers — direct handler calls
// ---------------------------------------------------------------------------

impl MsgHandler<EnqueueUserMessage> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &EnqueueUserMessage, _ctx: &mut MsgCtx<'_>) {
        self.handle_enqueue_user_message(msg).await;
    }
}

impl MsgHandler<SubmitSteeringMessage> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &SubmitSteeringMessage, _ctx: &mut MsgCtx<'_>) {
        self.handle_submit_steering_message(msg);
    }
}

impl MsgHandler<EnqueueResumeTurn> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &EnqueueResumeTurn, _ctx: &mut MsgCtx<'_>) {
        self.handle_enqueue_resume_turn(msg).await;
    }
}

impl MsgHandler<PushChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &PushChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_push_chat_entry(msg).await;
    }
}

impl MsgHandler<PinChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &PinChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_pin_chat_entry(msg).await;
    }
}

impl MsgHandler<UnpinChatEntry> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &UnpinChatEntry, _ctx: &mut MsgCtx<'_>) {
        self.handle_unpin_chat_entry(msg).await;
    }
}

impl MsgHandler<MarkSessionInteracted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &MarkSessionInteracted, _ctx: &mut MsgCtx<'_>) {
        self.handle_mark_session_interacted(msg).await;
    }
}

impl MsgHandler<SubmitHistoryMutations> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &SubmitHistoryMutations, _ctx: &mut MsgCtx<'_>) {
        self.handle_submit_history_mutations(msg).await;
    }
}

impl MsgHandler<RetryStalledSession> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &RetryStalledSession, _ctx: &mut MsgCtx<'_>) {
        self.on_retry_stalled_session(msg).await;
    }
}

impl MsgHandler<SendToLlmProvider> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.on_send_to_llm_provider(msg);
    }
}

// Event handlers

impl MsgHandler<StreamToken> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &StreamToken, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_token(msg);
    }
}

impl MsgHandler<StreamCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_completed(msg).await;
    }
}

impl MsgHandler<CancelTurn> for SessionPersistenceActor {
    /// The single settle entry point for a cancelled turn.
    ///
    /// Broadcast alongside the inference actor's own handling of the same
    /// message, which stops the stream and publishes nothing. One actor owns
    /// the report, so a cancel cannot settle the session twice.
    async fn handle(&mut self, msg: &CancelTurn, _ctx: &mut MsgCtx<'_>) {
        self.on_cancel_turn(msg).await;
    }
}

impl MsgHandler<TurnCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_completed(msg).await;
    }
}

impl MsgHandler<ToolUseStarted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolUseStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_use_started(msg);
    }
}

impl MsgHandler<ToolCallReceived> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolCallReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_received(msg);
    }
}

impl MsgHandler<ToolCallStreaming> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolCallStreaming, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_call_streaming(msg);
    }
}

impl MsgHandler<ToolExecutionCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_completed(msg).await;
    }
}

impl MsgHandler<ToolBatchCompleted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolBatchCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_batch_completed(msg).await;
    }
}

impl MsgHandler<ToolExecutionStarted> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolExecutionStarted, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_started(msg);
    }
}

impl MsgHandler<ToolExecutionOutput> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ToolExecutionOutput, _ctx: &mut MsgCtx<'_>) {
        self.on_tool_execution_output(msg);
    }
}

impl MsgHandler<CitationsReceived> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &CitationsReceived, _ctx: &mut MsgCtx<'_>) {
        self.on_citations_received(msg).await;
    }
}

impl MsgHandler<SkillsLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &SkillsLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_skills_loaded(msg);
    }
}

impl MsgHandler<ChatEntryPinChanged> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &ChatEntryPinChanged, _ctx: &mut MsgCtx<'_>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl MsgHandler<TaskListUpdated> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &TaskListUpdated, _ctx: &mut MsgCtx<'_>) {
        self.save_active_session(&msg.session_id).await;
    }
}

impl MsgHandler<PromptTemplatesLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &PromptTemplatesLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_prompt_templates_loaded(msg);
    }
}

impl MsgHandler<PersonasLoaded> for SessionPersistenceActor {
    async fn handle(&mut self, msg: &PersonasLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_personas_loaded(msg);
    }
}
