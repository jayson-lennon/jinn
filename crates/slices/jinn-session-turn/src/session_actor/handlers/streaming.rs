//! Streaming lifecycle handlers - manage token streaming and stream completion.
//!
//! Handles appending individual tokens to the assistant entry (including
//! reasoning/thinking tokens), and finalizing the stream with token accounting
//! and queue draining on `StreamCompleted`.

use std::collections::VecDeque;

use jinn_context_assembly_msg::ContextOverrideChanged;
use jinn_core_types::SessionId;
use jinn_core_types::tool_types::ToolCall;
use jinn_inference_msg::{CancelTurn, StreamCompleted, StreamCompletedReason, StreamToken};
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::protocol::{ChatEntry, ChatEntryId, ChatEntryKind};
use jinn_llm_support::token_estimator::{TiktokenCounter, TokenCounter};
use jinn_session_history_msg::CitationsReceived;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_session_state::ChatSessionState;
use jinn_tools_msg::CancelToolBatch;
use jinn_turn_dispatch_msg::QueueItem;

use super::super::SessionPersistenceActor;
use jinn_slices::PhaseView;

impl SessionPersistenceActor {
    /// Ends a turn and reports its end — the one routine that terminates a turn.
    ///
    /// Every path that ends a turn without a provider stream to end goes through
    /// here: a person pressing Escape, a watchdog giving up, a subagent teardown,
    /// a bot rerun. There is no second way to report a turn's end, which is
    /// what makes the report reliable enough to hang a phase transition on.
    ///
    /// Unconditional, and that is load-bearing. A watchdog cancel lands *after*
    /// this actor already published the resume it is meant to stop:
    /// `on_send_to_llm_provider` arms the in-flight guard on receipt of every
    /// `SendToLlmProvider`, including the one the intercept re-dispatches. The
    /// inference actor then drops that resume, because it is the only actor that
    /// can know the dispatch was latched away. Nothing downstream of that drop
    /// ever runs — no provider, no completion, no settle — so the guard is never
    /// cleared and the phase is never left. The session sits `Streaming` with a
    /// stream that does not exist, and the stall watchdog re-triggers on the
    /// silence.
    ///
    /// Skipping the report "because the phase already looks done" is what
    /// produced that wedge. The phase is the thing that is broken, so it cannot
    /// be the evidence that the turn is over.
    ///
    /// Re-reporting is safe when the caller settled the phase synchronously
    /// first, which the frontend's Escape path does: `apply_stream_completion`
    /// entering an already-`Idle` session is a no-op for the phase, because
    /// `machine.cancel()` rejects it, and this actor is the *only* publisher of
    /// a `StreamCompleted(Canceled)`. Both halves are serialized through this
    /// actor, so whichever order they arrive in, exactly one settle happens.
    pub(in crate::session_actor) async fn terminate_turn(&self, msg: &CancelTurn) {
        // A turn ends once. The once-ness lives in the ledger: `begin_cancel`
        // is idempotent — the first call flips the turn's intent to
        // `Cancel` and starts the drain, later calls are no-ops, and this
        // handler publishes nothing on them. Skipping on the phase instead
        // was wrong twice over: it skipped the report for a busy session
        // whose turn had genuinely ended, and it read `Idle` for two
        // different reasons it could not tell apart. The ledger's answer is
        // the turn's own state, not a phase the history happens to sit in.
        let outcome = self.services.turn_ledger.begin_cancel(&msg.session_id);
        let Some(outcome) = outcome else {
            tracing::debug!(
                session_id = %msg.session_id,
                "CancelTurn with nothing to drain (no live turn, or already \
                 draining); not reporting twice"
            );
            return;
        };

        // A confirmed ESC-ESC asks that the turn's buffered work — the
        // steering buffer and the turn queue — come back as input-box text
        // at the settle, instead of re-dispatching after it. The request is
        // latched on the session here and consumed by the settle's `Cancel`
        // arm, because the settle can trail this message: `StartedAndSettled`
        // settles inline, but a `Started` drain waits for the stream ack, and
        // anything the user enqueues behind this cancel must be dumped with
        // the rest rather than dispatched.
        if msg.drain_buffers_to_input {
            self.state.with_session(|view| {
                view.session
                    .map()
                    .get_or_create(&msg.session_id)
                    .request_cancel_drain();
            });
        }

        // The drain is armed. Open tool calls get a synthesized cancellation
        // from the orchestrator (`CancelToolBatch`), so the work set empties
        // without waiting on a tool that cannot be aborted; the stream's own
        // abort rides `CancelTurn`'s broadcast to the inference actor. The
        // completion published below retires the stream; the last closer —
        // synthesis or stream ack — settles the turn exactly once.
        self.publish(CancelToolBatch {
            session_id: msg.session_id.clone(),
        })
        .await;

        // The stamp must be the session's own live generation, not `now()`. A
        // fresh stamp on a session with a live generation sorts *before* it, and
        // the stale-generation guard drops the report on arrival — closing the
        // wedge by reproducing it. The mint's stamp is what the session armed;
        // a session with no live generation has nothing to settle, so the
        // current time is both correct and sufficient.
        let dispatched_at = self
            .state
            .read()
            .session
            .get(&msg.session_id)
            .and_then(jinn_kernel::ChatSessionState::stream_dispatched_at)
            .unwrap_or_else(jiff::Timestamp::now);

        // `StartedAndSettled` means the cancel emptied the work set itself:
        // the entry is already swept, so there is no live identity to stamp
        // and no ledger that would admit a completion — the settle runs
        // here instead, publishing the single `Canceled` report and the
        // phase change directly.
        if outcome == jinn_kernel::common::turn_ledger::CancelOutcome::StartedAndSettled {
            self.settle_turn(
                &msg.session_id,
                jinn_kernel::common::turn_ledger::Next::Cancel,
            )
            .await;
            return;
        }

        // The cancel's completion carries the live turn's identity: the
        // ledger's live stream is exactly the one this completion retires,
        // and its admission runs the settle. A session whose turn was
        // settled out from under this drain (a racing completion already
        // ran the settle) has nothing to stamp — and nothing that would
        // admit the report — so nothing publishes. Turn live but stream
        // already retired (tools running, the continuation window): the
        // unset stream retires nothing, so the ack refuses and the settle
        // waits for the last synthesized close; the completion's session
        // side (entries, queue drain) still runs.
        let Some(live) = self.services.turn_ledger.live(&msg.session_id) else {
            tracing::debug!(
                session_id = %msg.session_id,
                "cancel drain found no live turn to stamp; the settle already \
                 ran — no completion to publish"
            );
            return;
        };
        let (turn, stream) = (live.turn, live.stream.unwrap_or_default());

        self.publish(StreamCompleted {
            model_used: None,
            session_id: msg.session_id.clone(),
            reason: StreamCompletedReason::Canceled,
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
        })
        .await;
    }

    /// Handles a cancel request by terminating the turn it names.
    pub(in crate::session_actor) async fn on_cancel_turn(&self, msg: &CancelTurn) {
        self.terminate_turn(msg).await;
    }

    /// Appends a streaming token to the session's assistant entry,
    /// or to the thinking entry if the token is flagged as reasoning.
    pub(in crate::session_actor) fn on_stream_token(&self, event: &StreamToken) {
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            match session.phase() {
                PhaseView::Streaming => {}
                // No defensive arm in `Working`: a token there belongs to a
                // generation whose dispatch was refused or torn down, and
                // arming `Streaming` for it would back the phase with no
                // live turn. The token is appended only when the ledger's
                // mint has admitted the stream — the entry index below
                // exists exactly then.
                #[expect(
                    clippy::match_same_arms,
                    reason = "arms kept separate: `Streaming` is the healthy path and `Working` is deliberately tolerated, not defended"
                )]
                PhaseView::Working | PhaseView::Cancelling => {}
                PhaseView::Idle => {
                    tracing::warn!(
                        phase = ?session.phase(),
                        "StreamToken received in unexpected phase"
                    );
                }
            }
            if event.is_thinking {
                if session.streaming_thinking_entry_index().is_none() {
                    session.begin_thinking(event.dispatched_at);
                }
                if let Err(e) = session.append_thinking_token(&event.token) {
                    tracing::error!(err = ?e, "failed to append thinking token");
                }
            } else {
                // First non-thinking (content) token ends the reasoning phase:
                // finalize the thinking entry's duration before the content begins.
                if let Some(idx) = session.streaming_thinking_entry_index() {
                    session.finish_thinking_entry(idx);
                }
                if let Err(e) = session.append_stream_token(&event.token, event.dispatched_at) {
                    tracing::error!(err = ?e, "failed to append stream token");
                }
            }
        });
    }

    /// Handles `TurnCompleted`, which this actor itself publishes.
    ///
    /// The actor subscribes to its own event so the outcome is visible to any
    /// in-actor follow-up work without a second derivation. Currently a
    /// log-only sink; the bot slice runs its own subscriber for the
    /// re-run trigger.
    #[expect(
        clippy::unused_async,
        reason = "handler keeps the actor-method signature used by the subscription table"
    )]
    pub(in crate::session_actor) async fn on_turn_completed(&self, event: &TurnCompleted) {
        tracing::debug!(
            session_id = %event.session_id,
            outcome = ?event.outcome,
            "turn completed"
        );
    }

    /// Marks the session's stream as finished, records output tokens, and drains
    /// any queued messages into a new turn.
    ///
    /// This is orchestration only: token counting, locked-state mutation, and
    /// override-change emission are each delegated to a dedicated helper so the
    /// handler reads as a step-by-step recipe. See [`Self::apply_stream_completion`]
    /// for the under-lock state transitions and [`resolve_output_tokens`] for the
    /// token-accounting policy.
    pub(in crate::session_actor) async fn on_stream_completed(&self, event: &StreamCompleted) {
        let should_save = matches!(
            event.reason,
            StreamCompletedReason::Finished
                | StreamCompletedReason::Error
                | StreamCompletedReason::Canceled
                | StreamCompletedReason::RuleIntercept
                | StreamCompletedReason::RepetitionLoop,
        );

        // A rule intercept does not end the turn: the stream was stopped so
        // the offending output could be taken out of context and the turn
        // re-dispatched with the rule's body.
        //
        // Handled before the completion fold, which would otherwise finalize
        // the streaming entries and clear the in-flight-stream guard — the
        // two things the resume needs to tell a live intercept from a stale
        // one, and the entries it must exclude from the resumed request.
        // Falling through would also read the resulting phase transition as a
        // turn end and report the turn as finished.
        if event.reason == StreamCompletedReason::RuleIntercept {
            self.on_rule_intercept(&event.session_id, event.reason, event.dispatched_at)
                .await;
            return;
        }

        // A repetition loop does not end the turn either: the stream was
        // stopped so the looped output could be taken out of context and
        // the turn re-dispatched silently — no guidance, no fold.
        //
        // Handled before the completion fold for the same reason as the
        // intercept: the fold would finalize the streaming entries, clear
        // the in-flight-stream guard, and report the turn ended — every
        // one of which the retry needs intact or refused as stale.
        if event.reason == StreamCompletedReason::RepetitionLoop {
            self.on_loop_retry(&event.session_id, event.reason, event.dispatched_at)
                .await;
            return;
        }

        // Count output tokens outside the lock (may spawn_blocking).
        let output_tokens = resolve_output_tokens(self.counter, event).await;

        // Entry work under the write lock: push reason-specific entries,
        // finalize token accounting, sweep dangling tool calls on hard
        // cancel, drain queued messages on error/cancel. A `None` return
        // means the completion was from a superseded stream generation and
        // was dropped — emit nothing, settle nothing.
        let Some(fold) = self.apply_stream_completion(event, output_tokens) else {
            tracing::debug!(
                session_id = %event.session_id,
                "StreamCompleted dropped (stale generation); skipping downstream events"
            );
            return;
        };
        self.emit_override_changes(&event.session_id, fold.changed_overrides)
            .await;

        // Retire the completed stream in the ledger and learn what the
        // turn wants next. A ToolUse completion records `Continue` — the
        // tool calls are open work now, and the turn ends when the work
        // set empties, whichever message lands last. Every other reason
        // records `End`. A refused ack is a stale stream (a retry swapped
        // it out) or a turn that already ended; the fold's entries landed,
        // nothing else happens.
        //
        // A ToolUse ack registers the model's calls in the same critical
        // section that retires the stream: the batch can trail the ack on
        // the bus, and an empty work set at the ack would settle the turn
        // before any result landed. Registering here — and idempotently at
        // the batch — makes both orders one behaviour.
        let tool_call_ids = event
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default();
        let ack = self.services.turn_ledger.ack_stream_with_calls(
            &event.session_id,
            &event.turn,
            &event.stream,
            match event.reason {
                StreamCompletedReason::ToolUse => jinn_kernel::common::turn_ledger::Next::Continue,
                _ => jinn_kernel::common::turn_ledger::Next::End,
            },
            tool_call_ids,
        );

        match ack {
            Err(refused) => {
                tracing::debug!(
                    session_id = %event.session_id,
                    turn = ?event.turn,
                    stream = ?event.stream,
                    reason = ?refused,
                    "StreamCompleted refused by the turn ledger; no settle"
                );
            }
            Ok(Some(settlement)) => {
                // The work set emptied with this retirement: the turn is
                // over. `Continue` here means the batch landed before the
                // stream retirement — the continuation dispatches now.
                self.settle_turn(&event.session_id, settlement.next).await;
            }
            Ok(None) => {
                // Work still open (a ToolUse whose calls run, or a drain in
                // progress). The last closer settles.
            }
        }

        super::super::helpers::emit_history_appended(self.bus(), &event.session_id).await;

        // Persist session after stream finishes.
        if should_save {
            self.save_active_session(&event.session_id).await;
        }
    }

    /// Applies a turn settlement: the session's machine edge plus the turn
    /// outcome publish.
    ///
    /// `Continue` dispatches the tool continuation (the work set emptied
    /// before the stream retired — the batch won the race). `End` applies
    /// the `Idle` edge, publishes `TurnCompleted`, and drains the queue
    /// when the fold recorded a failure-shaped end. `Cancel` sweeps
    /// still-pending results to `Failure` and reports the cancellation once.
    ///
    /// Callable from both settlement sites: the stream-completion fold, and
    /// the tool-completion path when a close emptied the work set. The
    /// ledger emits exactly one settlement per round — a settled entry is
    /// swept, so later acks refuse and later closes return `None` — so
    /// whichever site observes the settlement runs this exactly once.
    pub(in crate::session_actor) async fn settle_turn(
        &self,
        session_id: &jinn_core_types::SessionId,
        next: jinn_kernel::common::turn_ledger::Next,
    ) {
        match next {
            jinn_kernel::common::turn_ledger::Next::Continue => {
                self.continue_tool_loop(session_id).await;
            }
            jinn_kernel::common::turn_ledger::Next::End => {
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(session_id);
                    let old = session.phase();
                    session.settle_to_idle();
                    // The settled turn owns the in-flight guard; the settle
                    // kills both. This is the projection side of the ledger's
                    // admission: the stamp the dispatch armed is exactly the
                    // one this turn's completion consumed.
                    session.clear_stream_generation();
                    (old, session.phase())
                });
                // The phase is a projection here; its change is broadcast so
                // subscribers see the settle the ledger adjudicated.
                if old != new {
                    jinn_kernel::common::phase_events::publish_phase_change(
                        self.bus(),
                        session_id,
                        old,
                        new,
                    )
                    .await;
                }
                let last_entry = self.state.with_session(|view| {
                    view.session
                        .map()
                        .get_or_create(session_id)
                        .history()
                        .last()
                        .cloned()
                });
                let outcome = outcome_from_history(last_entry);
                self.bus()
                    .publish(TurnCompleted {
                        session_id: session_id.clone(),
                        outcome,
                    })
                    .await;
            }
            jinn_kernel::common::turn_ledger::Next::Cancel => {
                // The ESC-ESC drain request, if this settle is the one the
                // flagged cancel armed, applies here — after the last
                // enqueue behind the cancel has landed and before the
                // `Idle` phase change sends the queue actor in. Draining
                // empties the buffers, so the idle transition finds nothing
                // to re-dispatch. An unflagged cancel (bot re-run, a
                // watchdog, a cascade child, the interrupt intent) takes
                // nothing, and its buffered work re-dispatches as before.
                // One `take` covers both settle routes — the inline
                // `StartedAndSettled` and this one — so a stale request
                // cannot outlive its settle.
                let drain_requested = self.state.with_session(|view| {
                    view.session
                        .map()
                        .get_or_create(session_id)
                        .take_pending_cancel_drain()
                });
                if drain_requested {
                    self.state.with_session(|view| {
                        view.session
                            .map()
                            .get_or_create(session_id)
                            .drain_cancelled_work_to_input();
                    });
                }
                let (old, new) = self.state.with_session(|view| {
                    let session = view.session.map().get_or_create(session_id);
                    session.sweep_pending_tool_results_to_failure();
                    let old = session.phase();
                    session.settle_to_idle();
                    // The cancelled turn owns the in-flight guard; killing it
                    // clears the stamp, so the stall watchdog finds nothing to
                    // re-trigger on.
                    session.clear_stream_generation();
                    (old, session.phase())
                });
                if old != new {
                    jinn_kernel::common::phase_events::publish_phase_change(
                        self.bus(),
                        session_id,
                        old,
                        new,
                    )
                    .await;
                }
                self.bus()
                    .publish(TurnCompleted {
                        session_id: session_id.clone(),
                        outcome: TurnOutcome::Canceled,
                    })
                    .await;
            }
        }
    }

    /// Handles `CitationsReceived`: appends a single display-only `Annotation`
    /// entry recording the turn's `url_citation` sources, then persists.
    pub(in crate::session_actor) async fn on_citations_received(&self, event: &CitationsReceived) {
        if event.citations.is_empty() {
            return;
        }

        {
            self.state.with_session(|view| {
                let session = view.session.map().get_or_create(&event.session_id);
                session.push_entry(ChatEntry::annotation(event.citations.clone()));
            });
        }

        super::super::helpers::emit_history_appended(self.bus(), &event.session_id).await;
        self.save_active_session(&event.session_id).await;
    }

    /// Applies all stream-completion state mutations under the write lock.
    ///
    /// Pushes reason-specific entries, finalizes token accounting, finishes
    /// streaming, sweeps dangling tool calls on hard cancel, applies pending
    /// history mutations, transitions the phase, and - on error/cancel - drains
    /// queued messages back into the input buffer for the user to retry.
    ///
    /// Returns the before/after phase and the entry IDs whose context overrides
    /// changed, so the caller can emit events outside the lock.
    fn apply_stream_completion(
        &self,
        event: &StreamCompleted,
        output_tokens: Option<u32>,
    ) -> Option<StreamCompletionFold> {
        let mut changed_overrides: Vec<ChatEntryId> = Vec::new();
        self.state
            .with_session(|view| -> Option<StreamCompletionFold> {
                let session = view.session.map().get_or_create(&event.session_id);

                // Stale-generation guard: reject terminal events from an aborted prior
                // stream (e.g. a retry re-dispatched while the old task was still
                // alive). A completion whose `dispatched_at` predates the current
                // generation is dropped silently.
                if let Some(active) = session.stream_dispatched_at()
                    && event.dispatched_at < active
                {
                    tracing::warn!(
                        session_id = %event.session_id,
                        event_dispatched_at = %event.dispatched_at,
                        active_dispatched_at = %active,
                        reason = ?event.reason,
                        "dropping stale StreamCompleted from superseded stream generation"
                    );
                    return None;
                }

                // Hard cancel: finalizes the partial entries so the terminal
                // entry lands last in history — finalization must precede
                // `apply_completion_entries` for `outcome_from_history` to
                // derive `Canceled`. The machine edge itself (the settle to
                // `Idle`) is the phase actor's, applied by the cancel that
                // preceded this completion.
                if event.reason == StreamCompletedReason::Canceled {
                    session.finalize_entries_for_cancel(event.dispatched_at);
                }

                apply_completion_entries(session, event, output_tokens);

                // Normal completion, error, and tool use: finalize the
                // entries the same way. The machine edge is the phase
                // actor's `StreamEndedFinished`/`StreamEndedToolUse`.
                if event.reason != StreamCompletedReason::Canceled {
                    let preserve_assistant = matches!(
                        event.reason,
                        StreamCompletedReason::Finished | StreamCompletedReason::ToolUse,
                    );
                    session.finalize_entries_for_finish(preserve_assistant, event.dispatched_at);
                }

                // Response-reported model override: a normal completion knows
                // which model actually answered, so the live entries' stamps
                // move from the requested model to the response's. The
                // requested model is never touched — the difference is the
                // interesting fact. Must run while the machine's indices are
                // intact, i.e. before the phase actor's settle, and after
                // finalization so an assistant entry created lazily by it
                // (empty streams) is stamped too.
                if matches!(
                    event.reason,
                    StreamCompletedReason::Finished | StreamCompletedReason::ToolUse
                ) {
                    session.override_live_entry_models(event.model_used.as_deref());
                }
                // The stream is over: one streaming round ends here. A
                // tool-use round hands off to its batch and the continuation
                // lazily creates fresh assistant/thinking entries below the
                // calls, so each round renders its own text next to its own
                // results. Cleared after the model override, which still
                // reads the indices.
                session.finish_streaming_round();
                // The generation is over (Finished/Error) or about to hand off
                // to its continuation dispatch — either way this generation's
                // request stamp is consumed. ToolUse relies on the continuation
                // dispatch receipt to restamp before any new entry is created.
                session.clear_stream_request_model();

                // Hard cancel: force-exclude dangling tool calls left by the interrupted stream.
                if event.reason == StreamCompletedReason::Canceled {
                    changed_overrides.extend(session.force_exclude_dangling_tool_calls());
                }

                // Apply pending history mutations for non-ToolUse completions.
                // ToolUse defers to on_tool_batch_completed.
                if event.reason != StreamCompletedReason::ToolUse {
                    let (count, changed) = session.drain_and_apply_pending_mutations();
                    changed_overrides.extend(changed);
                    if count > 0 {
                        tracing::debug!(
                            session_id = %event.session_id,
                            count,
                            reason = ?event.reason,
                            "applied pending history mutations at stream completion"
                        );
                    }
                }

                // Tool use means the conversation continues. The
                // `Streaming → Sending` edge is the phase actor's
                // `StreamEndedToolUse`, applied before the tool batch
                // dispatch asks its own `BeginStream(ToolContinuation)`.

                // When returning to Idle on error, drain queued messages back
                // to the input buffer so the user can review and retry. A
                // cancel does NOT: its queued messages survive the drain and
                // dispatch after the settle — the queue actor's idle
                // transition picks them up — so a cancel never eats the
                // user's queued work.
                if event.reason == StreamCompletedReason::Error {
                    let drained = session.message_queue_mut().drain();
                    if let Some(text) = drained_queue_to_text(&drained) {
                        session.update_input(|input| input.replace_all(text));
                    }
                }

                // The phase is not read here: the settle decision the
                // caller asks for next carries what the fold needs.
                Some(StreamCompletionFold { changed_overrides })
            })
    }

    /// Broadcasts [`ContextOverrideChanged`] for each entry whose override changed
    /// during stream completion. Called outside the write lock.
    async fn emit_override_changes(&self, session_id: &SessionId, entry_ids: Vec<ChatEntryId>) {
        for entry_id in entry_ids {
            self.publish(ContextOverrideChanged {
                session_id: session_id.clone(),
                entry_id,
            })
            .await;
        }
    }
}

/// Changed-entry IDs captured while mutating session state under the write
/// lock during stream completion, for the override-change broadcast outside
/// the lock. Phase facts are not here: the phase actor owns them, and the
/// settle decision carries what the caller needs.
struct StreamCompletionFold {
    changed_overrides: Vec<ChatEntryId>,
}

/// Counts output tokens locally by summing assistant content, thinking content,
/// and tool-call arguments/names.
///
/// Pure and side-effect-free so it can be unit-tested in isolation. Used as the
/// baseline when the provider undercounts (e.g., excludes tool-call arguments).
fn count_tokens_locally(
    counter: &dyn TokenCounter,
    content: &str,
    thinking: &str,
    tool_calls: Option<&[ToolCall]>,
) -> u32 {
    let base = counter.count(content) + counter.count(thinking);
    let tool_tokens = tool_calls.map_or(0, |calls| {
        calls
            .iter()
            .map(|tc| counter.count(&tc.arguments) + counter.count(&tc.name))
            .sum::<usize>()
    });
    (base + tool_tokens) as u32
}

/// Resolves the final output token count for a completed stream.
///
/// Counts locally via `spawn_blocking` (the tokenizer is CPU-bound) unless the
/// stream was canceled/errored, then takes the max of the local and
/// provider-reported counts. Providers that undercount are corrected by the
/// local count.
async fn resolve_output_tokens(counter: TiktokenCounter, event: &StreamCompleted) -> Option<u32> {
    let provider_tokens = event.provider_completion_tokens.map(|t| t as u32);

    let local_handle = if event.reason != StreamCompletedReason::Canceled
        && event.reason != StreamCompletedReason::Error
    {
        event.assistant_content.as_ref().map(|content| {
            let content = content.clone();
            let tool_calls = event.tool_calls.clone();
            let thinking = event.thinking_content.clone().unwrap_or_default();
            tokio::task::spawn_blocking(move || {
                count_tokens_locally(&counter, &content, &thinking, tool_calls.as_deref())
            })
        })
    } else {
        None
    };

    match local_handle {
        Some(handle) => {
            let local = handle.await.unwrap_or_else(|e| {
                tracing::warn!(
                    err = ?e,
                    "spawn_blocking panicked during output token counting"
                );
                0
            });
            Some(local.max(provider_tokens.unwrap_or(0)))
        }
        None => provider_tokens,
    }
}

/// Joins the display text of queued user messages into a single
/// newline-separated string.
///
/// Returns `None` when there are no user messages to drain. Tool continuations
/// contribute no text and are dropped.
fn drained_queue_to_text(items: &VecDeque<QueueItem>) -> Option<String> {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            QueueItem::UserMessage(entry) => match &entry.kind {
                ChatEntryKind::User { display, .. } => Some(display.as_str()),
                _ => None,
            },
            QueueItem::ToolContinuation => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

/// Pushes reason-specific entries and finalizes token accounting under the lock.
///
/// - `Canceled`: pushes a "Cancelled" error entry.
/// - `Error`: nothing - the error entry is pushed earlier by the LLM actor via
///   `PushChatEntry` before `StreamCompleted(Error)` is emitted.
/// - `Finished`/`ToolUse`: finalizes the last token record with output tokens,
///   cost, and model, if a record exists (i.e., a prompt was assembled first).
#[expect(clippy::else_if_without_else, reason = "no-op arms are intentional")]
fn apply_completion_entries(
    session: &mut ChatSessionState,
    event: &StreamCompleted,
    output_tokens: Option<u32>,
) {
    if event.reason == StreamCompletedReason::Canceled {
        session.push_entry(ChatEntry::error("Cancelled"));
    } else if event.reason == StreamCompletedReason::Error {
        // Error entry is pushed by the LLM actor via PushChatEntry before
        // emitting StreamCompleted(Error). Nothing to push here.
    } else if let Some(output_tokens) = output_tokens {
        // Finalize the last record if one exists (i.e., prompt assembled first).
        // If no record exists (e.g., session restored mid-stream), skip silently.
        if !session.token_ledger().is_empty()
            && let Err(e) = session.finalize_last_token_record(
                output_tokens,
                event.cost,
                event.model_used.clone(),
                event
                    .provider_prompt_tokens
                    .and_then(|t| u32::try_from(t).ok()),
                event.cached_tokens.and_then(|t| u32::try_from(t).ok()),
            )
        {
            tracing::error!(err = ?e, "failed to finalize token record");
        }
    }
}

/// Derives a turn's outcome from the session's last history entry.
///
/// The session actor pushes the terminal entries before this runs, so history
/// discriminates every outcome:
///
/// - `Error("Cancelled")` — pushed by [`apply_completion_entries`] on a cancel —
///   means the turn was cancelled.
/// - Any other error entry — pushed by the LLM actor before
///   `StreamCompleted(Error)` — means the turn failed.
/// - Anything else (an assistant entry, or no history at all) means the turn
///   succeeded.
///
/// Pure and side-effect-free so it can be unit-tested in isolation. Every
/// consumer of [`TurnCompleted`] shares this one derivation instead of
/// re-deriving the policy from transport reasons.
///
/// Note the `Cancelled` check must precede the general error check: a plain
/// error match would swallow the cancel.
fn outcome_from_history(last_entry: Option<ChatEntry>) -> TurnOutcome {
    match last_entry.map(|entry| entry.kind) {
        Some(ChatEntryKind::Error(text)) if text == "Cancelled" => TurnOutcome::Canceled,
        Some(ChatEntryKind::Error(_)) => TurnOutcome::Error,
        Some(_) | None => TurnOutcome::Succeeded,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::super::super::helpers::{
        test_actor, test_actor_recording, test_actor_with_store_recording,
    };
    use jinn_core_types::SessionId;
    use jinn_inference_msg::{CancelTurn, StreamCompleted, StreamCompletedReason, StreamToken};
    use jinn_kernel::protocol::{ChangeSource, ChatEntry, ChatEntryKind};
    use jinn_session_history_msg::CitationsReceived;
    use jinn_session_msg::SessionPhaseChanged;
    use jinn_slices::PhaseView;
    use jinn_token_count_msg::TokenRecord;

    use super::SessionPersistenceActor;
    use super::outcome_from_history;

    /// Builds the `StreamCompleted` event a provider emits when a turn ends.
    ///
    /// The identity is the live turn's, read the way production's dispatch
    /// stamped it: the ledger mint is what the completion resolves against.
    /// A fixture with no live turn cannot construct a refused event here —
    /// refused shapes are produced by supersession (settle or swap the
    /// turn), never by hand-stamping an unset identity.
    fn stream_completed(
        actor: &SessionPersistenceActor,
        session_id: &jinn_core_types::SessionId,
        reason: StreamCompletedReason,
        assistant_content: Option<&str>,
        tool_calls: Option<Vec<jinn_core_types::tool_types::ToolCall>>,
        provider_completion_tokens: Option<u64>,
        thinking_content: Option<&str>,
    ) -> StreamCompleted {
        let live = actor
            .services
            .turn_ledger
            .live(session_id)
            .expect("fixture completion needs a live turn to resolve against");
        StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason,
            assistant_content: assistant_content.map(str::to_owned),
            tool_calls,
            cost: None,
            provider_completion_tokens,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: thinking_content.map(str::to_owned),
            dispatched_at: jiff::Timestamp::now(),
            turn: live.turn,
            stream: live.stream.unwrap_or_default(),
        }
    }

    /// Builds a `StreamToken` carrying content (not thinking) for one index.
    fn content_token(
        session_id: &jinn_core_types::SessionId,
        index: usize,
        token: &str,
    ) -> StreamToken {
        StreamToken {
            session_id: session_id.clone(),
            index,
            token: token.to_owned(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        }
    }

    /// Builds a tool call the provider requests from the model.
    fn tool_call(id: &str, name: &str, arguments: &str) -> jinn_core_types::tool_types::ToolCall {
        jinn_core_types::tool_types::ToolCall {
            id: id.to_owned().into(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    /// Reads the `finished_at` stamp off the session's thinking entry.
    ///
    /// Returns `None` while the entry is still streaming; panics only when
    /// the entry's timing is not `Streamed` at all, which no streaming test
    /// can produce.
    #[expect(
        clippy::unwrap_in_result,
        reason = "the test actor is the only writer; a missing entry is a test bug"
    )]
    fn thinking_finished_at(
        actor: &SessionPersistenceActor,
        session_id: &jinn_core_types::SessionId,
    ) -> Option<jiff::Timestamp> {
        let state = actor.state.read();
        let session = state
            .session
            .get(session_id)
            .expect("session exists")
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        streamed_finished_at(session)
    }

    /// Unwraps a streamed entry's `finished_at` stamp.
    fn streamed_finished_at(entry: &jinn_kernel::protocol::ChatEntry) -> Option<jiff::Timestamp> {
        match &entry.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => *finished_at,
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    /// Puts the active session mid-turn: the ledger mints the turn
    /// (`FreshTurn`) and the projection helper writes the `Streaming` phase
    /// and arms the in-flight-stream guard — the same writes production's
    /// dispatch path makes before the first token can arrive. Returns the
    /// session id.
    ///
    /// Seeding the phase directly (`session.begin_streaming()`) would leave
    /// the ledger without a live turn, so every completion the test drives
    /// would be refused.
    async fn begin_streaming_session(
        actor: &SessionPersistenceActor,
    ) -> jinn_core_types::SessionId {
        let session_id = actor.state.read().session.active_session_id().clone();
        // One mint: the helper fuses the ledger's grant with the session-side
        // projection (phase + armed guard), exactly what production's dispatch
        // path runs. The grant it returns is the identity the completion
        // below resolves against.
        jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &actor.services,
            &actor.state,
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await
        .expect("a fresh turn's mint must be admitted");
        session_id
    }

    /// Puts the active session into the mid-tool-loop `Working` phase — the
    /// shape a tool-use completion leaves: the stream retired toward a
    /// continuation, the calls executing. Returns the session id.
    async fn begin_sending_session(actor: &SessionPersistenceActor) -> jinn_core_types::SessionId {
        let session_id = begin_streaming_session(actor).await;
        // The ledger side of the tool-use edge: the stream retires toward a
        // continuation, leaving the turn working. The identity retired is the
        // dispatch's own grant — the live stream the mint put in flight.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        actor
            .services
            .turn_ledger
            .ack_stream_ended(
                &session_id,
                &live.turn,
                &live.stream.expect("in-flight stream"),
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .expect("the tool-use edge retires the live stream");
        // The continuation joins: the orchestrator's dispatch minted a fresh
        // stream for the next round, so the turn again has an in-flight
        // stream — exactly the shape the completion below retires.
        actor
            .services
            .turn_ledger
            .mint(
                &session_id,
                jinn_kernel::common::turn_ledger::MintKind::ToolContinuation,
            )
            .expect("the continuation joins the live turn");
        // The projection side of the tool-use edge: the turn is working,
        // the stream has retired. The ledger ack above is the admission.
        actor.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .begin_sending();
        });
        session_id
    }

    /// Puts the active session into streaming phase with one queued
    /// context-override mutation, returning the targeted entry id and the
    /// session id.
    fn seed_streaming_session_with_pending_mutation(
        actor: &SessionPersistenceActor,
        assistant_text: &str,
    ) -> (jinn_core_types::ChatEntryId, jinn_core_types::SessionId) {
        let mut state = actor.state.write();
        let session = state.active_session_mut();
        session.push_entry(ChatEntry::user("hello"));
        let entry = ChatEntry::assistant(assistant_text);
        let entry_id = entry.id.clone();
        session.push_entry(entry);
        session.begin_streaming();
        session.queue_mutations(vec![jinn_core_types::HistoryMutation::SetContextOverride {
            entry_id: entry_id.clone(),
            value: jinn_core_types::ContextOverride::ForcedExclude,
            source: ChangeSource::Internal {
                label: "test".into(),
            },
        }]);
        (entry_id, state.session.active_session_id().clone())
    }

    /// Puts the active session into streaming phase with a token record
    /// pending finalization, returning its id.
    fn seed_streaming_session_with_token_record(
        actor: &SessionPersistenceActor,
    ) -> jinn_core_types::SessionId {
        let mut state = actor.state.write();
        let session = state.active_session_mut();
        session.push_token_record(TokenRecord {
            model_used: None,
            timestamp: jiff::Timestamp::now(),
            tokens_sent: 100,
            tokens_received: 0,
            cost: None,
            prompt_tokens: None,
            cached_tokens: None,
        });
        session.begin_streaming();
        state.session.active_session_id().clone()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_stops_streaming() {
        // Given a session in streaming phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session is no longer streaming.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(!matches!(session.phase(), PhaseView::Streaming));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_reason_drains_queue_to_input_buffer() {
        // Given a streaming session with one queued user message.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user("queued message"),
            )));
            state.session.active_session_id().clone()
        };

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the queue is empty and its text lands in the input buffer.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 0);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "queued message"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_with_multiple_queued_messages_joins_with_newline() {
        // Given a streaming session with two queued user messages.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user("first message"),
            )));
            session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user("second message"),
            )));
            state.session.active_session_id().clone()
        };

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then both messages are joined by a newline in the input buffer.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 0);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "first message\nsecond message"
        );
    }

    // ------------------------------------------------------------------
    // CancelTurn — the single settle entry point for a cancelled turn
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_turn_settles_a_streaming_session_to_idle() {
        // Given a session streaming.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;

        // When the turn is cancelled.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;

        // Then the session reports the turn's end.
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(
            completed.len(),
            1,
            "a cancel must report the turn's end exactly once"
        );
        assert_eq!(completed[0].reason, StreamCompletedReason::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_turn_reports_the_turns_end_for_a_sending_session() {
        // Given a session in sending phase — the shape a stream-rule watchdog
        // trip lands on, where the intercept rewound the session while it
        // prepared a resume that the trip now cancels. The continuation's
        // stream is in flight; the cancel arms the drain, and the terminate
        // publishes the completion that retires it.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When the turn is cancelled — the same chain the ESC-ESC cascade
        // drives: the drain arms, and the terminate's completion (the stamped
        // live identity, driven through the handler the recording bus cannot
        // round-trip) retires the stream and runs the settle in-line.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the turn's end is reported exactly once, as Canceled.
        let reports = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(reports.len(), 1, "a sending session has a turn to end");
        assert_eq!(reports[0].outcome, jinn_session_msg::TurnOutcome::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_during_the_tool_loop_settles_the_phase_idle() {
        // Given a session in `Sending` — a tool batch (a long-running bash
        // command) executing between stream turns, the exact shape ESC-ESC
        // meets when the user cancels mid-tool-loop. The continuation's
        // stream is in flight; the cancel arms the drain.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When the cancel is applied — the full chain the ESC-ESC cascade
        // drives. `begin_cancel` reports `Started` (the drain waits on the
        // stream's completion); the completion retires it and the settle
        // runs in-line.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;
        assert_eq!(
            audit.of_type::<jinn_session_msg::TurnCompleted>().len(),
            1,
            "setup: the cancel reported the turn"
        );

        // Then the phase is `Idle` and no in-flight guard survives: the
        // spinner stops, the queue drains, and the stall watchdog finds
        // nothing to re-trigger on.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
        assert!(!session.has_in_flight_stream());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_synchronously_settled_turn_is_reported_once_by_the_cancel() {
        // Given a session settled locally by the frontend, which never writes a
        // `"Cancelled"` entry itself — the fold does.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        actor
            .state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session exists")
            .finalize_entries_for_cancel(jiff::Timestamp::now());
        actor
            .state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session exists")
            .settle_to_idle();
        audit.clear();

        // When the command it published lands.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;

        // Then the end is still reported: the entry that tells the user the turn
        // was cancelled has not been written yet, and the phase being `Idle` is
        // not evidence that it has.
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(
            completed.len(),
            1,
            "a caller that settles locally leaves the entry unwritten; skipping \
             on the phase would lose it. got {completed:?}"
        );
        assert_eq!(completed[0].reason, StreamCompletedReason::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_after_a_synchronous_settle_leaves_the_phase_idle() {
        // Given a streaming session, cancelled the way the frontend's Escape
        // path does — settle locally, then publish the command, which the bus
        // broadcasts back to this actor.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        {
            let mut state = actor.state.write();
            state
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .finalize_entries_for_cancel(jiff::Timestamp::now());
            state
                .session
                .get_mut(&session_id)
                .expect("session exists")
                .settle_to_idle();
        }

        // When the command lands and its report is consumed.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the session is still idle: the second report found nothing to
        // settle, which is what the phase machine's rejection buys.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_carries_the_live_generation_stamp() {
        // Given a streaming session whose live generation carries the phase
        // actor's stream stamp (the mint's `dispatched_at`).
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        let live_stamp = actor
            .state
            .read()
            .session
            .get(&session_id)
            .expect("session exists")
            .stream_dispatched_at()
            .expect("the mint stamps the session");

        // When the turn is cancelled.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;

        // Then the completion carries that stamp. A freshly-timestamped one
        // would sort before the live generation and be dropped by the stale
        // guard, leaving the session never settled.
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0].dispatched_at, live_stamp,
            "the report must carry the live generation, or the stale guard \
             drops it and the session never settles"
        );
    }

    // ------------------------------------------------------------------
    // The watchdog-trip wedge: cancel after the resume was already dispatched
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_after_the_resume_was_dispatched_still_settles_the_turn() {
        // Given a session whose intercept armed the in-flight guard by
        // publishing a resume — the state a watchdog trip lands on. The resume
        // is then dropped downstream, so nothing else will ever complete.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        {
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session.arm_stream(jiff::Timestamp::now());
        }
        assert!(
            actor
                .state
                .read()
                .session
                .get(&session_id)
                .expect("session exists")
                .has_in_flight_stream(),
            "precondition: a stream is genuinely in flight"
        );

        // When the watchdog cancels the turn.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;

        // Then the turn's end is still reported — reporting nothing here is what
        // left the session wedged in a busy phase with the stall watchdog's
        // timer still armed.
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(
            completed.len(),
            1,
            "a cancel must report the end even when the resume it stops was \
             already dispatched; nothing downstream of the drop will"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_clears_the_in_flight_guard_it_terminated() {
        // Given the same armed-guard state.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        let dispatched_at = jiff::Timestamp::now();
        {
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session.arm_stream(dispatched_at);
        }

        // When the cancel is applied and the completion it publishes is
        // consumed — in production the bus delivers it back to this actor,
        // which is what retires the stream and settles the drain.
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                // A bare fixture cancel is not ESC-ESC: nothing drains.
                drain_buffers_to_input: false,
            })
            .await;
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(completed.len(), 1, "setup: the cancel reported the turn");
        actor.on_stream_completed(&completed[0]).await;

        // Then the guard is cleared and the session is idle, so the stall
        // watchdog's retry finds nothing in flight and stands down. The
        // guard belongs to the cancelled generation; the drain's settle
        // kills both — a stream that later unwinds cannot resurrect it.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
        assert!(
            !session.has_in_flight_stream(),
            "a terminated turn must leave no in-flight guard behind, or the \
             stall watchdog re-triggers against a stream that does not exist"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_second_cancel_for_the_same_turn_reports_nothing() {
        // Given a streaming session already terminated by a cancel.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;
        audit.clear();

        // When the same cancel is delivered again — the frontend settles
        // locally and publishes, and the bus delivers it back.
        actor.on_cancel_turn(&msg).await;

        // Then nothing more is reported. The termination record is what decides,
        // not the phase: a second report would push a second `"Cancelled"`
        // entry onto history, because the entry is written by the fold and the
        // fold's entry push is not itself idempotent.
        let completed = audit.of_type::<StreamCompleted>();
        assert!(completed.is_empty(), "a turn ends once; got {completed:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancelling_a_session_that_never_ran_leaves_the_phase_untouched() {
        // Given a session that never ran a turn.
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();

        // When a cancel arrives for it.
        let msg = CancelTurn {
            session_id: session_id.clone(),
            // A bare fixture cancel is not ESC-ESC: nothing drains.
            drain_buffers_to_input: false,
        };
        actor.on_cancel_turn(&msg).await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the phase is untouched — a cancel never *starts* a turn. The
        // report is still published, because "was never running" is not "already
        // ended", and only this routine may claim otherwise.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
        assert_eq!(
            audit.of_type::<StreamCompleted>().len(),
            1,
            "a session that never ran is not already-terminated, so the \
             routine reports it as ended rather than leaving the mark to \
             silently suppress a later real cancel"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_reason_keeps_the_queue_for_dispatch() {
        // Given a streaming session with one queued user message.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user("queued message"),
            )));
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the queue survives: the queued message dispatches after the
        // settle (the queue actor's idle transition), it is not bounced to
        // the input box.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 1);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "",
            "a cancel must not drain the queue into the input box"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_returns_sending_session_to_idle() {
        // Given a session in sending phase - a descendant mid-tool-loop when a
        // cascade cancel lands on it.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session is no longer busy.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
    }

    // ------------------------------------------------------------------
    // ESC-ESC buffer drain — the flagged cancel dumps buffered work into
    // the input box at the settle instead of re-dispatching it
    // ------------------------------------------------------------------

    /// Seeds one queued user message onto the streaming session.
    fn seed_queued_message(actor: &SessionPersistenceActor, session_id: &SessionId, text: &str) {
        let mut state = actor.state.write();
        state
            .session
            .get_mut(session_id)
            .expect("session exists")
            .enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
                ChatEntry::user(text),
            )));
    }

    /// Seeds one steering fragment onto the named session.
    fn seed_steering_fragment(actor: &SessionPersistenceActor, session_id: &SessionId, text: &str) {
        let mut state = actor.state.write();
        state
            .session
            .get_mut(session_id)
            .expect("session exists")
            .steering_buffer_mut()
            .push_fragment(text);
    }
    /// Builds a drain-flagged `CancelTurn` — the shape the confirmed ESC-ESC
    /// publishes.
    fn drain_flagged_cancel(session_id: &SessionId) -> CancelTurn {
        CancelTurn {
            session_id: session_id.clone(),
            drain_buffers_to_input: true,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn drain_flagged_cancel_dumps_steering_and_queue_to_input_at_settle() {
        // Given a streaming session holding one steering fragment and one
        // queued user message.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        seed_steering_fragment(&actor, &session_id, "steer this");
        seed_queued_message(&actor, &session_id, "queued work");

        // When the flagged cancel is applied — the full chain: the drain
        // arms, the completion retires the stream, and the settle runs.
        actor
            .on_cancel_turn(&drain_flagged_cancel(&session_id))
            .await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then both buffers landed in the input box, steering first, joined
        // by the drain helper's separator, and the buffers are empty.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "steer this\n\n---\n\nqueued work"
        );
        assert_eq!(session.queue_len(), 0);
        assert!(session.steering_buffer().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn drain_flagged_cancel_through_the_started_and_settled_route_dumps_too() {
        // Given a streaming session holding a queued message whose stream is
        // already retired — the shape the inline `StartedAndSettled` route
        // settles: `begin_cancel` finds an empty work set and settles
        // inside `terminate_turn` itself, never passing through the
        // completion ack.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        seed_queued_message(&actor, &session_id, "queued work");
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        actor
            .services
            .turn_ledger
            .ack_stream_ended(
                &session_id,
                &live.turn,
                &live.stream.expect("in-flight stream"),
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .expect("the retired stream leaves the turn live");

        // When the flagged cancel is applied (no completion is driven: this
        // route settles inline).
        actor
            .on_cancel_turn(&drain_flagged_cancel(&session_id))
            .await;

        // Then the queued message is in the input box — the latch covered
        // the settle route that never passes through the completion ack.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "queued work"
        );
        assert_eq!(session.queue_len(), 0);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unflagged_cancel_keeps_the_queue_for_redispatch() {
        // Given a streaming session holding a queued message.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        seed_queued_message(&actor, &session_id, "queued work");

        // When an unflagged cancel — the shape every non-ESC publisher
        // sends — is applied through the same chain.
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                // A bare fixture cancel is not ESC-ESC: nothing drains.
                drain_buffers_to_input: false,
            })
            .await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the queue survives for the idle transition to re-dispatch.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 1);
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "",
            "an unflagged cancel must not drain the queue into the input box"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_with_nothing_to_drain_leaves_the_input_untouched() {
        // Given a streaming session whose user had typed a fresh draft —
        // and no buffered work at all.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        actor
            .state
            .write()
            .session
            .get_mut(&session_id)
            .expect("session exists")
            .update_input(|input| input.insert_text("a draft the user is typing"));

        // When the flagged cancel is applied through the same chain —
        // nothing to drain, so the helper's empty-drain rule applies.
        actor
            .on_cancel_turn(&drain_flagged_cancel(&session_id))
            .await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the draft is still the user's.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.with_input(|i| i.text().to_owned(), String::new),
            "a draft the user is typing"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_stale_drain_request_does_not_outlive_its_settle() {
        // Given a streaming session whose first cancel was flagged and has
        // already settled.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        actor
            .on_cancel_turn(&drain_flagged_cancel(&session_id))
            .await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;
        // And a second turn is running with a queued message — the next
        // cancel, whatever publishes it, is not the flagged one.
        let second = jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &actor.services,
            &actor.state,
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await
        .expect("the second turn's mint must be admitted");
        let _ = second;
        seed_queued_message(&actor, &session_id, "queued work");

        // When an unflagged cancel settles that second turn.
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                // A bare fixture cancel is not ESC-ESC: nothing drains.
                drain_buffers_to_input: false,
            })
            .await;
        actor
            .on_stream_completed(&stream_completed(
                &actor,
                &session_id,
                StreamCompletedReason::Canceled,
                None,
                None,
                None,
                None,
            ))
            .await;

        // Then the queue survives: the first cancel's drain request cannot
        // dump a later turn's buffers.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.queue_len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_publishes_phase_change_from_sending() {
        // Given a session in sending phase when a cascade cancel lands on it.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then subscribers learn the session went idle.
        let phase_events = audit.of_type::<SessionPhaseChanged>();
        assert!(
            phase_events
                .iter()
                .any(|e| e.old_phase == PhaseView::Working && e.new_phase == PhaseView::Idle),
            "expected SessionPhaseChanged(Sending -> Idle); got: {:?}",
            audit.names()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_publishes_canceled_outcome_from_sending() {
        // Given a session in sending phase when a cascade cancel lands on it.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_sending_session(&actor).await;

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then exactly one turn completion reports the cancel.
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1);
        assert_eq!(
            completions[0].outcome,
            jinn_session_msg::TurnOutcome::Canceled
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_persists_session_from_sending() {
        // Given an interacted session in sending phase when a cascade cancel
        // lands on it.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session was persisted (should_save = true for Canceled).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Canceled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_from_sending_appends_only_cancel_entry() {
        // Given a sending session holding one user entry.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the user entry is followed only by the cancel entry - no
        // assistant entry is synthesized for a session that never streamed.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let kinds: Vec<ChatEntryKind> = session.history().iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds.len(), 2);
        assert!(matches!(kinds[0], ChatEntryKind::User { .. }));
        assert!(matches!(&kinds[1], ChatEntryKind::Error(text) if text == "Cancelled"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_emits_history_appended() {
        // Given a streaming session holding one user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes normally.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then HistoryAppended is emitted.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream completed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_emits_history_appended() {
        // Given a streaming session holding one user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then HistoryAppended is emitted.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_emits_history_appended() {
        // Given a streaming session holding one user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then HistoryAppended is emitted.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended event after stream canceled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancel_publishes_the_settle_even_when_the_report_races_it() {
        // Given a streaming session whose cancel and whose completion race:
        // the cancel drains the turn (publishing the report the drain waits
        // on), and a second `StreamCompleted(Canceled)` lands afterwards —
        // the order ESC produces when the frontend reports the turn while
        // the stream is still unwinding. The straggler carries the identity
        // the drain already retired: the live stream at cancel time.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        let stamp = actor
            .state
            .read()
            .session
            .get(&session_id)
            .expect("session exists")
            .stream_dispatched_at()
            .expect("the mint stamps the session");
        // The identity in flight when the cancel lands — exactly what the
        // drain's own report carries and retires.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn before the cancel");
        let straggler_identity = (live.turn, live.stream.expect("in-flight stream"));

        // When the cancel is applied and the canceled stream completes after it.
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
                // A bare fixture cancel is not ESC-ESC: nothing drains.
                drain_buffers_to_input: false,
            })
            .await;
        let completed = audit.of_type::<StreamCompleted>();
        assert_eq!(completed.len(), 1, "setup: the cancel reported the turn");
        actor.on_stream_completed(&completed[0]).await;
        let event = StreamCompleted {
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
            dispatched_at: stamp,
            turn: straggler_identity.0,
            stream: straggler_identity.1,
        };
        actor.on_stream_completed(&event).await;

        // Then exactly one busy announcement and one settle announcement:
        // the settle is the cancel drain's (the completion consumed above is
        // the drain's own retirement, carrying the live identity), and the
        // straggler the race produces announces nothing — a turn settles
        // once, whoever wins the race.
        let phase_events = audit.of_type::<SessionPhaseChanged>();
        assert_eq!(
            phase_events.len(),
            2,
            "one busy announcement, one settle announcement; got: {:?}",
            audit.names()
        );
        assert_eq!(phase_events[0].old_phase, PhaseView::Idle);
        assert_eq!(phase_events[0].new_phase, PhaseView::Streaming);
        assert_eq!(phase_events[1].old_phase, PhaseView::Streaming);
        assert_eq!(phase_events[1].new_phase, PhaseView::Idle);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_force_excludes_dangling_tool_calls() {
        // Given a streaming session whose last entry is a tool call with no
        // matching result, followed by a dangling empty assistant entry.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::assistant(""));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the empty assistant and tool-call entries are force-excluded
        // while the completed user and cancel entries stay in context.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let history = session.history();
        assert_eq!(
            history[0].context_override(),
            jinn_core_types::ContextOverride::Default
        );
        assert_eq!(
            history[1].context_override(),
            jinn_core_types::ContextOverride::ForcedExclude
        );
        assert_eq!(
            history[2].context_override(),
            jinn_core_types::ContextOverride::ForcedExclude
        );
        assert_eq!(
            history[3].context_override(),
            jinn_core_types::ContextOverride::Default
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_token_appends_text_to_assistant_entry() {
        // Given a session in streaming phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;

        // When two content tokens stream in.
        actor.on_stream_token(&content_token(&session_id, 0, "Hello"));
        actor.on_stream_token(&content_token(&session_id, 1, " world"));

        // Then one assistant entry holds both tokens concatenated.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant_text = session
            .history()
            .iter()
            .find_map(|e| match &e.kind {
                ChatEntryKind::Assistant(t) => Some(t.clone()),
                _ => None,
            })
            .expect("should have an assistant entry");
        assert_eq!(assistant_text, "Hello world");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_token_keeps_phase_as_streaming() {
        // Given a session in streaming phase.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;

        // When a content token streams in.
        actor.on_stream_token(&content_token(&session_id, 0, "hi"));

        // Then the phase remains Streaming.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseView::Streaming),
            "expected Streaming phase, got {:?}",
            session.phase()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_token_in_sending_neither_arms_the_phase_nor_writes_history() {
        // Given a session in sending phase, ahead of the first token — the
        // shape of a dispatch whose admission was refused or whose generation
        // was torn down.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("go"));
            session.begin_sending();
            state.session.active_session_id().clone()
        };

        // When a content token streams in.
        actor.on_stream_token(&content_token(&session_id, 0, "response"));

        // Then the phase is untouched and no assistant entry was created for
        // the stray token: arming Streaming here would back the phase with
        // no live generation, which is the R6 wedge.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(
            session.phase(),
            PhaseView::Working,
            "a token with no live generation must not arm the phase"
        );
        assert!(
            session.streaming_entry_index().is_none(),
            "no streaming entry may exist behind a refused dispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_persists_session() {
        // Given an interacted session in streaming phase.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When the stream completes normally.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session was persisted (should_save = true for Finished).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Finished"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_persists_session() {
        // Given an interacted session in streaming state.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session was persisted (should_save = true for Error).
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_persists_session() {
        // Given an interacted session in streaming state.
        let (actor, store, _audit) = test_actor_with_store_recording(vec![]).await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.mark_interacted();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session was persisted.
        assert!(
            store.last_saved_session(&session_id).is_some(),
            "expected session to be saved after Canceled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_does_not_count_tokens_on_error() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            Some("some error content"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the ledger records no output tokens for the failed turn.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger[0].tokens_received, 0,
            "expected 0 tokens_received on Error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_preserves_assistant_entry() {
        // Given a streaming session that already streamed one content token.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("do something"));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&content_token(&session_id, 0, "I will help"));

        // When the stream completes requesting a tool call.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::ToolUse,
            Some("response"),
            Some(vec![tool_call("tc-1", "bash", "{}")]),
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the streamed assistant entry is preserved.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let has_assistant = session
            .history()
            .iter()
            .any(|e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "I will help"));
        assert!(
            has_assistant,
            "expected assistant entry 'I will help' to be preserved after ToolUse"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_counts_tool_call_arguments() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes requesting a tool call with long arguments.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::ToolUse,
            Some("checking"),
            Some(vec![tool_call(
                "tc-1",
                "bash",
                r#"{"command":"ls -la /very/long/path"}"#,
            )]),
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the tool call arguments are counted alongside the text.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert!(
            ledger[0].tokens_received > 2,
            "expected tokens_received > 2 (text only), got {}",
            ledger[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_preserves_assistant_entry() {
        // Given a streaming session that already streamed one content token.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;
        actor.on_stream_token(&content_token(&session_id, 0, "world"));

        // When the stream completes normally.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("world"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the streamed assistant entry is preserved.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let has_world = session
            .history()
            .iter()
            .any(|e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t.contains("world")));
        assert!(
            has_world,
            "expected assistant entry with 'world' to be preserved after Finished"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_with_complete_tool_loop_does_not_exclude() {
        // Given a streaming session whose tool loop completed: the tool call
        // has a matching result, so nothing dangles.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("run it"));
            session.push_entry(ChatEntry::assistant(""));
            session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
            session.push_entry(ChatEntry::tool_result(
                "tc-1",
                "bash",
                "file.txt",
                jinn_core_types::ToolResultStatus::Success,
            ));
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then no entry is excluded from context.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        for entry in session.history() {
            assert_eq!(
                entry.context_override(),
                jinn_core_types::ContextOverride::Default,
                "expected Default for entry {:?}",
                entry.kind
            );
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_without_auto_compaction_goes_to_idle() {
        // Given a streaming session that has never grown an auto-compaction ledger.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes normally.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the session settles in Idle.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseView::Idle),
            "expected Idle after Finished without auto-compaction, got {:?}",
            session.phase()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_finished_applies_pending_mutations() {
        // Given a streaming session with one queued context-override mutation.
        let (actor, audit) = test_actor_recording().await;
        let (entry_id, session_id) =
            seed_streaming_session_with_pending_mutation(&actor, "response");

        // When the stream completes normally.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the queued mutation is applied to the assistant entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            jinn_core_types::ContextOverride::ForcedExclude
        );
        assert!(audit.contains_name("HistoryAppended"));
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_error_applies_pending_mutations() {
        // Given a streaming session with one queued context-override mutation.
        let (actor, _audit) = test_actor_recording().await;
        let (entry_id, session_id) =
            seed_streaming_session_with_pending_mutation(&actor, "partial");

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the mutation was applied.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("assistant entry exists");
        assert_eq!(
            assistant.context_override(),
            jinn_core_types::ContextOverride::ForcedExclude,
            "expected mutation to be applied at stream error"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_canceled_applies_pending_mutations() {
        // Given a session in streaming state with pending mutations.
        let actor = test_actor().await;
        let (entry_id, session_id) = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            let entry = ChatEntry::assistant("partial");
            let entry_id = entry.id.clone();
            session.push_entry(entry);
            session.begin_streaming();
            session.queue_mutations(vec![jinn_core_types::HistoryMutation::SetContextOverride {
                entry_id: entry_id.clone(),
                value: jinn_core_types::ContextOverride::ForcedExclude,
                source: ChangeSource::Internal {
                    label: "test".into(),
                },
            }]);
            (entry_id, state.session.active_session_id().clone())
        };

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the queued mutation was applied to the assistant entry.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            jinn_core_types::ContextOverride::ForcedExclude
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_tool_use_does_not_apply_mutations() {
        // Given a streaming session with one queued context-override mutation.
        let (actor, _audit) = test_actor_recording().await;
        let (entry_id, session_id) =
            seed_streaming_session_with_pending_mutation(&actor, "checking");

        // When the stream completes requesting a tool call.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::ToolUse,
            Some("response"),
            Some(vec![tool_call("tc-1", "bash", "{}")]),
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the mutation is still queued and the entry keeps its default override.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| e.id == entry_id)
            .expect("entry");
        assert_eq!(
            assistant.context_override(),
            jinn_core_types::ContextOverride::Default,
            "expected mutation to NOT be applied for ToolUse reason"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_provider_tokens_used_directly() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with a provider completion-token report.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("short"),
            None,
            Some(5000),
            Some("very long thinking content here"),
        );
        actor.on_stream_completed(&event).await;

        // Then the provider's count is recorded verbatim.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 5000);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_local_fallback_includes_thinking() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with thinking content and no provider report.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("short"),
            None,
            None,
            Some("a substantial amount of reasoning text"),
        );
        actor.on_stream_completed(&event).await;

        // Then the local count covers more than the two-word text alone.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.token_ledger()[0].tokens_received > 2,
            "expected tokens_received > 2, got {}",
            session.token_ledger()[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_local_fallback_without_thinking_backward_compat() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with text only and no provider report.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response text"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the local count still covers the response text.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.token_ledger()[0].tokens_received > 0,
            "expected nonzero tokens_received for 'response text'"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_provider_tokens_preferred_over_local() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with both a provider report and long thinking.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("short"),
            None,
            Some(9999),
            Some("extremely long thinking content that would produce many tokens"),
        );
        actor.on_stream_completed(&event).await;

        // Then the provider's count wins over the local maximum.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 9999);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_takes_max_when_provider_undercounts() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with text and no provider report.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("response text"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then tokens_received counts only the text (backward compat).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let ledger = session.token_ledger();
        assert_eq!(ledger.len(), 1);
        assert!(
            ledger[0].tokens_received > 0,
            "expected nonzero tokens_received for 'response text', got {}",
            ledger[0].tokens_received
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_takes_max_when_provider_overcounts() {
        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with a provider count far above the local one.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("ok"),
            None,
            Some(50000),
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the provider's larger count wins.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, 50000);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_stream_completed_uses_local_count_when_no_provider_report() {
        use jinn_llm_support::token_estimator::TokenCounter;

        // Given a streaming session with a token record pending finalization.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = seed_streaming_session_with_token_record(&actor);

        // When the stream completes with text and no provider report.
        let content = "hello world this is a test";
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some(content),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the locally counted tokens are recorded.
        let counter = jinn_llm_support::token_estimator::TiktokenCounter::o200k_base();
        let expected = counter.count(content) as u32;

        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.token_ledger()[0].tokens_received, expected);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatched_at_flows_from_stream_token_to_entry_timing() {
        // Given a session actor with a session in streaming state.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling a StreamToken with a specific dispatched_at.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Hello".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // Then the assistant entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            jinn_core_types::EntryTiming::Streamed {
                dispatched_at,
                first_token_at,
                finished_at,
            } => {
                assert_eq!(dispatched_at, &dispatched);
                assert!(first_token_at.is_some());
                assert!(finished_at.is_none());
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn thinking_entry_gets_dispatched_at_from_stream_token() {
        // Given a session actor with a session in streaming state.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };

        // When handling a thinking StreamToken with a specific dispatched_at.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // Then the thinking entry's timing has that dispatched_at.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            jinn_core_types::EntryTiming::Streamed {
                dispatched_at,
                first_token_at,
                finished_at,
            } => {
                assert_eq!(dispatched_at, &dispatched);
                assert!(first_token_at.is_some());
                assert!(finished_at.is_none());
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_completed_sets_finished_at_on_assistant_entry() {
        // Given a session actor with a session in streaming state and a token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        // The completion below resolves against the ledger like production's:
        // mint the turn the seeding phase belongs to.
        let (turn, stream) = {
            let grant = actor
                .services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("the seeded turn mints");
            (grant.turn, grant.stream)
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Hello".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // When handling StreamCompleted with Finished reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("Hello".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(10),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
            turn,
            stream,
        };
        actor.on_stream_completed(&event).await;

        // Then the assistant entry has finished_at set.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "finished_at should be set after completion"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn content_token_after_thinking_finalizes_thinking_entry_finished_at() {
        // Given a session actor streaming with a thinking entry already begun.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the first non-thinking (content) token arrives.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 1,
            token: "answer".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // Then the thinking entry's finished_at is set (duration resolved).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set after content token arrives"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn second_content_token_does_not_move_thinking_finished_at() {
        // Given a session actor streaming with thinking finalized by a content token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });
        actor.on_stream_token(&content_token(&session_id, 1, "answer"));
        let finished_at_first = thinking_finished_at(&actor, &session_id);

        // When a second content token arrives.
        actor.on_stream_token(&content_token(&session_id, 2, " more"));

        // Then the thinking entry's finished_at is unchanged (idempotent).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => {
                assert_eq!(
                    *finished_at, finished_at_first,
                    "thinking finished_at must not change on subsequent content tokens"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn pure_reasoning_stream_finalizes_thinking_on_stream_completion() {
        // Given a session actor streaming with ONLY thinking tokens (no content).
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        // The completion below resolves against the ledger like production's:
        // mint the turn the seeded phase belongs to.
        let (turn, stream) = {
            let grant = actor
                .services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("the seeded turn mints");
            (grant.turn, grant.stream)
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "only reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the stream completes without producing a content token.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: Some(10),
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: Some("only reasoning".to_owned()),
            dispatched_at: dispatched,
            model_used: None,
            turn,
            stream,
        };
        actor.on_stream_completed(&event).await;

        // Then the thinking entry's finished_at is set via the safety net.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match &thinking.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set by safety net on pure-reasoning completion"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancel_during_reasoning_finalizes_thinking_and_preserves_text() {
        // Given a session actor streaming with a thinking entry.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        // The completion below resolves against the ledger like production's:
        // mint the turn the seeding phase belongs to.
        let (turn, stream) = {
            let grant = actor
                .services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("the seeded turn mints");
            (grant.turn, grant.stream)
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "partial reasoning".to_owned(),
            is_thinking: true,
            dispatched_at: dispatched,
        });

        // When the stream is canceled before any content token.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
            turn,
            stream,
        };
        actor.on_stream_completed(&event).await;

        // Then the thinking entry's finished_at is set AND its text is preserved.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let thinking = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Thinking(_)))
            .expect("thinking entry");
        match (&thinking.kind, &thinking.timing) {
            (
                jinn_core_types::ChatEntryKind::Thinking(text),
                jinn_core_types::EntryTiming::Streamed { finished_at, .. },
            ) => {
                assert_eq!(
                    text, "partial reasoning",
                    "partial reasoning text preserved"
                );
                assert!(
                    finished_at.is_some(),
                    "thinking finished_at should be set on cancel during reasoning"
                );
            }
            other => panic!("expected Thinking + Streamed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cancelled_stream_records_finished_at() {
        // Given a session actor with a session in streaming state and a token.
        let actor = test_actor().await;
        let dispatched = jiff::Timestamp::now();
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            state.session.active_session_id().clone()
        };
        // The completion below resolves against the ledger like production's:
        // mint the turn the seeding phase belongs to.
        let (turn, stream) = {
            let grant = actor
                .services
                .turn_ledger
                .mint(
                    &session_id,
                    jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
                )
                .expect("the seeded turn mints");
            (grant.turn, grant.stream)
        };
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "Partial".to_owned(),
            is_thinking: false,
            dispatched_at: dispatched,
        });

        // When handling StreamCompleted with Canceled reason.
        let event = StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: dispatched,
            model_used: None,
            turn,
            stream,
        };
        actor.on_stream_completed(&event).await;

        // Then the assistant entry has finished_at set (cancellation is a finish event).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let assistant = session
            .history()
            .iter()
            .find(|e| matches!(e.kind, jinn_core_types::ChatEntryKind::Assistant(_)))
            .expect("assistant entry");
        match &assistant.timing {
            jinn_core_types::EntryTiming::Streamed { finished_at, .. } => {
                assert!(
                    finished_at.is_some(),
                    "finished_at should be set even on cancellation"
                );
            }
            other => panic!("expected Streamed, got {other:?}"),
        }
    }

    /// Deterministic counter for unit testing - counts characters.
    struct CharCounter;
    impl jinn_llm_support::token_estimator::TokenCounter for CharCounter {
        fn count(&self, text: &str) -> usize {
            text.chars().count()
        }
        fn name(&self) -> &'static str {
            "char"
        }
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_counts_assistant_content() {
        // Given a char-counting counter and assistant content only.
        let counter = CharCounter;

        // When counting locally.
        let total = super::count_tokens_locally(&counter, "hello", "", None);

        // Then the total equals the assistant content length.
        assert_eq!(total, 5);
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_adds_thinking_content() {
        // Given a char-counting counter with content and thinking text.
        let counter = CharCounter;

        // When counting locally.
        let total = super::count_tokens_locally(&counter, "abc", "de", None);

        // Then the total is the sum of content and thinking.
        assert_eq!(total, 5);
    }

    #[rstest::rstest]
    #[test]
    fn count_tokens_locally_includes_tool_call_arguments_and_names() {
        // Given a char-counting counter, content, and one tool call.
        let counter = CharCounter;
        let tool_calls = vec![jinn_core_types::tool_types::ToolCall {
            id: "tc-1".into(),
            name: "bash".to_owned(),
            arguments: "ls".to_owned(),
        }];

        // When counting locally (content "ab"=2 + name "bash"=4 + args "ls"=2).
        let total = super::count_tokens_locally(&counter, "ab", "", Some(&tool_calls));

        // Then tool call arguments and names are included.
        assert_eq!(total, 8);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_none_for_empty() {
        // Given an empty drained queue.
        let queue = std::collections::VecDeque::new();

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then no text is produced.
        assert_eq!(text, None);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_none_when_only_tool_continuation() {
        // Given a queue with only a tool continuation.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then no text is produced.
        assert_eq!(text, None);
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_returns_text_for_single_user_message() {
        // Given a queue with one user message.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("hello world"),
        )));

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then the user message text is produced.
        assert_eq!(text.as_deref(), Some("hello world"));
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_joins_multiple_user_messages_with_newline() {
        // Given a queue with two user messages.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("first"),
        )));
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("second"),
        )));

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then the messages are joined with a newline.
        assert_eq!(text.as_deref(), Some("first\nsecond"));
    }

    #[rstest::rstest]
    #[test]
    fn drained_queue_to_text_skips_tool_continuation_when_mixed() {
        // Given a queue with a user message and a tool continuation.
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
            ChatEntry::user("only user"),
        )));
        queue.push_back(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);

        // When converting to text.
        let text = super::drained_queue_to_text(&queue);

        // Then only the user message text is produced.
        assert_eq!(text.as_deref(), Some("only user"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_appends_annotation_entry() {
        // Given a recording session actor.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id: session_id.clone(),
            citations: vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }],
        };

        // When handling CitationsReceived.
        actor.on_citations_received(&event).await;

        // Then the session has one Annotation entry carrying the citation.
        let state = actor.state.read();
        let annotations: Vec<_> = state
            .session
            .active_session()
            .history()
            .iter()
            .filter(|e| matches!(e.kind, ChatEntryKind::Annotation { .. }))
            .collect();
        assert_eq!(
            annotations.len(),
            1,
            "expected exactly one annotation entry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_emits_history_appended() {
        // Given a recording session actor.
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id,
            citations: vec![jinn_provider::UrlCitation {
                url: "https://example.com/a".to_owned(),
                title: "Source A".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }],
        };

        // When handling CitationsReceived.
        actor.on_citations_received(&event).await;

        // Then HistoryAppended was broadcast.
        assert!(
            audit.contains_name("HistoryAppended"),
            "expected HistoryAppended after citations received"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_citations_received_empty_citations_creates_nothing() {
        // Given a recording session actor.
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        let event = CitationsReceived {
            session_id,
            citations: Vec::new(),
        };

        // When handling CitationsReceived with empty citations.
        actor.on_citations_received(&event).await;

        // Then no annotation entry was added.
        let count = actor
            .state
            .read()
            .session
            .active_session()
            .history()
            .iter()
            .filter(|e| matches!(e.kind, ChatEntryKind::Annotation { .. }))
            .count();
        assert_eq!(count, 0, "empty citations must create no entry");
        // And no HistoryAppended was broadcast.
        assert!(
            !audit.contains_name("HistoryAppended"),
            "empty citations must not emit HistoryAppended"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stale_generation_stream_completed_is_dropped() {
        // Given a streaming session with a current-generation dispatch timestamp.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            let now = jiff::Timestamp::now();
            session.arm_stream(now);
            state.session.active_session_id().clone()
        };
        // The backdated completion carries the live turn's identity — its
        // staleness is the dispatch stamp it rides, which the guard drops.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("the harness mints a live turn");

        // When a StreamCompleted arrives carrying an OLDER dispatched_at
        // (simulating an aborted prior stream's late terminal event).
        let backdated = jiff::Timestamp::now()
            .checked_sub(jiff::Span::new().seconds(30))
            .unwrap();
        let event = StreamCompleted {
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
            dispatched_at: backdated,
            turn: live.turn,
            stream: live.stream.unwrap_or_default(),
        };
        actor.on_stream_completed(&event).await;

        // Then the session is STILL Streaming (the stale event was dropped)
        // and the generation guard was not consumed.
        let guard = actor.state.read();
        let session = guard.session.get(&session_id).expect("session exists");
        assert!(
            matches!(session.phase(), PhaseView::Streaming),
            "stale-generation StreamCompleted must not transition the session"
        );
        assert!(
            session.has_in_flight_stream(),
            "generation guard must remain set for stale events"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_completion_from_a_superseded_stream_refuses_without_dispatch() {
        // Given a session mid-stream with a live, minted turn.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When a ToolUse stream completes under a stream a retry swapped
        // out — the superseded generation's straggler, produced the way
        // production produces one: a resume minted a fresh stream before
        // the old one's completion arrived.
        let superseded = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn")
            .stream
            .expect("in-flight stream");
        let joined = actor
            .services
            .turn_ledger
            .mint(
                &session_id,
                jinn_kernel::common::turn_ledger::MintKind::Resume,
            )
            .expect("the resume joins the live turn");
        assert_ne!(
            joined.stream, superseded,
            "the resume swapped the stream, superseding the old one"
        );
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::ToolUse,
            assistant_content: Some("let me check".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
            turn: joined.turn,
            stream: superseded,
        };
        actor.on_stream_completed(&event).await;

        // Then no SendToLlmProvider is emitted — the completion names a
        // stream that is no longer live, so the ledger refuses it and the
        // tool loop cannot start from a dead generation.
        assert!(
            !audit.contains_name("SendToLlmProvider"),
            "a completion from a superseded stream must not dispatch a continuation"
        );
    }

    #[rstest::rstest]
    #[case(ChatEntryKind::Error("Cancelled".to_owned()), jinn_session_msg::TurnOutcome::Canceled)]
    #[case(ChatEntryKind::Error("provider unreachable".to_owned()), jinn_session_msg::TurnOutcome::Error)]
    #[case(ChatEntryKind::Assistant("done".to_owned()), jinn_session_msg::TurnOutcome::Succeeded)]
    #[case(ChatEntryKind::System("note".to_owned()), jinn_session_msg::TurnOutcome::Succeeded)]
    fn outcome_from_history_reads_the_last_entry(
        #[case] last_kind: ChatEntryKind,
        #[case] expected: jinn_session_msg::TurnOutcome,
    ) {
        // Given a history whose last entry carries the terminal kind.
        let entry = ChatEntry {
            kind: last_kind,
            ..ChatEntry::assistant("base")
        };

        // When the outcome is derived from that entry.
        let outcome = outcome_from_history(Some(entry));

        // Then it matches the policy: the literal "Cancelled" is a cancel,
        // any other error is a failure, everything else succeeded.
        assert_eq!(outcome, expected);
    }

    #[rstest::rstest]
    #[test]
    fn outcome_from_empty_history_is_success() {
        // Given a session with no entries at all.

        // When the outcome is derived.
        let outcome = outcome_from_history(None);

        // Then the turn counts as succeeded — no error means no failure.
        assert_eq!(outcome, jinn_session_msg::TurnOutcome::Succeeded);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn finished_turn_publishes_succeeded_turn_completed() {
        // Given a streaming session holding a user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream finishes.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Finished,
            Some("all done"),
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then exactly one TurnCompleted is published, as Succeeded.
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1, "one event per dispatched turn");
        assert_eq!(
            completions[0].outcome,
            jinn_session_msg::TurnOutcome::Succeeded
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn errored_turn_publishes_error_turn_completed() {
        // Given a streaming session holding a user entry and a pushed error.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(ChatEntry::user("hello"));
            session.push_entry(ChatEntry::error("provider unreachable"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes with an error.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Error,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the published outcome is Error.
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].outcome, jinn_session_msg::TurnOutcome::Error);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn canceled_turn_publishes_canceled_turn_completed() {
        // Given a streaming session holding a user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes because it was canceled.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then the published outcome is Canceled — the "Cancelled" error
        // entry applied before derivation resolves it.
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1);
        assert_eq!(
            completions[0].outcome,
            jinn_session_msg::TurnOutcome::Canceled
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_completion_publishes_no_turn_completed() {
        // Given a streaming session holding a user entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
            state.session.active_session_id().clone()
        };
        begin_streaming_session(&actor).await;

        // When the stream completes because the model requested tools.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::ToolUse,
            Some("let me check"),
            Some(vec![tool_call("t1", "read", "{}")]),
            None,
            None,
        );
        actor.on_stream_completed(&event).await;

        // Then no TurnCompleted is published — the tool loop continues, and
        // the turn has not ended.
        assert!(
            audit
                .of_type::<jinn_session_msg::TurnCompleted>()
                .is_empty(),
            "a ToolUse completion is not a turn end"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn completion_landing_after_sync_cancel_resolves_as_canceled() {
        // Given a live turn the user synchronously cancelled: the cancel
        // settles it (Canceled, exactly once) and removes the entry.
        let (actor, audit) = test_actor_recording().await;
        let session_id = begin_streaming_session(&actor).await;
        assert!(
            actor
                .services
                .turn_ledger
                .begin_cancel(&session_id)
                .is_some(),
            "a live turn's cancel starts the drain"
        );
        assert!(
            actor.services.turn_ledger.live(&session_id).is_some(),
            "a cancelled turn with an open stream is not yet settled"
        );
        // The identity the provider's stream was minted under — what its
        // completion carries whether or not the turn is still live when
        // the completion lands.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        let minted_identity = (live.turn, live.stream.expect("in-flight stream"));
        // The cancel's TurnCompleted publishes from the settle path — the
        // session actor's, not the ledger's. Drive it the way production
        // does: the cancel's completion is a StreamCompleted(Canceled)
        // through the handler.
        let event = stream_completed(
            &actor,
            &session_id,
            StreamCompletedReason::Canceled,
            None,
            None,
            None,
            None,
        );
        actor.on_stream_completed(&event).await;
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1);
        assert_eq!(
            completions[0].outcome,
            jinn_session_msg::TurnOutcome::Canceled
        );

        // When the provider's own `Finished` completion lands late — the
        // sync-cancel race — it carries the settled turn's identity: the
        // mint the provider stream actually ran under.
        let event = StreamCompleted {
            model_used: None,
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Finished,
            assistant_content: Some("late result".to_owned()),
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
            turn: minted_identity.0,
            stream: minted_identity.1,
        };
        actor.on_stream_completed(&event).await;

        // Then nothing more publishes: the turn ended once, as Canceled —
        // a turn the user cancelled must never look like a success, and a
        // settled turn has no second completion to give.
        let completions = audit.of_type::<jinn_session_msg::TurnCompleted>();
        assert_eq!(completions.len(), 1);
        assert_eq!(
            completions[0].outcome,
            jinn_session_msg::TurnOutcome::Canceled
        );
    }
}

#[cfg(test)]
mod ledger_projection_tests {
    //! TM-19: the session's stored phase is a projection of the ledger, and
    //! the ledger's `view` derives every display state the UI reads.

    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use crate::session_actor::helpers::test_actor_recording;
    use jinn_slices::PhaseView;

    /// TM-19 (consumer side): after a fresh mint the session reads
    /// `Streaming` and the ledger's derived view agrees; after the tool-use
    /// edge the session reads `Working` and the ledger agrees; after settle
    /// both read `Idle`.
    #[rstest::rstest]
    #[tokio::test]
    async fn phase_view_is_derived_from_the_ledger_across_a_turn() {
        // Given a test actor whose harness-seeded turn is retired: the
        // fixture starts from Idle.
        let (actor, _audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
        actor.services.turn_ledger.finish_streaming(&session_id);
        actor.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .settle_to_idle();
        });

        // Then the view is Idle on both sides.
        assert_eq!(
            actor.services.turn_ledger.view(&session_id),
            PhaseView::Idle
        );
        assert_eq!(
            actor.state.read().session.active_session().phase(),
            PhaseView::Idle
        );

        // When a fresh turn is minted through the production helper.
        let stamp = jiff::Timestamp::now();
        jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &actor.services,
            &actor.state,
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            stamp,
        )
        .await
        .expect("a fresh turn's mint must be admitted");

        // Then both sides read Streaming.
        assert_eq!(
            actor.services.turn_ledger.view(&session_id),
            PhaseView::Streaming
        );
        assert_eq!(
            actor.state.read().session.active_session().phase(),
            PhaseView::Streaming
        );

        // When the tool-use edge retires the stream toward a continuation.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        actor
            .services
            .turn_ledger
            .ack_stream_ended(
                &session_id,
                &live.turn,
                &live.stream.expect("in-flight stream"),
                jinn_kernel::common::turn_ledger::Next::Continue,
            )
            .expect("the tool-use edge retires the live stream");
        actor.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .begin_sending();
        });

        // Then both sides read Working.
        assert_eq!(
            actor.services.turn_ledger.view(&session_id),
            PhaseView::Working
        );
        assert_eq!(
            actor.state.read().session.active_session().phase(),
            PhaseView::Working
        );

        // When the turn settles (the continuation's finish does this in
        // production).
        actor.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .settle_to_idle();
        });
        actor.services.turn_ledger.finish_streaming(&session_id);

        // Then both sides read Idle and the entry is gone.
        assert_eq!(
            actor.services.turn_ledger.view(&session_id),
            PhaseView::Idle
        );
        assert_eq!(
            actor.state.read().session.active_session().phase(),
            PhaseView::Idle
        );
    }
}

#[cfg(test)]
mod tool_loop_round_order {
    //! Regression for the sticky tool-call rendering bug: each tool-loop
    //! round must own its assistant/thinking entries, and a round's text
    //! must render next to that round's calls and results — never appended
    //! into a previous round's entry.

    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use crate::session_actor::helpers::test_actor_recording;
    use jinn_inference_msg::StreamToken;
    use jinn_kernel::protocol::ChatEntryKind;

    fn kinds(
        actor: &crate::session_actor::SessionPersistenceActor,
        sid: &jinn_core_types::SessionId,
    ) -> Vec<String> {
        actor
            .state
            .read()
            .session
            .get(sid)
            .map(|s| {
                s.history()
                    .iter()
                    .map(|e| match &e.kind {
                        ChatEntryKind::User { .. } => "User".into(),
                        ChatEntryKind::Assistant(t) => format!("Assistant({t})"),
                        ChatEntryKind::Thinking(t) => format!("Thinking({t})"),
                        ChatEntryKind::ToolCall { name, .. } => format!("ToolCall({name})"),
                        ChatEntryKind::ToolResult { name, .. } => format!("ToolResult({name})"),
                        other => format!("{other:?}"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn round_two_tokens_must_not_land_in_round_ones_assistant_entry() {
        let (actor, _audit) = test_actor_recording().await;
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.push_entry(jinn_kernel::protocol::ChatEntry::user("go"));
            state.session.active_session_id().clone()
        };
        // Mint + stream round 1 like the dispatch path does.
        jinn_kernel::common::turn_ledger::mint_dispatch_with_stamp(
            &actor.services,
            &actor.state,
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await
        .expect("mint");

        // Round 1: thinking + content tokens, then a tool call streams.
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 0,
            token: "round-one ".into(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });
        actor.on_tool_use_started(&jinn_tools_msg::ToolUseStarted {
            session_id: session_id.clone(),
            index: 0,
            id: "tc-1".into(),
            name: "bash".into(),
            dispatched_at: jiff::Timestamp::now(),
        });

        // The ToolUse completion runs production's fold: entries finalize,
        // the stream retires toward Continue, the round ends. The identity
        // is the dispatch's own grant — the live stream the mint put in
        // flight, exactly what a provider completion retires.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn behind the dispatch");
        let (turn, stream) = (live.turn, live.stream.expect("in-flight stream"));
        actor
            .on_stream_completed(&jinn_inference_msg::StreamCompleted {
                model_used: None,
                session_id: session_id.clone(),
                reason: jinn_inference_msg::StreamCompletedReason::ToolUse,
                assistant_content: None,
                tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                    id: "tc-1".into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                }]),
                cost: None,
                provider_completion_tokens: None,
                provider_prompt_tokens: None,
                cached_tokens: None,
                thinking_content: None,
                dispatched_at: jiff::Timestamp::now(),
                turn,
                stream,
            })
            .await;

        // The continuation joins (fresh stream) and round 2 streams tokens.
        let grant = actor
            .services
            .turn_ledger
            .mint(
                &session_id,
                jinn_kernel::common::turn_ledger::MintKind::ToolContinuation,
            )
            .expect("continuation join");
        actor.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&session_id)
                .begin_streaming();
            view.session
                .map()
                .get_or_create(&session_id)
                .arm_stream(jiff::Timestamp::now());
        });
        let _ = grant;
        actor.on_stream_token(&StreamToken {
            session_id: session_id.clone(),
            index: 1,
            token: "round-two".into(),
            is_thinking: false,
            dispatched_at: jiff::Timestamp::now(),
        });

        let history = kinds(&actor, &session_id);
        // The round-2 token must NOT land inside round-1's assistant entry.
        let assistants: Vec<_> = history
            .iter()
            .filter(|k| k.starts_with("Assistant"))
            .collect();
        assert_eq!(
            assistants.len(),
            2,
            "each round gets its own assistant entry; got {history:#?}"
        );
        assert!(
            history
                .iter()
                .position(|k| k.contains("round-two"))
                .expect("round-two text")
                > history
                    .iter()
                    .position(|k| k.starts_with("ToolCall"))
                    .expect("tool call"),
            "round-2 text must sit below round-1's tool call"
        );
    }
}
