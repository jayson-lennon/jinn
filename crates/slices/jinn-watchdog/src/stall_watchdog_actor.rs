//! Stall watchdog actor — restarts turns whose LLM stream went silent.
//!
//! Verbatim trouper port of the retired `stall-watchdog` component's state
//! machine: one timer per session, armed by [`SendToLlmProvider`] and
//! reset by every [`StreamActivity`]. When the actor's own [`StallTick`] reveals a session
//! has been silent past the configured timeout, the watchdog pushes the
//! visible retry marker ([`PushChatEntry`]) and re-dispatches the turn
//! ([`RetryStalledSession`]) — up to `max_restarts` consecutive times.
//! Beyond the budget it gives up instead: a surrender entry followed by
//! [`CancelTurn`].
//!
//! **Liveness is one contract, not a list.** The inference actor publishes
//! [`StreamActivity`] on *every* non-terminal provider event — text,
//! reasoning, tool-call construction, citations — and this actor subscribes
//! to that alone. An earlier port watched only [`StreamToken`], which made a
//! tool call being constructed (arguments streaming in for minutes, no text
//! at all) indistinguishable from a dead stream: the watchdog read minutes
//! of real progress as silence, tripped, and discarded the partial turn.
//! Owning the definition in the producer means a new provider event is
//! covered by construction rather than by remembering to add a
//! subscription.
//!
//! Supervision covers stream *construction*, never tool *execution*: the
//! stream ends in `ToolUse` before the tools run, and the watchdog disarms
//! there. A `bash` or a subagent may take as long as it needs.
//!
//! Budget semantics: the budget counts silent stalls *between completed
//! generations*. `Finished` and `ToolUse` both clear it — reaching either
//! proves a response ran to a natural end. `Canceled` and `Error` merely
//! disarm the timer while retaining the count (the turn did not complete, so
//! its stall history belongs to whatever comes next). Stream activity
//! resets the silence clock but **not** the budget: a provider that streams
//! tokens for a minute and then dies is precisely the fault this watchdog
//! exists to catch, and an earlier port reset on any activity, so such a
//! provider re-tripped forever at "attempt 1 of 3". The consequence,
//! accepted deliberately: a generation that stalls three times surrenders
//! even if each retry streamed for a while first.
//!
//! The tick is self-addressed ([`StallTick`], kicked by a detached task
//! after spawn and re-delivered after each processed tick — the
//! `SearchIndexActor` heartbeat pattern). It cannot live inside the
//! session actor: that actor's mailbox is the single sink for token
//! bursts, so an in-actor timer would queue behind the very activity it
//! is measuring. Liveness deliveries only touch recency here, so this
//! actor's own mailbox runs `DropNew` — backpressuring the inference
//! actor over *this* actor's slack would be the one failure mode a
//! watchdog must never cause; the newest delivery is the only fact that
//! matters and older ones carry no information. A dropped delivery merely
//! delays a trip by one more activity, never causes a false one, which is
//! why this is a documented property rather than a tested invariant.
//!
//! Elapsed time is measured from a monotonic [`std::time::Instant`]
//! captured at spawn, never against the wall clock: an NTP step or a
//! machine suspend would otherwise move the silence window in a direction
//! the watchdog cannot compensate for. The pure `on_*` seam takes the
//! timestamp as a parameter, so tests drive the state machine directly.
//!
//! Kernel dependency (see Cargo.toml): publishes through `Services`'
//! bus, granted at slice activation.

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::SendToLlmProvider;
use jinn_inference_msg::StreamActivity;
use jinn_inference_msg::StreamCompleted;
use jinn_inference_msg::StreamCompletedReason;
use jinn_inference_msg::{CancelCause, CancelTurn};
use jinn_kernel::Services;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::RetryStalledSession;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;

/// Production tick cadence. The stall window is seconds-scale, so a
/// 1-second heartbeat adds at most that much detection latency.
pub const STALL_TICK_INTERVAL: Duration = Duration::from_secs(1);

/// The stall watchdog actor's static trouper path.
pub const STALL_WATCHDOG_PATH: &str = "stall-watchdog";

/// Per-session stall timer and restart budget.
#[derive(Default)]
struct SessionStall {
    /// Whether an LLM stream is believed to be in flight.
    armed: bool,
    /// Elapsed-time timestamp of the last stream activity (or arm time),
    /// in milliseconds since spawn, from the monotonic clock.
    last_event_ms: u64,
    /// Silent stalls since the last completed generation (`Finished` or
    /// `ToolUse`). Stream activity does not clear it.
    restarts: u32,
}

/// Dependencies for [`StallWatchdogActor`].
#[derive(Clone)]
pub struct StallWatchdogActorDeps {
    /// Application-wide runtime services (bus publish).
    pub services: Services,
    /// Silence window before a restart, in milliseconds.
    pub timeout_ms: u64,
    /// Consecutive restarts allowed before giving up.
    pub max_restarts: u32,
    /// Tick cadence. Production uses [`STALL_TICK_INTERVAL`]; tests
    /// inject a small value.
    pub tick_interval: Duration,
}

/// The stall watchdog actor.
///
/// Event-driven: one timer per session, self-tick driven.
pub struct StallWatchdogActor {
    services: Services,
    /// The system this actor runs on — captured at spawn so the tick can
    /// self-address. (The services container may carry a different system
    /// in tests, where the harness spawns on its own.)
    system: ActorSystem,
    /// Monotonic base for [`Self::now_ms`], captured at spawn. Measuring
    /// elapsed time against a wall clock would mis-fire on an NTP step or a
    /// machine suspend; `Instant` counts real elapsed time through both.
    started_at: Instant,
    timeout_ms: u64,
    max_restarts: u32,
    tick_interval: Duration,
    /// Timers keyed by session id.
    sessions: HashMap<SessionId, SessionStall>,
}

impl ServiceActor for StallWatchdogActor {
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
                .attach("StallWatchdogActor is spawned via start_with"),
        )
    }
}

impl StallWatchdogActor {
    /// Milliseconds elapsed since spawn, from the monotonic system clock.
    ///
    /// Elapsed time measured against the wall clock would mis-fire in both
    /// directions: an NTP step forward reads as a stall, a step backward (or
    /// a machine suspend, on some clocks) reads as silence that never expires.
    /// `Instant` counts real elapsed time through both, and only differences
    /// are ever compared.
    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Spawns the actor at its static trouper path and returns the path.
    ///
    /// Subscriptions: the three stream contracts plus the self-addressed
    /// [`StallTick`]. The tick self-addresses through the same path, so
    /// the first tick is kicked by a detached task **after** this call
    /// resolves: the wiring moves on while the tick processes
    /// concurrently.
    ///
    /// The mailbox runs `DropNew` (see the module docs for why a
    /// watchdog must never backpressure its feed).
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into start_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: StallWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(STALL_WATCHDOG_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                let system = system.clone();
                move || {
                    let deps = deps.clone();
                    let system = system.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services: deps.services,
                            system,
                            started_at: Instant::now(),
                            timeout_ms: deps.timeout_ms,
                            max_restarts: deps.max_restarts,
                            tick_interval: deps.tick_interval,
                            sessions: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<SendToLlmProvider>()
            .handles::<StreamActivity>()
            .handles::<StreamCompleted>()
            // The turn-end signal. `StreamCompleted` is the *stream's* end and
            // is not published when a cancel drops a dispatch the session actor
            // had already armed a guard for; `TurnCompleted` is published by the
            // session actor's terminate routine on every terminal outcome. Without
            // it this actor keeps a timer for a session whose turn ended, and
            // re-triggers on the silence.
            .handles::<TurnCompleted>()
            .handles::<StallTick>()
            .mailbox(64, trouper::inbox::OverloadPolicy::DropNew)
            .start();
        // Kick the first tick. A failed send only means the actor is
        // already stopping.
        let kicker = system.clone();
        let kick_path = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let _ = kicker.tell(kick_path, StallTick).await;
        });
        path
    }

    /// Re-delivers one tick to this actor after `tick_interval`.
    ///
    /// The self-addressed tick keeps the "tick processes concurrently"
    /// semantics: the next tick is queued while the current one may
    /// still be running, and `DropNew` guarantees the mailbox never
    /// accumulates stale ticks.
    fn reschedule(&self) {
        let system = self.system.clone();
        let path = ActorPath::new(STALL_WATCHDOG_PATH);
        let interval = self.tick_interval;
        tokio::spawn(async move {
            tokio::time::sleep(interval).await;
            let _ = system.tell(path, StallTick).await;
        });
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    ///
    /// The bus broadcasts by schema; a command with no subscriber (or an
    /// event with none) is a silent no-op, so the watchdog stays correct
    /// regardless of what else is wired.
    async fn publish_actions(&self, actions: Vec<StallAction>) {
        for action in actions {
            match action {
                StallAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                StallAction::RetryStalledSession(command) => {
                    self.services.bus.publish(command).await;
                }
                StallAction::CancelTurn(session_id) => {
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

/// One watchdog output: a system-entry marker (text), the retry command,
/// or a cancel. Returned by [`StallWatchdogActor::on_tick`] in publish
/// order so tests can assert the pair sequencing without a bus.
#[derive(Debug)]
pub enum StallAction {
    Marker(SessionId, String),
    RetryStalledSession(RetryStalledSession),
    CancelTurn(SessionId),
}

/// The actor's self-addressed heartbeat: advances time and publishes
/// the resulting actions, then schedules the next tick.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, trouper::schema::Command)]
#[schema(description = "Stall watchdog heartbeat: trip silent armed sessions, then reschedule.")]
pub struct StallTick;

impl MsgHandler<StallTick> for StallWatchdogActor {
    async fn handle(&mut self, _msg: &StallTick, _ctx: &mut MsgCtx<'_>) {
        let actions = self.on_tick(self.now_ms());
        self.publish_actions(actions).await;
        self.reschedule();
    }
}

impl MsgHandler<SendToLlmProvider> for StallWatchdogActor {
    async fn handle(&mut self, msg: &SendToLlmProvider, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_start(&msg.session_id, self.now_ms());
    }
}

impl MsgHandler<StreamActivity> for StallWatchdogActor {
    async fn handle(&mut self, msg: &StreamActivity, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_event(&msg.session_id, self.now_ms());
    }
}

impl MsgHandler<StreamCompleted> for StallWatchdogActor {
    async fn handle(&mut self, msg: &StreamCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_stream_end(&msg.session_id, msg.reason);
    }
}

impl MsgHandler<TurnCompleted> for StallWatchdogActor {
    /// Stops monitoring a session whose turn ended, whatever ended it.
    ///
    /// Ignores a non-terminal outcome. `TurnCompleted` also carries
    /// [`TurnOutcome::RuleIntercepted`], which arrives every time a stream rule
    /// corrects the model — and the turn is then rewound and re-dispatched, not
    /// over. Disarming on it cleared the restart budget mid-turn, so a turn that
    /// genuinely stalled afterwards started again from attempt one and the stall
    /// budget could never be spent.
    ///
    /// `StreamCompleted` is the stream's end, and a cancel can end a turn with no
    /// stream to end: a watchdog trip drops the resume the session actor had
    /// already armed a guard for, so nothing downstream of that drop publishes a
    /// completion. The turn is over all the same, and this is the event that says
    /// so. Removing the session outright rather than disarming it is deliberate —
    /// a retained entry would re-arm on the next dispatch and a spent restart
    /// budget would make the next genuine stall look like the last one.
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl StallWatchdogActor {
    /// Stops monitoring a session entirely.
    ///
    /// Called on the turn-end event. Drops the timer *and* the accumulated
    /// restart budget, so a fresh turn starts from a clean slate rather than
    /// inheriting restarts spent by a turn that has ended.
    ///
    /// Takes the outcome rather than assuming the caller filtered, so the policy
    /// holds even if a future call site forgets. A stall watchdog that disarmed on
    /// an intercept cleared its restart budget mid-turn, so the budget could
    /// never be spent and the stall it exists to bound went unbounded.
    pub fn on_turn_end(&mut self, session_id: &SessionId, outcome: TurnOutcome) {
        if !outcome.is_terminal() {
            return;
        }
        if self.sessions.remove(session_id).is_some() {
            tracing::debug!(
                session_id = %session_id,
                "turn ended; stall watchdog disarmed and its budget cleared"
            );
        }
    }

    /// Arms (or re-arms) the session's timer at dispatch time.
    ///
    /// Arming at dispatch — not first token — covers the silent
    /// HTTP-handshake gap. A tool-loop turn produces one dispatch per
    /// generation, so each re-dispatch re-arms naturally. Consecutive stalls
    /// within one turn accumulate against the budget.
    ///
    /// The budget is deliberately *not* cleared here. A stall retry re-dispatches
    /// through a fresh `SendToLlmProvider`, which lands in this same method —
    /// so clearing the count on arm would restart every retry at attempt 1 and
    /// the budget could never exhaust. The budget is therefore cleared by the
    /// surrender path instead, where the turn is genuinely over.
    pub fn on_stream_start(&mut self, session_id: &SessionId, now_ms: u64) {
        let stall = self.sessions.entry(session_id.clone()).or_default();
        stall.armed = true;
        stall.last_event_ms = now_ms;
    }

    /// Records stream output — the timer resets, but the budget does not.
    ///
    /// Reached from [`StreamActivity`], so *any* non-terminal provider event
    /// counts, not only text. Activity proves the connection works, which is
    /// what the silence clock measures; it says nothing about whether the
    /// generation will finish. A stream that emits tokens for a minute and
    /// then dies is the failure mode this watchdog exists to catch, so the
    /// budget deliberately survives it — see [`Self::on_stream_end`] for the
    /// boundaries that do clear it.
    ///
    /// Activity for a session with no timer is harmless (the session may have
    /// been disarmed between publication and this delivery arriving).
    pub fn on_stream_event(&mut self, session_id: &SessionId, now_ms: u64) {
        if let Some(stall) = self.sessions.get_mut(session_id) {
            stall.last_event_ms = now_ms;
        }
    }

    /// Applies the stream-end policy per terminal reason.
    ///
    /// `Finished` removes the session entirely (budget reset — the turn
    /// completed genuinely). `ToolUse` disarms the timer and clears the
    /// budget, because reaching it proves this generation completed cleanly:
    /// the model finished its response and asked for tools. Every other
    /// reason disarms the timer while retaining the budget — `Canceled` and
    /// `Error` are endpoints of a turn that did not complete, and their
    /// stall history belongs to whatever re-dispatches next.
    ///
    /// A stall is a fault of one *generation*: the connection died
    /// mid-response. So the budget counts silent stalls between two completed
    /// generations rather than across a whole turn. A tool-heavy turn may
    /// stall once per generation and still never surrender; a single
    /// generation that dies three times will, which is the intended reading
    /// of "three retries".
    pub fn on_stream_end(&mut self, session_id: &SessionId, reason: StreamCompletedReason) {
        match reason {
            StreamCompletedReason::Finished => {
                self.sessions.remove(session_id);
            }
            StreamCompletedReason::ToolUse => {
                if let Some(stall) = self.sessions.get_mut(session_id) {
                    stall.armed = false;
                    stall.restarts = 0;
                }
            }
            // An intercept ends this generation but not the turn: the
            // session actor re-dispatches immediately, so the watchdog must
            // stand down rather than count the silence between them as a
            // stall. The resumed dispatch re-arms it.
            StreamCompletedReason::Canceled
            | StreamCompletedReason::Error
            | StreamCompletedReason::RuleIntercept => {
                if let Some(stall) = self.sessions.get_mut(session_id) {
                    stall.armed = false;
                }
            }
        }
    }

    /// Advances time and returns the actions to publish, in order.
    ///
    /// Every armed session silent past the timeout trips exactly once per
    /// window: within budget it yields one restart (and the window
    /// restarts from the tick); past budget it yields the give-up pair —
    /// system entry first, then cancel — and disarms so it cannot fire
    /// again until the next dispatch.
    #[must_use]
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<StallAction> {
        let timeout_ms = self.timeout_ms;
        let max_restarts = self.max_restarts;
        self.sessions
            .iter_mut()
            .filter(|(_, stall)| {
                stall.armed && now_ms.saturating_sub(stall.last_event_ms) >= timeout_ms
            })
            .flat_map(|(session, stall)| trip(session.clone(), stall, max_restarts, now_ms))
            .collect()
    }
}

/// Trips one stalled session: a restart within budget, otherwise the
/// give-up pair. Mutates the stall so the next tick cannot re-fire
/// early — a restart re-windows from the tick, the give-up disarms
/// entirely.
fn trip(
    session: SessionId,
    stall: &mut SessionStall,
    max_restarts: u32,
    now_ms: u64,
) -> Vec<StallAction> {
    if stall.restarts < max_restarts {
        stall.restarts += 1;
        stall.last_event_ms = now_ms;
        return vec![
            StallAction::Marker(session.clone(), retry_text(stall.restarts, max_restarts)),
            StallAction::RetryStalledSession(RetryStalledSession {
                session_id: session.clone(),
                attempt: stall.restarts,
                max_restarts,
            }),
        ];
    }
    stall.armed = false;
    // The turn is over and the stream is being cancelled. Drop the exhausted
    // budget so the session starts clean: the watchdog cancels the dispatch
    // it emits here, and that cancellation arrives as a `Canceled`
    // `StreamCompleted`, which only disarms — it cannot distinguish this
    // self-inflicted cancel from a user's ESC. Leaving `restarts` at its
    // ceiling would carry into the next turn the user starts, and every
    // subsequent stream would be cancelled on its first tick without ever
    // being given a stall window. A user who sends a new message after a
    // give-up is re-establishing control, not retrying the failed turn.
    stall.restarts = 0;
    vec![
        StallAction::Marker(session.clone(), give_up_text(max_restarts)),
        StallAction::CancelTurn(session),
    ]
}

/// The per-attempt retry marker pushed before each restart.
fn retry_text(attempt: u32, max: u32) -> String {
    format!("\u{21bb} LLM stream stalled, retrying (attempt {attempt} of {max})\u{2026}")
}

/// The surrender system-entry text after exhausting `max` restarts.
fn give_up_text(max: u32) -> String {
    format!(
        "\u{23f9} stall-watchdog: the LLM stream stalled {max} times without recovery; giving up — cancelling the turn."
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

    #[rstest::rstest]
    #[tokio::test]
    async fn an_intercept_outcome_does_not_disarm_the_watchdog() {
        // Given a watchdog armed for a session whose turn a rule corrected.
        let session = SessionId::new();
        let mut actor = watchdog(30, 2).await;
        actor.on_stream_start(&session, 0);

        // When the intercept's TurnCompleted(RuleIntercepted) is delivered — it
        // is published on every intercept, and the turn continues after it.
        actor.on_turn_end(&session, TurnOutcome::RuleIntercepted);

        // Then the timer survives and a later silence still trips. Disarming on
        // an intercept cleared the restart budget mid-turn, so the budget could
        // never be spent.
        let actions = actor.on_tick(60_000);
        assert!(!actions.is_empty(), "a corrected turn must stay monitored");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminal_turn_end_disarms_the_watchdog_and_clears_its_budget() {
        // Given a watchdog that has already spent one of its two restarts.
        let session = SessionId::new();
        let mut actor = watchdog(1, 2).await;
        actor.on_stream_start(&session, 0);
        let _ = actor.on_tick(2_000);

        // When the turn ends.
        actor.on_turn_end(&session, TurnOutcome::Canceled);

        // Then nothing fires on the silence that follows: the session ended, so
        // its silence is not a stall.
        let actions = actor.on_tick(60_000);
        assert!(
            actions.is_empty(),
            "an ended turn must not be restarted: {actions:?}"
        );
    }

    /// A fresh actor with the given window and budget, over a private
    /// fake `Services` (struct-direct tests never publish through the
    /// real fabric — they assert on `on_tick`'s returned actions).
    async fn watchdog(timeout_secs: u64, max_restarts: u32) -> StallWatchdogActor {
        let services = Services::new_fake().await;
        let system = services.trouper_system.clone();
        StallWatchdogActor {
            services,
            system,
            started_at: Instant::now(),
            timeout_ms: timeout_secs * 1_000,
            max_restarts,
            tick_interval: STALL_TICK_INTERVAL,
            sessions: HashMap::new(),
        }
    }

    /// Asserts the actions are exactly the retry pair (marker entry, then
    /// restart) for `session`.
    fn assert_restart(actions: &[StallAction], session: &SessionId, attempt: u32) {
        assert_eq!(
            actions.len(),
            2,
            "expected the retry pair, got: {actions:?}"
        );
        let StallAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the system entry, got: {actions:?}");
        };
        assert_eq!(marker_session, session);
        assert!(
            text.contains(&format!("attempt {attempt} of")),
            "retry marker must name the attempt, got: {text:?}"
        );
        let StallAction::RetryStalledSession(restart) = &actions[1] else {
            panic!("second action must be a RetryStalledSession, got: {actions:?}");
        };
        assert_eq!(&restart.session_id, session);
        // And the reported attempt matches the restart ordinal within the
        // stall lineage.
        assert_eq!(restart.attempt, attempt);
    }

    /// Asserts the actions are exactly the give-up pair (entry then cancel).
    fn assert_give_up(actions: &[StallAction], session: &SessionId) {
        assert_eq!(
            actions.len(),
            2,
            "expected the give-up pair, got: {actions:?}"
        );
        let StallAction::Marker(marker_session, text) = &actions[0] else {
            panic!("first action must be the system entry, got: {actions:?}");
        };
        assert_eq!(marker_session, session);
        assert!(
            text.contains("stall-watchdog:"),
            "surrender marker must carry the watchdog prefix, got: {text:?}"
        );
        let StallAction::CancelTurn(cancel_session) = &actions[1] else {
            panic!("second action must be a CancelTurn, got: {actions:?}");
        };
        assert_eq!(cancel_session, session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tick_inside_the_window_pushes_nothing() {
        // Given a watchdog armed for a session at t=0 with a 60s window.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);

        // When a tick arrives 59.9 seconds later.
        let actions = actor.on_tick(60_999);

        // Then nothing was produced — the stream is not yet silent long enough.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dispatch_to_first_token_gap_counts_as_stall_time() {
        // Given a watchdog armed by dispatch alone — no token ever arrived.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);

        // When a tick arrives past the window.
        let actions = actor.on_tick(61_000);

        // Then the session restarts — the silent handshake gap is covered.
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stream_event_resets_the_window() {
        // Given a stream that produced output 50 seconds into its window.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 51_000);

        // When a tick arrives 60 seconds after the original dispatch.
        let actions = actor.on_tick(61_000);

        // Then nothing was produced — the window runs from the last event.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn restart_rewindows_so_one_stall_trips_once() {
        // Given a stalled stream that tripped once at t=61s.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        assert_restart(&actor.on_tick(61_000), &session, 1);

        // When the next tick arrives 4 seconds later (1s cadence).
        let actions = actor.on_tick(65_000);

        // Then the same stall does not re-fire — the window restarted.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn consecutive_stall_windows_accumulate_budget() {
        // Given a stream that stalled and was restarted once.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        assert_restart(&actor.on_tick(61_000), &session, 1);
        // And the retry re-armed the session (the dispatch the session
        // actor emits in response).
        actor.on_stream_start(&session, 61_500);

        // When the retried stream also goes silent past the window.
        let actions = actor.on_tick(121_500);

        // Then a second restart fires — consecutive stalls count against
        // the budget.
        assert_restart(&actions, &session, 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn budget_exhaustion_gives_up_with_entry_then_cancel() {
        // Given a watchdog at its budget of 3 that already restarted 3 times.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        for window in 1..=3 {
            assert_restart(&actor.on_tick(window * 60_000), &session, window as u32);
        }

        // When the fourth window also goes silent.
        let actions = actor.on_tick(240_000);

        // Then the watchdog surrenders: the system entry first, then the
        // cancel.
        assert_give_up(&actions, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn give_up_fires_only_once() {
        // Given a watchdog that already surrendered for a session.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        for window in 1..=3 {
            let _ = actor.on_tick(window * 60_000);
        }
        let _ = actor.on_tick(240_000);

        // When more ticks arrive.
        let actions = actor.on_tick(300_000);

        // Then nothing is produced again — the session is disarmed.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_boundary_resets_the_budget() {
        // Given a watchdog at a budget of 2 that restarted once, then the
        // generation completed by ending in tool use.
        let session = SessionId::new();
        let mut actor = watchdog(60, 2).await;
        actor.on_stream_start(&session, 0);
        assert_restart(&actor.on_tick(60_000), &session, 1);
        actor.on_stream_end(&session, StreamCompletedReason::ToolUse);
        actor.on_stream_start(&session, 61_000);
        assert_restart(&actor.on_tick(121_000), &session, 1);

        // When that generation also completes in tool use and the next one
        // goes silent past the window.
        actor.on_stream_end(&session, StreamCompletedReason::ToolUse);
        actor.on_stream_start(&session, 122_000);
        let actions = actor.on_tick(182_000);

        // Then it is attempt 1 again — each completed generation is a clean
        // boundary, so a long tool-loop turn may stall once per generation
        // without ever surrendering.
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_end_resets_the_budget() {
        // Given a watchdog at a budget of 2 that restarted twice, then a
        // generation reached its tool-use end.
        let session = SessionId::new();
        let mut actor = watchdog(60, 2).await;
        actor.on_stream_start(&session, 0);
        assert_restart(&actor.on_tick(60_000), &session, 1);
        actor.on_stream_start(&session, 61_000);
        assert_restart(&actor.on_tick(121_000), &session, 2);
        actor.on_stream_end(&session, StreamCompletedReason::ToolUse);

        // When the generation after that boundary stalls past the window.
        actor.on_stream_start(&session, 122_000);
        let actions = actor.on_tick(182_000);

        // Then it is attempt 1 — reaching the tool-use end proves this
        // generation completed, which is what clears the budget.
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activity_between_stalls_does_not_clear_the_budget() {
        // Given a watchdog at a budget of 1 that restarted once and whose
        // retry then produced output before dying again.
        let session = SessionId::new();
        let mut actor = watchdog(60, 1).await;
        actor.on_stream_start(&session, 0);
        assert_restart(&actor.on_tick(60_000), &session, 1);
        actor.on_stream_start(&session, 61_000);
        actor.on_stream_event(&session, 62_000);

        // When that stream stalls again past the window.
        let actions = actor.on_tick(122_000);

        // Then the watchdog surrenders — output from a stream that is still
        // dying is not a completed generation, so the budget stands.
        assert_give_up(&actions, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activity_between_stalls_preserves_budget() {
        // Given a watchdog at a budget of 3 that restarted once, after which
        // the retried stream produced some output before going silent again.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        assert_restart(&actor.on_tick(60_000), &session, 1);
        actor.on_stream_start(&session, 61_000);
        actor.on_stream_event(&session, 62_000);

        // When the second generation also stalls past the window.
        let actions = actor.on_tick(122_000);

        // Then it is attempt 2, not a fresh attempt 1 — partial output proves
        // the connection worked, not that the generation finished.
        assert_restart(&actions, &session, 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stalls_separated_by_tokens_still_exhaust_the_budget() {
        // Given a watchdog at a budget of 3.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);

        // When three generations each stream output for half a window and
        // then go silent.
        for window in 1..=3u32 {
            actor.on_stream_start(&session, u64::from(window) * 60_000);
            actor.on_stream_event(&session, u64::from(window) * 60_000 + 30_000);
            assert_restart(
                &actor.on_tick(u64::from(window) * 60_000 + 90_000),
                &session,
                window,
            );
        }

        // And the fourth attempt also stalls.
        actor.on_stream_start(&session, 270_000);
        let actions = actor.on_tick(330_000);

        // Then the watchdog surrenders — tokens on a stream that keeps dying
        // never buy more attempts.
        assert_give_up(&actions, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn give_up_does_not_poison_a_later_dispatched_turn() {
        // Given a session that exhausted the budget and was given up on.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        // Each restart re-windows the clock from its own tick, so the give-up
        // lands one window after the third restart.
        assert_restart(&actor.on_tick(60_000), &session, 1);
        assert_restart(&actor.on_tick(120_000), &session, 2);
        assert_restart(&actor.on_tick(180_000), &session, 3);
        assert_give_up(&actor.on_tick(240_000), &session);

        // When the user sends a brand new message and the session dispatches a
        // fresh generation.
        actor.on_stream_start(&session, 600_000);

        // Then that new generation gets the full stall window before the
        // watchdog judges it — the exhausted budget belonged to the turn that
        // already surrendered.
        let actions = actor.on_tick(659_000);
        assert!(
            actions.is_empty(),
            "a newly dispatched turn must not be killed immediately; got: {actions:?}"
        );

        // And if it does stall, it is a first offense, so it is retried rather
        // than surrendered.
        assert_restart(&actor.on_tick(720_000), &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn finished_end_resets_the_budget() {
        // Given a watchdog at a budget of 2 that restarted once, then the
        // turn genuinely completed.
        let session = SessionId::new();
        let mut actor = watchdog(60, 2).await;
        actor.on_stream_start(&session, 0);
        assert_restart(&actor.on_tick(60_000), &session, 1);
        actor.on_stream_end(&session, StreamCompletedReason::Finished);

        // When a fresh turn stalls past the window.
        actor.on_stream_start(&session, 120_000);
        let actions = actor.on_tick(180_000);

        // Then it restarts again — the completion restored the full budget
        // (a give-up pair here would mean the budget carried over).
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn finished_end_removes_the_timer_entirely() {
        // Given a stream that ended in a genuine completion.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        actor.on_stream_end(&session, StreamCompletedReason::Finished);

        // When ticks arrive far into the future.
        let actions = actor.on_tick(600_000);

        // Then nothing is produced — a finished stream has no timer to trip.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn canceled_end_disarms_the_timer() {
        // Given a stream that was canceled mid-flight.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        actor.on_stream_end(&session, StreamCompletedReason::Canceled);

        // When ticks arrive past the window.
        let actions = actor.on_tick(120_000);

        // Then nothing is produced — a canceled turn must not be restarted.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn error_end_disarms_the_timer() {
        // Given a stream that failed mid-flight.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);
        actor.on_stream_end(&session, StreamCompletedReason::Error);

        // When ticks arrive past the window.
        let actions = actor.on_tick(120_000);

        // Then nothing is produced — an errored turn re-dispatches through
        // the request-retry path, not the stall watchdog.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_argument_delta_resets_the_silence_window() {
        // Given an armed session whose only stream activity is tool-call
        // argument deltas — no text token ever arrives.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        // And deltas stream in right up to the end of what would be a
        // several-minute argument payload.
        for delta_at in [30_000, 58_000, 90_000, 150_000, 200_000] {
            actor.on_stream_event(&session, delta_at);
        }

        // When a tick arrives 59 seconds after the last delta.
        let actions = actor.on_tick(259_000);

        // Then nothing was produced — minutes of tool-call construction are
        // forward progress, not silence.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn stall_midway_through_a_tool_call_trips() {
        // Given an armed session that stalled after streaming some tool-call
        // argument deltas.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 30_000);
        actor.on_stream_event(&session, 58_000);

        // When the payload stops mid-stream and the window elapses.
        let actions = actor.on_tick(118_000);

        // Then the session restarts — treating deltas as liveness must not
        // blunt detection of a stream that genuinely stopped.
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_start_resets_the_silence_window() {
        // Given an armed session whose first and only stream event is a tool
        // use starting.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 59_000);

        // When a tick arrives 60 seconds after the original dispatch.
        let actions = actor.on_tick(61_000);

        // Then nothing was produced — the tool use starting is liveness.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_complete_resets_the_silence_window() {
        // Given an armed session whose final construction event is a tool
        // call completing.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 59_500);

        // When a tick arrives 60 seconds after the original dispatch.
        let actions = actor.on_tick(61_000);

        // Then nothing was produced — the completion is liveness.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn citation_only_stream_does_not_trip() {
        // Given an armed session whose only stream activity is citations —
        // a stream that emits no text at all.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 59_000);

        // When a tick arrives 60 seconds after the original dispatch.
        let actions = actor.on_tick(61_000);

        // Then nothing was produced.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn tool_use_completion_disarms_before_the_tool_runs() {
        // Given a stream that ended in tool use, disarming the watchdog
        // before the tool batch executes.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 1_000);
        actor.on_stream_event(&session, 1_500);
        actor.on_stream_end(&session, StreamCompletedReason::ToolUse);

        // When ticks arrive hours later — a subagent or a long `bash` that
        // produces no stream events whatsoever.
        let after_an_hour = actor.on_tick(3_601_500);
        let after_four_hours = actor.on_tick(14_401_500);

        // Then nothing is produced at an hour: tool *execution* is not the
        // stream watchdog's concern.
        assert!(after_an_hour.is_empty());
        // And nothing is produced after four hours either — the watchdog must
        // not restart a turn whose subagent is legitimately still running.
        assert!(after_four_hours.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unmonitored_session_still_trips_after_full_budget() {
        // Given a session that never produced any stream event at all.
        let session = SessionId::new();
        let mut actor = watchdog(60, 2).await;
        actor.on_stream_start(&session, 0);

        // When each window elapses with no activity, re-arming as a retry
        // would between them.
        let first = actor.on_tick(60_000);
        actor.on_stream_start(&session, 61_000);
        let second = actor.on_tick(121_000);
        actor.on_stream_start(&session, 122_000);
        let third = actor.on_tick(182_000);

        // Then the first two windows restart.
        assert_restart(&first, &session, 1);
        // And the third, having exhausted the budget, surrenders — the new
        // liveness source did not weaken detection or extend the budget.
        assert_restart(&second, &session, 2);
        assert_give_up(&third, &session);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn batched_tool_calls_stay_monitored_until_the_last_one_completes() {
        // Given a turn streaming two tool calls back to back under one
        // dispatch — parallel tool calls arrive as a single stream, with
        // `index` distinguishing the content blocks.
        let session = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&session, 0);

        // And the first call completes.
        actor.on_stream_event(&session, 5_000);
        // But the second call's deltas then stop mid-payload.
        actor.on_stream_event(&session, 10_000);

        // When the window elapses.
        let actions = actor.on_tick(70_000);

        // Then the session restarts — the watchdog has one timer per session
        // and is not disarmed by the first call's completion, so a second
        // call that stalls is still caught.
        assert_restart(&actions, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sessions_are_tracked_independently() {
        // Given two sessions: one stalled, one actively streaming.
        let stalled = SessionId::new();
        let streaming = SessionId::new();
        let mut actor = watchdog(60, 3).await;
        actor.on_stream_start(&stalled, 0);
        actor.on_stream_start(&streaming, 0);
        actor.on_stream_event(&streaming, 55_000);

        // When a tick arrives past the window.
        let actions = actor.on_tick(61_000);

        // Then only the silent session tripped — the streaming one stays quiet.
        assert_restart(&actions, &stalled, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_session_events_are_harmless() {
        // Given a watchdog with no state for a session.
        let ghost = SessionId::new();
        let mut actor = watchdog(60, 3).await;

        // When tokens and stream ends arrive for that unknown session.
        actor.on_stream_event(&ghost, 0);
        actor.on_stream_end(&ghost, StreamCompletedReason::Error);

        // Then nothing panics and an unrelated session still trips normally.
        let known = SessionId::new();
        actor.on_stream_start(&known, 0);
        let actions = actor.on_tick(60_000);
        assert_restart(&actions, &known, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn configured_window_overrides_the_default() {
        // Given a watchdog configured with a 5-second window.
        let session = SessionId::new();
        let mut actor = watchdog(5, 3).await;
        actor.on_stream_start(&session, 0);

        // When ticks arrive at 4.9s and then 5s.
        let early = actor.on_tick(4_999);
        let on_time = actor.on_tick(5_000);

        // Then the trip lands exactly at the configured window, where the
        // 60s default would still be silent.
        assert!(early.is_empty());
        assert_restart(&on_time, &session, 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn configured_budget_of_one_gives_up_after_the_first_restart() {
        // Given a watchdog configured with a budget of 1.
        let session = SessionId::new();
        let mut actor = watchdog(1, 1).await;
        actor.on_stream_start(&session, 0);

        // When two stall windows pass.
        let first = actor.on_tick(1_000);
        actor.on_stream_start(&session, 1_500);
        let second = actor.on_tick(2_500);

        // Then the first window restarts and the second gives up — the
        // default budget of 3 would still have restarts left.
        assert_restart(&first, &session, 1);
        assert_give_up(&second, &session);
    }
}
