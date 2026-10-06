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
//! ## A match is a fact, not a decision
//!
//! [`StreamRuleSession::check`] answers exactly one question: did a rule match,
//! and which one. It holds no interrupt count, knows no budget, and decides
//! nothing about cancelling a stream. Those are policy questions that span a
//! whole turn — a turn streams many responses, and an interrupt is itself a
//! response boundary — so they belong to the watchdog actor that watches the
//! turn (`jinn-watchdog`), not to a per-delta predicate. A counter threaded
//! through here would have to be shared, mutable, and consulted per delta to
//! answer a question that is only asked once per response.
//!
//! ## Buffers are per response
//!
//! A turn and an assistant response are not the same span: one turn streams
//! many responses as the tool loop runs. The accumulation buffers are minted
//! per response and dropped with it, which is what makes "reset at the start of
//! every assistant response" true by construction — a retry cannot inherit an
//! aborted attempt's text, because it cannot reach that attempt's buffers.
//!
//! ## What is deliberately absent
//!
//! No interrupt count, no budget constant, no turn-end enum, and no
//! cancel-or-resume outcome variant live in this module or in the matcher
//! behind it. A monitor that owns its state in the vocabulary crate is a
//! monitor the matcher cannot avoid consulting, and a matcher that decides to
//! end a session cannot be tested without one.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use jinn_core_types::SessionId;

use crate::slices::SlotKey;

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

/// One assistant response's worth of matcher state: the accumulation buffers
/// for each stream, plus a handle on its session's interrupt count.
///
/// Minted per stream run and dropped with it.
pub trait StreamRuleSession: fmt::Debug + Send {
    /// Accumulates `delta` and returns the rule it trips, if any.
    ///
    /// Returns the first match in configuration order: when two rules match
    /// the same delta, the one written first in `jinn.toml` wins, since that
    /// is the file the user would expect to be authoritative.
    ///
    /// A fired rule is a fact and nothing more. Whether a session that has
    /// been corrected too many times in a row should be cancelled is decided
    /// by the stream-rule watchdog, from the response-completion signal an
    /// intercept already publishes.
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
    /// The returned session is owned and `'static`, not borrowed: it is
    /// minted inside a cell read guard that does not outlive the call, so a
    /// borrowing session could never be handed to the stream task. Owning its
    /// compiled rules outright is what makes that possible — the response
    /// holds an `Arc` of them, not a reference to this set.
    fn new_session(&self, session: &SessionId) -> Box<dyn StreamRuleSession + 'static>;

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

    /// The installed set, unscoped.
    ///
    /// The counterpart to [`Self::for_project`] for a session with no project
    /// association to narrow against. Returning `None` for an uninstalled cell
    /// keeps absence and emptiness reading the same way at every entry point.
    #[must_use]
    pub fn installed(&self) -> Option<Arc<dyn StreamRuleSet>> {
        self.0.clone()
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
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::{RuleFired, StreamContext, StreamRuleSession, StreamRuleSet, StreamRules};
    use crate::Slices;

    /// A matcher that fires on every delta, to exercise the cell and the
    /// per-response buffer split without compiling a regex.
    struct AlwaysSet {
        empty: bool,
    }

    impl AlwaysSet {
        fn new(empty: bool) -> Self {
            Self { empty }
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

        fn new_session(&self, _session: &jinn_core_types::SessionId) -> Box<dyn StreamRuleSession> {
            Box::new(AlwaysSession {
                buffers: std::collections::HashMap::new(),
            })
        }

        fn for_project(&self, _project: &std::path::Path) -> std::sync::Arc<dyn StreamRuleSet> {
            std::sync::Arc::new(Self { empty: self.empty })
        }
    }

    #[derive(Debug)]
    struct AlwaysSession {
        buffers: std::collections::HashMap<String, String>,
    }

    impl StreamRuleSession for AlwaysSession {
        fn check(&mut self, delta: &str, ctx: StreamContext<'_>) -> Option<RuleFired> {
            let key = format!("{:?}", ctx.source);
            self.buffers.entry(key.clone()).or_default().push_str(delta);
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
    fn a_match_carries_only_the_rule_that_fired() {
        // Given an installed matcher holding rules.
        let mut rules = StreamRules::empty();
        rules.install(std::sync::Arc::new(AlwaysSet::new(false)));

        // When a delta is checked.
        let mut session = rules
            .new_session(&jinn_core_types::SessionId::new())
            .expect("session");
        let fired = session.check("the offending text", StreamContext::text());

        // Then the value is the fired rule itself, carrying what the caller
        // needs to name it and to resume the turn with its guidance. There is
        // no second outcome to match on: a match is a fact, and whether it
        // ends the session is the watchdog's call.
        let fired = fired.expect("a match");
        assert_eq!(fired.body, "the offending text");
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
