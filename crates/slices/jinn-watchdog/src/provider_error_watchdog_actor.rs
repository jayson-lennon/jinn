//! Provider-error watchdog actor — retries turns whose provider stream failed.
//!
//! When the inference actor stamps a `StreamCompleted { reason: Error,
//! retryable: true }` — a transient provider failure, classified at the
//! failure site — the session actor defers the turn's settle and this actor
//! schedules the retry: after `waits_secs[attempt]` seconds it publishes the
//! visible retry marker (`PushChatEntry`) and re-dispatches through the
//! stall-retry path (`RetryStalledSession`). A failure past the list
//! surrenders to the ordinary error settle (surrender marker +
//! [`GiveUpProviderRetries`]).
//!
//! **Why a watchdog and not the request-retry decorator.** The decorator
//! wraps stream *construction* only; a mid-stream provider error surfaces as
//! an error item on the stream it already returned, with partial output
//! published to the chat log. The exclusion surgery that keeps the failed
//! attempt visible-but-excluded lives in the session actor, above
//! `jinn-provider`'s layer, so the retry must live there too.
//!
//! **Reuses the stall-retry shape verbatim.** The stall path already carries
//! what a failed attempt needs: it excludes the failed attempt from the
//! retried request while keeping it visible, rewinds, and re-dispatches
//! joining the live turn. A generation that failed mid-tool-call needs
//! exactly that surgery; the continue keybind's fresh-turn path performs
//! none of it. `attempt` carries the provider-error lineage's count and
//! `max_restarts` the schedule length, so the session actor's ledger
//! admission treats the two watchdogs identically.
//!
//! **Budget semantics mirror the stall watchdog's:** consecutive failures
//! *between completed generations*. `Finished` and `ToolUse` both restore
//! the schedule — reaching either proves a response ran to a natural end.
//! `Canceled`/`Error`-non-retryable disarm nothing here: the session actor
//! settles those turns itself, and [`TurnCompleted`] with a terminal outcome
//! is the authoritative end signal that drops the session's lineage. A
//! `RuleIntercepted` turn end is not terminal — the resumed dispatch re-arms
//! via `SendToLlmProvider` without resetting the spent count, the stall
//! watchdog's budget precedent.
//!
//! **Transport and tick** are the stall watchdog's, for the same reasons:
//! the tick is self-addressed and the mailbox runs `DropNew` (a watchdog
//! must never backpressure its feed), elapsed time is measured from a
//! monotonic [`std::time::Instant`], and the pure `on_*` seam takes the
//! timestamp as a parameter so tests drive the state machine directly.
//!
//! Kernel dependency (see Cargo.toml): publishes through `Services`' bus,
//! granted at slice activation.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::SendToLlmProvider;
use jinn_inference_msg::StreamCompleted;
use jinn_kernel::Services;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::GiveUpProviderRetries;
use jinn_session_msg::RetryStalledSession;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;

/// Production tick cadence. The shortest configured wait is seconds-scale,
/// so a 1-second heartbeat adds at most that much detection latency.
pub const PROVIDER_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// The provider-error watchdog actor's static trouper path.
pub const PROVIDER_ERROR_WATCHDOG_PATH: &str = "provider-error-watchdog";

/// Per-session retry schedule: the pending wait and the failures spent.
#[derive(Default)]
struct SessionRetry {
    /// Monotonic timestamp (ms since spawn) when the pending wait fires.
    /// `None` while no wait is scheduled — e.g. between the error event and
    /// nothing, since scheduling is synchronous with it; `None` after the
    /// wait fired and before the retried generation's verdict arrives.
    deadline_ms: Option<u64>,
    /// Retryable provider failures since the last completed generation
    /// (`Finished` or `ToolUse`). A re-arm dispatch does not clear it.
    retries_issued: u32,
}

/// Dependencies for [`ProviderErrorWatchdogActor`].
#[derive(Clone)]
pub struct ProviderErrorWatchdogActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
    /// Escalating waits before each successive retry. A failure past the
    /// list surrenders; an empty list disables the watchdog.
    pub waits: Vec<Duration>,
    /// Tick cadence. Production uses [`PROVIDER_TICK_INTERVAL`]; tests
    /// inject a small value.
    pub tick_interval: Duration,
}

/// The provider-error watchdog actor.
///
/// Event-driven: one schedule per session, self-tick driven.
pub struct ProviderErrorWatchdogActor {
    services: Services,
    /// Monotonic base for [`Self::now_ms`], captured at spawn. Elapsed time
    /// measured against the wall clock would mis-fire on an NTP step or a
    /// machine suspend; `Instant` counts real elapsed time through both.
    started_at: Instant,
    waits_ms: Vec<u64>,
    tick_interval: Duration,
    /// Schedules keyed by session id.
    sessions: HashMap<SessionId, SessionRetry>,
}

impl ServiceActor for ProviderErrorWatchdogActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`
        // (Services carries typed handles that cannot ride JSON args).
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ProviderErrorWatchdogActor is spawned via init_with"),
        )
    }
}

impl ProviderErrorWatchdogActor {
    /// Milliseconds elapsed since spawn, from the monotonic system clock.
    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Spawns the actor at its static trouper path and returns the path.
    ///
    /// Composition skips this spawn entirely when the configured wait list
    /// is empty; the actor additionally no-ops on one (defense in depth),
    /// so an `enabled`-style gate never lives inside the actor.
    ///
    /// Subscriptions: the dispatch marker that re-arms a retried generation,
    /// the completion carrying the retry verdict, the turn-end signal, and
    /// the self-addressed tick.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: ProviderErrorWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(PROVIDER_ERROR_WATCHDOG_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            waits_ms: deps.waits.iter().map(|d| d.as_millis() as u64).collect(),
                            services: deps.services,
                            started_at: Instant::now(),
                            tick_interval: deps.tick_interval,
                            sessions: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<SendToLlmProvider>()
            .handles::<StreamCompleted>()
            // The turn-end signal, for the same reason the stall watchdog
            // subscribes: a cancel can end a turn with no stream completion
            // to hear, and a session whose turn ended must not keep a live
            // schedule whose retry would fire into the void.
            .handles::<TurnCompleted>()
            .handles::<ProviderErrorTick>()
            .mailbox(64, trouper::inbox::OverloadPolicy::DropNew)
            // Arms the first tick ~1ms after spawn; the chain
            // self-perpetuates from there (see `reschedule`).
            .kickoff(ProviderErrorTick, Duration::from_millis(1))
            .start();
        path
    }

    /// Re-delivers one tick to this actor after `tick_interval`.
    fn reschedule(&self, ctx: &mut MsgCtx<'_>) {
        ctx.schedule_self(ProviderErrorTick, self.tick_interval);
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    async fn publish_actions(&self, actions: Vec<ProviderErrorAction>) {
        for action in actions {
            match action {
                ProviderErrorAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                ProviderErrorAction::RetryStalledSession(command) => {
                    self.services.bus.publish(command).await;
                }
                ProviderErrorAction::GiveUp(command) => {
                    self.services.bus.publish(command).await;
                }
            }
        }
    }
}

/// One watchdog output: a system-entry marker (text), the stall-retry
/// re-dispatch, or the give-up command. Returned by [`ProviderErrorWatchdogActor::on_tick`]
/// in publish order so tests can assert the sequencing without a bus.
#[derive(Debug)]
pub enum ProviderErrorAction {
    Marker(SessionId, String),
    RetryStalledSession(RetryStalledSession),
    GiveUp(GiveUpProviderRetries),
}

/// The actor's self-addressed heartbeat: fires due waits and publishes the
/// resulting actions, then schedules the next tick.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Provider-error watchdog heartbeat: fire due waits, then reschedule.")]
pub struct ProviderErrorTick;

impl MsgHandler<ProviderErrorTick> for ProviderErrorWatchdogActor {
    async fn handle(&mut self, _msg: &ProviderErrorTick, ctx: &mut MsgCtx<'_>) {
        let actions = self.on_tick(self.now_ms());
        self.publish_actions(actions).await;
        self.reschedule(ctx);
    }
}

impl MsgHandler<SendToLlmProvider> for ProviderErrorWatchdogActor {
    /// Re-arms the lineage when a generation dispatches.
    ///
    /// A retry re-dispatches through a fresh `SendToLlmProvider`, which
    /// lands here: the pending wait is cleared (its job — to re-dispatch —
    /// is done) while `retries_issued` deliberately survives, the stall
    /// watchdog's budget precedent. Clearing the count on arm would restart
    /// every retry at attempt 1 and the schedule could never exhaust.
    async fn handle(&mut self, msg: &SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_start(&msg.session_id);
    }
}

impl MsgHandler<StreamCompleted> for ProviderErrorWatchdogActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        let actions =
            self.on_stream_completed(&msg.session_id, msg.reason, msg.retryable, self.now_ms());
        self.publish_actions(actions).await;
    }
}

impl MsgHandler<TurnCompleted> for ProviderErrorWatchdogActor {
    /// Stops monitoring a session whose turn ended, whatever ended it.
    ///
    /// Ignores a non-terminal outcome for the same reason the stall watchdog
    /// does: `TurnOutcome::RuleIntercepted` arrives on every stream-rule
    /// correction and the turn then rewinds and re-dispatches, not over.
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl ProviderErrorWatchdogActor {
    /// Drops the session's lineage when its turn ends terminally.
    ///
    /// A fresh turn starts from a clean schedule rather than inheriting
    /// failures spent by a turn that has ended.
    pub fn on_turn_end(&mut self, session_id: &SessionId, outcome: TurnOutcome) {
        if !outcome.is_terminal() {
            return;
        }
        if self.sessions.remove(session_id).is_some() {
            tracing::debug!(
                session_id = %session_id,
                "turn ended; provider-error watchdog dropped the session's schedule"
            );
        }
    }

    /// Clears the pending wait on dispatch, keeping the spent count.
    pub fn on_stream_start(&mut self, session_id: &SessionId) {
        if let Some(retry) = self.sessions.get_mut(session_id) {
            retry.deadline_ms = None;
        }
    }

    /// Applies the completion policy and returns the actions to publish.
    ///
    /// A retryable failure within the schedule schedules the next wait and
    /// pushes the visible marker; at exhaustion it gives up (marker +
    /// [`GiveUpProviderRetries`]) and removes the lineage. A completed
    /// generation (`Finished`/`ToolUse`) restores the schedule by removing
    /// the lineage entirely. Everything else — a cancel, a deterministic
    /// error, an intercept — schedules nothing and touches nothing: the
    /// session actor settles those turns today-style and `TurnCompleted`
    /// cleans up.
    #[must_use]
    pub fn on_stream_completed(
        &mut self,
        session_id: &SessionId,
        reason: jinn_inference_msg::StreamCompletedReason,
        retryable: bool,
        now_ms: u64,
    ) -> Vec<ProviderErrorAction> {
        let Some(waits_ms) = self.waits_ms.non_empty() else {
            return Vec::new();
        };
        match (reason, retryable) {
            (jinn_inference_msg::StreamCompletedReason::Error, true) => {
                let max = waits_ms.len() as u32;
                let retry = self.sessions.entry(session_id.clone()).or_default();
                let attempt = retry.retries_issued;
                // `attempt < max` bounds the index above: `attempt` only
                // ever grows through the branch below, one step per
                // retryable error.
                if let Some(&wait_ms) = waits_ms.get(attempt as usize) {
                    retry.retries_issued += 1;
                    retry.deadline_ms = Some(now_ms + wait_ms);
                    vec![
                        ProviderErrorAction::Marker(
                            session_id.clone(),
                            retry_text(attempt + 1, max),
                        ),
                        ProviderErrorAction::Marker(session_id.clone(), wait_text(wait_ms)),
                    ]
                } else {
                    self.sessions.remove(session_id);
                    vec![
                        ProviderErrorAction::Marker(session_id.clone(), give_up_text(max)),
                        ProviderErrorAction::GiveUp(GiveUpProviderRetries {
                            session_id: session_id.clone(),
                        }),
                    ]
                }
            }
            (
                jinn_inference_msg::StreamCompletedReason::Finished
                | jinn_inference_msg::StreamCompletedReason::ToolUse,
                _,
            ) => {
                self.sessions.remove(session_id);
                Vec::new()
            }
            // Canceled, non-retryable Error, RuleIntercept: not this
            // watchdog's verdict to act on.
            _ => Vec::new(),
        }
    }

    /// Advances time and returns the actions to publish, in order.
    ///
    /// Every session whose wait has elapsed fires exactly once: the retry
    /// action carries the spent count as `attempt` (one-based for display
    /// upstream is the marker's job; the command's `attempt` is the lineage
    /// count the stall-retry ledger admission expects) and the deadline
    /// clears so the wait cannot fire twice.
    #[must_use]
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<ProviderErrorAction> {
        let due: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, retry)| retry.deadline_ms.is_some_and(|d| d <= now_ms))
            .map(|(session, _)| session.clone())
            .collect();
        let mut actions = Vec::new();
        for session in due {
            let Some(retry) = self.sessions.get_mut(&session) else {
                continue;
            };
            let Some(attempt) = retry.retries_issued.checked_sub(1) else {
                continue;
            };
            let Some(waits_ms) = self.waits_ms.non_empty() else {
                continue;
            };
            let Some(&_wait_ms) = waits_ms.get(attempt as usize) else {
                continue;
            };
            retry.deadline_ms = None;
            actions.push(ProviderErrorAction::RetryStalledSession(
                RetryStalledSession {
                    session_id: session,
                    attempt: attempt + 1,
                    max_restarts: waits_ms.len() as u32,
                },
            ));
        }
        actions
    }
}

/// The per-attempt retry marker pushed when a failure is scheduled for retry.
fn retry_text(attempt: u32, max: u32) -> String {
    format!(
        "\u{23f8} LLM stream failed (provider error), retrying (attempt {attempt} of {max})\u{2026}"
    )
}

/// The wait-length marker pushed beside the retry marker.
fn wait_text(wait_ms: u64) -> String {
    format!(
        "\u{23f8} waiting {}s before the retry\u{2026}",
        wait_ms / 1_000
    )
}

/// The surrender system-entry text after exhausting the schedule.
fn give_up_text(max: u32) -> String {
    format!(
        "\u{23f8} provider-error-watchdog: the provider errored {} times without recovery; giving up.",
        max + 1
    )
}

/// `Option::is_some_and`-shaped helper: returns `Some(slice)` when the slice
/// is non-empty. Guards the empty-list disable inside the actor itself.
trait NonEmpty {
    type Item;
    fn non_empty(&self) -> Option<&[Self::Item]>;
}

impl<T> NonEmpty for Vec<T> {
    type Item = T;
    fn non_empty(&self) -> Option<&[Self::Item]> {
        if self.is_empty() { None } else { Some(self) }
    }
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
    use jinn_inference_msg::StreamCompletedReason;

    /// Escalation helper: durations from seconds.
    fn waits(secs: &[u64]) -> Vec<Duration> {
        secs.iter().map(|s| Duration::from_secs(*s)).collect()
    }

    /// A fresh actor with the given waits, over a private fake `Services`
    /// (struct-direct tests never publish through the real fabric — they
    /// assert on the `on_*` methods' returned actions).
    async fn watchdog(waits_secs: &[u64]) -> ProviderErrorWatchdogActor {
        let services = Services::new_fake().await;
        ProviderErrorWatchdogActor {
            services,
            started_at: Instant::now(),
            waits_ms: waits(waits_secs)
                .iter()
                .map(Duration::as_millis)
                .map(|m| m as u64)
                .collect(),
            tick_interval: PROVIDER_TICK_INTERVAL,
            sessions: HashMap::new(),
        }
    }

    fn error_completed() -> (jinn_inference_msg::StreamCompletedReason, bool) {
        (StreamCompletedReason::Error, true)
    }

    /// Asserts the actions are exactly the schedule pair: the retry marker
    /// naming `attempt`, then the wait marker.
    fn assert_scheduled(actions: &[ProviderErrorAction], session: &SessionId, attempt: u32) {
        assert_eq!(
            actions.len(),
            2,
            "expected the schedule pair (retry + wait markers), got: {actions:?}"
        );
        let ProviderErrorAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the retry marker, got: {actions:?}");
        };
        assert_eq!(marker_session, session);
        assert!(
            text.contains(&format!("attempt {attempt} of")),
            "retry marker must name the attempt, got: {text:?}"
        );
    }

    /// Asserts the actions are exactly the give-up pair: the surrender
    /// marker, then the give-up command.
    fn assert_give_up(actions: &[ProviderErrorAction], session: &SessionId) {
        assert_eq!(
            actions.len(),
            2,
            "expected the give-up pair, got: {actions:?}"
        );
        let ProviderErrorAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the surrender marker, got: {actions:?}");
        };
        assert_eq!(marker_session, session);
        assert!(
            text.contains("provider-error-watchdog:"),
            "surrender marker must carry the watchdog prefix, got: {text:?}"
        );
        let ProviderErrorAction::GiveUp(command) = &actions[1] else {
            panic!("second action must be a GiveUpProviderRetries, got: {actions:?}");
        };
        assert_eq!(&command.session_id, session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_first_retryable_failure_schedules_the_first_wait() {
        // Given a watchdog with the default schedule.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;

        // When the first retryable failure arrives.
        let (reason, retryable) = error_completed();
        let actions = actor.on_stream_completed(&session, reason, retryable, 1_000);

        // Then the schedule pair is pushed, naming attempt 1 of 3.
        assert_scheduled(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn consecutive_failures_escalate_through_the_waits_list() {
        // Given a watchdog whose lineage already failed once (10s scheduled).
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When the retried generation fails again.
        let actions = actor.on_stream_completed(&session, reason, retryable, 10_000);

        // Then the second wait is scheduled — attempt 2 of 3.
        assert_scheduled(&actions, &session, 2);

        // And the third failure schedules the last wait.
        let actions = actor.on_stream_completed(&session, reason, retryable, 40_000);
        assert_scheduled(&actions, &session, 3);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failure_past_the_schedule_gives_up_once() {
        // Given a lineage that already spent all three waits.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        for now in [0, 10_000, 40_000] {
            let _ = actor.on_stream_completed(&session, reason, retryable, now);
        }

        // When the fourth failure arrives.
        let actions = actor.on_stream_completed(&session, reason, retryable, 100_000);

        // Then the watchdog surrenders with the marker + give-up pair.
        assert_give_up(&actions, &session);

        // And a further failure schedules nothing — the lineage is gone.
        let actions = actor.on_stream_completed(&session, reason, retryable, 200_000);
        // (a fresh lineage starts, so this is a first-failure schedule — the
        // surrender removed the spent count)
        assert_scheduled(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_completed_generation_restores_the_schedule() {
        // Given a lineage that failed once and was scheduled.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When the retried generation completes genuinely.
        let actions =
            actor.on_stream_completed(&session, StreamCompletedReason::Finished, false, 10_000);

        // Then nothing is published.
        assert!(actions.is_empty());

        // And a later failure starts from attempt 1 again — the completion
        // removed the spent count.
        let actions = actor.on_stream_completed(&session, reason, retryable, 20_000);
        assert_scheduled(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_tool_use_completion_restores_the_schedule_too() {
        // Given a lineage that failed once.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When the retried generation ends in tool use.
        let actions =
            actor.on_stream_completed(&session, StreamCompletedReason::ToolUse, false, 10_000);

        // Then a later failure starts from attempt 1 again.
        assert!(actions.is_empty());
        let actions = actor.on_stream_completed(&session, reason, retryable, 20_000);
        assert_scheduled(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminal_turn_end_drops_the_lineage_and_its_pending_wait() {
        // Given a lineage with a wait pending.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When the user cancels the turn while the wait is pending.
        actor.on_turn_end(&session, TurnOutcome::Canceled);

        // Then the due wait fires nothing.
        let actions = actor.on_tick(60_000);
        assert!(
            actions.is_empty(),
            "a cancelled turn's pending wait must die with the lineage: {actions:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercepted_turn_end_keeps_the_lineage() {
        // Given a lineage that failed once (count spent).
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When a stream rule corrects the model mid-turn.
        actor.on_turn_end(&session, TurnOutcome::RuleIntercepted);

        // Then the spent count survives: the next failure is attempt 2, not a
        // fresh attempt 1 — the intercept ended a generation, not the turn.
        let actions = actor.on_stream_completed(&session, reason, retryable, 10_000);
        assert_scheduled(&actions, &session, 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_non_retryable_error_schedules_nothing() {
        // Given a watchdog for a session whose provider failed deterministically.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;

        // When the non-retryable completion arrives.
        let actions = actor.on_stream_completed(&session, StreamCompletedReason::Error, false, 0);

        // Then nothing is scheduled and no lineage exists.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_canceled_completion_schedules_nothing() {
        // Given a watchdog for a session the user cancelled mid-flight.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;

        // When the cancel completion arrives.
        let actions =
            actor.on_stream_completed(&session, StreamCompletedReason::Canceled, false, 0);

        // Then nothing is scheduled.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_waits_disable_the_watchdog() {
        // Given a watchdog configured with an empty wait list.
        let session = SessionId::new();
        let mut actor = watchdog(&[]).await;

        // When a retryable failure arrives.
        let actions = actor.on_stream_completed(&session, StreamCompletedReason::Error, true, 0);

        // Then nothing is scheduled — the disable signal.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_due_wait_fires_exactly_one_retry() {
        // Given a lineage whose 10-second wait was scheduled at t=0.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When ticks arrive before, at, and after the deadline.
        let early = actor.on_tick(9_999);
        let on_time = actor.on_tick(10_000);
        let late = actor.on_tick(11_000);

        // Then only the deadline tick fires, exactly once.
        assert!(early.is_empty(), "an early tick must not fire: {early:?}");
        assert_eq!(on_time.len(), 1, "the deadline tick fires the retry");
        assert!(late.is_empty(), "a late tick must not re-fire: {late:?}");
        let ProviderErrorAction::RetryStalledSession(command) = &on_time[0] else {
            panic!("expected a RetryStalledSession, got: {on_time:?}");
        };
        assert_eq!(command.session_id, session);
        assert_eq!(command.attempt, 1);
        assert_eq!(command.max_restarts, 3);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_rearm_dispatch_does_not_reset_the_spent_count() {
        // Given a lineage that failed once.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When the retried dispatch re-arms the lineage.
        actor.on_stream_start(&session);

        // Then the next failure is attempt 2 — the dispatch cleared the wait,
        // not the count.
        let actions = actor.on_stream_completed(&session, reason, retryable, 10_000);
        assert_scheduled(&actions, &session, 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_rearm_dispatch_clears_a_pending_wait() {
        // Given a lineage with a wait pending.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&session, reason, retryable, 0);

        // When a dispatch arrives before the wait elapses (e.g. the stall
        // watchdog or the user re-dispatched first).
        actor.on_stream_start(&session);

        // Then the pending wait fires nothing.
        let actions = actor.on_tick(60_000);
        assert!(
            actions.is_empty(),
            "the consumed wait must not fire: {actions:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn surrender_does_not_poison_a_later_turn() {
        // Given a lineage that exhausted the schedule and surrendered.
        let session = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        for now in [0, 10_000, 40_000, 100_000] {
            let _ = actor.on_stream_completed(&session, reason, retryable, now);
        }

        // When the user sends a fresh turn and it fails again.
        actor.on_turn_end(&session, TurnOutcome::Error);
        let actions = actor.on_stream_completed(&session, reason, retryable, 200_000);

        // Then the fresh turn gets the full schedule — attempt 1, not the
        // exhausted lineage's give-up.
        assert_scheduled(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_are_tracked_independently() {
        // Given two sessions: one failed retryably, one finished.
        let failing = SessionId::new();
        let fine = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;
        let (reason, retryable) = error_completed();
        let _ = actor.on_stream_completed(&failing, reason, retryable, 0);
        let _ = actor.on_stream_completed(&fine, StreamCompletedReason::Finished, false, 0);

        // When a tick arrives past the failing session's wait.
        let actions = actor.on_tick(10_000);

        // Then only the failing session's retry fired.
        assert_eq!(actions.len(), 1);
        let ProviderErrorAction::RetryStalledSession(command) = &actions[0] else {
            panic!("expected a RetryStalledSession, got: {actions:?}");
        };
        assert_eq!(command.session_id, failing);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_session_completions_are_harmless() {
        // Given a watchdog with no state for a session.
        let ghost = SessionId::new();
        let mut actor = watchdog(&[10, 30, 60]).await;

        // When a Finished arrives for that unknown session.
        let actions = actor.on_stream_completed(&ghost, StreamCompletedReason::Finished, false, 0);

        // Then nothing panics and nothing is produced.
        assert!(actions.is_empty());
    }
}
