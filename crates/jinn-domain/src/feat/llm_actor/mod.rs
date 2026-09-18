//! LLM streaming actor.
//!
//! Subscribes to [`SendToLlmProvider`] and [`CancelStream`] commands, and
//! [`StreamCompleted`] events. On send, creates an
//! LLM service via the factory and streams tokens and tool call events back
//! as bus commands. When the LLM requests tool use, emits [`ExecuteToolBatch`]
//! - the session actor handles the continuation via context assembly.

use jinn_preferences_config::schemas::RequestRetryConfig;

/// Converts a [`RequestRetryConfig`] to the provider-crate
/// [`jinn_provider::RetryConfig`].
///
/// A free function (not an inherent method) because the config type lives in
/// `jinn-preferences-config`, which must not depend on `jinn-provider`; this
/// conversion is kernel-side behavior over the shared schema.
#[must_use]
pub fn request_retry_to_provider_config(config: &RequestRetryConfig) -> jinn_provider::RetryConfig {
    jinn_provider::RetryConfig {
        max_retries: config.max_retries,
        base_delay: std::time::Duration::from_secs(config.base_delay_secs),
        max_delay: std::time::Duration::from_secs(config.max_delay_secs),
    }
}

mod session;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::feat::chat_input::protocol::command::PushChatEntry;
use crate::feat::provider::protocol::command::{CancelStream, SendToLlmProvider, StreamOrigin};
use crate::feat::provider::protocol::event::{StreamCompleted, StreamCompletedReason, StreamToken};
use crate::feat::provider_infra::LlmServiceFactoryService;
use crate::feat::provider_infra::StopReason;
use crate::feat::provider_infra::StreamEvent;
use crate::protocol::{ChatEntry, SessionId};
use error_stack::Report;
use futures::StreamExt as _;
use jiff::Timestamp;
use jinn_core_types::tool_types::ToolCall;
use jinn_slices::SystemPrompt;
use jinn_tools_msg::CancelToolBatch;
use jinn_tools_msg::ExecuteToolBatch;
use jinn_tools_msg::{ToolCallReceived, ToolCallStreaming, ToolUseStarted};

use jinn_provider::{
    LlmMessage, LlmService, LlmServiceError, OnRetry, RetryingLlmService, ToolDefinition,
};
use session::SessionData;

/// OnRetry callback that pushes a system chat entry to notify the user.
struct PushEntryOnRetry {
    bus: BusService,
    session_id: SessionId,
}

impl PushEntryOnRetry {
    fn new(bus: BusService, session_id: SessionId) -> Self {
        Self { bus, session_id }
    }
}

impl OnRetry for PushEntryOnRetry {
    fn on_retry(
        &self,
        attempt: u32,
        max_retries: u32,
        wait_duration: Duration,
        error: &Report<LlmServiceError>,
    ) {
        let secs = wait_duration.as_secs();
        let message = format!(
            "LLM request failed ({error}), retrying in {secs}s (attempt {attempt}/{max_retries})"
        );
        let bus = self.bus.clone();
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            bus.publish(PushChatEntry {
                session_id,
                entry: ChatEntry::system(message),
            })
            .await;
        });
    }
}

/// Emits a [`PushChatEntry`] error and [`StreamCompleted`] error event.
///
/// Used at every point where an LLM operation fails and the session needs
/// to be notified of the error terminal state.
async fn emit_stream_error(
    bus: &BusService,
    session_id: &SessionId,
    message: String,
    dispatched_at: Timestamp,
) {
    bus.publish(PushChatEntry {
        session_id: session_id.clone(),
        entry: ChatEntry::error(message),
    })
    .await;
    bus.publish(StreamCompleted {
        model_used: None,
        session_id: session_id.clone(),
        reason: StreamCompletedReason::Error,
        assistant_content: None,
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at,
    })
    .await;
}

/// LLM streaming actor.
///
/// Holds a reference to the LLM service factory and tracks active
/// streaming tasks and per-session state.
pub struct LlmActor {
    /// Factory for creating LLM service instances.
    factory: LlmServiceFactoryService,
    /// Shared dependencies (services, bus).
    deps: ActorDeps,
    /// Shared application state (for reading tool definitions).
    _state: State,
    /// Active stream tasks, keyed by session ID.
    tasks: HashMap<SessionId, tokio::task::JoinHandle<()>>,
    /// Per-session state.
    sessions: HashMap<SessionId, SessionData>,
    /// Sessions tombstoned by a recent [`CancelStream`]. While tombstoned,
    /// `ToolContinuation` sends are dropped; a `User` send clears it.
    cancelled_sessions: HashSet<SessionId>,
}

impl BusPublish for LlmActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

/// Dependencies for [`LlmActor`].
#[derive(Clone)]
pub struct LlmActorDeps {
    /// Factory for creating LLM service instances.
    pub factory: LlmServiceFactoryService,
    /// Shared dependencies (services, bus).
    pub deps: ActorDeps,
    /// Shared application state (for reading tool definitions).
    pub state: State,
}

impl kameo::Actor for LlmActor {
    type Args = LlmActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(
        args: Self::Args,
        actor_ref: kameo::actor::ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        let bus = &args.deps.services.bus;
        bus.subscribe::<SendToLlmProvider, _>(&actor_ref).await;
        bus.subscribe::<CancelStream, _>(&actor_ref).await;
        bus.subscribe::<StreamCompleted, _>(&actor_ref).await;

        Ok(Self {
            factory: args.factory,
            deps: args.deps,
            _state: args.state,
            tasks: HashMap::new(),
            sessions: HashMap::new(),
            cancelled_sessions: HashSet::new(),
        })
    }
}

impl kameo::message::Message<SendToLlmProvider> for LlmActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: SendToLlmProvider,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.start_stream(&msg).await;
    }
}

impl kameo::message::Message<CancelStream> for LlmActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: CancelStream,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.cancel_stream(&msg.session_id).await;
    }
}

impl kameo::message::Message<StreamCompleted> for LlmActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: StreamCompleted,
        _ctx: &mut kameo::message::Context<Self, Self::Reply>,
    ) {
        self.handle_stream_completed(&msg);
    }
}

/// Processes events from an LLM stream, emitting token/tool events via the sink.
///
/// Runs until the stream terminates via a `Done`/`Error` event or stream end,
/// always publishing `StreamCompleted` itself. Stall detection lives in the
/// first-party `stall-watchdog` plugin (fed stream events by the plugin
/// coordinator), not here.
async fn process_stream_events(
    mut stream: jinn_provider::ToolStream,
    bus: &BusService,
    sid: &SessionId,
    model_id: &str,
    dispatched_at: jiff::Timestamp,
) {
    let mut accum = StreamAccumulator::new(model_id);
    let mut events_seen = 0usize;

    while let Some(item) = stream.next().await {
        events_seen += 1;
        match item {
            Ok(event) => match event {
                StreamEvent::Text(token) => {
                    handle_text_event(bus, sid, dispatched_at, &mut accum, token).await;
                }
                StreamEvent::Reasoning(token) => {
                    handle_reasoning_event(bus, sid, dispatched_at, &mut accum, token).await;
                }
                StreamEvent::ToolUseStart { index, id, name } => {
                    bus.publish(ToolUseStarted {
                        session_id: sid.clone(),
                        index,
                        id,
                        name,
                        dispatched_at,
                    })
                    .await;
                }
                StreamEvent::ToolUseInputDelta {
                    index,
                    partial_json,
                } => {
                    bus.publish(ToolCallStreaming {
                        session_id: sid.clone(),
                        index,
                        partial_json,
                    })
                    .await;
                }
                StreamEvent::ToolUseComplete { tool_call, .. } => {
                    accum.tool_calls.push(tool_call.clone());
                    bus.publish(ToolCallReceived {
                        session_id: sid.clone(),
                        tool_call,
                        dispatched_at,
                    })
                    .await;
                }
                StreamEvent::Citations(citations) => {
                    accum.citations.extend(citations);
                }
                StreamEvent::Done { stop_reason, usage } => {
                    handle_done_event(bus, sid, &mut accum, stop_reason, usage, dispatched_at)
                        .await;
                    return;
                }
                StreamEvent::Error { message, .. } => {
                    emit_terminal_error(
                        bus,
                        sid,
                        &mut accum,
                        events_seen,
                        format!("LLM stream error: {message}"),
                        dispatched_at,
                        "LLM stream error event",
                    )
                    .await;
                    return;
                }
            },
            Err(e) => {
                emit_terminal_error(
                    bus,
                    sid,
                    &mut accum,
                    events_seen,
                    format!("LLM stream error: {e:?}"),
                    dispatched_at,
                    "LLM stream chunk error",
                )
                .await;
                return;
            }
        }
    }

    // Stream ended without a terminal event (Done/Error).
    emit_terminal_error(
        bus,
        sid,
        &mut accum,
        events_seen,
        "LLM stream ended unexpectedly. The connection may have been interrupted.".to_owned(),
        dispatched_at,
        "LLM stream ended without a terminal event (Done/Error)",
    )
    .await;
}

/// Emit an `emit_stream_error` for a terminal stream failure, with matched
/// `info!` (diagnostic) and `error!` (operator) log lines.
async fn emit_terminal_error(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    events_seen: usize,
    message: String,
    dispatched_at: jiff::Timestamp,
    log_label: &str,
) {
    tracing::info!(
        session_id = ?sid,
        events_seen,
        tool_calls_buffered = accum.tool_calls.len(),
        error = %message,
        "{log_label} - terminal"
    );
    tracing::error!(session_id = ?sid, error = %message, "{log_label}");
    emit_stream_error(bus, sid, message, dispatched_at).await;
}

/// Accumulates streamed text, reasoning, and tool calls during an LLM stream.
struct StreamAccumulator {
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    citations: Vec<jinn_provider::UrlCitation>,
    token_index: usize,
    model_id: String,
    parser: Box<dyn reasoning_parser::ReasoningParser>,
}

impl StreamAccumulator {
    fn new(model_id: &str) -> Self {
        let parser = reasoning_parser::ParserFactory::new().create(model_id);
        Self {
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            citations: Vec::new(),
            token_index: 0,
            model_id: model_id.to_owned(),
            parser,
        }
    }

    /// Publishes a reasoning fragment to the bus and advances the token index.
    async fn publish_thinking(
        &mut self,
        bus: &BusService,
        sid: &SessionId,
        token: String,
        dispatched_at: jiff::Timestamp,
    ) {
        self.thinking.push_str(&token);
        bus.publish(StreamToken {
            session_id: sid.clone(),
            index: self.token_index,
            token,
            is_thinking: true,
            dispatched_at,
        })
        .await;
        self.token_index += 1;
    }

    /// Publishes a normal-text fragment to the bus and advances the token index.
    async fn publish_text(
        &mut self,
        bus: &BusService,
        sid: &SessionId,
        token: String,
        dispatched_at: jiff::Timestamp,
    ) {
        bus.publish(StreamToken {
            session_id: sid.clone(),
            index: self.token_index,
            token,
            is_thinking: false,
            dispatched_at,
        })
        .await;
        self.token_index += 1;
    }

    /// Takes the accumulated thinking text, returning `None` if empty.
    fn take_thinking(&mut self) -> Option<String> {
        if self.thinking.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.thinking))
        }
    }
}

/// Builds the terminal `StreamCompleted` payload shared by ToolUse and Finished.
async fn publish_stream_completed(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    reason: StreamCompletedReason,
    tool_calls: Option<Vec<ToolCall>>,
    cost: Option<f64>,
    provider_completion_tokens: Option<u64>,
    provider_prompt_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    dispatched_at: jiff::Timestamp,
) {
    bus.publish(StreamCompleted {
        model_used: Some(accum.model_id.clone()),
        session_id: sid.clone(),
        reason,
        thinking_content: accum.take_thinking(),
        assistant_content: Some(std::mem::take(&mut accum.text)),
        tool_calls,
        cost,
        provider_completion_tokens,
        provider_prompt_tokens,
        cached_tokens,
        dispatched_at,
    })
    .await;
}

/// Handles a `StreamEvent::Text`: parses reasoning/normal split, publishes both.
async fn handle_text_event(
    bus: &BusService,
    sid: &SessionId,
    dispatched_at: jiff::Timestamp,
    accum: &mut StreamAccumulator,
    token: String,
) {
    tracing::info!(
        session_id = ?sid,
        token_len = token.len(),
        token_preview = %token.get(..token.len().min(50)).unwrap_or_default(),
        "LLM ACTOR StreamEvent::Text"
    );
    accum.text.push_str(&token);
    let parsed = match accum.parser.parse_reasoning_streaming_incremental(&token) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(err = ?e, "reasoning parser error, treating as normal text");
            reasoning_parser::ParserResult::normal(token.clone())
        }
    };
    if !parsed.reasoning_text.is_empty() {
        accum
            .publish_thinking(bus, sid, parsed.reasoning_text, dispatched_at)
            .await;
    }
    if !parsed.normal_text.is_empty() {
        accum
            .publish_text(bus, sid, parsed.normal_text, dispatched_at)
            .await;
    }
}

/// Handles a `StreamEvent::Reasoning`: accumulates and publishes thinking text.
async fn handle_reasoning_event(
    bus: &BusService,
    sid: &SessionId,
    dispatched_at: jiff::Timestamp,
    accum: &mut StreamAccumulator,
    token: String,
) {
    tracing::info!(
        session_id = ?sid,
        token_len = token.len(),
        token_preview = %token.get(..token.len().min(50)).unwrap_or_default(),
        "LLM ACTOR StreamEvent::Reasoning"
    );
    accum.publish_thinking(bus, sid, token, dispatched_at).await;
}

/// Handles `StreamEvent::Done`: routes tool-use vs finished and publishes
/// `ExecuteToolBatch` + `StreamCompleted`.
async fn handle_done_event(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    stop_reason: StopReason,
    usage: Option<jinn_provider::StreamUsage>,
    dispatched_at: jiff::Timestamp,
) {
    tracing::trace!(
        session_id = ?sid,
        stop_reason = %stop_reason,
        tool_call_count = accum.tool_calls.len(),
        "stream Done"
    );
    if !accum.citations.is_empty() {
        let citations = std::mem::take(&mut accum.citations);
        bus.publish(
            crate::feat::session::protocol::citations_received::CitationsReceived {
                session_id: sid.clone(),
                citations,
            },
        )
        .await;
    }
    let cost = usage.as_ref().and_then(|u| u.cost);
    let provider_completion_tokens = usage.as_ref().and_then(|u| u.completion_tokens);
    let provider_prompt_tokens = usage.as_ref().and_then(|u| u.prompt_tokens);
    let cached_tokens = usage.as_ref().and_then(|u| u.cached_tokens);
    if stop_reason == StopReason::ToolUse {
        // Publish StreamCompleted(ToolUse) BEFORE ExecuteToolBatch so the
        // session actor transitions Streaming → Sending first. Otherwise
        // ToolBatchCompleted can race ahead, requiring the buffering path.
        // If ExecuteToolBatch's publish backpressures, we still receive the
        // StreamCompleted and can recover via the watchdog buffer-drain.
        let tool_calls = std::mem::take(&mut accum.tool_calls);
        publish_stream_completed(
            bus,
            sid,
            accum,
            StreamCompletedReason::ToolUse,
            Some(tool_calls.clone()),
            cost,
            provider_completion_tokens,
            provider_prompt_tokens,
            cached_tokens,
            dispatched_at,
        )
        .await;
        bus.publish(ExecuteToolBatch {
            session_id: sid.clone(),
            tool_calls,
            dispatched_at,
        })
        .await;
    } else {
        publish_stream_completed(
            bus,
            sid,
            accum,
            StreamCompletedReason::Finished,
            None,
            cost,
            provider_completion_tokens,
            provider_prompt_tokens,
            cached_tokens,
            dispatched_at,
        )
        .await;
    }
}

impl LlmActor {
    /// Dispatches incoming commands to the appropriate handler.
    /// Starts an LLM streaming response for a session, aborting any existing stream.
    async fn start_stream(&mut self, payload: &SendToLlmProvider) {
        // Cancel tombstone: a continuation arriving for a recently-cancelled
        // session is the in-flight remnant of the tool loop losing the race
        // against `CancelStream` — drop it silently (the `StreamCompleted(
        // Canceled)` already ended the turn and pushed its cancel entry).
        if payload.origin == StreamOrigin::ToolContinuation
            && self.cancelled_sessions.contains(&payload.session_id)
        {
            tracing::info!(
                session_id = ?payload.session_id,
                "dropping tool continuation for tombstoned (cancelled) session"
            );
            return;
        }
        // A user-originated send always lifts the tombstone.
        if payload.origin == StreamOrigin::User {
            self.cancelled_sessions.remove(&payload.session_id);
        }

        let prefs = self.deps.services.user_preferences_storage.read();
        let retry_config = prefs.request_retry.clone();

        let tools = payload.tool_definitions.clone();
        let system_prompt = payload.system_prompt.clone();
        let messages = payload.messages.clone();
        let session_id = payload.session_id.clone();

        let message_count = messages.len();
        tracing::trace!(
            session_id = ?session_id,
            message_count,
            tool_count = tools.len(),
            "start_stream"
        );

        // Abort any existing stream for this session.
        if let Some(handle) = self.tasks.remove(&session_id) {
            handle.abort();
        }

        // Track the session and store the resolved model.
        let model_used = payload.model_used.clone();
        self.sessions.insert(session_id.clone(), SessionData::new());
        if let Some(data) = self.sessions.get_mut(&session_id) {
            data.set_model_used(model_used);
        }

        // Resolve the factory: per-request if provider_id is set, global fallback otherwise.
        let factory = match self.resolve_factory(payload) {
            Ok(f) => f,
            Err(msg) => {
                emit_stream_error(
                    &self.deps.services.bus,
                    &session_id,
                    msg,
                    payload.dispatched_at,
                )
                .await;
                return;
            }
        };
        let model_id = payload
            .provider_id
            .as_deref()
            .map(std::borrow::ToOwned::to_owned)
            .unwrap_or_default();
        let bus = self.deps.services.bus.clone();
        let sid = session_id.clone();
        let dispatched_at = payload.dispatched_at;

        // Dump the complete assembled request payload (one file per dispatch).
        self.deps.services.request_dump.dump(payload);

        let handle = tokio::spawn(run_stream(
            factory,
            bus,
            sid,
            model_id,
            system_prompt,
            messages,
            tools,
            dispatched_at,
            retry_config,
        ));

        // Update session state.
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.begin_streaming(dispatched_at);
        }

        self.tasks.insert(session_id, handle);
    }

    /// Resolves the LLM factory for a request: per-request when `provider_id` is set,
    /// the global factory otherwise. Returns an error message on failure.
    fn resolve_factory(
        &self,
        payload: &SendToLlmProvider,
    ) -> Result<LlmServiceFactoryService, String> {
        if let Some(pid) = payload.provider_id.clone() {
            let id = crate::feat::provider_infra::ProviderId::new(pid.clone());
            let api_keys = self.deps.services.api_keys.read();
            match self.deps.services.provider_registry.create_factory(
                &id,
                &api_keys,
                payload.reasoning_effort,
                payload.endpoint_tag.as_deref(),
            ) {
                Ok(f) => {
                    tracing::debug!(provider_id = %pid, "created per-request LLM factory");
                    Ok(LlmServiceFactoryService::new(Arc::from(f)))
                }
                Err(e) => {
                    tracing::error!(err = ?e, provider_id = %pid, "failed to create per-request factory");
                    Err(format!("LLM factory creation failed for {pid}: {e:?}"))
                }
            }
        } else {
            Ok(self.factory.clone())
        }
    }

    /// Handles stream completion events to clean up session state.
    ///
    /// Removes the session from tracking for [`Finished`] and [`Error`] reasons.
    /// For [`ToolUse`], the session stays tracked until cancellation or the next
    /// stream starts - the session actor handles the continuation.
    fn handle_stream_completed(&mut self, payload: &StreamCompleted) {
        if !self.sessions.contains_key(&payload.session_id) {
            return;
        }

        match payload.reason {
            StreamCompletedReason::ToolUse => {
                // Session stays tracked - the continuation is handled by the
                // session actor when ToolBatchCompleted arrives. The next
                // start_stream call will reset this session.
                tracing::trace!(
                    session_id = ?payload.session_id,
                    reason = "ToolUse",
                    "handle_stream_completed - keeping session for continuation"
                );
            }
            StreamCompletedReason::Error | StreamCompletedReason::Finished => {
                tracing::trace!(
                    session_id = ?payload.session_id,
                    reason = ?payload.reason,
                    "handle_stream_completed - removing session"
                );
                self.sessions.remove(&payload.session_id);
            }
            StreamCompletedReason::Canceled => {
                // Already cleaned up by cancel_stream.
            }
        }
    }

    /// Cancels the active stream for a session and emits a completion event.
    async fn cancel_stream(&mut self, session_id: &SessionId) {
        // Arm the tombstone before anything else so any tool-loop continuation
        // already in flight is rejected when it arrives. Cleared by the next
        // user-originated send.
        self.cancelled_sessions.insert(session_id.clone());

        // If there's an active session, cancel any pending tool batches.
        if self.sessions.contains_key(session_id) {
            self.publish(CancelToolBatch {
                session_id: session_id.clone(),
            })
            .await;
        }

        if let Some(handle) = self.tasks.remove(session_id) {
            handle.abort();
        }
        let dispatched_at = self
            .sessions
            .get(session_id)
            .and_then(session::SessionData::dispatched_at);
        let had_session = self.sessions.remove(session_id).is_some();
        // Only emit StreamCompleted if there was actually an active session
        // to cancel. Avoids pushing a spurious "Cancelled" error entry when
        // the user presses ESC with nothing streaming.
        if had_session {
            self.publish(StreamCompleted {
                model_used: None,
                session_id: session_id.clone(),
                reason: StreamCompletedReason::Canceled,
                assistant_content: None,
                tool_calls: None,
                cost: None,
                provider_completion_tokens: None,
                provider_prompt_tokens: None,
                cached_tokens: None,
                thinking_content: None,
                dispatched_at: dispatched_at.unwrap_or_else(Timestamp::now),
            })
            .await;
        }
    }
}

/// Drives a single streaming conversation attempt to completion.
///
/// Builds a retrying service (for transient server errors), opens the stream,
/// and drives `process_stream_events` until the stream terminates. Stall
/// detection — silence on an in-flight provider stream — lives in the
/// first-party `stall-watchdog` plugin, which treats a stall like a hard
/// server error and re-dispatches the turn.
async fn run_stream(
    factory: LlmServiceFactoryService,
    bus: BusService,
    sid: SessionId,
    model_id: String,
    system_prompt: SystemPrompt,
    messages: Vec<LlmMessage>,
    tools: Vec<ToolDefinition>,
    dispatched_at: jiff::Timestamp,
    retry_config: RequestRetryConfig,
) {
    let service = match build_streaming_service(&factory, &retry_config, &bus, &sid) {
        Ok(s) => s,
        Err(message) => {
            emit_stream_error(&bus, &sid, message, dispatched_at).await;
            return;
        }
    };

    let stream = match service
        .chat_stream_with_tools(system_prompt.as_deref(), messages, tools)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(err = ?e, "failed to start LLM stream");
            emit_stream_error(
                &bus,
                &sid,
                format!("LLM stream error: {e:?}"),
                dispatched_at,
            )
            .await;
            return;
        }
    };

    process_stream_events(stream, &bus, &sid, &model_id, dispatched_at).await;
}

/// Constructs a fresh retrying service for one streaming attempt.
fn build_streaming_service(
    factory: &LlmServiceFactoryService,
    retry_config: &RequestRetryConfig,
    bus: &BusService,
    sid: &SessionId,
) -> Result<RetryingLlmService, String> {
    let service: Box<dyn LlmService> = factory.create().map_err(|e| {
        tracing::error!(err = ?e, "failed to create LLM service");
        format!("LLM service creation failed: {e:?}")
    })?;
    Ok(RetryingLlmService::new(
        service,
        request_retry_to_provider_config(retry_config),
        Box::new(PushEntryOnRetry::new(bus.clone(), sid.clone())),
    ))
}

#[cfg(test)]
mod test_fakes {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::items_after_statements,
        clippy::unused_async,
        reason = "test code"
    )]
    use super::*;
    use jinn_provider::{ChatStream, LlmServiceFactory, ToolStream};

    /// An LLM service whose stream never yields — used to test cancel paths
    /// while a stream is genuinely in flight.
    #[derive(Debug)]
    struct HangingLlmService;

    #[async_trait::async_trait]
    impl LlmService for HangingLlmService {
        fn name(&self) -> &'static str {
            "HangingLlm"
        }
        async fn chat_stream(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
        ) -> Result<ChatStream, Report<LlmServiceError>> {
            Ok(Box::pin(futures::stream::pending()))
        }
        async fn chat_stream_with_tools(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
            _tools: Vec<jinn_core_types::ToolDefinition>,
        ) -> Result<ToolStream, Report<LlmServiceError>> {
            Ok(Box::pin(futures::stream::pending()))
        }
    }

    /// Factory that produces [`HangingLlmService`] instances.
    #[derive(Debug)]
    pub(super) struct HangingLlmFactory;

    impl LlmServiceFactory for HangingLlmFactory {
        fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
            Ok(Box::new(HangingLlmService))
        }
        fn name(&self) -> &'static str {
            "HangingLlm"
        }
    }

    /// A factory whose stream creation fails immediately — used to verify the
    /// error path publishes a chat entry and stream completion.
    #[derive(Debug)]
    pub(super) struct ErroringLlmFactory;

    impl LlmServiceFactory for ErroringLlmFactory {
        fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
            Ok(Box::new(ErroringLlmService))
        }
        fn name(&self) -> &'static str {
            "ErroringLlm"
        }
    }

    /// A service whose stream immediately yields an error.
    #[derive(Debug)]
    struct ErroringLlmService;

    #[async_trait::async_trait]
    impl LlmService for ErroringLlmService {
        fn name(&self) -> &'static str {
            "ErroringLlm"
        }
        async fn chat_stream(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
        ) -> Result<ChatStream, Report<LlmServiceError>> {
            Err(Report::new(LlmServiceError::Provider))
        }
        async fn chat_stream_with_tools(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<jinn_provider::LlmMessage>,
            _tools: Vec<jinn_core_types::ToolDefinition>,
        ) -> Result<ToolStream, Report<LlmServiceError>> {
            Err(Report::new(LlmServiceError::Provider))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::unused_async,
        reason = "test code"
    )]
    use super::session::SessionState;
    use super::test_fakes::{ErroringLlmFactory, HangingLlmFactory};
    use super::*;

    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::feat::provider::protocol::event::StreamToken;
    use jinn_provider::FakeLlmServiceFactory;

    async fn test_llm_actor() -> LlmActor {
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![])));
        LlmActor {
            factory,
            deps: crate::common::actor_deps::ActorDeps {
                services: crate::common::services::Services::new_fake().await,
            },
            _state: State::new(crate::common::app_state::AppState::default()),
            tasks: HashMap::new(),
            sessions: HashMap::new(),
            cancelled_sessions: HashSet::new(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_stream_completed_error_reason_removes_session() {
        // Given an LLM actor with a streaming session.
        let mut actor = test_llm_actor().await;
        let session_id = SessionId::new();
        actor
            .sessions
            .insert(session_id.clone(), SessionData::new());

        // When handling StreamCompleted with Error reason.
        let payload = StreamCompleted {
            model_used: None,
            session_id,
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.handle_stream_completed(&payload);

        // Then the session is removed from the sessions map.
        assert!(actor.sessions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_stream_completed_finished_reason_removes_session() {
        // Given an LLM actor with a streaming session.
        let mut actor = test_llm_actor().await;
        let session_id = SessionId::new();
        actor
            .sessions
            .insert(session_id.clone(), SessionData::new());

        // When handling StreamCompleted with Finished reason.
        let payload = StreamCompleted {
            model_used: None,
            session_id,
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("hello".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.handle_stream_completed(&payload);

        // Then the session is removed.
        assert!(
            actor.sessions.is_empty(),
            "Finished should remove the session"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_stream_completed_tool_use_keeps_session() {
        // Given an LLM actor with a streaming session.
        let mut actor = test_llm_actor().await;
        let session_id = SessionId::new();
        actor
            .sessions
            .insert(session_id.clone(), SessionData::new());

        // When handling StreamCompleted with ToolUse reason.
        let payload = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("thinking...".to_owned()),
            tool_calls: Some(vec![]),
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.handle_stream_completed(&payload);

        // Then the session is kept for continuation.
        assert!(
            actor.sessions.contains_key(&session_id),
            "ToolUse should keep the session for continuation"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_stream_completed_unknown_session_is_noop() {
        // Given an LLM actor with NO sessions.
        let mut actor = test_llm_actor().await;
        let session_id = SessionId::new();

        // When handling StreamCompleted for an unknown session.
        let payload = StreamCompleted {
            model_used: None,
            session_id,
            reason: StreamCompletedReason::Error,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        };
        actor.handle_stream_completed(&payload);

        // Then nothing happens - no panic.
        assert!(actor.sessions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_stream_removes_session_and_task() {
        // Given an LLM actor with a session and a spawned task.
        let mut actor = test_llm_actor().await;
        let session_id = SessionId::new();
        actor
            .sessions
            .insert(session_id.clone(), SessionData::new());
        // Insert a dummy task that will be aborted.
        let handle = tokio::spawn(async { std::future::pending::<()>().await });
        actor.tasks.insert(session_id.clone(), handle);

        // When cancelling the stream.
        actor.cancel_stream(&session_id).await;

        // Then the session and task are removed.
        assert!(!actor.sessions.contains_key(&session_id));
        assert!(!actor.tasks.contains_key(&session_id));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_stream_without_session_emits_nothing() {
        // Given a test harness with the LLM actor and a recorder.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder = harness.spawn_recorder::<StreamCompleted>().await;

        // When cancelling a stream for a session that doesn't exist.
        harness
            .publish(CancelStream {
                session_id: SessionId::new(),
            })
            .await;

        // Then no StreamCompleted event is emitted.
        let messages = await_recorded(&recorder, 0, std::time::Duration::from_millis(100)).await;
        assert!(
            messages.is_empty(),
            "should not emit StreamCompleted for non-existent session"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn start_stream_emits_stream_completed_with_tokens() {
        // Given a test harness with an LLM actor.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "Hello".to_owned(),
            " World".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;

        let recorder_tokens = harness.spawn_recorder::<StreamToken>().await;
        let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();
        let payload = SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        };

        // When starting a stream.
        harness.publish(payload).await;

        // Then StreamCompleted was emitted.
        let completed =
            await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
        let finished = completed
            .iter()
            .find(|sc| sc.reason == StreamCompletedReason::Finished);
        assert!(finished.is_some(), "should emit StreamCompleted(Finished)");
        let finished = finished.unwrap();
        assert_eq!(finished.assistant_content.as_deref(), Some("Hello World"));

        // And StreamToken events were emitted with sequential indices.
        let token_events =
            await_recorded(&recorder_tokens, 2, std::time::Duration::from_secs(2)).await;
        assert_eq!(token_events.len(), 2, "should have 2 token events");
        assert_eq!(token_events[0].index, 0, "first token index should be 0");
        assert_eq!(token_events[1].index, 1, "second token index should be 1");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn start_stream_aborts_existing_stream_for_same_session() {
        // Given an LLM actor with an existing stream for a session.
        let mut actor = test_llm_actor().await;

        let session_id = SessionId::new();
        let payload = SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        };

        // Start first stream and save the handle.
        actor.start_stream(&payload.clone()).await;
        let first_handle = actor.tasks.remove(&session_id);
        assert!(first_handle.is_some());
        // Re-insert for the second start_stream to find and abort.
        actor
            .tasks
            .insert(session_id.clone(), first_handle.unwrap());

        // When starting a second stream for the same session.
        actor.start_stream(&payload).await;
        // Then a new task exists (the old one was aborted internally).
        assert!(
            actor.tasks.contains_key(&session_id),
            "second stream should create a new task"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn start_stream_sets_session_to_streaming() {
        // Given an LLM actor.
        let mut actor = test_llm_actor().await;

        let session_id = SessionId::new();
        let payload = SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        };

        // When starting a stream.
        actor.start_stream(&payload).await;

        // Then the session state is Streaming.
        let session_data = actor.sessions.get(&session_id);
        assert!(session_data.is_some(), "session should be tracked");
        assert_eq!(
            *session_data.unwrap().state(),
            SessionState::Streaming,
            "session should be in Streaming state"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_send_to_llm_via_bus() {
        // Given a test harness with an LLM actor.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "response".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();
        let payload = SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin: StreamOrigin::User,
            dispatched_at: jiff::Timestamp::now(),
        };

        // When sending via bus.
        harness.publish(payload).await;

        // Then the stream completes.
        let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Finished);
        assert!(found, "should emit StreamCompleted(Finished)");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_cancel_stream_with_no_active_stream_is_noop() {
        // Given a test harness with an LLM actor.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "response".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;

        let recorder = harness.spawn_recorder::<StreamCompleted>().await;

        // When sending CancelStream with no active stream.
        let session_id = SessionId::new();
        harness
            .publish(CancelStream {
                session_id: session_id.clone(),
            })
            .await;

        // Then no StreamCompleted is emitted (nothing to cancel).
        let completed = await_recorded(&recorder, 1, std::time::Duration::from_millis(100)).await;
        assert!(
            completed.is_empty(),
            "CancelStream with no active stream should be a no-op"
        );
    }

    /// Builds a minimal `SendToLlmProvider` for tombstone tests.
    fn tombstone_payload(session_id: &SessionId, origin: StreamOrigin) -> SendToLlmProvider {
        SendToLlmProvider {
            model_used: None,
            reasoning_effort: None,
            endpoint_tag: None,
            session_id: session_id.clone(),
            messages: vec![],
            system_prompt: SystemPrompt::default(),
            tool_definitions: vec![],
            provider_id: None,
            estimated_tokens: 0,
            origin,
            dispatched_at: jiff::Timestamp::now(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_continuation_after_cancel_is_dropped() {
        // Given a harness with an LLM actor and one queued stream response.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "should never stream".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder_tokens = harness.spawn_recorder::<StreamToken>().await;
        let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();

        // When the session is cancelled.
        harness
            .publish(CancelStream {
                session_id: session_id.clone(),
            })
            .await;
        // And an in-flight tool continuation arrives afterwards.
        harness
            .publish(tombstone_payload(
                &session_id,
                StreamOrigin::ToolContinuation,
            ))
            .await;

        // Then the continuation is dropped: no tokens stream and no completion fires.
        let tokens =
            await_recorded(&recorder_tokens, 0, std::time::Duration::from_millis(300)).await;
        let completions = await_recorded(
            &recorder_completed,
            0,
            std::time::Duration::from_millis(300),
        )
        .await;
        assert!(
            tokens.is_empty() && completions.is_empty(),
            "cancelled session must not accept a tool continuation: tokens={tokens:?} completions={completions:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn user_send_after_cancel_clears_tombstone() {
        // Given a harness with an LLM actor and one queued stream response.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "fresh turn".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();

        // When the session is cancelled.
        harness
            .publish(CancelStream {
                session_id: session_id.clone(),
            })
            .await;
        // And the user then sends a new message.
        harness
            .publish(tombstone_payload(&session_id, StreamOrigin::User))
            .await;

        // Then the user turn streams to completion — the tombstone is lifted.
        let completed =
            await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Finished);
        assert!(found, "user send after cancel should stream normally");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn continuation_allowed_without_cancel() {
        // Given a harness with an LLM actor and one queued stream response.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![
            "tool loop turn".to_owned(),
        ])));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder_completed = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();

        // When a tool continuation arrives with no prior cancel.
        harness
            .publish(tombstone_payload(
                &session_id,
                StreamOrigin::ToolContinuation,
            ))
            .await;

        // Then it streams to completion — the tool loop is unaffected.
        let completed =
            await_recorded(&recorder_completed, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Finished);
        assert!(found, "tool continuation without cancel should stream");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_stream_via_bus_emits_completion() {
        // Given a test harness with an LLM actor using a never-completing fake.
        // This ensures the stream is actively streaming when we cancel.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(HangingLlmFactory));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let recorder = harness.spawn_recorder::<StreamCompleted>().await;

        let session_id = SessionId::new();
        harness
            .publish(SendToLlmProvider {
                model_used: None,
                reasoning_effort: None,
                endpoint_tag: None,
                session_id: session_id.clone(),
                messages: vec![],
                system_prompt: SystemPrompt::default(),
                tool_definitions: vec![],
                provider_id: None,
                estimated_tokens: 0,
                origin: StreamOrigin::User,
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;
        // Give the stream a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // When sending CancelStream via bus.
        harness
            .publish(CancelStream {
                session_id: session_id.clone(),
            })
            .await;

        // Then StreamCompleted(Canceled) is emitted.
        let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Canceled);
        assert!(found, "should emit StreamCompleted(Canceled)");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_error_emits_chat_entry_and_completion_via_bus() {
        // Given a test harness with an LLM actor using an immediately-erroring fake.
        // The stream errors synchronously on creation, exercising emit_stream_error.
        let harness = TestHarness::new().await;
        let factory = LlmServiceFactoryService::new(Arc::new(ErroringLlmFactory));
        let _actor = harness
            .spawn_actor::<LlmActor>(LlmActorDeps {
                factory,
                deps: harness.actor_deps().await,
                state: State::new(crate::common::app_state::AppState::default()),
            })
            .await;
        let entry_recorder = harness.spawn_recorder::<PushChatEntry>().await;
        let completed_recorder = harness.spawn_recorder::<StreamCompleted>().await;

        // When sending SendToLlmProvider, which errors during stream creation.
        let session_id = SessionId::new();
        harness
            .publish(SendToLlmProvider {
                model_used: None,
                reasoning_effort: None,
                endpoint_tag: None,
                session_id: session_id.clone(),
                messages: vec![],
                system_prompt: SystemPrompt::default(),
                tool_definitions: vec![],
                provider_id: None,
                estimated_tokens: 0,
                origin: StreamOrigin::User,
                dispatched_at: jiff::Timestamp::now(),
            })
            .await;

        // Then an error PushChatEntry is emitted (from emit_stream_error).
        let entries = await_recorded(&entry_recorder, 1, std::time::Duration::from_secs(5)).await;
        assert!(
            !entries.is_empty(),
            "stream error should publish an error chat entry"
        );

        // And a StreamCompleted(Error) is emitted, so the session isn't stuck streaming.
        let completed =
            await_recorded(&completed_recorder, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Error);
        assert!(found, "stream error should emit StreamCompleted(Error)");
    }

    #[rstest::rstest]
    fn request_retry_to_provider_config_uses_actual_values_not_defaults() {
        // If the conversion returned Default::default(), all durations would be zero.
        let config = RequestRetryConfig {
            max_retries: 3,
            base_delay_secs: 5,
            max_delay_secs: 120,
        };

        let retry = super::request_retry_to_provider_config(&config);

        assert_eq!(retry.max_retries, 3);
        assert_eq!(retry.base_delay, std::time::Duration::from_secs(5));
        assert_eq!(retry.max_delay, std::time::Duration::from_mins(2));
    }

    // ------------------------------------------------------------------
    // Phase 1: Stream idle-stall detection + auto-retry tests
    // ------------------------------------------------------------------

    /// Builds a [`jinn_provider::ToolStream`] from a scripted event sequence,
    /// optionally inserting an idle gap (a pending future) at a chosen point.
    fn scripted_stream(events: Vec<jinn_provider::StreamEvent>) -> jinn_provider::ToolStream {
        let events: Vec<Result<jinn_provider::StreamEvent, Report<LlmServiceError>>> =
            events.into_iter().map(Ok).collect();
        Box::pin(futures::stream::iter(events))
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn process_stream_events_completes_on_done_event() {
        // Given a stream that yields a token then Done.
        use jinn_provider::{StopReason, StreamEvent};
        let harness = TestHarness::new().await;
        let stream = scripted_stream(vec![
            StreamEvent::Text("hi".to_owned()),
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
        ]);
        let sid = SessionId::new();

        // When processing the stream.
        let recorder = harness.spawn_recorder::<StreamCompleted>().await;
        process_stream_events(
            stream,
            &harness.bus(),
            &sid,
            "test-model",
            jiff::Timestamp::now(),
        )
        .await;

        // Then the stream completes.
        let completed = await_recorded(&recorder, 1, std::time::Duration::from_secs(5)).await;
        let found = completed
            .iter()
            .any(|sc| sc.reason == StreamCompletedReason::Finished);
        assert!(found, "Done should complete the stream");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn process_stream_events_publishes_citations_on_done_when_accumulated() {
        // Given a stream carrying url_citation annotations then Done.
        use jinn_provider::{StopReason, StreamEvent, UrlCitation};
        let harness = TestHarness::new().await;
        let recorder = harness
            .spawn_recorder::<crate::feat::session::protocol::citations_received::CitationsReceived>()
            .await;
        let stream = scripted_stream(vec![
            StreamEvent::Citations(vec![UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }]),
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            },
        ]);
        let sid = SessionId::new();

        // When processing the stream to completion.
        process_stream_events(
            stream,
            &harness.bus(),
            &sid,
            "test-model",
            jiff::Timestamp::now(),
        )
        .await;

        // Then the stream completes (no CitationsReceived asserted separately below).
        // And exactly one CitationsReceived was published on the bus.
        let events = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(events.len(), 1, "one CitationsReceived published");
        assert_eq!(events[0].citations.len(), 1);
        assert_eq!(events[0].citations[0].url, "https://example.com/a");
        assert_eq!(events[0].session_id, sid);
    }

    // --- Phase 1: publish order reversal ---------------------------------
    //
    // A single actor that records the arrival order of `StreamCompleted` and
    // `ExecuteToolBatch` into a shared `Arc<Mutex<Vec<String>>>`. Because kameo's
    // bus delivers each message as a separate mailbox message, the order of
    // `publish` calls in `handle_done_event` is preserved in arrival order.
    struct OrderLog(std::sync::Mutex<Vec<&'static str>>);

    impl kameo::Actor for OrderLog {
        type Args = ();
        type Error = kameo::error::Infallible;
        async fn on_start(
            _args: Self::Args,
            _actor_ref: kameo::actor::ActorRef<Self>,
        ) -> Result<Self, Self::Error> {
            Ok(Self(std::sync::Mutex::new(Vec::new())))
        }
    }

    impl kameo::prelude::Message<StreamCompleted> for OrderLog {
        type Reply = ();
        async fn handle(
            &mut self,
            _msg: StreamCompleted,
            _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
        ) {
            self.0.lock().unwrap().push("StreamCompleted");
        }
    }

    impl kameo::prelude::Message<ExecuteToolBatch> for OrderLog {
        type Reply = ();
        async fn handle(
            &mut self,
            _msg: ExecuteToolBatch,
            _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
        ) {
            self.0.lock().unwrap().push("ExecuteToolBatch");
        }
    }
    struct GetOrder;
    impl kameo::prelude::Message<GetOrder> for OrderLog {
        type Reply = Vec<&'static str>;
        async fn handle(
            &mut self,
            _msg: GetOrder,
            _ctx: &mut kameo::prelude::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            self.0.lock().unwrap().clone()
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_done_event_publishes_stream_completed_before_execute_tool_batch() {
        // Given a bus with an OrderLog recording both message types.
        use kameo::actor::Spawn;
        let harness = TestHarness::new().await;
        let bus = harness.bus();
        let order_actor = OrderLog::spawn(());
        harness
            .register(order_actor.clone().recipient::<StreamCompleted>())
            .await;
        harness
            .register(order_actor.clone().recipient::<ExecuteToolBatch>())
            .await;

        // And an accumulator carrying one tool call.
        let mut accum = StreamAccumulator::new("test-model");
        accum.tool_calls.push(ToolCall {
            id: "tc-1".to_owned(),
            name: "read".to_owned(),
            arguments: "{}".to_owned(),
        });
        let sid = SessionId::new();

        // When handling the Done event for a tool-use turn.
        handle_done_event(
            &bus,
            &sid,
            &mut accum,
            StopReason::ToolUse,
            None,
            jiff::Timestamp::now(),
        )
        .await;

        // Then StreamCompleted arrives before ExecuteToolBatch.
        let recorded = loop {
            let v: Vec<&'static str> = order_actor.ask(GetOrder).await.expect("get order");
            if v.len() == 2 {
                break v;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(recorded, vec!["StreamCompleted", "ExecuteToolBatch"]);
    }
}
