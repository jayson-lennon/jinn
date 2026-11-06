//! Loop-retry handler — silently retries a turn the repetition detector
//! stopped.
//!
//! When the inference actor's repetition detector sees the output repeating
//! verbatim, it ends the stream with a [`StreamCompleted`] carrying
//! [`StreamCompletedReason::RepetitionLoop`] and no guidance entry, and this
//! handler turns that into a resumed turn.
//!
//! The shape is the stall-retry path's, and for the same reason: a looped
//! attempt and a stalled attempt fail the same way — the response must be
//! re-sent, and whatever it produced so far is not something the model
//! should see as its own. Both take the partial entries out of context
//! (leaving them visible in the chat log), rewind the phase, and
//! re-dispatch.
//!
//! The difference from the rule intercept is deliberate: there is **no
//! guidance**. The intercept pairs an interrupted tool call with a synthetic
//! result explaining the interruption because the model must read its
//! mistake to avoid repeating it; here the model's mistake *is* the
//! repetition, and telling it so cannot help — the retry is silent, and the
//! model never learns it looped. A tool call the detector caught mid-garbage
//! is therefore excluded outright (the stall variant), not paired: the
//! model should not read its own looped arguments, and the fresh request
//! must not re-seed the loop.
//!
//! The user still sees what happened: the reasoning-loop watchdog publishes
//! a visible system marker per detection (system entries are excluded from
//! LLM context by default), so this handler publishes no chat entry of its
//! own.
//!
//! # Why the turn resumes as a dispatch
//!
//! The inference actor arms a cancel tombstone for any session it aborts,
//! and clears it *only* when a `User`-origin send arrives. A retried turn
//! dispatched any other way would be dropped silently, with no log, and
//! the session would sit idle forever. [`DispatchTurn`] is therefore the
//! correct handoff: the turn-dispatch queue actor emits the fresh
//! `SendToLlmProvider` as a user-originated send, which is the one path
//! that lifts the tombstone.

use jinn_inference_msg::StreamCompletedReason;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_slices::PhaseView;
use jinn_turn_dispatch_msg::DispatchTurn;

use super::super::SessionPersistenceActor;

impl SessionPersistenceActor {
    /// Resumes a turn the repetition detector interrupted.
    ///
    /// Runs only for [`StreamCompletedReason::RepetitionLoop`] and only
    /// while a stream is genuinely in flight — the same guard the stall and
    /// intercept paths use, so a loop completion that lands after the
    /// session has moved on is a no-op rather than a rewind from `Idle`.
    ///
    /// The watchdog's marker is *not* pushed here: the reasoning-loop
    /// watchdog actor publishes it, keyed on the same completion this
    /// handler consumes. This handler stays silent toward both the model
    /// and the log.
    pub(in crate::session_actor) async fn on_loop_retry(
        &self,
        session_id: &jinn_core_types::SessionId,
        reason: StreamCompletedReason,
        // The event's stamp is not read directly: admission resolves the
        // live turn through the ledger, not against the event's own claim.
        _dispatched_at: jiff::Timestamp,
    ) {
        debug_assert_eq!(
            reason,
            StreamCompletedReason::RepetitionLoop,
            "only a repetition loop reaches the loop-retry handler"
        );

        // Entry surgery first, on the stall variant: the partial entries
        // stay in the log for the user but leave the context for the
        // retry, and a tool call the detector caught mid-arguments is
        // excluded outright — the model never reads its looped attempt,
        // and a dangling call would make the resumed request one every
        // provider rejects. The rewind edge clears the tool-call index
        // map, which is the only record of which calls are mid-arguments,
        // so the exclusion must run while the indices still exist.
        let rewound = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            if matches!(
                session.phase(),
                PhaseView::Streaming | PhaseView::Working | PhaseView::Cancelling
            ) && session.has_in_flight_stream()
            {
                // The looped attempt's output stays in history for the
                // user to read — they watched it appear — and is only
                // excluded from the retried request.
                let excluded = session.reset_streaming_entries_for_retry();
                let dangling = session.force_exclude_dangling_tool_calls();
                tracing::warn!(
                    session_id = %session_id,
                    excluded_entries = excluded.len(),
                    excluded_dangling = dangling.len(),
                    "repetition loop detected; retrying the turn silently"
                );
                // The retried request must be admissible: staying in
                // `Streaming` would make the retry's first token an
                // illegal transition. The rewind edge mirrors the stall
                // retry's.
                session.rewind_for_retry();
                true
            } else {
                tracing::warn!(
                    session_id = %session_id,
                    phase = ?session.phase(),
                    stream_in_flight = session.has_in_flight_stream(),
                    "loop-retry refused: no in-flight stream"
                );
                false
            }
        });
        if !rewound {
            return;
        }

        // Admission after the surgery: the rewind edge is the ledger's —
        // the same turn must still be live, not draining, with the looped
        // stream still the live one. A cancelled turn refuses here — the
        // cancel and this completion race by construction (the completion
        // is published from inside the stream task the cancel tears down,
        // and the reasoning-loop watchdog's surrender cancel races the
        // retry the same way) — and a retried dispatch for a dead turn is
        // what re-armed a session nobody was streaming to. The excluded
        // entries stay excluded either way.
        let live = self.services.turn_ledger.live(session_id);
        if !live.is_some_and(|t| t.stream.is_some() && !t.draining) {
            tracing::warn!(
                session_id = %session_id,
                "loop-retry refused: no live, non-draining stream behind it"
            );
            return;
        }

        super::super::helpers::emit_history_appended(self.bus(), session_id).await;

        // Hand the turn back to the dispatch queue, which emits the fresh
        // user-originated `SendToLlmProvider` that continues the turn. The
        // queue's admission mints `Resume`, which joins this live turn and
        // swaps in a fresh stream.
        self.publish(DispatchTurn {
            session_id: session_id.clone(),
        })
        .await;

        // The turn did not end, but the outcome is still reported so
        // consumers learn what happened to this response. It is published
        // here rather than by the generic completion fold, which this
        // retry deliberately bypasses: a consumer reading `TurnCompleted`
        // must be able to tell a retried generation from a cancelled turn,
        // and only this handler knows which happened. `LoopRetried` is
        // non-terminal, so watchdog accumulators keep their lineage across
        // the retries.
        self.publish(TurnCompleted {
            session_id: session_id.clone(),
            outcome: TurnOutcome::LoopRetried,
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
    use jinn_core_types::ChatEntryKind;
    use jinn_core_types::SessionId;
    use jinn_core_types::ToolCallId;
    use jinn_inference_msg::StreamCompletedReason;
    use jinn_kernel::common::services::BusAudit;
    use jinn_session_msg::{PhaseView, TurnCompleted, TurnOutcome};

    use crate::session_actor::SessionPersistenceActor;

    /// A session mid-response: streaming, holding a partial assistant
    /// entry, with the in-flight-stream guard armed and the turn's
    /// generation minted through the ledger — the shape a looped response
    /// presents. The same fixture the intercept tests build, deliberately:
    /// the two paths must be interchangeable at the harness level.
    async fn loop_setup() -> (SessionPersistenceActor, BusAudit, SessionId) {
        let (actor, audit) = test_actor_recording().await;
        let session_id = actor.state.read().session.active_session_id().clone();
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
        {
            let mut state = actor.state.write();
            let session = state.session.get_mut(&session_id).expect("session exists");
            session
                .append_stream_token("the looped text", stamp)
                .expect("append first token");
            session.arm_stream(stamp);
        }
        (actor, audit, session_id)
    }

    /// A `StreamCompleted` carrying the loop reason, stamped with the
    /// identity the looped dispatch minted — the live turn and its
    /// in-flight stream, read the way the inference actor stamps them.
    fn loop_completion(
        services: &jinn_kernel::common::services::Services,
        session_id: &SessionId,
    ) -> jinn_inference_msg::StreamCompleted {
        let live = services
            .turn_ledger
            .live(session_id)
            .expect("a looped response rides a live, minted turn");
        jinn_inference_msg::StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::RepetitionLoop,
            model_used: Some("m".to_owned()),
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
            turn: live.turn,
            stream: live.stream.expect("a looped stream is in flight"),
            // The loop completion is the reasoning-loop watchdog's, never
            // the provider-error watchdog's.
            retryable: false,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_looped_response_excludes_its_output_from_the_retried_request() {
        // Given a streaming session whose output looped.
        let (actor, _audit, session_id) = loop_setup().await;

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the output is still in the chat log — the user watched it
        // appear — but out of context for the retried turn.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "the looped text")),
            "the looped output must stay visible"
        );
        assert!(
            !session.history().iter().any(|e| {
                matches!(&e.kind, ChatEntryKind::Assistant(t) if t == "the looped text")
                    && e.is_in_context()
            }),
            "the looped output must not be sent to the provider on retry"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retry_reports_its_own_outcome_not_a_cancel() {
        // Given a streaming session whose output looped.
        let (actor, audit, session_id) = loop_setup().await;

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn outcome is a loop retry, so no consumer reads it
        // as a user cancel and watchdog lineages keep their count.
        let completed = audit.of_type::<TurnCompleted>();
        assert_eq!(completed.len(), 1, "a retry reports exactly one outcome");
        assert_eq!(completed[0].outcome, TurnOutcome::LoopRetried);
        assert_ne!(completed[0].outcome, TurnOutcome::Canceled);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retry_pushes_no_guidance_entry() {
        // Given a streaming session whose output looped.
        let (actor, _audit, session_id) = loop_setup().await;

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then no guidance or cancel entry was pushed — the retry is
        // silent: the model is never told it looped, and the user-facing
        // marker is the watchdog's.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::RuleInterrupt { .. })),
            "a loop retry must not carry guidance"
        );
        assert!(
            !session
                .history()
                .iter()
                .any(|e| matches!(&e.kind, ChatEntryKind::Error(t) if t == "Cancelled")),
            "a loop retry must not look like a user cancel"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retry_hands_the_turn_back_for_redispatch() {
        // Given a streaming session whose output looped.
        let (actor, audit, session_id) = loop_setup().await;

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn is re-dispatched — the dispatch queue emits the
        // fresh user-originated send that lifts the abort tombstone.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(handed_off.len(), 1, "a loop retry resumes exactly one turn");
        assert_eq!(handed_off[0].session_id, session_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retry_rewinds_the_phase_so_the_retry_can_stream() {
        // Given a streaming session whose output looped.
        let (actor, _audit, session_id) = loop_setup().await;

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the session is back in `Working`; staying in `Streaming`
        // would make the retried turn's first token an illegal transition.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Working);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_caught_mid_arguments_is_excluded_outright_not_paired() {
        // Given a session looping while a tool call's arguments streamed —
        // the call-loop sample's shape.
        let (actor, _audit, session_id) = loop_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_tool_call(0, &ToolCallId::from("tc_1"), "bash", jiff::Timestamp::now());
            session
                .append_tool_call_delta(0, r#"{"command":"dot dot dot"#)
                .expect("append argument delta");
        }

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the call was excluded outright: no synthetic result explains
        // it, because the model must not read its looped attempt.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            session
                .history()
                .iter()
                .all(|e| !matches!(&e.kind, ChatEntryKind::ToolResult { .. })),
            "a looped tool call must not be paired with a result"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retry_noops_when_no_stream_is_in_flight() {
        // Given an idle session carrying a stale guard value.
        let (actor, audit, session_id) = loop_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.clear_stream_generation();
        }

        // When the loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
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
    async fn a_looped_turn_refuses_to_retry_after_a_cancel() {
        // Given a streaming session whose turn a cancel really ended — the
        // ledger's `begin_cancel`, which is what ends a turn.
        let (actor, audit, session_id) = loop_setup().await;
        let outcome = actor.services.turn_ledger.begin_cancel(&session_id);
        assert!(outcome.is_some(), "the live turn's cancel must be admitted");

        // When the loop's completion lands — published from inside the
        // stream task the cancel tore down, so after it.
        actor
            .on_stream_completed(&loop_completion(&actor.services, &session_id))
            .await;

        // Then no redispatch is requested — the observable consequence of
        // the refusal. This is also the watchdog's surrender race: either
        // order settles correctly, and cancel-first refuses here.
        assert!(
            !audit.names().iter().any(|n| n.contains("DispatchTurn")),
            "a terminated turn must not be handed back for redispatch"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_second_loop_in_the_same_session_still_retries() {
        // Given a session whose response looped once already, with its
        // first retry's outcome already reported.
        let (actor, audit, session_id) = loop_setup().await;
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session
                .append_stream_token("second loop", jiff::Timestamp::now())
                .expect("append first token");
            session.arm_stream(jiff::Timestamp::now());
        }

        // When a second loop completion lands.
        let event = loop_completion(&actor.services, &session_id);
        actor.on_stream_completed(&event).await;

        // Then the turn retries again — the abort tombstone the first
        // retry armed must not wedge the session.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert_eq!(
            handed_off.len(),
            1,
            "a repeated loop must still retry the turn"
        );
    }
}
