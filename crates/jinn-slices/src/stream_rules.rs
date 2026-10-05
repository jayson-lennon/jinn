//! Stream-rule vocabulary, and the cell a rules slice installs its matcher into.
//!
//! A *stream rule* is a regex over the assistant's live output. When one
//! matches, the turn is interrupted before the offending output is published
//! downstream, and resumed with the rule's body injected as guidance. This
//! is the vocabulary the stream loop speaks; the compiled rules themselves
//! belong to the slice that reads them from `jinn.toml`.
//!
//! ## Why this lives below the kernel
//!
//! The consumer is the inference stream loop, which is kernel-side, and the
//! producer is a slice. Neither direction of that edge may name the other: a
//! slice must not be reachable from the kernel's crate list, and a kernel
//! vocabulary must not live in a slice's implementation crate. So the
//! *vocabulary* — what a match is, what a fired rule carries, and the two
//! traits that describe the matcher — sits here, in the shared crate both
//! sides already depend on.
//!
//! ## Two lifetimes, deliberately
//!
//! A turn and an assistant response are not the same span. One turn streams
//! many responses as the tool loop runs, so:
//!
//! - **Buffers** are per response. Fresh buffers per response are what make
//!   "reset at the start of every assistant response" true by construction:
//!   a retry cannot inherit an aborted attempt's text, because it cannot
//!   reach that attempt's buffers.
//! - **The interrupt count** is per session, and it is *consecutive*, not
//!   per-turn. The failure it bounds is a model that trips, resumes, and
//!   trips again: a rule whose condition also matches the guidance it
//!   injects, which would otherwise interrupt forever. A response that
//!   completes without an interrupt debits it, so a rule the model needed to
//!   be reminded about twice still gets its two chances.
//!
//! The count deliberately survives across the responses of one turn, because
//! an interrupt *is* a response boundary — it ends the stream and the turn
//! re-dispatches. A per-turn cap would be cleared by the very act it exists
//! to bound, and would be unreachable on the only path where it mattered.
//!
//! Splitting the two is why [`StreamRuleSet::end_turn`] exists: the session's
//! count outlives every response in it and is debited when a response ends
//! cleanly.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use jinn_core_types::SessionId;

use crate::slices::SlotKey;

/// How many times a rule may interrupt a session in a row before the stream
/// is cancelled.
///
/// Three is enough for a model that needed a second reminder and bounded
/// enough that a rule matching its own injected guidance cannot loop.
pub const MAX_INTERRUPTS_PER_SESSION: usize = 3;

/// Which of the three streams a delta arrived on.
///
/// The three are buffered and matched separately. A rule scoped to tool
/// arguments must never fire on a prose chunk that merely *mentions* the same
/// text, so the isolation is structural rather than a prefix on one buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamSource {
    /// Assistant prose.
    Text,
    /// Reasoning output.
    Thinking,
    /// Serialized tool-call arguments.
    Tool,
}

/// What a delta belongs to, plus what a tool scope needs to narrow itself.
///
/// `tool_name` and `tool_index` come from the provider: the name arrives with
/// the tool-use-start event, and the index keys the per-call argument buffer,
/// so two tool calls in one response cannot bleed into each other.
#[derive(Debug, Clone, Copy)]
pub struct StreamContext<'a> {
    /// Which stream produced this delta.
    pub source: StreamSource,
    /// The tool this delta's arguments belong to, for [`StreamSource::Tool`].
    pub tool_name: Option<&'a str>,
    /// The tool call this delta belongs to within the response.
    pub tool_index: usize,
}

impl<'a> StreamContext<'a> {
    /// A delta of assistant prose.
    #[must_use]
    pub fn text() -> Self {
        Self {
            source: StreamSource::Text,
            tool_name: None,
            tool_index: 0,
        }
    }

    /// A delta of reasoning output.
    #[must_use]
    pub fn thinking() -> Self {
        Self {
            source: StreamSource::Thinking,
            tool_name: None,
            tool_index: 0,
        }
    }

    /// A delta of tool-call arguments for the call at `index` named `name`.
    #[must_use]
    pub fn tool(index: usize, name: &'a str) -> Self {
        Self {
            source: StreamSource::Tool,
            tool_name: Some(name),
            tool_index: index,
        }
    }
}

/// A rule that matched, with everything the caller needs to act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleFired {
    /// The rule's `name`, unique per configuration.
    pub name: String,
    /// The rule's `description`, carried for the message that names it.
    pub description: String,
    /// The rule's `body`: the guidance to inject when the turn resumes.
    pub body: String,
}

/// How a response ended, for the interrupt budget's turn-end policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEnd {
    /// The response completed without an interrupt.
    ///
    /// Debits the count: a model that finished its response after being
    /// corrected has heard the correction.
    Finished,
    /// The response ended some other way — interrupted, cancelled, or errored.
    ///
    /// Retains the count, so a session interrupted three times in a row stays
    /// over its budget into the retry that followed. Debiting on every
    /// non-clean end would make the budget unreachable on exactly the path it
    /// exists to bound.
    Aborted,
}

/// How many interrupts a session has taken in a row.
///
/// One counter for the session, shared between every response of a turn, so a
/// second response sees what the first already tripped. Per session rather
/// than per rule because the failure being bounded is a model that trips,
/// resumes, and trips again — which several rules can conspire to cause.
///
/// Mutex-guarded because the set is shared across turns while the count
/// belongs to one session.
#[derive(Default)]
pub struct InterruptCount(Mutex<usize>);

impl fmt::Debug for InterruptCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.count(), f)
    }
}

impl InterruptCount {
    /// Records an interrupt and reports whether the session has stayed within
    /// `maximum`.
    ///
    /// Returns `false` once the count reaches the threshold. The caller treats
    /// that as a budget trip rather than as silence: a rule that keeps
    /// matching a model that keeps ignoring it is a loop, and the remedy is to
    /// stop the stream rather than to silently stop correcting.
    ///
    /// The threshold is a parameter rather than stored state so one session's
    /// count cannot be judged against two budgets — a set narrowed per project
    /// must not hand the same session two different limits.
    pub fn record(&self, maximum: usize) -> bool {
        let mut count = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count += 1;
        *count < maximum
    }

    /// How many consecutive interrupts this session has taken.
    #[must_use]
    pub fn count(&self) -> usize {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Debits one interrupt, clamping at zero.
    ///
    /// A response that completed without an interrupt is evidence the model
    /// heard the last correction, so the debt is repaid before the next
    /// response is judged. Clamping rather than wrapping is what keeps a long
    /// conversation from accumulating a count past the cap's meaning.
    pub fn debit(&self) {
        let mut count = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *count = count.saturating_sub(1);
    }

    /// Zeroes the count, called when the budget trips.
    ///
    /// Latching rather than leaving the session pinned at the maximum: the
    /// trip is over, and a count stuck there would cancel every subsequent
    /// response the model starts.
    pub fn reset(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
    }
}

/// One assistant response's worth of matcher state: the accumulation buffers
/// for each stream, plus a handle on its session's interrupt count.
///
/// Minted per stream run and dropped with it.
pub trait StreamRuleSession: fmt::Debug + Send {
    /// Accumulates `delta` and returns what it trips, if anything.
    ///
    /// Returns the first match in configuration order: when two rules match
    /// the same delta, the one written first in `jinn.toml` wins, since that
    /// is the file the user would expect to be authoritative.
    ///
    /// A [`RuleMatch::BudgetSpent`] is returned in place of the interrupt it
    /// would otherwise have produced, so the caller learns from one value both
    /// that a rule matched and that the session is out of budget.
    fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleMatch>;

    /// The buffers' current contents, keyed by stream — for tests and
    /// diagnostics.
    fn buffers(&self) -> &HashMap<String, String>;
}

/// What a match is worth to the caller, beyond the rule itself.
///
/// A match is either an interrupt the stream loop should act on, or the trip
/// that says the session has taken enough of them. The trip is separate rather
/// than folded into "no match" because the two demand opposite responses:
/// an interrupt resumes the turn, while a trip stops the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleMatch {
    /// A rule matched and the turn should be interrupted and resumed.
    Interrupt(RuleFired),
    /// The session has taken `maximum` interrupts in a row; the stream should
    /// be cancelled rather than resumed a fourth time.
    ///
    /// Carries the rule that tripped, so the cancellation can name what looped.
    BudgetSpent {
        /// The rule whose match took the session over its budget.
        rule: RuleFired,
        /// The threshold that was reached.
        maximum: usize,
    },
}

impl RuleMatch {
    /// The rule that matched, whichever outcome it produced.
    #[must_use]
    pub fn fired(&self) -> &RuleFired {
        match self {
            Self::Interrupt(rule) | Self::BudgetSpent { rule, .. } => rule,
        }
    }

    /// Whether this match means the stream should be cancelled rather than
    /// resumed.
    #[must_use]
    pub fn is_budget_spent(&self) -> bool {
        matches!(self, Self::BudgetSpent { .. })
    }

    /// Whether this match resumes the turn rather than cancelling it.
    #[must_use]
    pub fn is_interrupt(&self) -> bool {
        matches!(self, Self::Interrupt(_))
    }
}

/// The compiled rule set: stateless with respect to any one turn, shareable
/// across every concurrent turn.
pub trait StreamRuleSet: fmt::Debug + Send + Sync {
    /// The implementing slice's name, for diagnostics.
    fn name(&self) -> &'static str;

    /// Whether any rule survived compilation.
    ///
    /// The stream loop consults this once per response, so a configuration
    /// with no usable rules costs no per-delta work at all.
    fn is_empty(&self) -> bool;

    /// Mints the state for one assistant response, attaching it to
    /// `session`'s current turn.
    ///
    /// The returned session is owned, not borrowed: it is minted inside a
    /// cell read guard that does not outlive the call, so a borrowing
    /// session could never be handed to the stream task.
    fn new_session(&self, session: &SessionId) -> Box<dyn StreamRuleSession>;

    /// Records that one of `session`'s responses ended `how`.
    ///
    /// A clean completion debits the interrupt count; anything else retains
    /// it, so consecutive interrupts accumulate across the responses of a
    /// turn rather than resetting at each one.
    fn end_response(&self, session: &SessionId, how: TurnEnd);

    /// This set with every rule whose `project` glob excludes `project`
    /// removed.
    ///
    /// A rule is configured against the projects it applies in, but a set is
    /// installed once and consulted by every session, so the project is bound
    /// here rather than carried into each match. A consumer holding a set
    /// derived this way needs no knowledge of globs.
    fn for_project(&self, project: &std::path::Path) -> std::sync::Arc<dyn StreamRuleSet>;
}

/// The cell payload: the installed matcher, or the absence of one.
///
/// An absent set is the no-rules-configured state and the common one. The
/// stream loop holds `None` for its whole run in that case, so a delta costs
/// a branch and nothing else — no buffer allocation, no regex.
#[derive(Clone, Default)]
pub struct StreamRules(Option<Arc<dyn StreamRuleSet>>);

impl fmt::Debug for StreamRules {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(set) => f
                .debug_struct("StreamRules")
                .field("slice", &set.name())
                .field("empty", &set.is_empty())
                .finish(),
            None => f.write_str("StreamRules(none)"),
        }
    }
}

impl StreamRules {
    /// The state with no matcher installed.
    #[must_use]
    pub fn empty() -> Self {
        Self(None)
    }

    /// Installs `set` as the matcher, replacing any previous one.
    pub fn install(&mut self, set: Arc<dyn StreamRuleSet>) {
        self.0 = Some(set);
    }

    /// Whether a matcher is installed, ignoring whether it holds rules.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.0.is_some()
    }

    /// Mints the state for one assistant response, or `None` when no matcher
    /// is installed or the installed one holds no usable rule.
    #[must_use]
    pub fn new_session(&self, session: &SessionId) -> Option<Box<dyn StreamRuleSession>> {
        let set = self.0.as_ref()?;
        if set.is_empty() {
            return None;
        }
        Some(set.new_session(session))
    }

    /// Records that one of `session`'s responses ended `how`.
    pub fn end_response(&self, session: &SessionId, how: TurnEnd) {
        if let Some(set) = self.0.as_ref() {
            set.end_response(session, how);
        }
    }

    /// The installed set narrowed to the rules that apply in `project`.
    ///
    /// Returns `None` when no matcher is installed, so a consumer can hold the
    /// result as an `Option` and treat absence as "no rules apply" — the same
    /// reading it gives an uninstalled cell.
    pub fn for_project(
        &self,
        project: &std::path::Path,
    ) -> Option<std::sync::Arc<dyn StreamRuleSet>> {
        let set = self.0.as_ref()?;
        Some(set.for_project(project))
    }
}

/// The slot key the installed stream-rule matcher is stored under.
///
/// The same cell the scope-hint registry and the pre-render hook list use: one
/// shared handle to a value a slice writes and a consumer reads, reached
/// through [`Slices`](crate::Slices) rather than a new `Services` field or a
/// new `SliceHost` constructor argument.
#[must_use]
pub fn stream_rules_slot() -> SlotKey {
    SlotKey::builtin("jinn", "stream-rules")
}

#[cfg(test)]
mod tests {
    use super::{
        InterruptCount, MAX_INTERRUPTS_PER_SESSION, RuleFired, RuleMatch, StreamContext,
        StreamRuleSession, StreamRuleSet, StreamRules, TurnEnd,
    };
    use crate::Slices;

    /// A matcher that fires on every delta, to exercise the cell, the
    /// interrupt count and the per-response split without compiling a regex.
    struct AlwaysSet {
        empty: bool,
        counts: std::sync::Mutex<
            std::collections::HashMap<jinn_core_types::SessionId, std::sync::Arc<InterruptCount>>,
        >,
        maximum: usize,
    }

    impl AlwaysSet {
        fn new(empty: bool) -> Self {
            Self {
                empty,
                counts: std::sync::Mutex::new(std::collections::HashMap::new()),
                maximum: MAX_INTERRUPTS_PER_SESSION,
            }
        }
    }

    impl std::fmt::Debug for AlwaysSet {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AlwaysSet")
                .field("empty", &self.empty)
                .finish()
        }
    }

    impl StreamRuleSet for AlwaysSet {
        fn name(&self) -> &'static str {
            "always"
        }

        fn is_empty(&self) -> bool {
            self.empty
        }

        fn new_session(&self, session: &jinn_core_types::SessionId) -> Box<dyn StreamRuleSession> {
            let mut counts = self.counts.lock().unwrap();
            let interrupts = std::sync::Arc::clone(
                counts
                    .entry(session.clone())
                    .or_insert_with(|| std::sync::Arc::new(InterruptCount::default())),
            );
            Box::new(AlwaysSession {
                interrupts: interrupts.clone(),
                maximum: self.maximum,
                buffers: std::collections::HashMap::new(),
            })
        }

        fn end_response(&self, session: &jinn_core_types::SessionId, how: TurnEnd) {
            let Some(count) = self.counts.lock().unwrap().get(session).cloned() else {
                return;
            };
            if how == TurnEnd::Finished {
                count.debit();
            }
        }

        fn for_project(&self, _project: &std::path::Path) -> std::sync::Arc<dyn StreamRuleSet> {
            std::sync::Arc::new(Self {
                empty: self.empty,
                counts: std::sync::Mutex::new(std::collections::HashMap::new()),
                maximum: self.maximum,
            })
        }
    }

    #[derive(Debug)]
    struct AlwaysSession {
        interrupts: std::sync::Arc<InterruptCount>,
        maximum: usize,
        buffers: std::collections::HashMap<String, String>,
    }

    impl StreamRuleSession for AlwaysSession {
        fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleMatch> {
            let key = format!("{:?}", ctx.source);
            self.buffers.entry(key.clone()).or_default().push_str(delta);
            let rule = RuleFired {
                name: key,
                description: String::new(),
                body: delta.to_owned(),
            };
            Some(if self.interrupts.record(self.maximum) {
                RuleMatch::Interrupt(rule)
            } else {
                self.interrupts.reset();
                RuleMatch::BudgetSpent {
                    rule,
                    maximum: self.maximum,
                }
            })
        }

        fn buffers(&self) -> &std::collections::HashMap<String, String> {
            &self.buffers
        }
    }

    #[rstest::rstest]
    #[test]
    fn no_matcher_installed_yields_no_session() {
        // Given a cell with no matcher installed.
        let rules = StreamRules::empty();

        // When a response's state is minted.
        let session = rules.new_session(&jinn_core_types::SessionId::new());

        // Then there is none.
        assert!(session.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn an_installed_but_empty_set_yields_no_session() {
        // Given a matcher that holds no rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(true)));

        // When a response's state is minted.
        let session = rules.new_session(&jinn_core_types::SessionId::new());

        // Then there is none, so the loop pays nothing per delta.
        assert!(session.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn each_response_gets_its_own_buffers() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();

        // When two responses of one turn each accumulate a delta.
        let mut first = rules.new_session(&session_id).expect("session");
        let mut second = rules.new_session(&session_id).expect("session");
        first.check("first attempt", StreamContext::text());
        second.check("second attempt", StreamContext::text());

        // Then neither buffer holds the other's text.
        assert_eq!(first.buffers()["Text"], "first attempt");
        assert_eq!(second.buffers()["Text"], "second attempt");
    }

    #[rstest::rstest]
    #[test]
    fn a_clean_response_debits_the_interrupt_count() {
        // Given a session that has been interrupted once.
        let count = InterruptCount::default();
        assert!(count.record(3), "the first interrupt is within budget");
        assert_eq!(count.count(), 1);

        // When a response ends without an interrupt.
        count.debit();

        // Then the count is back to zero, never negative.
        assert_eq!(count.count(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn debiting_a_zero_count_stays_at_zero() {
        // Given a session that has never been interrupted.
        let count = InterruptCount::default();

        // When a clean response ends.
        count.debit();
        count.debit();

        // Then the count clamps at zero rather than wrapping.
        assert_eq!(count.count(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn interrupts_accumulate_across_responses() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();

        // When three separate responses each trip a rule.
        let mut outcomes = Vec::new();
        for _ in 0..MAX_INTERRUPTS_PER_SESSION {
            let mut response = rules.new_session(&session_id).expect("session");
            outcomes.push(response.check("a", StreamContext::text()).expect("a match"));
        }

        // Then the count carried across the response boundaries: the third
        // response trips the budget. A per-response count would have reset on
        // each new session and interrupted all three times, which is the loop
        // the budget exists to stop.
        assert!(matches!(outcomes.first(), Some(RuleMatch::Interrupt(_))));
        assert!(
            outcomes.get(1).is_some_and(RuleMatch::is_interrupt),
            "the second response must still interrupt"
        );
        assert!(
            outcomes.get(2).is_some_and(RuleMatch::is_budget_spent),
            "consecutive interrupts must accumulate across responses"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_budget_trips_on_the_last_interrupt() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();

        // When one response trips a rule past the budget.
        let mut response = rules.new_session(&session_id).expect("session");
        let outcomes: Vec<RuleMatch> = (0..MAX_INTERRUPTS_PER_SESSION)
            .map(|_| response.check("a", StreamContext::text()).expect("a match"))
            .collect();

        // Then the budget is spent exactly once, on the threshold.
        let spent: Vec<bool> = outcomes.iter().map(RuleMatch::is_budget_spent).collect();
        assert_eq!(
            spent,
            vec![false, false, true],
            "the trip must land on the configured maximum"
        );
        assert!(
            outcomes
                .iter()
                .all(|o| o.is_budget_spent() || o.is_interrupt()),
            "every outcome must be one of the two the loop knows how to act on"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_trip_latches_rather_than_staying_spent() {
        // Given a session whose budget has just been spent.
        let count = InterruptCount::default();
        for _ in 0..MAX_INTERRUPTS_PER_SESSION {
            count.record(MAX_INTERRUPTS_PER_SESSION);
        }
        assert_eq!(count.count(), MAX_INTERRUPTS_PER_SESSION);

        // When the trip latches.
        count.reset();

        // Then the count is zero, so the next response is judged afresh
        // rather than cancelled forever.
        assert_eq!(count.count(), 0);
        assert!(
            count.record(MAX_INTERRUPTS_PER_SESSION),
            "a latched session must be able to interrupt again"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_clean_response_lets_a_rule_fire_again() {
        // Given an installed matcher holding rules, on a session whose count
        // is already spent.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();
        {
            let mut response = rules.new_session(&session_id).expect("session");
            // Bounded, not `while is_some()`: the latch makes every later
            // match return `Some` again, so an unbounded loop would spin.
            for _ in 0..=MAX_INTERRUPTS_PER_SESSION {
                response.check("a", StreamContext::text());
            }
        }

        // When a response completes without an interrupt, and the next one
        // violates again.
        rules.end_response(&session_id, TurnEnd::Finished);
        let mut next = rules.new_session(&session_id).expect("session");
        let fired = next.check("a", StreamContext::text());

        // Then the rule is eligible again, because a model that complied once
        // has earned the chance to err again.
        assert!(matches!(fired, Some(RuleMatch::Interrupt(_))));
    }

    #[rstest::rstest]
    #[test]
    fn an_aborted_response_retains_the_count() {
        // Given an installed matcher holding rules, on a session one
        // interrupt short of its budget.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();
        {
            let mut response = rules.new_session(&session_id).expect("session");
            for _ in 0..MAX_INTERRUPTS_PER_SESSION - 1 {
                assert!(matches!(
                    response.check("a", StreamContext::text()),
                    Some(RuleMatch::Interrupt(_))
                ));
            }
        }

        // When the response that was interrupted ends as interrupted.
        rules.end_response(&session_id, TurnEnd::Aborted);
        let mut next = rules.new_session(&session_id).expect("session");

        // Then the count carried over and the next violation trips, so
        // consecutive interrupts accumulate rather than resetting per response.
        assert!(
            next.check("a", StreamContext::text())
                .expect("a match")
                .is_budget_spent(),
            "an interrupted response must not repay the count"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_count_is_per_session() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let first_id = jinn_core_types::SessionId::new();
        let second_id = jinn_core_types::SessionId::new();
        {
            let mut first = rules.new_session(&first_id).expect("session");
            for _ in 0..=MAX_INTERRUPTS_PER_SESSION {
                first.check("a", StreamContext::text());
            }
        }

        // When another session's response runs.
        let mut second = rules.new_session(&second_id).expect("session");

        // Then it is unaffected by the first session's exhausted budget.
        assert!(matches!(
            second.check("a", StreamContext::text()),
            Some(RuleMatch::Interrupt(_))
        ));
    }

    #[rstest::rstest]
    #[test]
    fn the_matcher_resolves_through_the_registry_by_slot_key() {
        // Given a registry holding the stream-rules cell.
        let slices = Slices::new();
        let cell = slices
            .register(super::stream_rules_slot(), StreamRules::empty())
            .expect("cell registers");

        // When a producer installs a matcher into it.
        cell.update(|rules| rules.install(std::sync::Arc::new(AlwaysSet::new(false))));

        // Then a consumer resolving by slot key sees it.
        let reader = slices
            .reader::<StreamRules>(&super::stream_rules_slot())
            .expect("cell resolves");
        assert!(reader.read().is_installed());
    }
}
