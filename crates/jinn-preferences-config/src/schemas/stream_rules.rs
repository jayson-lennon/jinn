//! Stream-rule configuration — the `jinn.toml` `[[stream_rules.entry]]`
//! rules, and the removed `[stream_rules] max_interrupts` budget.
//!
//! Two shapes, because the section had two jobs. The rules are a list, each
//! entry one rule; the budget was a scalar tuning the interrupt loop's bound.
//! The rules are live. The budget is a legacy read-only shape, kept only so a
//! file written before the budget moved to
//! [`StreamRuleWatchdogConfig`](super::stream_rule_watchdog::StreamRuleWatchdogConfig)
//! keeps the value the user tuned.
//!
//! Pure serde data. Compilation — turning `conditions`, `scopes`, and `project`
//! into a matcher — lives in the `jinn-stream-rules` slice, which reads these
//! shapes.

use serde::{Deserialize, Serialize};

/// The `jinn.toml` key the stream rules live at.
///
/// Dotted on purpose: the patcher resolves a list's parent table by the
/// head of the key and refuses to drop unmatched entries from a
/// single-segment one, so a bare `rules` would leave every deleted rule in
/// the file forever.
pub const STREAM_RULES_KEY: &str = "stream_rules.entry";

/// The `jinn.toml` key the removed stream-rule budget lived at.
///
/// Named only so the migration warning can quote the key it is replacing.
/// The live key is
/// [`STREAM_RULE_WATCHDOG_KEY`](super::stream_rule_watchdog::STREAM_RULE_WATCHDOG_KEY).
pub const STREAM_RULES_BUDGET_KEY: &str = "stream_rules";

/// The removed `[stream_rules] max_interrupts` budget, as an older `jinn.toml`
/// wrote it.
///
/// This is the value shape of a section that no longer exists. It is kept only
/// so a file written before the budget moved to
/// `[watchdog.stream_rules] max_failures` keeps the threshold its owner tuned:
/// the watchdog slice reads it at activation and uses its value as its
/// maximum, warning about the migration.
///
/// Nothing writes this key, nothing enforces it directly, and nothing else
/// should depend on it — it exists at the boundary with an older file, and
/// goes away when that boundary does. In particular it is deliberately
/// **not** registered in
/// [`register_all_sections`](crate::registration::register_all_sections), so
/// no code path can write the file back with this key present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyStreamRulesBudget {
    /// Consecutive interrupts tolerated before the stream is cancelled.
    #[serde(default)]
    pub max_interrupts: usize,
}

impl Default for LegacyStreamRulesBudget {
    fn default() -> Self {
        Self { max_interrupts: 0 }
    }
}

impl LegacyStreamRulesBudget {
    /// The threshold an older file asked for, floored at one.
    ///
    /// The floor matches the live watchdog's own: a configured zero would
    /// cancel on the first interrupt, which is not what either default means.
    #[must_use]
    pub fn effective_max_interrupts(&self) -> usize {
        self.max_interrupts.max(1)
    }
}

impl jinn_config::Configurable for LegacyStreamRulesBudget {
    const KEY: &'static str = STREAM_RULES_BUDGET_KEY;
}

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
    /// Naming a glob confines the rule to sessions whose project association
    /// matches it, so a rule about one codebase's layout does not fire in
    /// another's.
    ///
    /// Bound once per stream, when the session's rule set is minted, rather
    /// than per delta: the project's stamp does not move under a running
    /// turn, so binding it at the mint point is sufficient and costs nothing
    /// per delta.
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
    fn a_file_carrying_the_old_key_is_still_read() {
        // Given a document written before the budget moved.
        let config: LegacyStreamRulesBudget = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            max_interrupts = 5
        "#,
        )
        .get()
        .expect("read");

        // Then the tuned threshold survives the move rather than silently
        // reverting to the new default.
        assert_eq!(config.max_interrupts, 5);
        assert_eq!(config.effective_max_interrupts(), 5);
    }

    #[rstest::rstest]
    #[test]
    fn an_absent_old_key_reads_zero_and_floors_at_one() {
        // Given a document with no `[stream_rules]` table at all.
        let config: LegacyStreamRulesBudget = jinn_config::testutil::config_layer("")
            .get()
            .expect("defaulted");

        // Then it reads the floor, never zero — zero would cancel the first
        // interrupt whatever the user meant.
        assert_eq!(config.effective_max_interrupts(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn the_old_budget_and_the_rules_coexist_in_one_file() {
        // Given a document carrying both the old budget and the rules.
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
        let budget: LegacyStreamRulesBudget = layer.get().expect("read budget");
        let rules: Vec<StreamRuleConfig> = layer.get_list().expect("read rules");

        // Then each is read from the same file without disturbing the other.
        assert_eq!(budget.max_interrupts, 2);
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
