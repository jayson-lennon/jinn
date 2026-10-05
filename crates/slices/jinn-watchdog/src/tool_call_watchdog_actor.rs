//! Tool-call watchdog actor — cancels turns stuck in a tool-failure spiral.
//!
//! Verbatim trouper port of the retired `tool-call-watchdog` component's
//! accumulator: one saturating counter per session. A failed
//! [`ToolExecutionCompleted`]
//! increments it, a successful one debits it by one (floor at zero), and
//! reaching the configured maximum trips the watchdog — the actor pushes
//! the trip marker ([`PushChatEntry`]) followed by
//! [`CancelTurn`], then resets the counter so the same session is not
//! re-killed immediately. A turn that ends in a genuine final answer
//! ([`StreamCompleted`] with `Finished`) resets the counter (recovery
//! latch); a turn ended by error/cancel retains it.
//!
//! This actor holds no `AppState` — its counter is actor-internal and its
//! only outputs are bus publishes (the deliberate, sanctioned shape of the
//! watchdog family slice). Kernel dependency (see Cargo.toml): publishes
//! through `Services`' bus, granted at slice activation.

use std::collections::HashMap;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::{CancelCause, CancelTurn};
use jinn_kernel::Services;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_tools_msg::ToolExecutionCompleted;

/// The tool-call watchdog actor's static trouper path.
pub const TOOL_CALL_WATCHDOG_PATH: &str = "tool-call-watchdog";

/// One watchdog output: the trip marker (text) or a cancel. Returned by
/// [`ToolCallWatchdogActor::on_tool_result`] in publish order so tests
/// can assert the pair sequencing without a bus.
#[derive(Debug)]
pub enum ToolWatchdogAction {
    Marker(SessionId, String),
    CancelTurn(SessionId),
}

/// Dependencies for [`ToolCallWatchdogActor`].
#[derive(Clone)]
pub struct ToolCallWatchdogActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
    /// Consecutive failures tolerated before tripping.
    pub max_failures: u8,
}

/// The tool-call watchdog actor.
///
/// Per-session failure accumulator; trip = marker entry + cancel.
pub struct ToolCallWatchdogActor {
    services: Services,
    max_failures: u8,
    /// Failure counters keyed by session id.
    accumulators: HashMap<SessionId, u32>,
}

impl ServiceActor for ToolCallWatchdogActor {
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
                .attach("ToolCallWatchdogActor is spawned via start_with"),
        )
    }
}

impl ToolCallWatchdogActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: ToolCallWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(TOOL_CALL_WATCHDOG_PATH);
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
            .handles::<ToolExecutionCompleted>()
            // The turn-end signal. A cancel can end a turn with no stream
            // completion to hear about, so `StreamCompleted` alone leaves this
            // counter armed for a session that is no longer running.
            .handles::<TurnCompleted>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    async fn publish_actions(&self, actions: Vec<ToolWatchdogAction>) {
        for action in actions {
            match action {
                ToolWatchdogAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                ToolWatchdogAction::CancelTurn(session_id) => {
                    self.services
                        .bus
                        .publish(CancelTurn {
                            session_id,
                            cause: CancelCause::Turn,
                        })
                        .await;
                }
            }
        }
    }
}

impl MsgHandler<ToolExecutionCompleted> for ToolCallWatchdogActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        let actions = self.on_tool_result(&msg.session_id, msg.result.success);
        self.publish_actions(actions).await;
    }
}

impl MsgHandler<TurnCompleted> for ToolCallWatchdogActor {
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl ToolCallWatchdogActor {
    /// Builds an actor with an explicit maximum (test seam).
    #[must_use]
    pub fn with_max_failures(services: Services, max_failures: u8) -> Self {
        Self {
            services,
            max_failures,
            accumulators: HashMap::new(),
        }
    }

    /// Records one tool result and returns the actions to publish, in
    /// order.
    ///
    /// Success debits the session's counter (never below zero); failure
    /// increments it, and hitting the maximum produces the watchdog pair —
    /// the system entry first, then the cancel — and zeroes the counter.
    #[must_use]
    pub fn on_tool_result(
        &mut self,
        session_id: &SessionId,
        success: bool,
    ) -> Vec<ToolWatchdogAction> {
        let session = session_id.clone();
        if success {
            if let Some(count) = self.accumulators.get_mut(&session) {
                *count = count.saturating_sub(1);
            }
            return Vec::new();
        }
        let count = self.accumulators.get(&session).copied().unwrap_or(0) + 1;
        if count < u32::from(self.max_failures) {
            self.accumulators.insert(session, count);
            return Vec::new();
        }
        // Latch: zero the counter so the same session is not re-killed
        // immediately.
        self.accumulators.insert(session.clone(), 0);
        vec![
            ToolWatchdogAction::Marker(session.clone(), trip_text(self.max_failures, count)),
            ToolWatchdogAction::CancelTurn(session),
        ]
    }

    /// Clears the session's counter when its turn ends.
    ///
    /// The previous policy keyed this on `StreamCompleted(Finished)` alone, so
    /// a counter survived every cancelled and errored turn. A watchdog cancel
    /// reports its own end and then must not carry the count into the next turn,
    /// or a session that tripped once is one failure away from tripping again
    /// on a fresh, unrelated turn.
    ///
    /// Takes the outcome rather than assuming the caller filtered, so the policy
    /// holds even if a future call site forgets.
    pub fn on_turn_end(&mut self, session_id: &SessionId, outcome: TurnOutcome) {
        if !outcome.is_terminal() {
            return;
        }
        self.accumulators.remove(session_id);
    }
}

/// The watchdog system-entry text for a trip after `failures`
/// consecutive failures with a `max` limit.
fn trip_text(max: u8, failures: u32) -> String {
    format!(
        "\u{1f6d1} tool-call-watchdog: {failures} consecutive tool failures reached the allowed maximum ({max}); cancelling the stream."
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
    async fn watchdog(max_failures: u8) -> ToolCallWatchdogActor {
        ToolCallWatchdogActor::with_max_failures(Services::new_fake().await, max_failures)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn four_consecutive_failures_send_entry_then_cancel() {
        // Given a watchdog with the default maximum of 4.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When four tool results fail in a row.
        let mut actions = Vec::new();
        for call in 1..=4 {
            actions.extend(actor.on_tool_result(&session, false));
            let _ = call;
        }

        // Then exactly two actions were produced — the marker first, then
        // the cancel — both for the failing session.
        assert_eq!(actions.len(), 2, "expected the trip pair, got: {actions:?}");
        let ToolWatchdogAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the marker entry, got: {actions:?}");
        };
        assert_eq!(marker_session, &session);
        assert!(
            text.contains('4'),
            "trip text must name the failure count, got: {text:?}"
        );
        let ToolWatchdogAction::CancelTurn(cancel_session) = &actions[1] else {
            panic!("second action must be the cancel, got: {actions:?}");
        };
        assert_eq!(cancel_session, &session);

        // And the accumulator was zeroed: the next lone failure produces
        // nothing (the latch prevents an immediate re-kill).
        assert!(actor.on_tool_result(&session, false).is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fewer_than_max_failures_push_nothing() {
        // Given a watchdog with the default maximum of 4.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When three failures arrive.
        let actions: Vec<_> = (0..3)
            .flat_map(|_| actor.on_tool_result(&session, false))
            .collect();

        // Then nothing was produced.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn successes_debit_and_floor_at_zero() {
        // Given a watchdog that has seen two failures.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        let _ = actor.on_tool_result(&session, false);
        let _ = actor.on_tool_result(&session, false);

        // When three successes follow.
        for _ in 0..3 {
            let _ = actor.on_tool_result(&session, true);
        }

        // Then the counter debited to zero and floored there: the next two
        // failures alone cannot trip it.
        let actions: Vec<_> = (0..2)
            .flat_map(|_| actor.on_tool_result(&session, false))
            .collect();
        assert!(actions.is_empty(), "successes must have debited to 0");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn successes_before_reaching_max_prevent_the_trip() {
        // Given a watchdog that saw three failures then one success.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..3 {
            let _ = actor.on_tool_result(&session, false);
        }
        let _ = actor.on_tool_result(&session, true);

        // When a fourth failure arrives.
        let actions = actor.on_tool_result(&session, false);

        // Then the watchdog did not trip — only three failures are counted.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn accumulator_is_per_session() {
        // Given a watchdog that tripped for session A (counter reset to 0)
        // and session B untouched.
        let a = SessionId::new();
        let b = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_tool_result(&a, false);
        let _ = actor.on_tool_result(&a, false);

        // When session B fails once.
        let actions = actor.on_tool_result(&b, false);

        // Then B's trip did not fire off A's history: each session counts
        // alone (B needs its own second failure).
        assert!(actions.is_empty());
        let actions = actor.on_tool_result(&b, false);
        assert_eq!(actions.len(), 2, "B tripped on its own count");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn configured_max_below_default_trips_earlier() {
        // Given a watchdog configured with a maximum of 2.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;

        // When two failures arrive.
        let _ = actor.on_tool_result(&session, false);
        let actions = actor.on_tool_result(&session, false);

        // Then it tripped at 2, per the config — where the default of 4
        // would still be one failure short.
        assert_eq!(actions.len(), 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_turn_end_resets_the_accumulator() {
        // Given a watchdog holding three failures for a session.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..3 {
            let _ = actor.on_tool_result(&session, false);
        }

        // When that turn ends and a new turn logs one more failure.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let actions = actor.on_tool_result(&session, false);

        // Then the watchdog did not trip: the ended turn's count was cleared.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_cancelled_turn_end_resets_the_accumulator() {
        // Given a watchdog holding three failures for a session.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..3 {
            let _ = actor.on_tool_result(&session, false);
        }

        // When the turn ends because the watchdog itself cancelled it, and the
        // next turn logs one more failure.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let actions = actor.on_tool_result(&session, false);

        // Then it does not trip. Retaining the count across a turn that ended
        // is what made a session that tripped once one failure away from
        // tripping again on an unrelated turn.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_trip_zeros_the_accumulator_so_the_next_turn_starts_clean() {
        // Given a watchdog that trips on the fourth failure.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;
        for _ in 0..4 {
            let _ = actor.on_tool_result(&session, false);
        }

        // When the trip's own turn end is reported and the next turn logs one
        // failure.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let actions = actor.on_tool_result(&session, false);

        // Then it does not trip: a count of one is not four.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_session_turn_end_is_harmless() {
        // Given a watchdog with no state.
        let ghost = SessionId::new();
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When a turn ends for a session it never saw.
        actor.on_turn_end(&ghost, TurnOutcome::Succeeded);

        // Then nothing happens and the watchdog still trips normally later.
        for call in 1..=4 {
            let actions = actor.on_tool_result(&session, false);
            if call == 4 {
                assert_eq!(actions.len(), 2);
            } else {
                assert!(actions.is_empty());
            }
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_session_success_is_harmless() {
        // Given a watchdog with no state.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When a success arrives for a session it never saw.
        let actions = actor.on_tool_result(&session, true);

        // Then nothing was produced and nothing tripped afterwards.
        assert!(actions.is_empty());
        for call in 1..=4 {
            let actions = actor.on_tool_result(&session, false);
            if call == 4 {
                assert_eq!(actions.len(), 2);
            } else {
                assert!(actions.is_empty());
            }
        }
    }
}
