//! Rule-intercept handler — resumes a turn a stream rule stopped mid-sentence.
//!
//! When the inference actor sees a delta that trips a configured stream rule,
//! it aborts the provider stream *before* that delta is published, so nothing
//! of the offending output reaches the chat log. What it publishes instead is
//! a [`StreamCompleted`] carrying [`StreamCompletedReason::RuleIntercept`] and
//! the rule's name, and this handler turns that into a resumed turn.
//!
//! The shape is the stall-retry path's, deliberately. A stalled attempt and an
//! intercepted attempt fail the same way — the response must be re-sent, and
//! whatever it produced so far is not something the model should see as its
//! own — so both take the partial entries out of context, leave them visible
//! in the chat log, rewind the phase, and re-dispatch.
//!
//! # Why an interrupted tool call is kept
//!
//! A rule that catches a tool call mid-arguments interrupts the very stream
//! that would have dispatched it, so the call is never executed and never
//! answered. Excluding it — which is what this path used to do, treating it as
//! part of the abandoned attempt — leaves the model resuming with no memory of
//! a call it just began, and a model that cannot see its mistake makes it
//! again. So the call is paired with a synthetic result explaining that it did
//! not run and quoting the arguments it had produced, and *that* pair is kept
//! in context. Everything else the interrupted attempt produced is still
//! excluded, exactly as before: a partial assistant entry is not something to
//! re-send.
//!
//! Pairing also settles a protocol question that exclusion was hiding. A
//! request carrying `tool_calls` with no matching results is rejected by every
//! provider; the old exclusion hid the dangling call just well enough to pass
//! them a request with nothing in it.
//!
//! # Why the turn resumes as a user send
//!
//! The inference actor arms a cancel tombstone for any session it aborts, and
//! clears it *only* when a `User`-origin send arrives. A resumed turn
//! dispatched any other way would be dropped silently, with no log, and the
//! session would sit idle forever. [`DispatchTurn`] is therefore the correct
//! handoff: the turn-dispatch queue actor emits the fresh
//! `SendToLlmProvider` as a user-originated send, which is the one path that
//! lifts the tombstone.

use jinn_inference_msg::StreamCompletedReason;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_msg::PhaseKind;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_turn_dispatch_msg::DispatchTurn;

use super::super::SessionPersistenceActor;

impl SessionPersistenceActor {
    /// Resumes a turn a stream rule interrupted.
    ///
    /// Runs only for [`StreamCompletedReason::RuleIntercept`] and only while a
    /// stream is genuinely in flight — the same guard the stall path uses, so
    /// an intercept that lands after the session has moved on is a no-op
    /// rather than a rewind from `Idle`.
    ///
    /// The injected guidance is *not* pushed here: it is delivered as a user
    /// entry by the turn that this handler dispatches, so the rule's body
    /// enters the conversation exactly once and in the position the model
    /// will read it.
    pub(in crate::session_actor) async fn on_rule_intercept(
        &self,
        session_id: &jinn_core_types::SessionId,
        reason: StreamCompletedReason,
        // The event's stamp is not read directly: the rewind resolves
        // against the stamp the live generation was admitted with, read
        // under the surgery gate below.
        _dispatched_at: jiff::Timestamp,
    ) {
        debug_assert_eq!(reason, StreamCompletedReason::RuleIntercept);

        // Entry surgery first: the rewind edge below clears the machine's
        // tool-call index map, and it is the only record of which calls
        // this generation left mid-arguments. A call the rule caught
        // never reaches the executor, so nothing else will ever answer it,
        // and an unanswered call is a request every provider rejects.
        // Pairing each with a synthetic result — reading the fragments
        // while the indices still exist — is what makes the resumed turn
        // both valid and legible: the model reads its own failed attempt
        // instead of resuming blind and re-emitting it.
        let stamp = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            if matches!(session.phase(), PhaseKind::Sending | PhaseKind::Streaming)
                && session.has_in_flight_stream()
            {
                let explained = session.explain_interrupted_tool_calls();
                // The rest of the intercepted output stays in history for the
                // user to read — they watched it appear — and is only excluded
                // from the resumed request. Must run before the reset below
                // the entry indices it reads. A call already paired above is
                // left in context by that same rule.
                let excluded = session.reset_streaming_entries_for_retry();
                let dangling = session.force_exclude_dangling_tool_calls();
                tracing::warn!(
                    session_id = %session_id,
                    explained_calls = explained,
                    excluded_entries = excluded.len(),
                    excluded_dangling = dangling.len(),
                    "stream rule interrupted the turn; resuming with the rule body"
                );
                session.stream_dispatched_at()
            } else {
                tracing::warn!(
                    session_id = %session_id,
                    phase = ?session.phase(),
                    stream_in_flight = session.has_in_flight_stream(),
                    "rule-intercept resume refused: no in-flight stream"
                );
                None
            }
        });
        let Some(stamp) = stamp else {
            return;
        };

        // Admission after the surgery: the rewind edge is the phase
        // actor's (`InterceptRewind`), resolving against the stamp read
        // above. A cancelled turn refuses — the cancel and this intercept
        // race by construction (the completion is published from inside
        // the stream task the cancel tears down) — and a resumed dispatch
        // for a dead generation is what re-armed a session nobody was
        // streaming to. The interrupted entries stay excluded either way.
        let decision = crate::phase_actor::admit_rewind(&self.services, session_id, stamp).await;
        if !decision.admitted {
            tracing::warn!(
                session_id = %session_id,
                "rule-intercept resume refused: the turn was cancelled"
            );
            return;
        }

        super::super::helpers::emit_history_appended(self.bus(), session_id).await;

        // Hand the turn back to the dispatch queue, which emits the fresh
        // user-originated `SendToLlmProvider` that continues the turn. The
        // queue's admission ask re-mints against the rewound generation:
        // the phase actor's `BeginStream(ResumeTurn)` accepts it because
        // the intercept kept the generation live.
        self.publish(DispatchTurn {
            session_id: session_id.clone(),
        })
        .await;

        // The turn did not end, but the outcome is still reported so
        // consumers learn what happened to this response. It is published
        // here rather than by the generic completion fold, which this
        // intercept deliberately bypasses: a consumer reading
        // `TurnCompleted` must be able to tell an intercepted generation from
        // a cancelled turn, and only this handler knows which happened.
        self.publish(TurnCompleted {
            session_id: session_id.clone(),
            outcome: TurnOutcome::RuleIntercepted,
        })
        .await;
    }
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    pub(in crate::session_actor::handlers) use super::super::super::helpers::test_actor_recording;
    use jinn_chat_input_msg::EnqueueUserMessage;
    use jinn_core_types::ChatEntry;
    use jinn_core_types::ChatEntryKind;
    use jinn_core_types::SessionId;
    use jinn_inference_msg::CancelTurn;
    use jinn_inference_msg::StreamCompletedReason;
    use jinn_kernel::common::phase_command::apply_phase;
    use jinn_kernel::common::services::BusAudit;
    use jinn_session_msg::{DispatchKind, PhaseCommand, PhaseDecision};
    use jinn_session_msg::{PhaseKind, TurnCompleted, TurnOutcome};

    use crate::session_actor::SessionPersistenceActor;

    /// A session mid-response: streaming, holding a partial assistant entry,
    /// with the in-flight-stream guard armed and the turn's generation minted
    /// through the phase actor — the shape an intercepted response presents.
    pub(in crate::session_actor::handlers) async fn intercept_setup()
    -> (SessionPersistenceActor, BusAudit, SessionId) {
        let (actor, audit) = test_actor_recording().await;
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = actor.state.read().session.active_session_id().clone();
        // The generation the live stream runs under. Production mints it at
        // dispatch — the ask applies the fused `Idle → Sending → Streaming`
        // edge — and the record's stamp is what the intercept's rewind
        // resolves against. A harness without it refuses every intercept at
        // admission and exercises nothing downstream.
        let minted = apply_phase(
            &actor.services,
            PhaseCommand::BeginStream {
                session_id: session_id.clone(),
                kind: DispatchKind::FreshTurn,
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await
        .expect("phase actor reachable");
        assert!(minted.admitted, "a fresh turn's mint must be admitted");
        let stamp = minted
            .stream_stamp
            .expect("an admitted mint carries its generation's stamp");
        {
            // The session-side facts of the live stream. The guard carries the
            // generation's stamp — the same value the dispatch carried — which
            // is what lets the cancel's own report resolve against it.
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session
                .append_stream_token("the offending text", stamp)
                .expect("append first token");
            session.arm_stream(stamp);
        }
        (actor, audit, session_id)
    }

    /// Asks the phase actor to apply one command for these tests: a real
    /// cancel, or the dispatch-queue's admission ask for a resume.
    async fn ask(
        services: &jinn_kernel::common::services::Services,
        command: PhaseCommand,
    ) -> PhaseDecision {
        apply_phase(services, command)
            .await
            .expect("phase actor reachable")
    }

    /// The `StreamCompleted(Canceled)` a real cancel publishes, carrying the
    /// stamp the phase decision reported — what `terminate_turn` sends.
    fn cancelled_report(
        session_id: &SessionId,
        decision: &PhaseDecision,
    ) -> jinn_inference_msg::StreamCompleted {
        jinn_inference_msg::StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Canceled,
            model_used: None,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: decision.stream_stamp.unwrap_or_else(jiff::Timestamp::now),
        }
    }

    /// A `StreamCompleted` carrying the intercept reason.
    pub(in crate::session_actor::handlers) fn intercept_completion(
        session_id: &SessionId,
    ) -> jinn_inference_msg::StreamCompleted {
        jinn_inference_msg::StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::RuleIntercept,
            model_used: Some("m".to_owned()),
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercepted_response_excludes_its_output_from_the_resumed_request() {
        // Given a streaming session whose partial output tripped a rule.
        let (actor, _audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the output is still in the chat log — the user watched it
        // appear — but out of context for the resumed turn.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.history().iter().any(
                |e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "the offending text")
            ),
            "the intercepted output must stay visible"
        );
        assert!(
            !session.history().iter().any(|e| {
                matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "the offending text")
                    && e.is_in_context()
            }),
            "the intercepted output must not be sent to the provider on resume"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_reports_its_own_outcome_not_a_cancel() {
        // Given a streaming session whose response tripped a rule.
        let (actor, audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn outcome is an intercept, so no consumer reads it as a
        // user cancel.
        let completed = audit.of_type::<TurnCompleted>();
        assert_eq!(
            completed.len(),
            1,
            "an intercept reports exactly one outcome"
        );
        assert_eq!(completed[0].outcome, TurnOutcome::RuleIntercepted);
        assert_ne!(completed[0].outcome, TurnOutcome::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_never_pushes_the_cancelled_entry() {
        // Given a streaming session whose response tripped a rule.
        let (actor, _audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the literal `Cancelled` error entry is absent: `outcome_from_history`
        // reads that exact string as a user cancel.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Error(t) if t == "Cancelled")),
            "an intercept must not look like a user cancel to outcome_from_history"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_hands_the_turn_back_for_redispatch() {
        // Given a streaming session whose response tripped a rule.
        let (actor, audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn is re-dispatched — the dispatch queue emits the fresh
        // user-originated send that lifts the abort tombstone.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handed_off.len(),
            1,
            "an intercept must resume exactly one turn"
        );
        assert_eq!(handed_off[0].session_id, session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_rewinds_the_phase_so_the_resumed_turn_can_stream() {
        // Given a streaming session whose response tripped a rule.
        let (actor, _audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the session is back in `Sending`; staying in `Streaming` would
        // make the resumed turn's first token an illegal transition.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseKind::Sending);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_noops_when_no_stream_is_in_flight() {
        // Given an idle session carrying a stale guard value.
        let (actor, audit, session_id) = intercept_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.clear_stream_generation();
        }

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then nothing was re-dispatched and no outcome was reported.
        assert!(
            audit
                .of_type::<jinn_turn_dispatch_msg::DispatchTurn>()
                .is_empty()
        );
        assert!(audit.of_type::<TurnCompleted>().is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_answers_a_tool_call_it_caught_mid_arguments() {
        // Given a session interrupted while a tool call's arguments streamed.
        let (actor, _audit, session_id) = intercept_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_tool_call(0, "tc_1", "bash", jiff::Timestamp::now());
            session
                .append_tool_call_delta(0, r#"{"command":"rm -rf "#)
                .expect("append argument delta");
        }

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the call the rule caught is answered, so the resumed request is
        // one the provider will accept.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session.history().iter().any(|e| matches!(
                &e.kind,
                ChatEntryKind::ToolResult { id, .. } if id == "tc_1"
            )),
            "an interrupted tool call must be paired with a result"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_answered_interrupted_call_survives_into_the_resumed_request() {
        // Given a session interrupted while a tool call's arguments streamed.
        let (actor, _audit, session_id) = intercept_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_tool_call(0, "tc_1", "bash", jiff::Timestamp::now());
            session
                .append_tool_call_delta(0, r#"{"command":"rm -rf "#)
                .expect("append argument delta");
        }

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the resumed request carries the call and its result together,
        // so the model can read the attempt it just lost.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        let messages =
            jinn_llm_support::entries_to_messages::entries_to_messages(session.history());
        let answered = messages.iter().any(|m| {
            matches!(m, jinn_provider::LlmMessage::Tool { tool_call_id, .. } if tool_call_id == "tc_1")
        });
        assert!(
            answered,
            "the resumed request must carry the interrupted call's result"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_prose_interrupt_emits_no_tool_result() {
        // Given a session interrupted on prose, with no tool call in flight.
        let (actor, _audit, session_id) = intercept_setup().await;

        // When the intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then nothing is paired, because there was no call to pair.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session
                .history()
                .iter()
                .all(|e| !matches!(&e.kind, ChatEntryKind::ToolResult { .. })),
            "a prose interrupt has no call to answer"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_second_intercept_in_the_same_session_still_resumes() {
        // Given a session whose response tripped a rule once already, with
        // its first intercept's outcome already reported.
        let (actor, audit, session_id) = intercept_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session
                .append_stream_token("second offence", jiff::Timestamp::now())
                .expect("append first token");
            session.arm_stream(jiff::Timestamp::now());
        }

        // When a second intercept completes the response.
        let event = intercept_completion(&session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn resumes again — the abort tombstone the first
        // intercept armed must not wedge the session.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handed_off.len(),
            1,
            "a repeated intercept must still resume the turn"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_arriving_after_a_termination_does_not_resume() {
        // Given a session whose watchdog cancelled the turn, and whose
        // intercept completion — published from inside the stream task the
        // cancel tore down — arrives afterwards. This is the reported
        // ordering, reproduced: the session is even wound back to a busy
        // phase with a live guard, the shape that used to let the
        // intercept's own in-flight check pass on a turn nobody was
        // running. Under the turn record the refusal no longer depends on
        // that shape: the cancel killed the generation, so the rewind is
        // refused whatever the session looks like.
        let (actor, audit, session_id) = intercept_setup().await;
        actor
            .on_cancel_turn(&CancelTurn {
                session_id: session_id.clone(),
            })
            .await;
        {
            // Reproduce the settle: the cancel's own report is consumed,
            // which is what leaves the session looking busy.
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session.rewind_for_retry();
            session.arm_stream(jiff::Timestamp::now());
        }

        // When the intercept's completion lands.
        actor
            .on_stream_completed(&intercept_completion(&session_id))
            .await;

        // Then no resume is dispatched. Dispatching here is what re-armed the
        // in-flight guard on a terminated session and let the stall watchdog
        // re-trigger on a stream that never existed.
        let redispatched = audit.names().iter().any(|n| n.contains("DispatchTurn"));
        assert!(
            !redispatched,
            "a terminated turn must not resume: it would re-arm the in-flight \
             guard on a dead session and the stall watchdog would fire on it"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminated_turn_refuses_to_resume() {
        // Given a streaming session whose turn a cancel really ended — the
        // phase actor asked, which is what ends a turn now.
        let (actor, audit, session_id) = intercept_setup().await;
        let decision = apply_phase(
            &actor.services,
            PhaseCommand::TurnCanceled {
                session_id: session_id.clone(),
            },
        )
        .await
        .expect("phase actor reachable");
        assert!(decision.admitted, "the live turn's cancel must be admitted");

        // When the intercept's completion lands — published from inside the
        // stream task the cancel tore down, so after it.
        actor
            .on_stream_completed(&intercept_completion(&session_id))
            .await;

        // Then no redispatch is requested — the observable consequence of
        // the refusal, not the guard itself.
        assert!(
            !audit.names().iter().any(|n| n.contains("DispatchTurn")),
            "a terminated turn must not be handed back for redispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_late_resume_after_a_cancel_re_arms_nothing_and_the_turn_stays_ended() {
        // Given a session whose turn a cancel really ended — the phase actor
        // asked, and its report consumed, which is how the cancel reaches the
        // session.
        let (actor, _audit, session_id) = intercept_setup().await;
        let decision = ask(
            &actor.services,
            PhaseCommand::TurnCanceled {
                session_id: session_id.clone(),
            },
        )
        .await;
        assert!(decision.admitted, "the live turn's cancel must be admitted");
        actor
            .on_stream_completed(&cancelled_report(&session_id, &decision))
            .await;

        // When the resume queued before the cancel lands — asking the phase
        // actor to mint a `ResumeTurn`, the admission ask the dispatch queue
        // makes before any `SendToLlmProvider` is published.
        let resume = ask(
            &actor.services,
            PhaseCommand::BeginStream {
                session_id: session_id.clone(),
                kind: DispatchKind::ResumeTurn,
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await;

        // Then it is refused — no send follows it, so nothing re-arms the
        // in-flight guard. An armed guard with no stream behind it is the
        // wedge the stall watchdog reports 60 seconds later as a stall.
        assert!(
            !resume.admitted,
            "a resume of a cancelled turn must be refused at mint"
        );
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session.has_in_flight_stream(),
            "a late resume must not re-arm the in-flight guard"
        );
        // And the turn stays ended: a second cancel reports the turn as
        // already ended rather than ending it twice.
        let second = ask(
            &actor.services,
            PhaseCommand::TurnCanceled {
                session_id: session_id.clone(),
            },
        )
        .await;
        assert!(
            !second.admitted,
            "the turn must stay ended after a refused late resume"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_late_resume_does_not_re_arm_the_in_flight_guard() {
        // Given a session whose turn a cancel really ended — the phase actor
        // asked, and its report consumed, which clears the guard the live
        // stream had armed.
        let (actor, _audit, session_id) = intercept_setup().await;
        let decision = ask(
            &actor.services,
            PhaseCommand::TurnCanceled {
                session_id: session_id.clone(),
            },
        )
        .await;
        assert!(decision.admitted, "the live turn's cancel must be admitted");
        actor
            .on_stream_completed(&cancelled_report(&session_id, &decision))
            .await;

        // When a dispatch for the ended turn arrives — asking the phase
        // actor to mint a `ResumeTurn`, the admission ask every resume
        // passes before its `SendToLlmProvider` is published.
        let resume = ask(
            &actor.services,
            PhaseCommand::BeginStream {
                session_id: session_id.clone(),
                kind: DispatchKind::ResumeTurn,
                dispatched_at: jiff::Timestamp::now(),
            },
        )
        .await;

        // Then no guard is armed. An armed guard with nothing behind it is
        // the wedge the stall watchdog reports 60 seconds later as a stall.
        assert!(
            !resume.admitted,
            "a dispatch for a cancelled turn must be refused at mint"
        );
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session.has_in_flight_stream(),
            "a dispatch for a terminated turn must not arm the in-flight guard"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_new_user_message_mints_a_turn_after_a_cancelled_one() {
        // Given a session whose previous turn a cancel really ended — the
        // phase actor asked, its generation dead and its record ended.
        let (actor, audit, session_id) = intercept_setup().await;
        let ended = ask(
            &actor.services,
            PhaseCommand::TurnCanceled {
                session_id: session_id.clone(),
            },
        )
        .await;
        assert!(ended.admitted, "the live turn's cancel must be admitted");

        // When the user submits a new message.
        actor
            .handle_enqueue_user_message(&EnqueueUserMessage {
                session_id: session_id.clone(),
                entry: ChatEntry::user("carry on"),
            })
            .await;

        // Then the new turn is dispatched, not refused as a resume of a turn
        // that no longer exists: a fresh mint is always admitted, and the
        // handoff below is published only past it.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handed_off.len(),
            1,
            "a new user message after a cancelled turn must start a new turn"
        );
        assert_eq!(handed_off[0].session_id, session_id);
    }
}
