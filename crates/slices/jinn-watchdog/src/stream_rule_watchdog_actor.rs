//! Stream-rule watchdog actor — cancels turns a rule keeps interrupting.
//!
//! The third sibling of the watchdog family, and the one that watches what the
//! harness itself does to a turn. A stream rule that matches a live response
//! interrupts it and resumes with the rule's body as guidance; when the model
//! produces the same violation again, the correction has become a loop. This
//! actor is where that loop is bounded.
//!
//! One counter per session. A [`StreamCompleted`] carrying
//! [`StreamCompletedReason::RuleIntercept`] increments it; a response that ran
//! to completion ([`StreamCompletedReason::Finished`]) debits it by one, floor
//! at zero; reaching the configured maximum trips the watchdog — a system
//! entry naming the rule, then a [`CancelTurn`] that also drops the looping
//! turn's pending resume, then the counter is
//! zeroed so the same session is not re-killed immediately.
//!
//! # Why the count is per session and consecutive
//!
//! Per session because the failure is one model's refusal to be corrected, and
//! several rules can conspire to cause it. Consecutive because the alternative
//! is wrong in both directions: a *cumulative* budget would eventually cancel
//! a long healthy session that tripped a rule twice over an hour, and a
//! *per-response* budget would be cleared by the very event it counts, since
//! an interrupt is itself a response boundary.
//!
//! # Why a `Finished` response debits rather than resets
//!
//! The failure being bounded is a model that trips, resumes, and trips again.
//! A model that completes a response after being corrected has heard the
//! correction, so it earns its budget back one step at a time. Debiting rather
//! than clearing is what makes two early slips survivable while a sustained
//! refusal is not.
//!
//! # Why this actor and not the matcher
//!
//! The matcher answers one question — "did a rule match" — and holds no count
//! and decides nothing about cancelling. A rule that fires is a fact; whether
//! that fact ends the session is a policy question, and policy that spans a
//! whole turn belongs to an actor that sees the whole turn.
//!
//! # Why the trip latches rather than cancels
//!
//! An intercept ends the response it caught and the turn re-dispatches at once,
//! so by the time this actor counts the interrupt there is no stream left to
//! cancel — the generation that tripped the rule was already torn down by the
//! very intercept that reported it. A plain [`CancelCause::Turn`] published
//! here finds nothing to end and does nothing.
//!
//! Worse, it does nothing *safely*: the resume it is meant to stop is a
//! three-actor chain (rewind, dispatch, assemble) that has not arrived yet, and
//! the resume arrives as a user-originated send, which lifts the cancel
//! tombstone an abort had armed. The marker would appear and the turn would
//! resume anyway — the loop, unbounded and indefinitely, which is the one
//! outcome this actor exists to prevent.
//!
//! So the trip publishes [`CancelTurn`] with
//! [`CancelCause::TurnAndQueuedDispatch`], which ends the turn *and* latches
//! away the *pending* resume rather than relying on cancelling a stream that
//! is gone. The latch
//! is spent by that resume, so the user's next genuine message dispatches
//! normally.
//!
//! This actor holds no `AppState` — its counter is actor-internal and its only
//! outputs are bus publishes (the sanctioned shape of the watchdog family).

use std::collections::HashMap;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::CancelCause;
use jinn_inference_msg::CancelTurn;
use jinn_inference_msg::StreamCompleted;
use jinn_inference_msg::StreamCompletedReason;
use jinn_kernel::Services;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::TurnCompleted;

/// The stream-rule watchdog actor's static trouper path.
pub const STREAM_RULE_WATCHDOG_PATH: &str = "stream-rule-watchdog";

/// One watchdog output: the trip marker (text) or a cancel.
///
/// Returned by [`StreamRuleWatchdogActor::on_rule_interrupt`] in publish order
/// so tests can assert the pair's sequencing without a bus. The same shape as
/// the tool-call watchdog's action enum, and the reason the watchdog family has
/// one: the decision and the publishing are separately testable, and the order
/// is asserted rather than inferred.
#[derive(Debug)]
pub enum StreamRuleWatchdogAction {
    /// The system entry naming the rule that looped.
    Marker(SessionId, String),
    /// The cancel that ends the turn and drops its pending resume.
    CancelTurn(SessionId),
}

/// Dependencies for [`StreamRuleWatchdogActor`].
#[derive(Clone)]
pub struct StreamRuleWatchdogActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
    /// Consecutive interrupts tolerated before tripping.
    pub max_failures: u8,
}

/// The stream-rule watchdog actor.
///
/// Per-session interrupt accumulator; trip = marker entry + cancel.
pub struct StreamRuleWatchdogActor {
    services: Services,
    max_failures: u8,
    /// Interrupt counters keyed by session id.
    ///
    /// Actor-internal by design: nothing outside the watchdog family needs to
    /// observe the count, and giving the counter a shared handle would let a
    /// second component decide the same trip.
    accumulators: HashMap<SessionId, u32>,
}

impl ServiceActor for StreamRuleWatchdogActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `start_with`
        // (Services carries typed handles that cannot ride JSON args).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("StreamRuleWatchdogActor is spawned via start_with"),
        )
    }
}

impl StreamRuleWatchdogActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: StreamRuleWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(STREAM_RULE_WATCHDOG_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services: deps.services,
                            max_failures: deps.max_failures,
                            accumulators: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<StreamCompleted>()
            // The turn-end signal. This actor's own trip ends the turn *and*
            // drops the resume the session actor had already armed a guard for,
            // so no `StreamCompleted(Canceled)` follows it — without this the
            // accumulator survives a turn this actor just terminated, and the
            // session's next turn is one intercept away from tripping again.
            .handles::<TurnCompleted>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    async fn publish_actions(&self, actions: Vec<StreamRuleWatchdogAction>) {
        for action in actions {
            match action {
                StreamRuleWatchdogAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                StreamRuleWatchdogAction::CancelTurn(session_id) => {
                    self.services
                        .bus
                        .publish(CancelTurn {
                            session_id,
                            cause: CancelCause::TurnAndQueuedDispatch,
                        })
                        .await;
                }
            }
        }
    }
}

impl MsgHandler<StreamCompleted> for StreamRuleWatchdogActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        let actions = self.on_response_end(&msg.session_id, msg.reason);
        self.publish_actions(actions).await;
    }
}

impl MsgHandler<TurnCompleted> for StreamRuleWatchdogActor {
    /// Clears the session's interrupt count when its turn ends.
    ///
    /// A turn that ends — succeeded, cancelled, or terminated by this watchdog —
    /// closes the chapter the count was measuring. Carrying it forward would mean
    /// the next turn starts part-way to a trip it has done nothing to earn.
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id);
    }
}

impl StreamRuleWatchdogActor {
    /// Builds an actor with an explicit maximum (test seam).
    #[must_use]
    pub fn with_max_failures(services: Services, max_failures: u8) -> Self {
        Self {
            services,
            max_failures,
            accumulators: HashMap::new(),
        }
    }

    /// Applies the response-end policy and returns the actions to publish, in
    /// order.
    ///
    /// The three reasons split three ways, and each split is the design:
    ///
    /// - `Finished` — the model produced a whole response after being
    ///   corrected. It heard the correction, so the debt is repaid one step,
    ///   clamped at zero.
    /// - `RuleIntercept` — a rule fired again. The count rises, and exceeding
    ///   the maximum produces the watchdog pair.
    /// - everything else (`ToolUse`, `Canceled`, `Error`) — the response ended
    ///   without a final answer. Retained deliberately: debiting on every
    ///   non-clean end would repay the debt on exactly the path the budget
    ///   exists to bound, and the count could never be spent.
    #[must_use]
    pub fn on_response_end(
        &mut self,
        session_id: &SessionId,
        reason: StreamCompletedReason,
    ) -> Vec<StreamRuleWatchdogAction> {
        match reason {
            StreamCompletedReason::RuleIntercept => self.record_interrupt(session_id),
            StreamCompletedReason::Finished => {
                if let Some(count) = self.accumulators.get_mut(session_id) {
                    *count = count.saturating_sub(1);
                }
                Vec::new()
            }
            StreamCompletedReason::ToolUse
            | StreamCompletedReason::Canceled
            | StreamCompletedReason::Error => Vec::new(),
        }
    }

    /// Clears the session's interrupt count when its turn ends.
    pub fn on_turn_end(&mut self, session_id: &SessionId) {
        self.accumulators.remove(session_id);
    }

    /// Records one rule interrupt for `session_id`, returning the actions the
    /// trip produces.
    ///
    /// The comparison is against the count *after* the increment, so a budget
    /// of `n` permits exactly `n` interrupts and the `n + 1`th cancels.
    /// Tripping on the nth would silently allow only `n - 1`, which reads as
    /// an off-by-one against the configured number even though the counter is
    /// right.
    #[must_use]
    fn record_interrupt(&mut self, session_id: &SessionId) -> Vec<StreamRuleWatchdogAction> {
        let count = self.accumulators.get(session_id).copied().unwrap_or(0) + 1;
        if count <= u32::from(self.max_failures) {
            self.accumulators.insert(session_id.clone(), count);
            return Vec::new();
        }
        // Latch: zero the counter rather than leaving it at the maximum, so
        // the next response the model starts is judged afresh instead of
        // being cancelled before it produces anything.
        self.accumulators.insert(session_id.clone(), 0);
        vec![
            StreamRuleWatchdogAction::Marker(
                session_id.clone(),
                trip_text(self.max_failures, count),
            ),
            StreamRuleWatchdogAction::CancelTurn(session_id.clone()),
        ]
    }
}

/// The watchdog system-entry text for a trip after `failures` consecutive
/// interrupts with a `max` limit.
fn trip_text(max: u8, failures: u32) -> String {
    format!(
        "\u{1f6d1} stream-rules: a rule interrupted this turn {failures} times in a row, \
         reaching the allowed maximum ({max}); cancelling the stream."
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]

    use super::*;

    /// A fresh actor with the given maximum over a private fake `Services`
    /// (struct-direct tests assert on the returned actions).
    async fn watchdog(max_failures: u8) -> StreamRuleWatchdogActor {
        StreamRuleWatchdogActor::with_max_failures(Services::new_fake().await, max_failures)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_default_maximum_permits_four_interrupts_and_cancels_the_fifth() {
        // Given a watchdog at the default maximum of four.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When five responses in a row trip a rule.
        let mut actions = Vec::new();
        for _ in 0..5 {
            actions.extend(actor.on_response_end(&session, StreamCompletedReason::RuleIntercept));
        }

        // Then the fifth produced the trip pair and the first four produced
        // nothing: a budget of N permits N interrupts.
        assert_eq!(actions.len(), 2, "expected the trip pair, got: {actions:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fewer_than_the_maximum_push_nothing() {
        // Given a watchdog at the default maximum of four.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When four responses in a row trip a rule.
        let actions: Vec<_> = (0..4)
            .flat_map(|_| actor.on_response_end(&session, StreamCompletedReason::RuleIntercept))
            .collect();

        // Then nothing was produced.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_finished_response_debits_and_floors_at_zero() {
        // Given a watchdog that has seen two interrupts.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..2 {
            let _ = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);
        }

        // When four more responses finish cleanly.
        for _ in 0..4 {
            let _ = actor.on_response_end(&session, StreamCompletedReason::Finished);
        }

        // Then the count clamped at zero rather than wrapping: two further
        // interrupts leave the watchdog one short of its maximum.
        let actions: Vec<_> = (0..3)
            .flat_map(|_| actor.on_response_end(&session, StreamCompletedReason::RuleIntercept))
            .collect();
        assert!(actions.is_empty(), "the count must have floored at zero");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_finished_response_before_the_maximum_prevents_the_trip() {
        // Given a watchdog that saw three interrupts then one clean finish.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..3 {
            let _ = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);
        }
        let _ = actor.on_response_end(&session, StreamCompletedReason::Finished);

        // When two more interrupts arrive.
        let actions: Vec<_> = (0..2)
            .flat_map(|_| actor.on_response_end(&session, StreamCompletedReason::RuleIntercept))
            .collect();

        // Then the watchdog did not trip: only three interrupts are counted.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_trip_zeroes_the_count_rather_than_pinning_it() {
        // Given a watchdog that just tripped a session.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..3 {
            let _ = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);
        }

        // When the next response trips a rule again.
        let actions = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);

        // Then nothing was produced: the trip left the count at zero, so the
        // model's next response is judged afresh rather than cancelled before
        // it has produced anything.
        assert!(
            actions.is_empty(),
            "a latched session must not be cancelled again immediately"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_trip_publishes_its_marker_before_its_cancel() {
        // Given a watchdog that has just tripped.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let mut actions = Vec::new();
        for _ in 0..3 {
            actions.extend(actor.on_response_end(&session, StreamCompletedReason::RuleIntercept));
        }

        // Then the marker comes first and the cancel second, both for the
        // failing session: the user is told which rule looped before the
        // stream is torn down.
        assert_eq!(actions.len(), 2, "expected the trip pair, got: {actions:?}");
        let StreamRuleWatchdogAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the marker entry, got: {actions:?}");
        };
        assert_eq!(marker_session, &session);
        assert!(
            text.contains("stream-rules"),
            "the marker must name the watchdog that tripped, got: {text:?}"
        );
        let StreamRuleWatchdogAction::CancelTurn(cancel_session) = &actions[1] else {
            panic!("second action must be the cancel, got: {actions:?}");
        };
        assert_eq!(cancel_session, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_trip_names_the_maximum_it_reached() {
        // Given a watchdog whose maximum is 2.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;

        // When it trips.
        let mut actions = Vec::new();
        for _ in 0..3 {
            actions.extend(actor.on_response_end(&session, StreamCompletedReason::RuleIntercept));
        }
        let StreamRuleWatchdogAction::Marker(_, text) = &actions[0] else {
            panic!("the first action must be the marker, got: {actions:?}");
        };

        // Then the message states the threshold, so the user can see which
        // configured number was crossed.
        assert!(
            text.contains("2"),
            "the trip text must name the maximum, got: {text:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_response_ending_any_other_way_retains_the_count() {
        // Given a watchdog that has seen three interrupts.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..3 {
            let _ = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);
        }

        // When two responses end for a non-interrupt, non-finish reason, and
        // two more interrupts arrive.
        let _ = actor.on_response_end(&session, StreamCompletedReason::ToolUse);
        let _ = actor.on_response_end(&session, StreamCompletedReason::Error);
        let actions: Vec<_> = (0..2)
            .flat_map(|_| actor.on_response_end(&session, StreamCompletedReason::RuleIntercept))
            .collect();

        // Then the trip fired: the retained count carried across the response
        // boundaries, which is what makes consecutive interrupts accumulate.
        assert_eq!(
            actions.len(),
            2,
            "an interrupted response must not repay its own interrupt"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn accumulators_are_per_session() {
        // Given a watchdog that tripped session A (count latched to zero).
        let a = SessionId::new();
        let b = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..3 {
            let _ = actor.on_response_end(&a, StreamCompletedReason::RuleIntercept);
        }

        // When session B takes its own interrupts.
        let actions: Vec<_> = (0..2)
            .flat_map(|_| actor.on_response_end(&b, StreamCompletedReason::RuleIntercept))
            .collect();

        // Then B's trip did not fire off A's history: each session counts alone,
        // so B needed its own maximum worth of interrupts to trip.
        assert!(actions.is_empty());
        let actions = actor.on_response_end(&b, StreamCompletedReason::RuleIntercept);
        assert_eq!(actions.len(), 2, "B tripped on its own count");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_unknown_session_finishing_is_harmless() {
        // Given a watchdog with no state.
        let ghost = SessionId::new();
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When a response finishes for a session it never saw.
        let actions = actor.on_response_end(&ghost, StreamCompletedReason::Finished);

        // Then nothing happened and the watchdog still counts normally.
        assert!(actions.is_empty());
        let actions: Vec<_> = (0..4)
            .flat_map(|_| actor.on_response_end(&session, StreamCompletedReason::RuleIntercept))
            .collect();
        assert!(actions.is_empty());
        let actions = actor.on_response_end(&session, StreamCompletedReason::RuleIntercept);
        assert_eq!(actions.len(), 2);
    }
}
