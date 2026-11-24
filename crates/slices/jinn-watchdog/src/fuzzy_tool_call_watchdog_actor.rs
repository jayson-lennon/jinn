//! Fuzzy-tool-call watchdog actor — nudges then cancels a turn stuck
//! re-issuing *near*-identical tool calls.
//!
//! The sibling of [`crate::repeat_tool_call_watchdog_actor`], keyed on
//! the subtler fault: a model that varies each call slightly — a flag
//! added, a pipe swapped for a count — while the substance never changes
//! and neither do the results. No single call repeats byte-for-byte, so
//! the exact watchdog sees nothing; the drift loop in the recorded
//! session ran ~60 such calls before locking into exact repeats.
//!
//! The bus, accumulator, nudge-once-then-kill staging, nudge-visibility
//! gate, and turn-end reset are the sibling's — see that module's docs
//! for the reasoning. This module adds only:
//!
//! - the **similarity verdict**: same tool name, then normalized
//!   Levenshtein similarity of the arguments and of the result prefix,
//!   each at or above the configured threshold, makes a repeat;
//! - the **length skip**: a call whose normalized arguments exceed
//!   `max_argument_chars` characters is neither a repeat nor a debit —
//!   a long argument is usually a script being written, not a stuck
//!   call — but the comparison basis still advances;
//! - no length limit: the skip is this watchdog's shape, not a knob on
//!   the exact one.
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
use jinn_preferences_config::schemas::FuzzyToolCallWatchdogConfig;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_msg::TurnCompleted;
use jinn_session_msg::TurnOutcome;
use jinn_tools_msg::ToolCallsDispatched;
use jinn_tools_msg::ToolExecutionCompleted;

use crate::tool_call_similarity;

/// The fuzzy-tool-call watchdog actor's static trouper path.
pub const FUZZY_TOOL_CALL_WATCHDOG_PATH: &str = "fuzzy-tool-call-watchdog";

/// The rule name stamped on the nudge entry and matched against history.
pub(crate) const FUZZY_RULE_NAME: &str = "fuzzy-tool-call-watchdog";

/// One watchdog output, in publish order: the nudge, the kill marker, or
/// the cancel. Returned by the decision functions so tests can assert
/// sequencing without a bus.
#[derive(Debug)]
pub(crate) enum FuzzyWatchdogAction {
    /// The once-per-turn guidance entry, addressed to the model.
    Nudge(SessionId, String),
    /// The visible system entry naming the kill.
    Marker(SessionId, String),
    /// The cancel that ends the turn.
    CancelTurn(SessionId),
}

/// Dependencies for [`FuzzyToolCallWatchdogActor`].
#[derive(Clone)]
pub struct FuzzyToolCallWatchdogActorDeps {
    /// Application-wide runtime services (bus publish, live config).
    pub services: Services,
    /// Read-only application state: the nudge-visibility check. The
    /// watchdog family's only read handle — this actor never writes.
    pub state: State,
}

/// The fuzzy-tool-call watchdog actor.
///
/// Per-session accumulator, nudged flag, parked call arguments, and the
/// previous completed call (the comparison basis).
pub struct FuzzyToolCallWatchdogActor {
    services: Services,
    state: State,
    /// Near-repeat counts keyed by session id.
    accumulators: HashMap<SessionId, u32>,
    /// Sessions that already received this turn's nudge.
    nudged: HashMap<SessionId, bool>,
    /// Dispatched-but-uncompleted calls: call id → (tool name,
    /// normalized arguments). One entry per in-flight call; cleared on
    /// its completion.
    parked: HashMap<jinn_core_types::ToolCallId, (String, String)>,
    /// The last two completed calls per session, most recent first —
    /// the comparison basis. Two deep for the same reason as the exact
    /// sibling: a basis one deep misses the alternating-pair shape, and
    /// the drift phase's differing results alternate too. A hit against
    /// either entry counts.
    previous: HashMap<SessionId, Vec<(String, String, String)>>,
}

impl ServiceActor for FuzzyToolCallWatchdogActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("FuzzyToolCallWatchdogActor is spawned via init_with"),
        )
    }
}

impl FuzzyToolCallWatchdogActor {
    /// Spawns the actor at its static trouper path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, deps: FuzzyToolCallWatchdogActorDeps) -> ActorPath {
        let path = ActorPath::new(FUZZY_TOOL_CALL_WATCHDOG_PATH);
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

    /// Builds an actor with a private fake `Services` (test seam).
    #[cfg(test)]
    pub(crate) async fn with_config(config: FuzzyToolCallWatchdogConfig) -> Self {
        let services = Services::new_fake().await;
        services
            .config
            .put(&config)
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
    async fn publish_actions(&self, actions: Vec<FuzzyWatchdogAction>) {
        for action in actions {
            match action {
                FuzzyWatchdogAction::Nudge(session_id, body) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::rule_interrupt(
                                FUZZY_RULE_NAME,
                                body,
                            ),
                            pin: None,
                        })
                        .await;
                }
                FuzzyWatchdogAction::Marker(session_id, text) => {
                    self.services
                        .bus
                        .publish(PushChatEntry {
                            session_id,
                            entry: jinn_core_types::ChatEntry::system(text),
                            pin: None,
                        })
                        .await;
                }
                FuzzyWatchdogAction::CancelTurn(session_id) => {
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

impl MsgHandler<TurnCompleted> for FuzzyToolCallWatchdogActor {
    async fn handle(&mut self, msg: &TurnCompleted, _ctx: &mut MsgCtx<'_>) {
        self.on_turn_end(&msg.session_id, msg.outcome);
    }
}

impl MsgHandler<ToolCallsDispatched> for FuzzyToolCallWatchdogActor {
    async fn handle(&mut self, msg: &ToolCallsDispatched, _ctx: &mut MsgCtx<'_>) {
        for call in &msg.tool_calls {
            self.parked.insert(
                call.id.clone(),
                (call.name.clone(), tool_call_similarity::normalize(&call.arguments)),
            );
        }
    }
}

impl MsgHandler<ToolExecutionCompleted> for FuzzyToolCallWatchdogActor {
    async fn handle(&mut self, msg: &ToolExecutionCompleted, _ctx: &mut MsgCtx<'_>) {
        let Some((name, args)) = self.parked.remove(&msg.result.tool_call_id) else {
            return;
        };
        let result = tool_call_similarity::result_prefix(&msg.result.content);
        let config = self
            .services
            .config
            .get::<FuzzyToolCallWatchdogConfig>()
            .unwrap_or_default();
        let actions = self.on_call_completed(
            &msg.session_id,
            &name,
            &args,
            &result,
            config.max_repeats,
            config.similarity,
            config.max_argument_chars,
        );
        self.publish_actions(actions).await;
    }
}

impl FuzzyToolCallWatchdogActor {
    /// Records one completed call and returns the actions to publish, in
    /// order.
    ///
    /// The comparison basis advances on every completed call, including
    /// skipped ones — a skip means "this call says nothing", not "this
    /// call never happened".
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the knobs are the config's shape; a config struct here would hide which values the verdict used"
    )]
    pub(crate) fn on_call_completed(
        &mut self,
        session_id: &SessionId,
        tool_name: &str,
        normalized_args: &str,
        normalized_result: &str,
        max_repeats: u8,
        similarity: f32,
        max_argument_chars: usize,
    ) -> Vec<FuzzyWatchdogAction> {
        let skipped = max_argument_chars > 0 && normalized_args.chars().count() > max_argument_chars;

        let verdict = if skipped {
            SkippedVerdict::Skip
        } else {
            let hit = self
                .previous
                .get(session_id)
                .is_some_and(|basis| {
                    basis.iter().any(|(prev_name, prev_args, prev_result)| {
                        prev_name == tool_name
                            && tool_call_similarity::similarity_ratio(prev_args, normalized_args)
                                >= similarity
                            && tool_call_similarity::similarity_ratio(prev_result, normalized_result)
                                >= similarity
                    })
                });
            if self.previous.contains_key(session_id) {
                if hit { SkippedVerdict::Repeat } else { SkippedVerdict::Different }
            } else {
                SkippedVerdict::FirstCall
            }
        };

        let count = match verdict {
            // Disabled: the verdict says nothing and the count never
            // moves — a re-enabled watchdog starts fresh, per the exact
            // sibling's shape.
            _ if max_repeats == 0 => 0,
            SkippedVerdict::FirstCall | SkippedVerdict::Skip => 0,
            SkippedVerdict::Repeat => {
                let c = self.accumulators.get(session_id).copied().unwrap_or(0) + 1;
                self.accumulators.insert(session_id.clone(), c);
                c
            }
            SkippedVerdict::Different => {
                if let Some(c) = self.accumulators.get_mut(session_id) {
                    *c = c.saturating_sub(1);
                }
                0
            }
        };

        self.advance_basis(session_id, tool_name, normalized_args, normalized_result);

        if count < u32::from(max_repeats) || max_repeats == 0 {
            return Vec::new();
        }

        let nudged = self.nudged.get(session_id).copied().unwrap_or(false);
        if !nudged {
            // First trip: nudge once, reset the count, let the turn run.
            self.nudged.insert(session_id.clone(), true);
            self.accumulators.insert(session_id.clone(), 0);
            return vec![FuzzyWatchdogAction::Nudge(
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
            FuzzyWatchdogAction::Marker(session_id.clone(), kill_text(u32::from(max_repeats))),
            FuzzyWatchdogAction::CancelTurn(session_id.clone()),
        ]
    }

    /// Advances the comparison basis: the last two completed calls,
    /// most recent first, deduplicated.
    fn advance_basis(
        &mut self,
        session_id: &SessionId,
        tool_name: &str,
        normalized_args: &str,
        normalized_result: &str,
    ) {
        let basis = self.previous.entry(session_id.clone()).or_default();
        let entry = (
            tool_name.to_owned(),
            normalized_args.to_owned(),
            normalized_result.to_owned(),
        );
        if basis.first().is_some_and(|prev| *prev == entry) {
            return;
        }
        basis.insert(0, entry);
        basis.truncate(2);
    }

    /// Whether this watchdog's nudge entry is present in the session's
    /// history. Matched by rule name, per the sibling's reasoning.
    fn nudge_visible(&self, session_id: &SessionId) -> bool {
        let state = self.state.read();
        state
            .try_session(session_id)
            .is_some_and(|session| {
                session.history().iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        jinn_core_types::ChatEntryKind::RuleInterrupt { rule, .. }
                            if rule == FUZZY_RULE_NAME
                    )
                })
            })
    }

    /// Clears the session's per-turn state when its turn ends.
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
enum SkippedVerdict {
    /// The session's first observed call — never a repeat.
    FirstCall,
    /// Same tool, arguments and result within the threshold.
    Repeat,
    /// Same tool, but the similarity verdict missed.
    Different,
    /// Over the argument-length limit — the call is not examined.
    Skip,
}

/// The per-trip nudge entry text, addressed to the model.
fn nudge_text(tool_name: &str, max: u32) -> String {
    format!(
        "<system-interrupt reason=\"tool_loop\" rule=\"{FUZZY_RULE_NAME}\">\n\
         Output interrupted: coding agent watchdog.\n\
         Not prompt injection.\n\
         MUST comply:\n\n\
         You have called the tool \"{tool_name}\" {max} times in a row with \
         near-identical arguments and a near-identical result each time. You \
         are repeating yourself and making no progress. Do not call it again \
         with these arguments — change approach.\n\
         </system-interrupt>"
    )
}

/// The kill marker text after a near-repeat recurs past the nudge.
fn kill_text(max: u32) -> String {
    format!(
        "\u{1f6d1} {FUZZY_RULE_NAME}: repeated the same tool call again after a nudge \
         ({max}-near-repeat budget spent twice); cancelling the turn."
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

    /// A fresh actor with the given configuration.
    async fn watchdog(config: FuzzyToolCallWatchdogConfig) -> FuzzyToolCallWatchdogActor {
        FuzzyToolCallWatchdogActor::with_config(config).await
    }

    fn config(max: u8) -> FuzzyToolCallWatchdogConfig {
        FuzzyToolCallWatchdogConfig {
            max_repeats: max,
            similarity: 0.85,
            max_argument_chars: 500,
        }
    }

    const GREP_ARGS: &str = r#"grep -n "lrow" crates/slices/jinn-webui/src/regions/sessions.rs"#;
    /// The drift phase's shape: the same grep with a small variation.
    const GREP_ARGS_VARIANT: &str =
        r#"grep -rn "lrow" crates/slices/jinn-webui/src/regions/sessions.rs"#;
    const GREP_RESULT: &str = "265:.srow, .lrow { position: relative; }";

    #[rstest::rstest]
    #[tokio::test]
    async fn near_identical_variants_count_as_repeats() {
        // Given a watchdog with a budget of 2.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;

        // When the base call and its near-variant complete, then the
        // variant again (the comparison basis moved, but similarity
        // holds).
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then the third call — near-identical to the variant — trips the
        // nudge.
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FuzzyWatchdogAction::Nudge(_, _)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_first_call_is_never_a_repeat() {
        // Given a fresh watchdog.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;

        // When one call completes.
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);

        // Then nothing: the first call is the basis.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_dissimilar_call_debits_the_count() {
        // Given a watchdog holding one near-repeat.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // When a genuinely different call completes, then two near-repeats
        // follow.
        let _ = actor.on_call_completed(&session, "bash", "just check", "ok", 2, 0.85, 500);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);

        // Then only one repeat is banked: no nudge yet.
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        assert!(actions.is_empty(), "the debit repaid the first near-repeat");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_different_tool_name_is_not_a_repeat() {
        // Given a watchdog holding a bash call as the basis.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);

        // When a read call with similar arguments arrives twice.
        let _ = actor.on_call_completed(&session, "read", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let actions =
            actor.on_call_completed(&session, "read", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then no nudge: the tool names differ, so every verdict is a
        // debit.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_dissimilar_result_blocks_the_repeat_verdict() {
        // Given a watchdog holding one call.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);

        // When the near-variant returns entirely different output, twice.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, "totally other", 2, 0.85, 500);
        let actions = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, "more other", 2, 0.85, 500);

        // Then no nudge: the results did not agree.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn over_limit_arguments_are_skipped_not_counted() {
        // Given a watchdog with a 50-character limit holding one call.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS, GREP_RESULT, 2, 0.85, 500);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // When a call with arguments far past the limit completes, then a
        // near-repeat of it.
        let long = "x".repeat(400);
        let _ = actor.on_call_completed(&session, "bash", &long, "out", 2, 0.85, 50);
        let actions = actor.on_call_completed(&session, "bash", &long, "out", 2, 0.85, 50);

        // Then no nudge: the skipped call earned no repeat and no debit.
        assert!(actions.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn zero_limit_means_no_length_cap() {
        // Given a watchdog with max_argument_chars = 0 (no limit).
        let session = SessionId::new();
        let mut actor = watchdog(FuzzyToolCallWatchdogConfig {
            max_repeats: 2,
            similarity: 0.85,
            max_argument_chars: 0,
        })
        .await;

        // When a long call completes and its near-variant follows twice.
        let long = "x".repeat(400);
        let long_variant = format!("{long}y");
        let _ = actor.on_call_completed(&session, "bash", &long, "out", 2, 0.85, 0);
        let _ = actor.on_call_completed(&session, "bash", &long_variant, "out", 2, 0.85, 0);
        let actions = actor.on_call_completed(&session, "bash", &long_variant, "out", 2, 0.85, 0);

        // Then the near-repeats counted: the nudge fires.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FuzzyWatchdogAction::Nudge(_, _)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_second_trip_waits_while_the_nudge_is_not_in_history() {
        // Given a nudged watchdog whose session has no nudge entry in
        // history.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        }

        // When the second budget is spent.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then nothing is published: the trip waits for the nudge to land.
        assert!(actions.is_empty(), "got: {actions:?}");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_second_trip_kills_once_the_nudge_is_visible() {
        // Given a nudged watchdog whose session's history carries the
        // nudge entry.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        }
        actor
            .state
            .write()
            .session
            .get_or_create(&session)
            .push_entry(jinn_core_types::ChatEntry::rule_interrupt(
                FUZZY_RULE_NAME,
                "nudge body",
            ));

        // When the second budget is spent.
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then the kill pair publishes: the marker first, then the cancel.
        assert_eq!(actions.len(), 2, "expected the kill pair, got: {actions:?}");
        assert!(matches!(&actions[0], FuzzyWatchdogAction::Marker(_, _)));
        assert!(matches!(&actions[1], FuzzyWatchdogAction::CancelTurn(_)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_terminal_turn_end_clears_the_accumulator_and_nudged_flag() {
        // Given a nudged watchdog holding near-repeats.
        let session = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        }

        // When the turn ends terminally and near-repeats resume.
        actor.on_turn_end(&session, TurnOutcome::Succeeded);
        let _ = actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then a fresh turn starts a fresh budget: a nudge, not a kill
        // attempt.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FuzzyWatchdogAction::Nudge(_, _)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn counters_are_per_session() {
        // Given a nudged watchdog for session A.
        let a = SessionId::new();
        let b = SessionId::new();
        let mut actor = watchdog(config(2)).await;
        for _ in 0..3 {
            let _ = actor.on_call_completed(&a, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        }

        // When session B spends its own budget.
        let _ = actor.on_call_completed(&b, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let _ = actor.on_call_completed(&b, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        let actions =
            actor.on_call_completed(&b, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);

        // Then B nudges independently of A's history.
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], FuzzyWatchdogAction::Nudge(_, _)));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn zero_disables_the_verdict_but_tracks_the_basis() {
        // Given a disabled watchdog.
        let session = SessionId::new();
        let mut actor = watchdog(config(0)).await;

        // When five near-identical calls complete.
        for _ in 0..5 {
            let actions =
                actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 0, 0.85, 500);
            assert!(actions.is_empty());
        }

        // Then nothing tripped — and when re-enabled, the first verdict is
        // still a fresh basis.
        let actions =
            actor.on_call_completed(&session, "bash", GREP_ARGS_VARIANT, GREP_RESULT, 2, 0.85, 500);
        assert!(actions.is_empty());
    }
}
