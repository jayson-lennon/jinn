//! Stream-rule configuration — the `jinn.toml` `[stream_rules]` section and
//! its `[[stream_rules.entry]]` rules.
//!
//! Two shapes, because the section has two jobs. The rules are a list, each
//! entry one rule; the budget is a scalar tuning the interrupt loop's bound.
//! They are declared apart so a rule's own fields and the section's knob do
//! not read as one another's.
//!
//! Pure serde data. Compilation — turning `conditions`, `scopes`, and `project`
//! into a matcher — lives in the `jinn-stream-rules` slice, which reads these
//! shapes.

use serde::{Deserialize, Serialize};

/// The `jinn.toml` key the stream rules' budget lives at.
///
/// Dotted on purpose, matching the sibling sections' convention: the dot is
/// what separates the section from its list, so `[stream_rules]` and
/// `[[stream_rules.entry]]` can coexist in one file.
pub const STREAM_RULES_BUDGET_KEY: &str = "stream_rules";

/// Default number of consecutive interrupts tolerated before the stream is
/// cancelled.
///
/// Three is enough for a model that needed a second reminder, and bounded
/// enough that a rule whose condition also matches its own injected guidance
/// cannot loop.
pub const DEFAULT_MAX_INTERRUPTS: usize = 3;

/// The interrupt budget for the stream loop.
///
/// Serialized as `[stream_rules]` in `jinn.toml`. The rules themselves are
/// unaffected by this section's absence: an absent budget is the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRulesConfig {
    /// Consecutive interrupts tolerated before the stream is cancelled. A
    /// response that completes without an interrupt debits the count by one.
    ///
    /// Default: 3.
    #[serde(default = "default_max_interrupts")]
    pub max_interrupts: usize,
}

fn default_max_interrupts() -> usize {
    DEFAULT_MAX_INTERRUPTS
}

impl StreamRulesConfig {
    /// The trip threshold for the interrupt accumulator.
    ///
    /// A zero maximum is nonsense — the loop would cancel on the first
    /// interrupt regardless of what the user asked for — so a configured zero
    /// is floored at one rather than honoured. The same shape the tool-call
    /// watchdog uses for its own floor.
    #[must_use]
    pub fn effective_max_interrupts(&self) -> usize {
        self.max_interrupts.max(1)
    }
}

impl Default for StreamRulesConfig {
    fn default() -> Self {
        Self {
            max_interrupts: DEFAULT_MAX_INTERRUPTS,
        }
    }
}

impl jinn_config::Configurable for StreamRulesConfig {
    const KEY: &'static str = STREAM_RULES_BUDGET_KEY;
}

/// The `jinn.toml` key the stream rules live at.
///
/// Dotted on purpose: the patcher resolves a list's parent table by the
/// head of the key and refuses to drop unmatched entries from a
/// single-segment one, so a bare `rules` would leave every deleted rule in
/// the file forever.
pub const STREAM_RULES_KEY: &str = "stream_rules.entry";

/// One rule, as written in `jinn.toml`.
///
/// An absent or empty `scopes` admits every stream — assistant prose,
/// reasoning, and serialized tool-call arguments alike. Naming scopes
/// narrows that to where the rule is meaningful: a rule about TypeScript
/// belongs on tool arguments, not on prose.
///
/// A rule missing a usable `body` is skipped with a warning rather than
/// interrupting a turn with nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRuleConfig {
    /// The rule's identity, unique per session, and the name the
    /// interruption reports.
    pub name: String,
    /// Prose description of what the rule forbids. Carried for the user's
    /// own benefit — jinn reads the `body`, not this.
    #[serde(default)]
    pub description: String,
    /// Regexes tested against the accumulated stream buffer.
    ///
    /// A pattern the regex engine rejects is dropped with a warning naming
    /// the rule, and the rule survives on its remaining patterns.
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Scope tokens naming where the rule applies.
    ///
    /// `text`, `thinking`, `tool`, and `tool:<name>(<glob>)` are the whole
    /// grammar; anything else is dropped with a warning. Absent or empty
    /// means every stream.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The markdown guidance injected when the rule fires.
    pub body: String,
    /// A path glob selecting the projects this rule applies in.
    ///
    /// Absent or empty means every project, which is what a rule about a
    /// footgun in a shell needs — those habits are not repository-specific.
    /// Naming a glob confines the rule to sessions whose working directory
    /// matches it, so a rule about one codebase's layout does not fire in
    /// another's.
    ///
    /// Precedence runs global-first: a rule matching every project is a floor
    /// that a project-scoped rule can add to, never lift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl jinn_config::ConfigList for StreamRuleConfig {
    const KEY: &'static str = STREAM_RULES_KEY;
    const ENTRY_KEY: &'static str = "name";
    const ENTRY_FIELDS: &'static [&'static str] = &[
        "name",
        "description",
        "conditions",
        "scopes",
        "body",
        "project",
    ];
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn an_absent_budget_is_the_default() {
        // Given a document with no `[stream_rules]` section.
        let config: StreamRulesConfig = jinn_config::testutil::config_layer("")
            .get()
            .expect("defaulted");

        // Then the budget is the documented default.
        assert_eq!(config.max_interrupts, DEFAULT_MAX_INTERRUPTS);
    }

    #[rstest::rstest]
    #[test]
    fn a_configured_budget_is_read() {
        // Given a document setting the budget.
        let config: StreamRulesConfig = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            max_interrupts = 5
        "#,
        )
        .get()
        .expect("read");

        // Then it is what the user asked for.
        assert_eq!(config.max_interrupts, 5);
    }

    #[rstest::rstest]
    #[test]
    fn a_zero_budget_is_floored_at_one() {
        // Given a document setting the budget to zero.
        let config = StreamRulesConfig { max_interrupts: 0 };

        // Then the threshold is one, because zero would cancel on the first
        // interrupt whatever the user meant.
        assert_eq!(config.effective_max_interrupts(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn the_budget_and_the_rules_coexist_in_one_file() {
        // Given a document carrying both the section and its rules.
        let layer = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            max_interrupts = 2

            [[stream_rules.entry]]
            name = 'no-todo'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it or remove it.'
        "#,
        );

        // When both are read.
        let config: StreamRulesConfig = layer.get().expect("read budget");
        let rules: Vec<StreamRuleConfig> = layer.get_list().expect("read rules");

        // Then each is read from the same file without disturbing the other.
        assert_eq!(config.max_interrupts, 2);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "no-todo");
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_naming_the_removed_trigger_key_still_deserializes() {
        // Given a rule written against the removed `on_trigger` key.
        let rules: Vec<StreamRuleConfig> = jinn_config::testutil::config_layer(
            r#"
            [[stream_rules.entry]]
            name = 'no-rm'
            conditions = ['rm -rf']
            scopes = ['tool:bash']
            on_trigger = 'fail_tool'
            body = 'Never.'
        "#,
        )
        .get_list()
        .expect("read");

        // Then the rule is intact: an unknown key is ignored, not a failure,
        // so a file written before the removal keeps its rules.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].conditions, vec!["rm -rf".to_owned()]);
    }
}
