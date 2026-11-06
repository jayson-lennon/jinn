use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use futures::StreamExt as _;
use jiff::Timestamp;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_inference_msg::{
    AbortStream, CancelTurn, SendToLlmProvider, StreamActivity, StreamCompleted,
    StreamCompletedReason, StreamToken,
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
use jinn_slices::{RuleFired, StreamRuleSession};
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
///
/// `turn` and `stream` are the identity the failing dispatch carried — the
/// grant the dispatch minted, threaded through every failure site. The
/// session actor's ledger admission resolves the completion against the
/// live turn by exactly this pair, which is what makes a mid-turn error
/// settle the session instead of being refused as a straggler.
async fn emit_stream_error(
    bus: &BusService,
    session_id: &SessionId,
    message: String,
    dispatched_at: Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
    retryable: bool,
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
        turn,
        stream,
        retryable,
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
    /// Shared application state, read for the session's live working
    /// directory at rule-mint time.
    state: jinn_kernel::common::state::State,
    /// Application-wide runtime services (bus publish, LLM factory,
    /// provider registry, api keys, request dump, prefs reads).
    services: Services,
    /// Active stream tasks, keyed by session ID.
    tasks: HashMap<SessionId, tokio::task::JoinHandle<()>>,
    /// Per-session state.
    sessions: HashMap<SessionId, SessionData>,
}

impl jinn_kernel::common::actor_deps::BusPublish for InferenceActor {
    fn bus(&self) -> &BusService {
        &self.services.bus
    }
}

impl ServiceActor for InferenceActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(
        _args: &trouper::json::Json,
    ) -> Result<Self, Report<trouper::registry::RegistryError>> {
        // Never called: the spawn helper injects services via `init_with`.
        Err(
            error_stack::IntoReport::into_report(trouper::registry::RegistryError::InvalidSpec)
                .attach("InferenceActor is spawned via init_with"),
        )
    }
}

impl InferenceActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the inference topic (composition's
    /// `SliceHost::subscribe_service`) — subscribe is the readiness
    /// point, so it must follow this call before any publish.
    pub fn spawn(
        system: &ActorSystem,
        state: jinn_kernel::common::state::State,
        services: Services,
    ) -> ActorPath {
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(ActorPath::new(INFERENCE_PATH))
            .init_with({
                move || {
                    let state = state.clone();
                    let services = services.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state,
                            services,
                            tasks: HashMap::new(),
                            sessions: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<SendToLlmProvider>()
            .handles::<CancelTurn>()
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
    async fn handle(&mut self, msg: &AbortStream, _ctx: &mut MsgCtx<'_>) {
        self.abort_stream(&msg.session_id, Some(msg.dispatched_at))
            .await;
    }
}

impl MsgHandler<CancelTurn> for InferenceActor {
    /// Stops the stream, and publishes nothing.
    ///
    /// Reporting the turn's end is the session actor's job — it owns the
    /// phase, and its `StreamCompleted(Canceled)` handler settles it. This
    /// actor used to publish that completion itself, and the session actor
    /// settles independently of anything published, so a cancel that both
    /// reported and settled ran the settle twice.
    ///
    /// The latch is armed here rather than on the dispatch side because this
    /// is the only actor that sees dispatches. The stream-rule watchdog's
    /// resume is already queued by the time its interrupt is counted — the
    /// intercept tore the generation down first — and it arrives as a
    /// user-originated send, the only origin that lifts the tombstone this
    /// teardown just armed. Dropping it is therefore the only way the trip
    /// stops the loop rather than resetting its counter.
    async fn handle(&mut self, msg: &CancelTurn, _ctx: &mut MsgCtx<'_>) {
        self.abort_stream(&msg.session_id, None).await;
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
/// How one response's event loop came to an end.
///
/// Two outcomes, and the distinction matters only to the log. A response a
/// rule interrupted has already published its `StreamCompleted(RuleIntercept)`
/// by the time this returns, so the loop holds no budget to update and no
/// decision left to make: whether a session that keeps violating a rule should
/// be cancelled belongs to the stream-rule watchdog, which reads that same
/// completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    /// The response ran to completion: `Done`, or a terminal `Error`.
    Completed,
    /// A rule matched and the turn is being resumed rather than finished.
    Interrupted,
}

/// Processes one response's stream events, returning how the stream ended.
#[expect(
    clippy::too_many_lines,
    reason = "the event loop mirrors the provider's StreamEvent enum arm for arm; splitting it would separate each arm's activity publish from its domain message"
)]
async fn process_stream_events(
    mut stream: jinn_provider::ToolStream,
    bus: &BusService,
    sid: &SessionId,
    model_id: &str,
    dispatched_at: jiff::Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream_id: jinn_core_types::turn_identity::StreamId,
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
) -> StreamOutcome {
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
                        return apply_rule_match(
                            bus,
                            sid,
                            &mut accum,
                            fired,
                            dispatched_at,
                            turn,
                            stream_id,
                        )
                        .await;
                    }
                    // Rule verdict first, loop verdict second: both end the
                    // stream before this delta or any later one is handled.
                    if let Some(signal) = crate::repetition::scan(&accum.text) {
                        return apply_loop_interrupt(
                            bus,
                            sid,
                            signal,
                            "text",
                            dispatched_at,
                            turn,
                            stream_id,
                            &accum.model_id,
                        )
                        .await;
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
                        return apply_rule_match(
                            bus,
                            sid,
                            &mut accum,
                            fired,
                            dispatched_at,
                            turn,
                            stream_id,
                        )
                        .await;
                    }
                    // As on the text path: the loop check runs only after
                    // the rule check declined, before the next delta.
                    if let Some(signal) = crate::repetition::scan(&accum.thinking) {
                        return apply_loop_interrupt(
                            bus,
                            sid,
                            signal,
                            "thinking",
                            dispatched_at,
                            turn,
                            stream_id,
                            &accum.model_id,
                        )
                        .await;
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
                    // The repetition detector scans the accumulated argument
                    // text (kept here; the rule session buffers privately),
                    // one step behind the rule verdict — same rule-first
                    // ordering as the text and reasoning paths.
                    let args = accum.tool_args.entry(index).or_default();
                    args.push_str(&partial_json);
                    let loop_signal = crate::repetition::scan(args);
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
                        return apply_rule_match(
                            bus,
                            sid,
                            &mut accum,
                            fired,
                            dispatched_at,
                            turn,
                            stream_id,
                        )
                        .await;
                    }
                    if let Some(signal) = loop_signal {
                        return apply_loop_interrupt(
                            bus,
                            sid,
                            signal,
                            "tool-args",
                            dispatched_at,
                            turn,
                            stream_id,
                            &accum.model_id,
                        )
                        .await;
                    }
                }
                StreamEvent::ToolUseComplete { index, tool_call } => {
                    publish_activity(bus, sid).await;
                    accum.tool_calls.push(tool_call.clone());
                    // The index is reusable: drop this call's argument
                    // buffer so the next call on the same index starts
                    // clean.
                    accum.tool_args.remove(&index);
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
                StreamEvent::Done {
                    stop_reason,
                    usage,
                    model,
                } => {
                    // Terminal: `StreamCompleted` governs a stream's end, so
                    // this arm declares no liveness.
                    //
                    // Finalize before routing: a match held at a buffer's
                    // edge mid-stream is proven by the stream's end, and the
                    // intercept must preempt the tool batch the Done arm is
                    // about to publish — an intercepted call never executes.
                    // The error and abort paths deliberately do not
                    // finalize: a dying stream must not manufacture an
                    // interrupt nobody watched arrive.
                    let finalized = rules
                        .as_deref_mut()
                        .and_then(jinn_slices::StreamRuleSession::finalize);
                    if let Some(fired) = finalized {
                        return apply_rule_match(
                            bus,
                            sid,
                            &mut accum,
                            fired,
                            dispatched_at,
                            turn,
                            stream_id,
                        )
                        .await;
                    }
                    handle_done_event(
                        bus,
                        sid,
                        &mut accum,
                        stop_reason,
                        usage,
                        model,
                        dispatched_at,
                        turn,
                        stream_id,
                    )
                    .await;
                    return StreamOutcome::Completed;
                }
                StreamEvent::Error {
                    message, retryable, ..
                } => {
                    // Terminal: as with `Done`, no liveness is declared.
                    emit_terminal_error(
                        bus,
                        sid,
                        &mut accum,
                        events_seen,
                        format!("LLM stream error: {message}"),
                        dispatched_at,
                        turn,
                        stream_id,
                        "LLM stream error event",
                        retryable,
                    )
                    .await;
                    return StreamOutcome::Completed;
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
                    turn,
                    stream_id,
                    "LLM stream chunk error",
                    true,
                )
                .await;
                return StreamOutcome::Completed;
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
        turn,
        stream_id,
        "LLM stream ended without a terminal event (Done/Error)",
        true,
    )
    .await;
    StreamOutcome::Completed
}

/// Acts on what a stream rule matched.
///
/// One outcome, which is the point: a match is a fact, and the loop's only
/// response to a fact is to interrupt and resume. The matcher holds no count
/// and no budget, so there is nothing here to branch on — a rule that keeps
/// matching a model that keeps ignoring it has stopped being a correction, and
/// the stream-rule watchdog is what notices and ends the session.
async fn apply_rule_match(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    fired: RuleFired,
    dispatched_at: jiff::Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
) -> StreamOutcome {
    intercept_and_resume(bus, sid, accum, fired, dispatched_at, turn, stream).await;
    StreamOutcome::Interrupted
}

/// Ends a stream a rule interrupted, and asks for the turn to resume.
///
/// Three publishes, in this order:
///
/// 1. [`AbortStream`] — the teardown. It shares the cancel path's own teardown
///    so the tombstone is armed exactly as a user cancel arms it, and so the
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
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
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
        turn,
        stream,
        retryable: false,
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

/// Ends a stream the repetition detector flagged, returning the loop's
/// stream outcome.
///
/// The intercept's teardown minus the guidance entry: two publishes, in
/// this order —
///
/// 1. [`StreamCompleted`] with
///    [`StreamCompletedReason::RepetitionLoop`] — the terminal fact the
///    session actor's retry handler acts on.
/// 2. [`AbortStream`] — the teardown, stamped with this generation for
///    the same reason the intercept stamps it, and published last so it
///    cannot cut off the completion (the abort cancels the very task
///    that issued it).
///
/// There is deliberately *no* [`PushChatEntry`]: the model is never told
/// it looped — a guidance entry cannot help it and would poison the
/// retry's context. The user-facing marker is the watchdog's job
/// (`jinn-watchdog`), and system entries are already excluded from LLM
/// context by default, so nothing here speaks for the model to read.
#[expect(
    clippy::too_many_arguments,
    reason = "one argument per part of the completion payload, mirroring intercept_and_resume"
)]
async fn apply_loop_interrupt(
    bus: &BusService,
    sid: &SessionId,
    signal: crate::repetition::RepetitionSignal,
    buffer: &'static str,
    dispatched_at: jiff::Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
    model_id: &str,
) -> StreamOutcome {
    tracing::warn!(
        session_id = ?sid,
        buffer,
        period = signal.period,
        repeats = signal.repeats,
        "repetition loop detected; ending the stream for a silent retry"
    );

    bus.publish(StreamCompleted {
        model_used: Some(model_id.to_owned()),
        session_id: sid.clone(),
        reason: StreamCompletedReason::RepetitionLoop,
        assistant_content: None,
        tool_calls: None,
        cost: None,
        provider_completion_tokens: None,
        provider_prompt_tokens: None,
        cached_tokens: None,
        thinking_content: None,
        dispatched_at,
        turn,
        stream,
        // The loop retry is the stuck-loop watchdog's domain, not this
        // one: a `true` here would schedule a provider-error retry on top
        // of the silent loop retry racing the same turn.
        retryable: false,
    })
    .await;

    // Published after the completion, stamped with this generation, and
    // last — the same ordering contract as the intercept's teardown.
    bus.publish(AbortStream {
        session_id: sid.clone(),
        dispatched_at,
    })
    .await;
    StreamOutcome::Interrupted
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
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
    log_label: &str,
    retryable: bool,
) {
    tracing::info!(
        session_id = ?sid,
        events_seen,
        tool_calls_buffered = accum.tool_calls.len(),
        error = %message,
        "{log_label} - terminal"
    );
    tracing::error!(session_id = ?sid, error = %message, "{log_label}");
    emit_stream_error(bus, sid, message, dispatched_at, turn, stream, retryable).await;
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
    /// The accumulated argument text of each in-flight tool call, keyed by
    /// its index — the repetition detector's input for the args buffer,
    /// which the rule session keeps privately.
    ///
    /// The entry is cleared when the call completes: providers reuse an
    /// index for the next call, and a fresh call must not inherit the
    /// previous call's argument text.
    tool_args: HashMap<usize, String>,
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
            tool_args: HashMap::new(),
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
///
/// `response_model` is the model id the provider echoed on its stream. When
/// present it wins over the request-time stamp — it is the authoritative
/// answer to "which model actually answered", which is the only interesting
/// answer for a routing provider. `None` (transport never echoes a model)
/// keeps `accum.model_id`.
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
    response_model: Option<String>,
    dispatched_at: jiff::Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream: jinn_core_types::turn_identity::StreamId,
) {
    let model_used = {
        let requested = accum.model_id.clone();
        response_model
            .filter(|m| !m.is_empty())
            .unwrap_or(requested)
    };
    bus.publish(StreamCompleted {
        model_used: Some(model_used),
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
        turn,
        stream,
        // Only a failed generation carries a retry verdict; a completed
        // one (`Finished`/`ToolUse`/`RuleIntercept`) never asks for a retry.
        retryable: false,
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
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
    ctx: StreamContext<'_>,
) -> Option<RuleFired> {
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
async fn handle_reasoning_event(
    bus: &BusService,
    sid: &SessionId,
    dispatched_at: jiff::Timestamp,
    accum: &mut StreamAccumulator,
    token: String,
    mut rules: Option<&mut (dyn StreamRuleSession + '_)>,
    ctx: StreamContext<'_>,
) -> Option<RuleFired> {
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
///
/// `model` is the provider-reported model from the stream; it overrides the
/// request-time model in `StreamCompleted.model_used` whenever present
/// (see [`publish_stream_completed`]).
async fn handle_done_event(
    bus: &BusService,
    sid: &SessionId,
    accum: &mut StreamAccumulator,
    stop_reason: StopReason,
    usage: Option<jinn_provider::StreamUsage>,
    model: Option<String>,
    dispatched_at: jiff::Timestamp,
    turn: jinn_core_types::turn_identity::TurnToken,
    stream_id: jinn_core_types::turn_identity::StreamId,
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
        // Order on the bus does not decide anything: the session actor
        // registers the model's calls from the StreamCompleted(ToolUse) and
        // the orchestrator registers them again idempotently at dispatch,
        // so either message may land first.
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
            model,
            dispatched_at,
            turn,
            stream_id,
        )
        .await;
        bus.publish(ExecuteToolBatch {
            session_id: sid.clone(),
            tool_calls,
            dispatched_at,
            turn,
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
            model,
            dispatched_at,
            turn,
            stream_id,
        )
        .await;
    }
}

impl InferenceActor {
    /// Dispatches incoming commands to the appropriate handler.
    /// Starts an LLM streaming response for a session, aborting any existing stream.
    /// Narrows the installed rule set to `session_id`'s working directory and
    /// mints the matching state for one response.
    ///
    /// The cwd is bound *here*, once per stream, rather than per delta. A
    /// rule's `project` key is a plain glob against the session's working
    /// directory — nothing else; the live value is read from shared state so
    /// it follows `cd`-style moves, with a stream seeing the directory it was
    /// minted under.
    ///
    /// A session whose cwd cannot be read from the live map gets the global
    /// rules only: a rule carrying no `project` applies everywhere, so a rule
    /// carrying one must not fire where the directory is unknown.
    //
    // The `async` stays even though nothing awaits: callers (and tests)
    // `await` this method, and keeping the signature spares them churn when a
    // rule source that needs awaits arrives.
    #[expect(
        clippy::unused_async,
        reason = "signature symmetry: callers await resolve_rule_session"
    )]
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "signature symmetry: callers await resolve_rule_session"
    )]
    async fn resolve_rule_session(
        &self,
        session_id: &SessionId,
    ) -> Option<Box<dyn StreamRuleSession + 'static>> {
        let cell = self
            .services
            .slices
            .reader::<jinn_slices::StreamRules>(&jinn_slices::stream_rules_slot())?;

        // The cell handle is cloned out first: the session borrows the
        // installed set, so the handle must outlive the local `reader` binding
        // above.
        let cell = cell.clone();

        let cwd = self
            .state
            .read()
            .session
            .get(session_id)
            .map(|session| session.cwd().clone());

        let guard = cell.read();
        let set = guard.for_cwd(cwd.as_ref());
        set.filter(|set| !set.is_empty())
            .map(|set| set.new_session(session_id))
    }

    async fn start_stream(&mut self, payload: &SendToLlmProvider) {
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
            Err(report) => {
                emit_stream_error(
                    &self.services.bus,
                    &session_id,
                    report.to_string(),
                    payload.dispatched_at,
                    payload.turn,
                    payload.stream,
                    false,
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
        let bus = self.services.bus.clone();
        let sid = session_id.clone();
        let dispatched_at = payload.dispatched_at;

        // Dump the complete assembled request payload (one file per dispatch).
        self.services.request_dump.dump(payload);

        // Resolved once per stream, not per delta: with no rules configured
        // this is `None` and the stream path is byte-for-byte what it was.
        let rules = self.resolve_rule_session(&session_id).await;

        let handle = tokio::spawn(run_stream(
            factory,
            bus,
            sid,
            model_id,
            system_prompt,
            messages,
            tools,
            dispatched_at,
            payload.turn,
            payload.stream,
            retry_config,
            rules,
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
    ) -> Result<LlmServiceFactoryService, Report<LlmResolveError>> {
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
                    Err(Report::from(LlmResolveError(format!(
                        "LLM factory creation failed for {pid}: {e:?}"
                    ))))
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
                // Session stays tracked - the continuation runs when the
                // session actor sees the turn's work set empty (the calls'
                // results landed and the tool-use stream retired). The next
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
            #[expect(
                clippy::match_same_arms,
                reason = "arms kept separate: each comment names the distinct path that already cleaned the session up"
            )]
            StreamCompletedReason::RuleIntercept => {
                // Already cleaned up by abort_stream, which the stream task
                // reached through `AbortStream` before publishing this.
            }
            #[expect(
                clippy::match_same_arms,
                reason = "arm kept separate: the loop path's cleanup is the abort's, same as an intercept — merging would hide which teardown owns it"
            )]
            StreamCompletedReason::RepetitionLoop => {
                // Already cleaned up by abort_stream, which the stream task
                // reaches through `AbortStream` after publishing this.
            }
        }
    }

    /// Tears a session's stream down: arms the tombstone, cancels pending
    /// tool batches, aborts the task, and forgets the session.
    ///
    /// Shared by every teardown: a rule intercept needs the same work with a
    /// *different* terminal reason. Publishing a
    /// `CancelTurn` to get it would end the turn rather than resume it, and
    /// would race the intercept's own `RuleIntercept` completion.
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
    turn: jinn_core_types::turn_identity::TurnToken,
    stream_id: jinn_core_types::turn_identity::StreamId,
    retry_config: RequestRetryConfig,
    rules: Option<Box<dyn StreamRuleSession + '_>>,
) {
    let service = match build_streaming_service(&factory, &retry_config, &bus, &sid) {
        Ok(s) => s,
        Err(report) => {
            emit_stream_error(
                &bus,
                &sid,
                report.to_string(),
                dispatched_at,
                turn,
                stream_id,
                false,
            )
            .await;
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
            let retryable = classify_construction_error(&e);
            emit_stream_error(
                &bus,
                &sid,
                format!("LLM stream error: {e:?}"),
                dispatched_at,
                turn,
                stream_id,
                retryable,
            )
            .await;
            return;
        }
    };

    let mut rules = rules;
    let outcome = process_stream_events(
        stream,
        &bus,
        &sid,
        &model_id,
        dispatched_at,
        turn,
        stream_id,
        rules
            .as_mut()
            .map(|boxed| boxed.as_mut() as &mut (dyn StreamRuleSession + '_)),
    )
    .await;

    // Dropping the buffers means a rule cannot see content from a response
    // that has already ended.
    drop(rules.take());
    tracing::debug!(
        session_id = %sid,
        ?outcome,
        "provider stream task finished"
    );
}

/// Error for provider-service resolution inside the inference actor.
///
/// The failure is consumed as `Display` text at the `emit_stream_error`
/// boundary; the type exists so signatures carry a domain, not a string.
#[derive(Debug, wherror::Error)]
#[error("{0}")]
struct LlmResolveError(String);

/// Classifies a stream-construction failure for the provider-error watchdog.
///
/// `true` when the construction error is a transient transport class
/// ([`LlmServiceError::Retryable`] or [`LlmServiceError::RateLimited`]);
/// `false` for provider, api-key, and config failures a retry cannot fix.
fn classify_construction_error(error: &Report<LlmServiceError>) -> bool {
    matches!(
        error.downcast_ref::<LlmServiceError>(),
        Some(LlmServiceError::RateLimited { .. } | LlmServiceError::Retryable)
    )
}

/// Constructs a fresh retrying service for one streaming attempt.
fn build_streaming_service(
    factory: &LlmServiceFactoryService,
    retry_config: &RequestRetryConfig,
    bus: &BusService,
    sid: &SessionId,
) -> Result<RetryingLlmService, Report<LlmResolveError>> {
    let service: Box<dyn LlmService> = factory.create().map_err(|e| {
        tracing::error!(err = ?e, "failed to create LLM service");
        Report::from(LlmResolveError(format!(
            "LLM service creation failed: {e:?}"
        )))
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
