//! Reasoning-loop watchdog actor — surrenders turns stuck in a
//! repetition spiral.
//!
//! A trouper actor over the accumulator the tool-call watchdog established,
//! keyed on a different fault: a
//! [`StreamCompleted`] carrying [`StreamCompletedReason::RepetitionLoop`]
//! (the inference actor's repetition detector flagged the output) increments
//! a saturating per-session counter, a clean response (`Finished` or
//! `ToolUse`) debits it by one (floor at zero), and reaching the configured
//! maximum trips the watchdog — the actor pushes the surrender marker
//! ([`PushChatEntry`]) followed by [`CancelTurn`], then zeroes the counter
//! so the same session is not re-killed immediately. `Canceled`, `Error`,
//! and `RuleIntercept` are deliberately ignored: they are endpoints that
//! say nothing about repetition, and the reasoning-loop budget is spent
//! across the turn's retry lineage, not per response.
//!
//! A turn ended by a `LoopRetried` outcome does **not** clear the counter —
//! [`TurnOutcome::is_terminal`] excludes it, so a loop that retries twice
//! and surrenders is one budget, not three. A terminal turn end clears it.
//!
//! The markers are visible system entries; system entries are excluded
//! from LLM context by default, so the user sees what happened and the
//! model never reads loop guidance. This actor holds no `AppState` — its
//! counter is actor-internal and its only outputs are bus publishes (the
//! deliberate, sanctioned shape of the watchdog family slice). Kernel
//! dependency (see Cargo.toml): publishes through `Services`' bus,
//! granted at slice activation.

use std::collections::HashMap;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::CancelTurn;
use jinn_inference_msg::StreamCompleted;
use jinn_inference_msg::StreamCompletedReason;
use jinn_kernel::Services;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;

/// The reasoning-loop watchdog actor's static trouper path.
pub const REASONING_LOOP_WATCHDOG_PATH: &str = "reasoning-loop-watchdog";

/// One watchdog output: the marker (text) or a cancel. Returned by
/// [`ReasoningLoopWatchdogActor::on_stream_end`] in publish order so tests
/// can assert the pair sequencing without a bus.
#[derive(Debug)]
pub enum LoopWatchdogAction {
    Marker(SessionId, String),
    CancelTurn(SessionId),
}

/// Dependencies for [`ReasoningLoopWatchdogActor`].
#[derive(Clone)]
pub struct ReasoningLoopWatchdogActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
    /// Detected loops tolerated before the watchdog surrenders the turn.
    pub max_failures: u8,
}

/// The reasoning-loop watchdog actor.
///
/// Per-session loop counter; trip = surrender marker entry + cancel.
pub struct ReasoningLoopWatchdogActor {
    services: Services,
    max_failures: u8,
    /// Loop counters keyed by session id.
    accumulators: HashMap<SessionId, u32>,
}

impl ServiceActor for ReasoningLoopWatchdogActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`
        // (Services carries typed handles that cannot ride JSON args).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ReasoningLoopWatchdogActor is spawned via init_with"),
        )
    }
}

impl ReasoningLoopWatchdogActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: ReasoningLoopWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(REASONING_LOOP_WATCHDOG_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
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
            // The turn-end signal. A cancel can end a turn with no stream
            // completion to hear about, so `StreamCompleted` alone leaves
            // this counter armed for a session that is no longer running.
            .handles::<TurnCompleted>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    async fn publish_actions(&self, actions: Vec<LoopWatchdogAction>) {
        for action in actions {
            match action {
                LoopWatchdogAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                LoopWatchdogAction::CancelTurn(session_id) => {
                    self.services
                        .bus
                        .publish(CancelTurn {
                            session_id,
                            // A watchdog stop is not ESC-ESC: buffered work
                            // re-dispatches after the settle.
                            drain_buffers_to_input: false,
                        })
                        .await;
                }
            }
        }
    }
}

impl MsgHandler<StreamCompleted> for ReasoningLoopWatchdogActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        let actions = self.on_stream_end(&msg.session_id, msg.reason);
        self.publish_actions(actions).await;
    }
}

impl MsgHandler<TurnCompleted> for ReasoningLoopWatchdogActor {
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl ReasoningLoopWatchdogActor {
    /// Builds an actor with an explicit maximum (test seam).
    #[must_use]
    pub fn with_max_failures(services: Services, max_failures: u8) -> Self {
        Self {
            services,
            max_failures,
            accumulators: HashMap::new(),
        }
    }

    /// Records one stream completion and returns the actions to publish,
    /// in order.
    ///
    /// `Finished` and `ToolUse` debit the session's counter (never below
    /// zero) — the model completed a response without looping, so one
    /// detected loop is forgiven. `RepetitionLoop` increments it, and
    /// hitting the maximum produces the watchdog pair — the surrender
    /// entry first, then the cancel — and zeroes the counter. `Canceled`,
    /// `Error`, and `RuleIntercept` do nothing: they are endpoints of a
    /// response that neither looped nor completed, and the budget exists
    /// to bound retry lineage, which a cancel or an error ends outright.
    #[must_use]
    pub fn on_stream_end(
        &mut self,
        session_id: &SessionId,
        reason: StreamCompletedReason,
    ) -> Vec<LoopWatchdogAction> {
        match reason {
            StreamCompletedReason::RepetitionLoop => {
                let count = self.accumulators.get(session_id).copied().unwrap_or(0) + 1;
                if count < u32::from(self.max_failures) {
                    self.accumulators.insert(session_id.clone(), count);
                    return vec![LoopWatchdogAction::Marker(
                        session_id.clone(),
                        retry_text(count, self.max_failures),
                    )];
                }
                // Latch: zero the counter so the same session is not
                // re-killed immediately.
                self.accumulators.insert(session_id.clone(), 0);
                vec![
                    LoopWatchdogAction::Marker(
                        session_id.clone(),
                        surrender_text(self.max_failures),
                    ),
                    LoopWatchdogAction::CancelTurn(session_id.clone()),
                ]
            }
            StreamCompletedReason::Finished | StreamCompletedReason::ToolUse => {
                if let Some(count) = self.accumulators.get_mut(session_id) {
                    *count = count.saturating_sub(1);
                }
                Vec::new()
            }
            StreamCompletedReason::Canceled
            | StreamCompletedReason::Error
            | StreamCompletedReason::RuleIntercept => Vec::new(),
        }
    }

    /// Clears the session's counter when its turn ends.
    ///
    /// Takes the outcome rather than assuming the caller filtered: the
    /// policy holds even if a future call site forgets. `LoopRetried` is
    /// non-terminal, so a retried generation keeps its lineage — the count
    /// that tripped the retry is what the surrender needs to reach.
    pub fn on_turn_end(&mut self, session_id: &SessionId, outcome: TurnOutcome) {
        if !outcome.is_terminal() {
            return;
        }
        self.accumulators.remove(session_id);
    }
}

/// The per-detection retry marker pushed under budget.
fn retry_text(attempt: u32, max: u8) -> String {
    format!(
        "\u{21bb} response stuck in a repetition loop, retrying (attempt {attempt} of {max})\u{2026}"
    )
}

/// The surrender system-entry text after exhausting `max` detections.
fn surrender_text(max: u8) -> String {
    format!(
        "\u{23f8} reasoning-loop-watchdog: the response repeated itself {max} times without recovery; giving up — cancelling the turn."
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

    /// A fresh actor with the given maximum over a private fake
    /// `Services` (struct-direct tests assert on the returned actions).
    async fn watchdog(max_failures: u8) -> ReasoningLoopWatchdogActor {
        ReasoningLoopWatchdogActor::with_max_failures(Services::new_fake().await, max_failures)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn detections_under_budget_push_a_retry_marker_each() {
        // Given a watchdog with a budget of 3.
        let session = SessionId::new();
        let mut actor = watchdog(3).await;

        // When two loop completions arrive.
        let first = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        let second = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then each pushed exactly one retry marker naming its attempt,
        // and no cancel.
        assert_eq!(first.len(), 1, "one marker per detection under budget");
        let LoopWatchdogAction::Marker(_, text) = &first[0] else {
            panic!("a marker is expected, got: {first:?}");
        };
        assert!(text.contains("1 of 3"), "first marker: {text:?}");
        let LoopWatchdogAction::Marker(_, text) = &second[0] else {
            panic!("a marker is expected, got: {second:?}");
        };
        assert!(text.contains("2 of 3"), "second marker: {text:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_final_detection_surrenders_with_entry_then_cancel() {
        // Given a watchdog with a budget of 3 that already saw two loops.
        let session = SessionId::new();
        let mut actor = watchdog(3).await;
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // When the third loop completion arrives.
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then exactly two actions were produced — the surrender entry
        // first, then the cancel — both for the looping session.
        assert_eq!(actions.len(), 2, "expected the surrender pair");
        let LoopWatchdogAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the surrender entry, got: {actions:?}");
        };
        assert_eq!(marker_session, &session);
        assert!(
            text.contains("3 times"),
            "surrender text must name the budget, got: {text:?}"
        );
        let LoopWatchdogAction::CancelTurn(cancel_session) = &actions[1] else {
            panic!("second action must be the cancel, got: {actions:?}");
        };
        assert_eq!(cancel_session, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_clean_response_debits_the_counter() {
        // Given a watchdog that saw one loop.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // When a clean response completes, then two loops follow.
        let _ = actor.on_stream_end(&session, StreamCompletedReason::Finished);
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then only the second loop surrenders: the debit repaid the first
        // detection, so the budget was spent once, not twice.
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        assert_eq!(actions.len(), 2, "the repaid budget reached its max again");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn debits_floor_at_zero() {
        // Given a watchdog with no history.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;

        // When two clean responses complete.
        let _ = actor.on_stream_end(&session, StreamCompletedReason::Finished);
        let _ = actor.on_stream_end(&session, StreamCompletedReason::ToolUse);

        // Then the counter is still at zero: one loop trips a retry, not a
        // surrender.
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        assert_eq!(actions.len(), 1, "floor held; one loop is one marker");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn non_loop_completions_are_ignored() {
        // Given a watchdog with a budget of 2.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;

        // When cancel, error, and intercept completions arrive.
        let canceled = actor.on_stream_end(&session, StreamCompletedReason::Canceled);
        let errored = actor.on_stream_end(&session, StreamCompletedReason::Error);
        let intercepted = actor.on_stream_end(&session, StreamCompletedReason::RuleIntercept);

        // Then nothing was produced.
        assert!(canceled.is_empty() && errored.is_empty() && intercepted.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminal_turn_end_clears_the_counter() {
        // Given a watchdog holding two loops for a session.
        let session = SessionId::new();
        let mut actor = watchdog(3).await;
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // When the turn ends terminally and a fresh turn loops once.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then it does not surrender: the ended turn's count was cleared.
        assert_eq!(actions.len(), 1, "a fresh turn starts a fresh budget");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_loop_retried_turn_end_keeps_the_counter() {
        // Given a watchdog holding two loops for a session.
        let session = SessionId::new();
        let mut actor = watchdog(3).await;
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // When the turn reports its retry (non-terminal) and a third loop
        // arrives.
        actor.on_turn_end(&session, TurnOutcome::LoopRetried);
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then it surrenders: the retry lineage is one budget, and this is
        // its third loop.
        assert_eq!(actions.len(), 2, "the retry lineage kept its count");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_surrender_latches_so_the_session_is_not_re_killed() {
        // Given a watchdog that surrendered on the third loop.
        let session = SessionId::new();
        let mut actor = watchdog(3).await;
        for _ in 0..3 {
            let _ = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);
        }

        // When the latch's turn end is reported and one more loop arrives.
        actor.on_turn_end(&session, TurnOutcome::Canceled);
        let actions = actor.on_stream_end(&session, StreamCompletedReason::RepetitionLoop);

        // Then nothing beyond a single marker: a count of one is not three.
        assert_eq!(actions.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn counters_are_per_session() {
        // Given a watchdog that surrendered for session A.
        let a = SessionId::new();
        let b = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..2 {
            let _ = actor.on_stream_end(&a, StreamCompletedReason::RepetitionLoop);
        }

        // When session B loops once.
        let actions = actor.on_stream_end(&b, StreamCompletedReason::RepetitionLoop);

        // Then B's marker is its own first attempt, not a surrender off
        // A's history.
        let LoopWatchdogAction::Marker(_, text) = &actions[0] else {
            panic!("a marker is expected, got: {actions:?}");
        };
        assert!(text.contains("1 of 2"), "marker: {text:?}");
    }
}
