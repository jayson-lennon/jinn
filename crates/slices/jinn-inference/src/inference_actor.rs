use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use futures::StreamExt as _;
use jiff::Timestamp;
use jinn_core_types::tool_types::ToolCall;
use jinn_domain::common::actor_deps::BusPublish;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusService;
use jinn_domain::feat::chat_input::protocol::command::PushChatEntry;
use jinn_domain::feat::provider_infra::LlmServiceFactoryService;
use jinn_domain::feat::provider_infra::StopReason;
use jinn_domain::feat::provider_infra::StreamEvent;
use jinn_domain::protocol::{ChatEntry, SessionId};
use jinn_inference_msg::{
    CancelStream, SendToLlmProvider, StreamCompleted, StreamCompletedReason, StreamOrigin,
    StreamToken,
};
use jinn_preferences_config::schemas::RequestRetryConfig;
use jinn_provider::{
    LlmMessage, LlmService, LlmServiceError, OnRetry, RetryingLlmService, ToolDefinition,
};
use jinn_slices::SystemPrompt;
use jinn_tools_msg::CancelToolBatch;
use jinn_tools_msg::ExecuteToolBatch;
use jinn_tools_msg::{ToolCallReceived, ToolCallStreaming, ToolUseStarted};
use trouper::actor::ActorPath;
use trouper::actor::{MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::system::ActorSystem;

use crate::session::SessionData;

/// The inference actor's static trouper path.
pub const INFERENCE_PATH: &str = "inference";

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

/// The inference actor — drives LLM provider streams.
///
/// Resolves the LLM service factory from [`Services`], tracks active
/// streaming tasks and per-session state, and enforces the cancel
/// tombstone. Streams run as plain tokio tasks; the actor loop only
/// sees the three crossing messages.
pub struct InferenceActor {
    /// Application-wide runtime services (bus publish, LLM factory,
    /// provider registry, api keys, request dump, prefs reads).
    services: Services,
    /// Active stream tasks, keyed by session ID.
    tasks: HashMap<SessionId, tokio::task::JoinHandle<()>>,
    /// Per-session state.
    sessions: HashMap<SessionId, SessionData>,
    /// Sessions tombstoned by a recent [`CancelStream`]. While tombstoned,
    /// `ToolContinuation` sends are dropped; a `User` send clears it.
    cancelled_sessions: HashSet<SessionId>,
}

impl jinn_domain::common::actor_deps::BusPublish for InferenceActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl ServiceActor for InferenceActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(
        _args: &serde_json::Value,
    ) -> Result<Self, Report<trouper::registry::RegistryError>> {
        // Never called: the spawn helper injects services via `start_with`.
        Err(
            error_stack::IntoReport::into_report(trouper::registry::RegistryError::InvalidSpec)
                .attach("InferenceActor is spawned via start_with"),
        )
    }
}

impl InferenceActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the inference topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(system: &ActorSystem, services: Services) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(INFERENCE_PATH))
            .start_with({
                move || {
                    let services = services.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services,
                            tasks: HashMap::new(),
                            sessions: HashMap::new(),
                            cancelled_sessions: HashSet::new(),
                        })
                    })
                }
            })
            .handles::<SendToLlmProvider>()
            .handles::<CancelStream>()
            .handles::<StreamCompleted>()
            .start()
    }
}

impl MsgHandler<SendToLlmProvider> for InferenceActor {
    async fn handle(&mut self, msg: SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.start_stream(&msg).await;
    }
}

impl MsgHandler<CancelStream> for InferenceActor {
    async fn handle(&mut self, msg: CancelStream, _ctx: &mut MsgCtx<'_>) {
        self.cancel_stream(&msg.session_id).await;
    }
}

impl MsgHandler<StreamCompleted> for InferenceActor {
    async fn handle(&mut self, msg: StreamCompleted, _ctx: &mut MsgCtx<'_>) {
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
            jinn_domain::feat::session::protocol::citations_received::CitationsReceived {
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

impl InferenceActor {
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

        let prefs = self.services.user_preferences_storage.read();
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
                emit_stream_error(&self.services.bus, &session_id, msg, payload.dispatched_at)
                    .await;
                return;
            }
        };
        let model_id = payload
            .provider_id
            .as_deref()
            .map(std::borrow::ToOwned::to_owned)
            .unwrap_or_default();
        let bus = self.services.bus.clone();
        let sid = session_id.clone();
        let dispatched_at = payload.dispatched_at;

        // Dump the complete assembled request payload (one file per dispatch).
        self.services.request_dump.dump(payload);

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
            let id = jinn_domain::feat::provider_infra::ProviderId::new(pid.clone());
            let api_keys = self.services.api_keys.read();
            match self.services.provider_registry.create_factory(
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
            Ok(self.services.llm_service.clone())
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
            .and_then(SessionData::dispatched_at);
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
        jinn_domain::feat::provider_infra::request_retry_to_provider_config(retry_config),
        Box::new(PushEntryOnRetry::new(bus.clone(), sid.clone())),
    ))
}

#[cfg(test)]
#[path = "inference_actor_tests.rs"]
mod tests;

#[cfg(test)]
/// Builds fake [`Services`] over the given bus for struct-direct tests: the
/// actor publishes stream events through `services.bus`, so tests hand in the
/// harness's bus to observe them.
pub async fn test_services_with_bus(
    bus: jinn_domain::common::services::bus_service::BusService,
) -> jinn_domain::common::services::Services {
    jinn_domain::common::services::Services::new_fake_with_bus(bus).await
}
