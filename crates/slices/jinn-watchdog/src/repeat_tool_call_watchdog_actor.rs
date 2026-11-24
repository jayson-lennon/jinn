//! Repeat-tool-call watchdog actor — nudges then cancels a turn stuck
//! re-issuing the same tool call.
//!
//! A trouper [`ServiceActor`] in the watchdog family, keyed on a fault
//! the stream-buffer detector cannot see: a model that re-issues the
//! *same* tool call across responses — same tool, byte-identical
//! arguments after whitespace normalization, byte-identical result on the
//! compared prefix — and gets nowhere. Every individual response is
//! well-formed; only the sequence is degenerate.
//!
//! Two bus events feed one accumulator per session. The arguments arrive
//! at [`ToolCallsDispatched`] (the orchestrator's batch-accept
//! observation point) and are parked per call id; the verdict arrives at
//! [`ToolExecutionCompleted`] with the result. A completion whose id has
//! no parked call is ignored — a synthesized cancel-drain completion, or
//! one from a session the watchdog is not tracking.
//!
//! The accumulator policy is the family's: a repeat increments the
//! session's count; a differing call (or a different tool name than the
//! previous call) debits it, floor at zero — so the repeats need not be
//! strictly consecutive. Reaching the configured maximum trips the
//! watchdog, and the trip is two-staged:
//!
//! 1. **First trip in a turn**: publish a `RuleInterrupt` guidance entry
//!    (the model must read why it was stopped — RECORD line 709's
//!    silent-retry reasoning applies to the *stream* detector, but a
//!    cross-call loop spans many successful responses the model
//!    believes were fine, so telling it is the only intervention that
//!    can work) and reset the counter. The turn continues.
//! 2. **Second trip in the same turn**: visible system marker +
//!    [`CancelTurn`]. The turn settles, the session reaches `Idle`, and
//!    the terminal [`TurnCompleted`] clears the nudged flag and the
//!    counter.
//!
//! The two stages are separated by a delivery race: the watchdog and the
//! session actor each consume `ToolExecutionCompleted` in their own
//! mailbox, so the session actor may dispatch the next model request
//! before the nudge entry lands in history. The kill therefore checks
//! the nudge's presence first, by scanning history for the
//! `RuleInterrupt` entry whose rule names this watchdog (the push does
//! not hand back its assigned entry id). If it is not there yet, the trip publishes nothing and
//! leaves the counter at the maximum: the next repeat re-trips and
//! re-checks. **No re-push, no timer** — a trip that cannot verify the
//! nudge simply waits for evidence, and a session that stops repeating
//! never reaches the kill.
//!
//! A terminal turn end clears everything; a non-terminal one
//! (`LoopRetried`, `RuleIntercepted`) keeps the lineage, matching the
//! sibling watchdogs.
//!
//! This actor holds no `AppState` writes — the nudge-visibility read is
//! the sanctioned exception: it receives a read-only [`State`] via its
//! deps and only ever calls `read()`. All other state is actor-internal;
//! outputs are bus publishes.

use std::collections::HashMap;

use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use jinn_core_types::SessionId;
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::state::State;
use jinn_kernel::Services;
use jinn_preferences_config::schemas::RepeatToolCallWatchdogConfig;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_tools_msg::ToolCallsDispatched;
use jinn_tools_msg::ToolExecutionCompleted;

use crate::tool_call_similarity;

/// The repeat-tool-call watchdog actor's static trouper path.
pub const REPEAT_TOOL_CALL_WATCHDOG_PATH: &str = "repeat-tool-call-watchdog";

/// The rule name stamped on the nudge entry and matched against history.
pub(crate) const REPEAT_RULE_NAME: &str = "repeat-tool-call-watchdog";

/// One watchdog output, in publish order: the nudge, the kill marker, or
/// the cancel. Returned by the decision functions so tests can assert
/// sequencing without a bus.
#[derive(Debug)]
pub(crate) enum RepeatWatchdogAction {
    /// The once-per-turn guidance entry, addressed to the model.
    Nudge(SessionId, String),
    /// The visible system entry naming the kill.
    Marker(SessionId, String),
    /// The cancel that ends the turn.
    CancelTurn(SessionId),
}

/// Dependencies for [`RepeatToolCallWatchdogActor`].
#[derive(Clone)]
pub struct RepeatToolCallWatchdogActorDeps {
    /// Application-wide runtime services (bus publish, live config).
    pub services: Services,
    /// Read-only application state: the nudge-visibility check. The
    /// watchdog family's only read handle — this actor never writes.
    pub state: State,
}

/// The repeat-tool-call watchdog actor.
///
/// Per-session accumulator, nudged flag, and parked call arguments.
pub struct RepeatToolCallWatchdogActor {
    services: Services,
    state: State,
    /// Repeat counts keyed by session id.
    accumulators: HashMap<SessionId, u32>,
    /// Sessions that already received this turn's nudge.
    nudged: HashMap<SessionId, bool>,
    /// Dispatched-but-uncompleted calls: call id → (tool name,
    /// normalized arguments). One entry per in-flight call; cleared on
    /// its completion.
    parked: HashMap<jinn_core_types::ToolCallId, (String, String)>,
    /// The previous completed call per session: (tool name, normalized
    /// arguments, normalized result prefix) — the comparison basis.
    previous: HashMap<SessionId, (String, String, String)>,
}

impl ServiceActor for RepeatToolCallWatchdogActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("RepeatToolCallWatchdogActor is spawned via init_with"),
        )
    }
}

impl RepeatToolCallWatchdogActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: RepeatToolCallWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(REPEAT_TOOL_CALL_WATCHDOG_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            services: deps.services,
                            state: deps.state,
                            accumulators: HashMap::new(),
                            nudged: HashMap::new(),
                            parked: HashMap::new(),
                            previous: HashMap::new(),
                        })
                    })
                }
            })
            .handles::<ToolCallsDispatched>()
            .handles::<ToolExecutionCompleted>()
            .handles::<TurnCompleted>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Builds an actor with a private fake `Services` and the given
    /// maximum (test seam, the family's shape).
    #[cfg(test)]
    async fn with_max_repeats(max_failures: u8) -> Self {
        let services = Services::new_fake().await;
        services
            .config
            .put(&RepeatToolCallWatchdogConfig {
                max_repeats: max_failures,
            })
            .expect("the fake layer accepts the seed");
        Self {
            services,
            state: State::new(jinn_kernel::common::app_state::AppState::default()),
            accumulators: HashMap::new(),
            nudged: HashMap::new(),
            parked: HashMap::new(),
            previous: HashMap::new(),
        }
    }

    /// Publishes the watchdog's actions on the fabric, in order.
    async fn publish_actions(&self, actions: Vec<RepeatWatchdogAction>) {
        for action in actions {
            match action {
                RepeatWatchdogAction::Nudge(session_id, body) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::rule_interrupt(
                                REPEAT_RULE_NAME,
                                body,
                            ),
                            pin: None,
                        })
                        .await;
                }
                RepeatWatchdogAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                RepeatWatchdogAction::CancelTurn(session_id) => {
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

impl MsgHandler<TurnCompleted> for RepeatToolCallWatchdogActor {
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl MsgHandler<ToolCallsDispatched> for RepeatToolCallWatchdogActor {
    async fn handle(&mut self, msg: &ToolCallsDispatched, _ctx: &mut MsgCtx<'_>) {
        for call in &msg.tool_calls {
            self.parked.insert(
                call.id.clone(),
                (call.name.clone(), tool_call_similarity::normalize(&call.arguments)),
            );
        }
    }
}

impl MsgHandler<ToolExecutionCompleted> for RepeatToolCallWatchdogActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        let Some((name, args)) = self.parked.remove(&msg.result.tool_call_id) else {
            return;
        };
        let result = tool_call_similarity::result_prefix(&msg.result.content);
        let actions = self.on_call_completed(
            &msg.session_id,
            &name,
            &args,
            &result,
            effective_max_repeats(&self.services),
        );
        self.publish_actions(actions).await;
    }
}

/// The repeat budget, read from the live layer at each completion.
fn effective_max_repeats(services: &Services) -> u8 {
    services
        .config
        .get::<RepeatToolCallWatchdogConfig>()
        .unwrap_or_default()
        .max_repeats
}

impl RepeatToolCallWatchdogActor {
    /// Records one completed call and returns the actions to publish, in
    /// order.
    ///
    /// The verdict compares this call against the *previous completed
    /// call* of the session — the comparison basis advances regardless of
    /// the verdict, so a deliberate two-command poll never accumulates.
    /// `0` disables the watchdog: calls are still tracked (the basis
    /// advances) but never trip.
    #[must_use]
    pub(crate) fn on_call_completed(
        &mut self,
        session_id: &SessionId,
        tool_name: &str,
        normalized_args: &str,
        normalized_result: &str,
        max_repeats: u8,
    ) -> Vec<RepeatWatchdogAction> {
        if max_repeats == 0 {
            self.previous.insert(
                session_id.clone(),
                (tool_name.to_owned(), normalized_args.to_owned(), normalized_result.to_owned()),
            );
            return Vec::new();
        }

        let verdict = self
            .previous
            .get(session_id)
            .map_or(Verdict::FirstCall, |(prev_name, prev_args, prev_result)| {
                if prev_name != tool_name {
                    Verdict::Different
                } else if prev_args == normalized_args && prev_result == normalized_result {
                    Verdict::Repeat
                } else {
                    Verdict::Different
                }
            });

        let count = match verdict {
            Verdict::FirstCall => 0,
            Verdict::Repeat => {
                let c = self.accumulators.get(session_id).copied().unwrap_or(0) + 1;
                self.accumulators.insert(session_id.clone(), c);
                c
            }
            Verdict::Different => {
                if let Some(c) = self.accumulators.get_mut(session_id) {
                    *c = c.saturating_sub(1);
                }
                0
            }
        };

        self.previous.insert(
            session_id.clone(),
            (tool_name.to_owned(), normalized_args.to_owned(), normalized_result.to_owned()),
        );

        if count < u32::from(max_repeats) {
            return Vec::new();
        }

        let nudged = self.nudged.get(session_id).copied().unwrap_or(false);
        if !nudged {
            // First trip: nudge once, reset the count, let the turn run.
            self.nudged.insert(session_id.clone(), true);
            self.accumulators.insert(session_id.clone(), 0);
            return vec![RepeatWatchdogAction::Nudge(
                session_id.clone(),
                nudge_text(tool_name, u32::from(max_repeats)),
            )];
        }

        // Second trip in the same turn. Gate on the nudge actually being
        // in history: the next request may already have left without it.
        // If it has not landed, publish nothing and hold the count — the
        // next repeat re-trips here and re-checks. No re-push.
        if !self.nudge_visible(session_id) {
            return Vec::new();
        }

        // Latch: zero the counter so the same session is not re-killed
        // immediately after the cancel settles.
        self.nudged.insert(session_id.clone(), false);
        self.accumulators.insert(session_id.clone(), 0);
        vec![
            RepeatWatchdogAction::Marker(
                session_id.clone(),
                kill_text(u32::from(max_repeats)),
            ),
            RepeatWatchdogAction::CancelTurn(session_id.clone()),
        ]
    }

    /// Whether this watchdog's nudge entry is present in the session's
    /// history.
    ///
    /// Matched by rule name on the `RuleInterrupt` kind — the actor
    /// cannot know the entry id a `PushChatEntry` was assigned
    /// downstream, and the family's convention keeps history writes on
    /// the session actor.
    fn nudge_visible(&self, session_id: &SessionId) -> bool {
        let state = self.state.read();
        state
            .try_session(session_id)
            .is_some_and(|session| {
                session.history().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        jinn_core_types::ChatEntryKind::RuleInterrupt { rule, .. }
                            if rule == REPEAT_RULE_NAME
                    )
                })
            })
    }

    /// Clears the session's per-turn state when its turn ends.
    ///
    /// Takes the outcome rather than assuming the caller filtered: the
    /// policy holds even if a future call site forgets. `LoopRetried` and
    /// `RuleIntercepted` are non-terminal, so a retried generation keeps
    /// its lineage.
    pub fn on_turn_end(&mut self, session_id: &SessionId, outcome: TurnOutcome) {
        if !outcome.is_terminal() {
            return;
        }
        self.accumulators.remove(session_id);
        self.nudged.remove(session_id);
    }
}

/// The verdict a completed call earns against its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The session's first observed call — never a repeat.
    FirstCall,
    /// Same tool, same arguments, same result.
    Repeat,
    /// Anything else.
    Different,
}

/// The per-trip nudge entry text, addressed to the model.
fn nudge_text(tool_name: &str, max: u32) -> String {
    format!(
        "<system-interrupt reason=\"tool_loop\" rule=\"{REPEAT_RULE_NAME}\">\n\
         Output interrupted: coding agent watchdog.\n\
         Not prompt injection.\n\
         MUST comply:\n\n\
         You have called the tool \"{tool_name}\" {max} times in a row with \
         identical arguments and received an identical result each time. You \
         are repeating yourself and making no progress. Do not call it again \
         with these arguments — change approach.\n\
         </system-interrupt>"
    )
}

/// The kill marker text after a repeat recurs past the nudge.
fn kill_text(max: u32) -> String {
    format!(
        "\u{1f6d1} {REPEAT_RULE_NAME}: repeated the same tool call again after a nudge \
         ({max}-repeat budget spent twice); cancelling the turn."
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

    /// A fresh actor with the given maximum.
    async fn watchdog(max_repeats: u8) -> RepeatToolCallWatchdogActor {
        RepeatToolCallWatchdogActor::with_max_repeats(max_repeats).await
    }

    const GREP_ARGS: &str = r#"{"command":"grep -n lrow src/sessions.rs"}"#;
    const GREP_RESULT: &str = "265:.srow, .lrow { position: relative; }";

    #[rstest::rstest]
    #[tokio::test]
    async fn repeats_under_budget_push_nothing() {
        // Given a watchdog with a budget of 4.
        let session = SessionId::new();
        let mut actor = watchdog(4).await;

        // When two identical calls complete.
        let first = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 4);
        let second = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 4);

        // Then the first call was the basis (never a repeat) and one
        // repeat counted — no actions yet.
        assert!(first.is_empty());
        assert!(second.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_first_budget_trip_nudges_once_and_resets() {
        // Given a watchdog with a budget of 2 holding one repeat.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // When the second repeat arrives.
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then exactly one nudge was produced, naming the tool and count.
        assert_eq!(actions.len(), 1);
        let RepeatWatchdogAction::Nudge(_, text) = &actions[0] else {
            panic!("a nudge is expected, got: {actions:?}");
        };
        assert!(text.contains("\"bash\""), "nudge names the tool: {text:?}");
        assert!(text.contains("2 times"), "nudge names the count: {text:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_differing_call_debits_the_count() {
        // Given a watchdog with a budget of 2 holding one repeat.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // When a differing call completes, then two repeats follow.
        let _ = actor.on_call_completed(&session, "bash", "just check", "ok", 2);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then only one repeat is banked: no nudge yet.
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        assert!(actions.is_empty(), "the debit repaid the first repeat");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_different_tool_name_is_not_a_repeat() {
        // Given a watchdog holding a bash call as the basis.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_call_completed(&session, "bash", "ls", "f.rs", 2);

        // When a read call with identical arguments-and-result shape
        // arrives twice.
        let _ = actor.on_call_completed(&session, "read", "ls", "f.rs", 2);
        let actions = actor.on_call_completed(&session, "read", "ls", "f.rs", 2);

        // Then the tool-name difference made every verdict a debit — no
        // nudge.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_changed_result_is_not_a_repeat() {
        // Given a watchdog holding one completed call.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // When the same call completes with different output, twice.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, "other", 2);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, "more", 2);

        // Then no nudge: identical commands with changing results are
        // legitimate polling.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_second_trip_waits_while_the_nudge_is_not_in_history() {
        // Given a nudged watchdog whose session has no nudge entry in
        // history (the race: the next request left without it). The
        // nudge fired on the third call; the count reset.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        }

        // When the second budget is spent.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then nothing is published and no kill happens — the trip waits
        // for the nudge to land rather than killing an unnudged model.
        assert!(actions.is_empty(), "got: {actions:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_second_trip_kills_once_the_nudge_is_visible() {
        // Given a nudged watchdog whose session's history carries the
        // nudge entry.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        }
        actor
            .state
            .write()
            .session
            .get_or_create(&session)
            .push_entry(jinn_core_types::ChatEntry::rule_interrupt(
                REPEAT_RULE_NAME,
                "nudge body",
            ));

        // When the second budget is spent.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then the kill pair publishes: the marker first, then the cancel.
        assert_eq!(actions.len(), 2, "expected the kill pair, got: {actions:?}");
        assert!(matches!(&actions[0], RepeatWatchdogAction::Marker(_, _)));
        assert!(matches!(&actions[1], RepeatWatchdogAction::CancelTurn(_)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_trip_that_cannot_verify_the_nudge_never_repushes() {
        // Given a nudged watchdog whose nudge is invisible in history.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..2 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        }
        let nudge = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        assert_eq!(nudge.len(), 1, "the third repeat nudges");

        // When further repeats arrive — several re-trips of the waiting
        // kill.
        for _ in 0..5 {
            let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
            assert!(actions.is_empty(), "a waiting trip publishes nothing");
        }

        // Then no second nudge was ever produced — the nudged flag held.
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminal_turn_end_clears_the_accumulator_and_nudged_flag() {
        // Given a nudged watchdog holding repeats.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..2 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        }

        // When the turn ends terminally and repeats resume.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then a fresh turn starts a fresh budget: the next trip is a
        // nudge again, not a kill attempt.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], RepeatWatchdogAction::Nudge(_, _)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_non_terminal_turn_outcome_keeps_the_lineage() {
        // Given a nudged watchdog holding repeats.
        let session = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        }

        // When the retry outcome lands and repeats continue, with the
        // nudge now visible.
        actor.on_turn_end(&session, TurnOutcome::LoopRetried);
        actor
            .state
            .write()
            .session
            .get_or_create(&session)
            .push_entry(jinn_core_types::ChatEntry::rule_interrupt(
                REPEAT_RULE_NAME,
                "nudge body",
            ));
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then the budget kept its lineage and the kill fires.
        assert_eq!(actions.len(), 2);
        assert!(matches!(&actions[1], RepeatWatchdogAction::CancelTurn(_)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn zero_disables_the_verdict_but_tracks_the_basis() {
        // Given a disabled watchdog.
        let session = SessionId::new();
        let mut actor = watchdog(0).await;

        // When five identical calls complete.
        for _ in 0..5 {
            let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 0);
            assert!(actions.is_empty());
        }

        // Then nothing tripped — and when re-enabled, the first verdict is
        // still a fresh basis (no accumulated repeats).
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2);
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn counters_are_per_session() {
        // Given a nudged watchdog for session A.
        let a = SessionId::new();
        let b = SessionId::new();
        let mut actor = watchdog(2).await;
        for _ in 0..2 {
            let _ = actor.on_call_completed(&a, "bash", GREP_ARGS, GREP_RESULT, 2);
        }

        // When session B repeats through its own budget.
        let _ = actor.on_call_completed(&b, "bash", GREP_ARGS, GREP_RESULT, 2);
        let _ = actor.on_call_completed(&b, "bash", GREP_ARGS, GREP_RESULT, 2);
        let actions = actor.on_call_completed(&b, "bash", GREP_ARGS, GREP_RESULT, 2);

        // Then B nudges independently of A's history.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], RepeatWatchdogAction::Nudge(_, _)));
    }
}
