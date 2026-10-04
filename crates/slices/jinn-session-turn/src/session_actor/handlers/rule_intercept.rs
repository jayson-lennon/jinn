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
    ) {
        debug_assert_eq!(reason, StreamCompletedReason::RuleIntercept);

        let acted = self.state.with_session(|view| {
            let session = view.session.map().get_or_create(session_id);
            if matches!(session.phase(), PhaseKind::Sending | PhaseKind::Streaming)
                && session.has_in_flight_stream()
            {
                // The intercepted output stays in history for the user to
                // read — they watched it appear — and is only excluded from
                // the resumed request. Must run before the rewind below,
                // which drops the streaming indices it reads.
                let excluded = session.reset_streaming_entries_for_retry();
                let dangling = session.force_exclude_dangling_tool_calls();
                tracing::warn!(
                    session_id = %session_id,
                    excluded_entries = excluded.len(),
                    excluded_dangling = dangling.len(),
                    "stream rule interrupted the turn; resuming with the rule body"
                );
                // Rewind so the resumed dispatch re-enters streaming through
                // the legal path; staying in `Streaming` would make its first
                // token an invalid transition.
                session.rewind_for_retry();
                true
            } else {
                tracing::warn!(
                    session_id = %session_id,
                    phase = ?session.phase(),
                    stream_in_flight = session.has_in_flight_stream(),
                    "rule-intercept resume refused: no in-flight stream"
                );
                false
            }
        });

        if !acted {
            return;
        }

        super::super::helpers::emit_history_appended(self.bus(), session_id).await;

        // Hand the turn back to the dispatch queue, which emits the fresh
        // user-originated `SendToLlmProvider` that lifts the tombstone this
        // intercept armed.
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
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::super::super::helpers::test_actor_recording;
    use jinn_core_types::ChatEntryKind;
    use jinn_core_types::SessionId;
    use jinn_inference_msg::StreamCompletedReason;
    use jinn_kernel::common::services::BusAudit;
    use jinn_session_msg::{PhaseKind, TurnCompleted, TurnOutcome};

    use crate::session_actor::SessionPersistenceActor;

    /// A session mid-response: streaming, holding a partial assistant entry,
    /// with the in-flight-stream guard armed — the shape an intercepted
    /// response presents.
    async fn intercept_setup() -> (SessionPersistenceActor, BusAudit, SessionId) {
        let (actor, audit) = test_actor_recording().await;
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.begin_streaming();
            session
                .append_stream_token("the offending text", jiff::Timestamp::now())
                .expect("append first token");
            session.arm_stream(jiff::Timestamp::now());
            state.session.active_session_id().clone()
        };
        (actor, audit, session_id)
    }

    /// A `StreamCompleted` carrying the intercept reason.
    fn intercept_completion(session_id: &SessionId) -> jinn_inference_msg::StreamCompleted {
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
}
