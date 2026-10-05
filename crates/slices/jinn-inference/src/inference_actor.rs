use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use futures::StreamExt as _;
use jiff::Timestamp;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_inference_msg::{
    AbortStream, CancelStream, SendToLlmProvider, StreamActivity, StreamCompleted,
    StreamCompletedReason, StreamOrigin, StreamToken,
};
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::services::Services;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::protocol::ChatEntry;
use jinn_preferences_config::schemas::RequestRetryConfig;
use jinn_provider::{
    LlmMessage, LlmService, LlmServiceError, OnRetry, RetryingLlmService, ToolDefinition,
};
use jinn_provider_config::LlmServiceFactoryService;
use jinn_provider_config::StopReason;
use jinn_provider_config::StreamEvent;
use jinn_session_history_msg::PushChatEntry;
use jinn_slices::StreamContext;
use jinn_slices::SystemPrompt;
use jinn_slices::render_rule_interrupt;
use jinn_slices::{RuleFired, RuleMatch, StreamRuleSession, TurnEnd};
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
                pin: None,
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
        pin: None,
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

impl jinn_kernel::common::actor_deps::BusPublish for InferenceActor {
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
        _args: &trouper::json::Json,
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
            .handles::<AbortStream>()
            .handles::<StreamCompleted>()
            .start()
    }
}

impl MsgHandler<SendToLlmProvider> for InferenceActor {
    async fn handle(&mut self, msg: &SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.start_stream(msg).await;
    }
}

impl MsgHandler<AbortStream> for InferenceActor {
    /// Tears the stream down without ending the turn.
    ///
    /// The stream task that published this publishes its own
    /// `StreamCompleted(RuleIntercept)` when it returns, so this handler
    /// completes no stream of its own — emitting one here would race a
    /// `Canceled` completion against it.
    #[expect(
        clippy::unused_async,
        reason = "the MsgHandler signature is async; the teardown awaits its own publishes"
    )]
    async fn handle(&mut self, msg: &AbortStream, _ctx: &mut MsgCtx<'_>) {
        self.abort_stream(&msg.session_id, Some(msg.dispatched_at))
            .await;
    }
}

impl MsgHandler<CancelStream> for InferenceActor {
    async fn handle(&mut self, msg: &CancelStream, _ctx: &mut MsgCtx<'_>) {
        self.cancel_stream(&msg.session_id).await;
    }
}

impl MsgHandler<StreamCompleted> for InferenceActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        self.handle_stream_completed(msg);
    }
}

/// Declares a stream alive: publishes [`StreamActivity`] for one non-terminal
/// provider event.
///
/// The one contract a stream supervisor needs — "this stream is still
/// producing" — owned by the producer so every kind of forward progress is
/// covered by construction. A supervisor that watched only `StreamToken`
/// would read a tool call being constructed (deltas streaming in for
/// minutes) as silence.
async fn publish_activity(bus: &BusService, sid: &SessionId) {
    bus.publish(StreamActivity {
        session_id: sid.clone(),
    })
    .await;
}

/// Processes events from an LLM stream, emitting token/tool events via the sink.
///
/// Runs until the stream terminates via a `Done`/`Error` event or stream end,
/// always publishing `StreamCompleted` itself. Stall detection lives in the
/// `jinn-watchdog` slice's stall-watchdog actor (which consumes this actor's
/// [`StreamActivity`] by schema broadcast), not here.
///
/// Every non-terminal event publishes [`StreamActivity`] first, before its
/// own domain message: the liveness signal is declared by the arm that
/// handles the event, so a future [`StreamEvent`] variant cannot silently
/// blind the watchdog. The terminal `Done`/`Error` arms publish no activity —
/// [`StreamCompleted`] governs a stream's end.
async fn process_stream_events(
    mut stream: jinn_provider::ToolStream,
    bus: &BusService,
    sid: &SessionId,
    model_id: &str,
    dispatched_at: jiff::Timestamp,
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
) {
    let mut accum = StreamAccumulator::new(model_id);
    let mut events_seen = 0usize;

    while let Some(item) = stream.next().await {
        events_seen += 1;
        match item {
            Ok(event) => match event {
                StreamEvent::Text(token) => {
                    publish_activity(bus, sid).await;
                    let fired = handle_text_event(
                        bus,
                        sid,
                        dispatched_at,
                        &mut accum,
                        token,
                        rules.as_deref_mut(),
                        StreamContext::text(),
                    )
                    .await;
                    if let Some(fired) = fired {
                        apply_rule_match(bus, sid, &mut accum, fired, dispatched_at).await;
                        return;
                    }
                }
                StreamEvent::Reasoning(token) => {
                    publish_activity(bus, sid).await;
                    let fired = handle_reasoning_event(
                        bus,
                        sid,
                        dispatched_at,
                        &mut accum,
                        token,
                        rules.as_deref_mut(),
                        StreamContext::thinking(),
                    )
                    .await;
                    if let Some(fired) = fired {
                        apply_rule_match(bus, sid, &mut accum, fired, dispatched_at).await;
                        return;
                    }
                }
                StreamEvent::ToolUseStart { index, id, name } => {
                    publish_activity(bus, sid).await;
                    // The name arrives only here, so it is recorded against
                    // the index the argument deltas will carry.
                    accum.tool_names.insert(index, name.clone());
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
                    publish_activity(bus, sid).await;
                    // Tested before publishing, as on every other stream path.
                    // A rule scoped to a tool call is tested against the
                    // arguments as they accumulate, so the tool name the
                    // start event recorded is what names the stream.
                    let fired = rules.as_deref_mut().and_then(|session| {
                        let name = accum.tool_names.get(&index).map_or("", String::as_str);
                        session.check(&partial_json, StreamContext::tool(index, name))
                    });
                    // Published even when a rule fired, unlike the text and
                    // reasoning paths. Because a tool rule accumulates, the
                    // delta that trips it is usually the first one — dropping
                    // it left the log showing a tool call whose arguments had
                    // never arrived, rendering as a bare `$` with nothing in
                    // it. The user watched the model begin this call, so the
                    // attempt is shown up to the interrupt point.
                    //
                    // Showing the arguments does not run them: the tool never
                    // executes, because `ToolUseComplete` — and with it
                    // `ExecuteToolBatch` — is never reached once this handler
                    // returns.
                    bus.publish(ToolCallStreaming {
                        session_id: sid.clone(),
                        index,
                        partial_json,
                    })
                    .await;
                    if let Some(fired) = fired {
                        apply_rule_match(bus, sid, &mut accum, fired, dispatched_at).await;
                        return;
                    }
                }
                StreamEvent::ToolUseComplete { tool_call, .. } => {
                    publish_activity(bus, sid).await;
                    accum.tool_calls.push(tool_call.clone());
                    bus.publish(ToolCallReceived {
                        session_id: sid.clone(),
                        tool_call,
                        dispatched_at,
                    })
                    .await;
                }
                StreamEvent::Citations(citations) => {
                    publish_activity(bus, sid).await;
                    accum.citations.extend(citations);
                }
                StreamEvent::Done { stop_reason, usage } => {
                    // Terminal: `StreamCompleted` governs a stream's end, so
                    // this arm declares no liveness.
                    handle_done_event(bus, sid, &mut accum, stop_reason, usage, dispatched_at)
                        .await;
                    return;
                }
                StreamEvent::Error { message, .. } => {
                    // Terminal: as with `Done`, no liveness is declared.
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

/// Acts on what a stream rule matched.
///
/// Two outcomes, and the distinction is the whole point of returning a
/// [`RuleMatch`] rather than a bare rule: a match inside the session's
/// interrupt budget resumes the turn, while a match past it cancels the
/// stream. Resuming a fourth time would be resuming a loop.
async fn apply_rule_match(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    fired: RuleMatch,
    dispatched_at: jiff::Timestamp,
) {
    match fired {
        RuleMatch::Interrupt(rule) => {
            intercept_and_resume(bus, sid, accum, rule, dispatched_at).await;
        }
        RuleMatch::BudgetSpent { rule, maximum } => {
            give_up_on_the_session(bus, sid, rule, maximum).await;
        }
    }
}

/// Ends a stream whose session has taken too many interrupts in a row, and
/// gives up on it.
///
/// A plain [`CancelStream`], deliberately, and that is the design: it reuses
/// the one cancel path everything else uses, so the session ends exactly as a
/// user-initiated cancel ends — the tombstone is armed, the task is torn down,
/// and the chat log carries the same `Cancelled` entry. A rule that matches a
/// model which keeps ignoring it has stopped being a correction and become a
/// loop, and the honest report of that is a cancelled turn.
///
/// No guidance is injected: there is nobody left to give it to, and injecting
/// it would read as a correction the model may act on before the cancel
/// lands.
async fn give_up_on_the_session(
    bus: &BusService,
    sid: &SessionId,
    rule: RuleFired,
    maximum: usize,
) {
    tracing::warn!(
        session_id = %sid,
        rule = %rule.name,
        maximum,
        "stream rule interrupted the turn too many times in a row; cancelling the stream"
    );
    bus.publish(jinn_session_history_msg::PushChatEntry {
        session_id: sid.clone(),
        entry: ChatEntry::system(format!(
            "\u{1f6d1} stream-rules: `{}` interrupted this turn {maximum} times in a row; \
             cancelling the stream.",
            rule.name
        )),
        pin: None,
    })
    .await;

    // Undated, so it applies to whatever generation is current — including
    // this one, which the actor tears down as it handles it.
    bus.publish(CancelStream {
        session_id: sid.clone(),
    })
    .await;
}

/// Ends a stream a rule interrupted, and asks for the turn to resume.
///
/// Three publishes, in this order:
///
/// 1. [`CancelStream`] — the abort. It reuses the actor's own cancel path so
///    the tombstone is armed exactly as a user cancel arms it, and so the
///    aborted task is cleaned up by the one code path that already does it.
///    A task cannot abort *itself* from inside, so the loop returns after
///    publishing this rather than calling `JoinHandle::abort`.
/// 2. A [`ChatEntryKind::RuleInterrupt`] entry — the guidance, entering the
///    conversation once, ahead of the resumed request. It reaches the model as
///    a user turn, unchanged from when this published a `User` entry; it only
///    renders differently, so harness steering is not mistaken for typed input.
/// 3. [`StreamCompleted`] with [`StreamCompletedReason::RuleIntercept`] —
///    the terminal fact the session actor's resume handler acts on. It
///    carries a reason distinct from a cancel so the turn is never reported
///    as cancelled.
///
/// The partial output is deliberately *not* published: the offending delta
/// never reached the chat log, and the earlier deltas stay visible there and
/// are excluded from the resumed request by the session actor.
async fn intercept_and_resume(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    fired: RuleFired,
    dispatched_at: jiff::Timestamp,
) {
    tracing::warn!(
        session_id = ?sid,
        rule = %fired.name,
        "stream rule matched; interrupting the turn before publishing"
    );

    bus.publish(jinn_session_history_msg::PushChatEntry {
        session_id: sid.clone(),
        entry: ChatEntry::rule_interrupt(
            &fired.name,
            render_rule_interrupt(&fired.name, &fired.body),
        ),
        pin: None,
    })
    .await;

    // The accumulator's text is released here rather than published: it is
    // what the user watched appear, and the session actor has already taken
    // it out of context by the time this lands.
    let _partial = std::mem::take(&mut accum.text);

    bus.publish(StreamCompleted {
        model_used: Some(accum.model_id.clone()),
        session_id: sid.clone(),
        reason: StreamCompletedReason::RuleIntercept,
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

    // Published after the guidance and the completion, and stamped with this
    // generation so the actor tears down the generation that just ended rather
    // than the one this intercept resumes into. Sending it last also keeps it
    // from cutting off the publishes above: the abort cancels the very task
    // that issued it.
    bus.publish(AbortStream {
        session_id: sid.clone(),
        dispatched_at,
    })
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
    /// The name of each in-flight tool call, keyed by its index.
    ///
    /// Recorded from `ToolUseStart`, which is the only event that names the
    /// tool: the argument deltas that follow carry an index and a partial
    /// JSON fragment and nothing else. A `tool:<name>(<glob>)` scope needs
    /// the name, and the provider will not repeat it.
    tool_names: HashMap<usize, String>,
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
            tool_names: HashMap::new(),
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
#[expect(
    clippy::too_many_arguments,
    reason = "the stream handlers share one shape: bus, session, time, accumulator, payload, rules"
)]
async fn handle_text_event(
    bus: &BusService,
    sid: &SessionId,
    dispatched_at: jiff::Timestamp,
    accum: &mut StreamAccumulator,
    token: String,
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
    ctx: StreamContext<'_>,
) -> Option<RuleMatch> {
    tracing::info!(
        session_id = ?sid,
        token_len = token.len(),
        token_preview = %token.get(..token.len().min(50)).unwrap_or_default(),
        "LLM ACTOR StreamEvent::Text"
    );
    // Asked before any publish, so a delta that trips a rule has not yet
    // reached the chat log.
    if let Some(session) = rules.as_mut()
        && let Some(fired) = session.check(&token, ctx)
    {
        return Some(fired);
    }
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
    None
}

/// Handles a `StreamEvent::Reasoning`: accumulates and publishes thinking text.
#[expect(
    clippy::too_many_arguments,
    reason = "the stream handlers share one shape: bus, session, time, accumulator, payload, rules"
)]
async fn handle_reasoning_event(
    bus: &BusService,
    sid: &SessionId,
    dispatched_at: jiff::Timestamp,
    accum: &mut StreamAccumulator,
    token: String,
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
    ctx: StreamContext<'_>,
) -> Option<RuleMatch> {
    tracing::info!(
        session_id = ?sid,
        token_len = token.len(),
        token_preview = %token.get(..token.len().min(50)).unwrap_or_default(),
        "LLM ACTOR StreamEvent::Reasoning"
    );
    if let Some(session) = rules.as_mut()
        && let Some(fired) = session.check(&token, ctx)
    {
        return Some(fired);
    }
    accum.publish_thinking(bus, sid, token, dispatched_at).await;
    None
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
        bus.publish(jinn_session_history_msg::CitationsReceived {
            session_id: sid.clone(),
            citations,
        })
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

        // Read the retry policy from the configuration layer at the point
        // of use, so a reload is observed by the next turn without a restart.
        let retry_config = self
            .services
            .config
            .get::<RequestRetryConfig>()
            .unwrap_or_default();

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

        // Resolved once per stream, not per delta: with no rules configured
        // this is `None` and the stream path is byte-for-byte what it was.
        let rules_cell = self
            .services
            .slices
            .reader::<jinn_slices::StreamRules>(&jinn_slices::stream_rules_slot());
        let rules = rules_cell.as_ref().and_then(|cell| {
            // The cell handle is cloned out first: the session borrows
            // the installed set, so the handle must outlive the local
            // `reader` binding above.
            let cell = cell.clone();
            cell.read().new_session(&session_id)
        });

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
            rules,
            rules_cell.map(|cell| cell.clone()),
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
            let id = jinn_provider_config::ProviderId::new(pid.clone());
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
            StreamCompletedReason::RuleIntercept => {
                // Already cleaned up by abort_stream, which the stream task
                // reached through `AbortStream` before publishing this.
            }
        }
    }

    /// Tears a session's stream down: arms the tombstone, cancels pending
    /// tool batches, aborts the task, and forgets the session.
    ///
    /// Split out of [`Self::cancel_stream`] because a rule intercept needs
    /// the same teardown with a *different* terminal reason. Publishing a
    /// `CancelStream` to get it would emit a competing
    /// `StreamCompleted(Canceled)` — which pushes the literal `"Cancelled"`
    /// history entry that every consumer reads as a user cancel — so the
    /// teardown is called directly instead.
    ///
    /// A rule intercept carries the dispatch it is aborting, and a mismatch
    /// means the abort is stale: the turn already resumed on a newer
    /// generation and this one would tear down the resumed stream, leaving the
    /// session with neither the original nor its replacement. A cancel is
    /// always undated, so it applies to whatever generation is current.
    ///
    /// Returns the dispatch time of the aborted generation and whether there
    /// was one to abort.
    async fn abort_stream(
        &mut self,
        session_id: &SessionId,
        only_dispatched_at: Option<jiff::Timestamp>,
    ) -> (Option<jiff::Timestamp>, bool) {
        let current = self
            .sessions
            .get(session_id)
            .and_then(SessionData::dispatched_at);
        if let Some(wanted) = only_dispatched_at
            && current != Some(wanted)
        {
            tracing::warn!(
                session_id = %session_id,
                aborted = ?wanted,
                current = ?current,
                "dropping a stream-rule abort for a superseded generation"
            );
            return (current, false);
        }

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
        (dispatched_at, had_session)
    }

    /// Cancels the active stream for a session and emits a completion event.
    async fn cancel_stream(&mut self, session_id: &SessionId) {
        let (dispatched_at, had_session) = self.abort_stream(session_id, None).await;

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
/// `jinn-watchdog` slice's stall-watchdog actor, which treats a stall like
/// a hard server error and re-dispatches the turn.
#[expect(
    clippy::too_many_arguments,
    reason = "one argument per part of the provider request, plus the rules cell"
)]
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
    rules: Option<Box<dyn StreamRuleSession + '_>>,
    // The stream-rules cell, carried so the turn's fire record can be ended
    // when the stream task finishes. `None` when no matcher is installed.
    rules_cell: Option<jinn_slices::TypedCell<jinn_slices::StreamRules>>,
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

    let mut rules = rules;
    process_stream_events(
        stream,
        &bus,
        &sid,
        &model_id,
        dispatched_at,
        rules
            .as_mut()
            .map(|boxed| boxed.as_mut() as &mut (dyn StreamRuleSession + '_)),
    )
    .await;

    // The session's interrupt count outlives every response in it, and this is
    // where the last one is done with.
    //
    // Only a response that ran to completion without an interrupt debits it.
    // An interrupted one returns above without reaching here, so its debt
    // carries into the resumed attempt — which is the whole point, since two
    // consecutive interrupts are the failure the budget bounds. Dropping the
    // buffers first means a rule cannot see content from a response it has
    // already ended.
    drop(rules.take());
    if let Some(cell) = rules_cell {
        cell.read().end_response(&sid, TurnEnd::Finished);
    }
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
        jinn_provider_config::request_retry_to_provider_config(retry_config),
        Box::new(PushEntryOnRetry::new(bus.clone(), sid.clone())),
    ))
}

#[cfg(test)]
mod inference_actor_tests;

#[cfg(test)]
/// Builds fake [`Services`] over the given bus for struct-direct tests: the
/// actor publishes stream events through `services.bus`, so tests hand in the
/// harness's bus to observe them.
pub async fn test_services_with_bus(
    bus: jinn_kernel::common::services::bus_service::BusService,
) -> jinn_kernel::common::services::Services {
    jinn_kernel::common::services::Services::new_fake_with_bus(bus).await
}
