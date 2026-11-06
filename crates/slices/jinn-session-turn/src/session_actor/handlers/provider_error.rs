//! Provider-error deferral and give-up handlers.
//!
//! A retryable provider error (`StreamCompleted` with `Error` stamped
//! `retryable`) does not end the turn. The provider-error watchdog
//! (`jinn-watchdog`) schedules the escalating waits and re-dispatches
//! through the stall-retry path; this module is the session actor's half:
//!
//! - [`SessionPersistenceActor::defer_provider_error_settle`] holds the
//!   turn across the waits by skipping the completion fold entirely —
//!   the streaming indices, the in-flight-stream guard, the ledger's
//!   live stream, and the message queue all stay exactly as the failed
//!   generation left them, which is precisely the state
//!   [`on_retry_stalled_session`](super::stall_retry) gates on and
//!   operates on.
//! - [`SessionPersistenceActor::on_give_up_provider_retries`] settles the
//!   deferred error once the schedule is exhausted: the fold's error-arm
//!   effects run now, the ledger's stream retires with `Next::End`, and
//!   [`settle_turn`](super::streaming) applies the `Idle` edge, publishes
//!   `TurnCompleted`, and derives the error outcome from history.
//!
//! The deferral is all-or-nothing by construction: the gate runs before
//! any mutation, and a gate miss returns `false` so the ordinary error
//! settle runs unchanged.

use jinn_inference_msg::StreamCompleted;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_kernel::common::turn_ledger::Next;
use jinn_session_msg::GiveUpProviderRetries;
use jinn_turn_dispatch_msg::QueueItem;

use super::super::SessionPersistenceActor;
use super::streaming::drained_queue_to_text;

impl SessionPersistenceActor {
    /// Decides whether a retryable error completion defers the settle.
    ///
    /// The gate fails open: if the turn is not live with an in-flight,
    /// non-draining stream behind it — or the phase is not working — the
    /// deferral is refused, nothing is mutated, and the caller's ordinary
    /// error settle runs unchanged. A refused deferral costs nothing: the
    /// turn ends exactly as a non-retryable error ends it today.
    pub(in crate::session_actor) fn defer_provider_error_settle(
        &self,
        event: &StreamCompleted,
    ) -> bool {
        let admitted = {
            let live = self.services.turn_ledger.live(&event.session_id);
            let turn_live = live.is_some_and(|t| t.stream.is_some() && !t.draining);
            self.state
                .read()
                .session
                .get(&event.session_id)
                .is_some_and(|session| {
                    turn_live && session.phase().is_working() && session.has_in_flight_stream()
                })
        };
        if !admitted {
            tracing::debug!(
                session_id = %event.session_id,
                "retryable error completion not deferrable; settling ordinarily"
            );
            return false;
        }
        tracing::info!(
            session_id = %event.session_id,
            "provider error deferred: holding the turn for the watchdog's retry"
        );
        true
    }

    /// Settles a deferred provider error after the watchdog gives up.
    ///
    /// Re-runs the completion fold's error-arm effects under one write lock
    /// — finalize entries, end the streaming round, drop the request-model
    /// stamp, drain queued messages to the input buffer — then retires the
    /// ledger's stream and settles the turn through the ordinary
    /// [`Next::End`] edge, which publishes the phase change and
    /// `TurnCompleted` and derives `Error` from the error entry the
    /// inference actor already pushed to history.
    ///
    /// Gated like the retry handler: a session whose turn a cancel already
    /// ended is `Idle` with no in-flight stream, so a straggling give-up is
    /// refused with one warn — the settle it would race already happened.
    pub(in crate::session_actor) async fn on_give_up_provider_retries(
        &self,
        payload: &GiveUpProviderRetries,
    ) {
        let session_id = &payload.session_id;

        // The fold's error-arm effects, under one write lock, gated on the
        // same working-phase + in-flight-stream shape the retry handler
        // refuses outside of.
        let gate_passed = {
            let mut state = self.state.write();
            match state.session.get_mut(session_id) {
                Some(session) if session.phase().is_working() && session.has_in_flight_stream() => {
                    // Finalize the partial entries so they carry finish
                    // stamps (the assistant entry is not preserved: the
                    // generation failed).
                    session.finalize_entries_for_finish(false, jiff::Timestamp::now());
                    // End the streaming round: the indices the deferred
                    // state kept alive are consumed here, exactly as the
                    // fold would have.
                    session.finish_streaming_round();
                    // The generation's request stamp is consumed with it.
                    session.clear_stream_request_model();
                    // Drain queued messages to the input buffer, the
                    // error arm's behavior, so the user's queued work
                    // survives for re-sending.
                    let drained: std::collections::VecDeque<QueueItem> =
                        session.message_queue_mut().drain();
                    if let Some(text) = drained_queue_to_text(&drained) {
                        session.update_input(|input| input.replace_all(text));
                    }
                    true
                }
                _ => false,
            }
        };
        if !gate_passed {
            tracing::warn!(
                session_id = %session_id,
                "provider-error give-up refused: no deferred turn to settle"
            );
            return;
        }

        // Retire the deferred stream in the ledger, ending the turn. The
        // deferred completion's identity names the turn and stream it
        // belongs to; the give-up carries only the session, so the live
        // entry's identity is read here — the same admission shape the
        // retry path uses.
        let live = self.services.turn_ledger.live(session_id);
        let Some(entry) = live else {
            tracing::warn!(
                session_id = %session_id,
                "provider-error give-up refused: no live turn in the ledger"
            );
            return;
        };
        let ack = self.services.turn_ledger.ack_stream_with_calls(
            session_id,
            &entry.turn,
            &entry.stream.unwrap_or_default(),
            Next::End,
            vec![],
        );
        if let Err(refused) = ack {
            tracing::warn!(
                session_id = %session_id,
                reason = ?refused,
                "provider-error give-up ack refused by the turn ledger"
            );
            return;
        }
        // The `Err` arm returned above, so the `Ok` payload is a plain value
        // here — no re-`expect` on a `Result` clippy already knows is checked.
        if let Ok(Some(settlement)) = ack {
            self.settle_turn(session_id, settlement.next).await;
        }

        super::super::helpers::emit_history_appended(self.bus(), session_id).await;
        self.save_active_session(session_id).await;
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
    use super::super::super::SessionPersistenceActor;
    use jinn_core_types::ChatEntryKind;
    use jinn_inference_msg::{StreamCompleted, StreamCompletedReason};
    use jinn_kernel::common::turn_ledger::{self, MintKind};
    use jinn_session_msg::{GiveUpProviderRetries, TurnCompleted, TurnOutcome};
    use jinn_slices::PhaseView;
    use jinn_turn_dispatch_msg::QueueItem;

    /// Seeds a Streaming session with an armed, in-flight stream — the
    /// exact shape a retryable provider error defers: a minted turn with a
    /// registered stream, a partial assistant entry holding a streaming
    /// index, the guard armed.
    async fn deferred_setup() -> (
        SessionPersistenceActor,
        jinn_kernel::common::services::BusAudit,
        jinn_core_types::SessionId,
    ) {
        let (actor, audit) = super::super::super::helpers::test_actor_recording().await;
        let _ = jinn_context_assembly::service::ensure_spawned(&actor.services.trouper_system);
        let session_id = actor.state.read().session.active_session_id().clone();
        // The mint registers the stream: `entry.stream` is the grant's, which
        // is what `live()` reads back.
        let grant = turn_ledger::mint_dispatch_with_stamp(
            &actor.services,
            &actor.state,
            &session_id,
            MintKind::FreshTurn,
            jiff::Timestamp::now(),
        )
        .await
        .expect("test seed: the deferred setup mints a turn");
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session
                .append_stream_token("partial", jiff::Timestamp::now())
                .expect("append partial token");
        }
        let _ = grant;
        (actor, audit, session_id)
    }

    /// A retryable-error completion stamped with `grant`'s identity, built
    /// structurally — the completion's `turn`/`stream` fields are typed
    /// counters, not strings.
    fn retryable_error_event(
        session_id: &jinn_core_types::SessionId,
        turn: jinn_core_types::turn_identity::TurnToken,
        stream: jinn_core_types::turn_identity::StreamId,
    ) -> StreamCompleted {
        StreamCompleted {
            session_id: session_id.clone(),
            reason: StreamCompletedReason::Error,
            retryable: true,
            assistant_content: None,
            tool_calls: None,
            cost: None,
            provider_completion_tokens: None,
            provider_prompt_tokens: None,
            cached_tokens: None,
            thinking_content: None,
            dispatched_at: jiff::Timestamp::now(),
            model_used: None,
            turn,
            stream,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_retryable_error_deferral_keeps_the_turn_live() {
        // Given a Streaming session whose in-flight stream failed retryably.
        let (actor, _audit, session_id) = deferred_setup().await;

        // When the deferral gate runs.
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        let deferred = actor.defer_provider_error_settle(&event);

        // Then the deferral is granted.
        assert!(deferred, "a live, streaming turn's retryable error defers");

        // And the turn stays exactly as the failed generation left it:
        // the streaming indices, the in-flight guard, the ledger stream,
        // and the phase are all intact — the state the retry handler
        // requires.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Streaming);
        assert!(session.has_in_flight_stream(), "the guard stays armed");
        assert!(
            session.history().iter().any(
                |e| matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial")
                    && e.is_in_context()
            ),
            "the partial entry stays in context"
        );
        assert!(
            actor
                .services
                .turn_ledger
                .live(&session_id)
                .is_some_and(|t| t.stream.is_some()),
            "the ledger stream stays live"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_deferred_session_satisfies_the_stall_retry_gate() {
        // Given a session whose retryable error was deferred.
        let (actor, audit, session_id) = deferred_setup().await;
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        assert!(actor.defer_provider_error_settle(&event));

        // When the watchdog's retry command arrives.
        let payload = jinn_session_msg::RetryStalledSession {
            session_id: session_id.clone(),
            attempt: 1,
            max_restarts: 3,
        };
        actor.on_retry_stalled_session(&payload).await;

        // Then the retry proceeds: the failed attempt is excluded from
        // context but visible, and the turn is handed off for re-dispatch.
        let handed_off = audit.of_type::<jinn_turn_dispatch_msg::DispatchTurn>();
        assert!(
            handed_off.iter().any(|s| s.session_id == session_id),
            "the deferred state must be dispatchable through the stall-retry path"
        );
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert!(
            !session.history().iter().any(
                |e| matches!(e.kind, ChatEntryKind::Assistant(ref t) if t == "partial")
                    && e.is_in_context()
            ),
            "the failed attempt must be excluded from the retried request"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_non_retryable_error_settles_ordinarily() {
        // Given a live streaming session.
        let (actor, audit, session_id) = deferred_setup().await;
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        // The error entry production's LLM actor pushes (PushChatEntry
        // precedes StreamCompleted(Error)) — history is what the settle
        // derives the outcome from.
        {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(jinn_kernel::protocol::ChatEntry::error(
                    "provider unreachable",
                ));
        }

        // When a non-retryable error completion arrives.
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        let event = StreamCompleted {
            retryable: false,
            ..event
        };
        actor.on_stream_completed(&event).await;

        // Then the turn settles today-style: Idle, with the guard cleared.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle, "the turn settles to Idle");
        assert!(!session.has_in_flight_stream(), "the guard is consumed");
        // And a TurnCompleted(Error) was published.
        let completed = audit.of_type::<TurnCompleted>();
        assert!(
            completed
                .iter()
                .any(|t| t.session_id == session_id && t.outcome == TurnOutcome::Error),
            "the ordinary error settle reports the turn: {:?}",
            completed
                .iter()
                .map(|t| (&t.session_id, t.outcome))
                .collect::<Vec<_>>()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_deferral_gate_miss_falls_through() {
        // Given a session whose guard was already consumed (the
        // self-resolved race shape).
        let (actor, _audit, session_id) = deferred_setup().await;
        {
            let mut state = actor.state.write();
            state.active_session_mut().clear_stream_generation();
        }
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");

        // When the deferral gate runs.
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        let deferred = actor.defer_provider_error_settle(&event);

        // Then the deferral is refused — today's error settle will run.
        assert!(!deferred, "no in-flight stream, no deferral");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn give_up_settles_the_deferred_error_exactly_once() {
        // Given a session whose retryable error was deferred, with the error
        // entry production's LLM actor pushes already in history.
        let (actor, audit, session_id) = deferred_setup().await;
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        {
            let mut state = actor.state.write();
            state
                .active_session_mut()
                .push_entry(jinn_kernel::protocol::ChatEntry::error(
                    "provider overloaded",
                ));
        }
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        assert!(actor.defer_provider_error_settle(&event));
        // And the user queued a message that the deferred state preserved.
        {
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.enqueue(QueueItem::UserMessage(Box::new(
                jinn_kernel::protocol::ChatEntry::user("queued work"),
            )));
        }

        // When the give-up command arrives.
        let payload = GiveUpProviderRetries {
            session_id: session_id.clone(),
        };
        actor.on_give_up_provider_retries(&payload).await;

        // Then the turn settles exactly once, to Idle, reporting Error.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
        assert!(!session.has_in_flight_stream());
        // And the queued work drained to the input buffer, the error arm's
        // behavior.
        let input = session.with_input(|i| i.text().to_owned(), String::new);
        assert!(
            input.contains("queued work"),
            "the error arm drains the queue to the input buffer, got: {input:?}"
        );
        let completed = audit.of_type::<TurnCompleted>();
        assert_eq!(
            completed
                .iter()
                .filter(|t| t.session_id == session_id)
                .count(),
            1,
            "the give-up settle reports the turn exactly once: {:?}",
            audit.names()
        );
        assert!(
            completed
                .iter()
                .any(|t| t.session_id == session_id && t.outcome == TurnOutcome::Error),
            "the give-up settle derives Error from the error entry in history"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn give_up_is_refused_after_a_cancel() {
        // Given a session whose deferred turn a cancel already settled.
        let (actor, _audit, session_id) = deferred_setup().await;
        let live = actor
            .services
            .turn_ledger
            .live(&session_id)
            .expect("live turn");
        let event = retryable_error_event(&session_id, live.turn, live.stream.unwrap_or_default());
        assert!(actor.defer_provider_error_settle(&event));
        {
            // The cancel path's settle: entries finalized, guard cleared,
            // phase to Idle.
            let mut state = actor.state.write();
            let session = state.active_session_mut();
            session.finalize_entries_for_finish(false, jiff::Timestamp::now());
            session.settle_to_idle();
            session.clear_stream_generation();
        }

        // When the give-up command arrives.
        let payload = GiveUpProviderRetries {
            session_id: session_id.clone(),
        };
        actor.on_give_up_provider_retries(&payload).await;

        // Then it is refused: the session stays Idle and no second settle
        // announcement raced the cancel's.
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn give_up_refused_without_a_deferred_turn_is_a_warn_noop() {
        // Given an Idle session with no deferred state at all.
        let (actor, _audit, session_id) = deferred_setup().await;
        {
            let mut state = actor.state.write();
            state.active_session_mut().settle_to_idle();
        }

        // When the give-up command arrives.
        let payload = GiveUpProviderRetries {
            session_id: session_id.clone(),
        };
        actor.on_give_up_provider_retries(&payload).await;

        // Then nothing panicked and the session is untouched (still Idle).
        let state = actor.state.read();
        let session = state.session.get(&session_id).expect("session exists");
        assert_eq!(session.phase(), PhaseView::Idle);
    }
}
