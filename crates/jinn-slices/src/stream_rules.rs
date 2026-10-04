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
//! - **Fire counts** are per turn. A rule the model ignored once must be able
//!   to fire again on the resumed output, or the single correction the user
//!   asked for would be the only one. The cap exists for the opposite
//!   failure — a rule whose condition matches the guidance it injects, which
//!   would otherwise interrupt forever — so it bounds repeats without
//!   disarming the rule.
//!
//! Splitting the two is why [`StreamRuleSet::end_turn`] exists: the turn's
//! record outlives every response in it and is dropped when the turn ends.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use jinn_core_types::SessionId;

use crate::slices::SlotKey;

/// How many times one rule may fire within a single turn.
///
/// Three is enough for a model that needed a second reminder and bounded
/// enough that a rule matching its own injected guidance cannot loop.
pub const MAX_FIRES_PER_RULE_PER_TURN: usize = 3;

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

/// How many times each rule has fired in the turn a session belongs to.
///
/// Shared between every response of one turn, so a second assistant response
/// can see what the first already tripped. Mutex-guarded because the set is
/// shared across turns while the counts are keyed per session.
#[derive(Default)]
pub struct TurnFires(Mutex<HashMap<String, usize>>);

impl fmt::Debug for TurnFires {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(
            &*self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            f,
        )
    }
}

impl TurnFires {
    /// Records a fire of `rule` and reports whether it is permitted.
    ///
    /// Returns `false` once the rule has reached
    /// [`MAX_FIRES_PER_RULE_PER_TURN`], without recording another: the
    /// cap must bound the interruptions, not the bookkeeping.
    pub fn record(&self, rule: &str) -> bool {
        let mut counts = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = counts.entry(rule.to_owned()).or_insert(0);
        if *count >= MAX_FIRES_PER_RULE_PER_TURN {
            return false;
        }
        *count += 1;
        true
    }

    /// How many times `rule` has fired this turn.
    #[must_use]
    pub fn count(&self, rule: &str) -> usize {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(rule)
            .copied()
            .unwrap_or(0)
    }
}

/// One assistant response's worth of matcher state: the accumulation buffers
/// for each stream, plus a handle on its turn's fire record.
///
/// Minted per stream run and dropped with it.
pub trait StreamRuleSession: fmt::Debug + Send {
    /// Accumulates `delta` and returns the rule it trips, if any.
    ///
    /// Returns the first match in configuration order: when two rules match
    /// the same delta, the one written first in `jinn.toml` wins, since that
    /// is the file the user would expect to be authoritative.
    fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleFired>;

    /// The buffers' current contents, keyed by stream — for tests and
    /// diagnostics.
    fn buffers(&self) -> &HashMap<String, String>;
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

    /// Ends `session`'s turn, dropping its fire record.
    fn end_turn(&self, session: &SessionId);

    /// The rule denying a completed call named `tool_name` with
    /// `arguments`, if one matches.
    ///
    /// The tool executor consults this against the call's complete arguments
    /// before spawning anything, so a `fail_tool` rule denies a command with
    /// the same content whether the provider streamed its arguments in one
    /// piece or a thousand. Returns `None` when no such rule matches, which
    /// is every rule set with no `fail_tool` rule in it.
    fn deny_tool_call(&self, tool_name: &str, arguments: &str) -> Option<RuleFired>;

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

    /// Ends `session`'s turn, dropping its fire record.
    pub fn end_turn(&self, session: &SessionId) {
        if let Some(set) = self.0.as_ref() {
            set.end_turn(session);
        }
    }

    /// The rule denying a completed tool call, if one matches.
    pub fn deny_tool_call(&self, tool_name: &str, arguments: &str) -> Option<RuleFired> {
        let set = self.0.as_ref()?;
        if set.is_empty() {
            return None;
        }
        set.deny_tool_call(tool_name, arguments)
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
        MAX_FIRES_PER_RULE_PER_TURN, RuleFired, StreamContext, StreamRuleSession, StreamRuleSet,
        StreamRules, TurnFires,
    };
    use crate::Slices;

    /// A matcher that fires on every delta, to exercise the cell, the turn
    /// record and the per-response split without compiling a regex.
    struct AlwaysSet {
        empty: bool,
        turns: std::sync::Mutex<
            std::collections::HashMap<jinn_core_types::SessionId, std::sync::Arc<TurnFires>>,
        >,
    }

    impl AlwaysSet {
        fn new(empty: bool) -> Self {
            Self {
                empty,
                turns: std::sync::Mutex::new(std::collections::HashMap::new()),
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
            let mut turns = self.turns.lock().unwrap();
            let fires = std::sync::Arc::clone(
                turns
                    .entry(session.clone())
                    .or_insert_with(|| std::sync::Arc::new(TurnFires::default())),
            );
            Box::new(AlwaysSession {
                fires,
                buffers: std::collections::HashMap::new(),
            })
        }

        fn end_turn(&self, session: &jinn_core_types::SessionId) {
            let mut turns = self.turns.lock().unwrap();
            turns.remove(session);
        }

        fn deny_tool_call(&self, _tool_name: &str, _arguments: &str) -> Option<RuleFired> {
            None
        }

        fn for_project(&self, _project: &std::path::Path) -> std::sync::Arc<dyn StreamRuleSet> {
            std::sync::Arc::new(AlwaysSet::new(self.empty))
        }
    }

    #[derive(Debug)]
    struct AlwaysSession {
        fires: std::sync::Arc<TurnFires>,
        buffers: std::collections::HashMap<String, String>,
    }

    impl StreamRuleSession for AlwaysSession {
        fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleFired> {
            let key = format!("{:?}", ctx.source);
            self.buffers.entry(key.clone()).or_default().push_str(delta);
            if !self.fires.record(&key) {
                return None;
            }
            Some(RuleFired {
                name: key,
                description: String::new(),
                body: delta.to_owned(),
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
    fn a_rule_fires_again_on_a_later_response_of_the_same_turn() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();

        // When two responses of one turn both violate the same rule.
        let mut first = rules.new_session(&session_id).expect("session");
        assert!(first.check("a", StreamContext::text()).is_some());
        let mut second = rules.new_session(&session_id).expect("session");

        // Then the second response can fire it too.
        assert!(second.check("b", StreamContext::text()).is_some());
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_stops_firing_at_the_turn_cap() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();

        // When one response violates the same rule past the cap.
        let mut session = rules.new_session(&session_id).expect("session");
        let fired: Vec<bool> = (0..MAX_FIRES_PER_RULE_PER_TURN + 1)
            .map(|_| session.check("a", StreamContext::text()).is_some())
            .collect();

        // Then it fires up to the cap and is ignored after it.
        assert_eq!(
            fired,
            vec![true, true, true, false],
            "a rule must fire {MAX_FIRES_PER_RULE_PER_TURN} times and then stop"
        );
    }

    #[rstest::rstest]
    #[test]
    fn ending_a_turn_clears_its_fire_record() {
        // Given an installed matcher holding rules, on a session whose turn
        // has already exhausted the cap.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let session_id = jinn_core_types::SessionId::new();
        {
            let mut session = rules.new_session(&session_id).expect("session");
            for _ in 0..MAX_FIRES_PER_RULE_PER_TURN {
                session.check("a", StreamContext::text());
            }
            assert!(session.check("a", StreamContext::text()).is_none());
        }

        // When the turn ends and the next one begins.
        rules.end_turn(&session_id);
        let mut next = rules.new_session(&session_id).expect("session");

        // Then the new turn starts from a clean record.
        assert!(next.check("a", StreamContext::text()).is_some());
    }

    #[rstest::rstest]
    #[test]
    fn the_fire_record_is_per_session() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));
        let first_id = jinn_core_types::SessionId::new();
        let second_id = jinn_core_types::SessionId::new();
        {
            let mut first = rules.new_session(&first_id).expect("session");
            for _ in 0..MAX_FIRES_PER_RULE_PER_TURN {
                first.check("a", StreamContext::text());
            }
        }

        // When another session's turn runs.
        let mut second = rules.new_session(&second_id).expect("session");

        // Then it is unaffected by the first session's exhausted cap.
        assert!(second.check("a", StreamContext::text()).is_some());
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
