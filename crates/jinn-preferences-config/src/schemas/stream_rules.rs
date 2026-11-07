//! Stream-rule configuration — the `jinn.toml` `[[stream_rules.entry]]`
//! rules, the `disabled_rules` name list under `[stream_rules]`, and the
//! removed `[stream_rules] max_interrupts` budget.
//!
//! Three shapes, because the section had three jobs. The rules are a list,
//! each entry one rule; the disabled state is a scalar list of names living
//! beside them under the same `[stream_rules]` header; the budget was a
//! scalar tuning the interrupt loop's bound. The rules and the name list
//! are live. The budget is a legacy read-only shape, kept only so a
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyStreamRulesBudget {
    /// Consecutive interrupts tolerated before the stream is cancelled.
    ///
    /// Zero is the derived default and means *absent*, which is what
    /// distinguishes "no old key in the file" from "the old key set to
    /// something": a caller that sees zero must ignore the section, not honour
    /// a threshold of zero.
    #[serde(default)]
    pub max_interrupts: usize,
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
/// # Behavior: Event-Dependent.
///
/// The compiled matcher is an artifact; a bus handler recompiles it when
/// the reload event lands (a bad file is refused there and the old
/// matcher stays live).
///
/// An absent or empty `scopes` admits every stream — assistant prose,
/// reasoning, and serialized tool-call arguments alike. Naming scopes
/// narrows that to where the rule is meaningful: a rule about TypeScript
/// belongs on tool arguments, not on prose.
///
/// A rule missing a usable `body` is skipped with a warning rather than
/// interrupting a turn with nothing to say. A rule whose `name` appears in
/// [`StreamRulesConfig::disabled_rules`] is skipped silently — switching a
/// rule off is deliberate, not a defect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRuleConfig {
    /// The rule's identity, unique per session, and the name the
    /// interruption reports. This is also the token
    /// [`StreamRulesConfig::disabled_rules`] names to switch the rule off,
    /// which is how a rule defined outside `jinn.toml` — in a standalone
    /// `stream_rules.toml`, or converted from a removed legacy section — is
    /// addressed: it has no entry of its own to carry a flag.
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
    /// A path glob selecting the projects this rule applies in, anchored to
    /// the whole working directory.
    ///
    /// Absent or empty means every project, which is what a rule about a
    /// footgun in a shell needs — those habits are not repository-specific.
    /// A present value is tested whole-path against the session's working
    /// directory (with its trailing slash, so a `/**` suffix also covers the
    /// project directory itself), never against one path segment:
    ///
    /// - `**/myapp` — the `myapp` directory, exactly;
    /// - `**/myapp/**` — `myapp` itself and everything below it;
    /// - `**/jinn/image-tilth` — a nested project, named directly.
    ///
    /// A glob that cannot anchor to an absolute path — one starting with
    /// neither `**/`, an absolute `/`, nor a drive prefix — or one the glob
    /// engine rejects stops jinn at startup with an error naming the rule
    /// and the pattern. It would otherwise compile and then never fire.
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

/// The `[stream_rules]` section's scalar values — today, which rules are
/// switched off.
///
/// A rule is disabled exactly when its `name` appears here. The array lives
/// beside the `[[stream_rules.entry]]` list under the same header, which is
/// what lets it address rules the entry list cannot: a shipped rule, a rule
/// from a standalone `stream_rules.toml`, or one converted from a removed
/// legacy section has no entry here to carry a flag, but every rule has a
/// name.
///
/// # Behavior: Event-Dependent.
///
/// The compiled matcher is an artifact; a bus handler recompiles it when
/// the reload event lands (a bad file is refused there and the old
/// matcher stays live).
///
/// An absent or empty array means every rule is armed, and an emptied array
/// is removed from the file rather than written as `[]` — an armed rule
/// leaves no trace. Hand-written entries naming a rule that does not exist
/// are inert; names are matched at read time against whatever rule set
/// assembled.
///
/// This shape is read-on-demand and deliberately **not** registered in
/// [`register_all_sections`](crate::registration::register_all_sections)
/// (the same stance as [`LegacyStreamRulesBudget`]): nothing joins the
/// launch roster, so no whole-section write can ever delete the
/// `[[stream_rules.entry]]` list the table also carries.
///
/// A `disabled = true` key inside an entry — written by an earlier build of
/// this configuration — is **ignored, not honored and not migrated**. It
/// survives every save untouched as the user's own unknown key (the entry
/// field list no longer names it, so the patcher preserves it), and serde
/// ignores it on read. Honoring it would keep two sources of truth for one
/// concept, and a one-time rewrite of user files contradicts this project's
/// comment-preservation stance. There is no warning, matching the removed
/// `on_trigger` key's silent ignore.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRulesConfig {
    /// The names of the rules that never fire.
    ///
    /// Removing a name re-arms that rule on the next read or refresh.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_rules: Vec<String>,
}

impl jinn_config::Configurable for StreamRulesConfig {
    const KEY: &'static str = STREAM_RULES_BUDGET_KEY;
}

/// One rule, as written in the standalone `stream_rules.toml` document.
///
/// A superset of [`StreamRuleConfig`]: the same live fields, plus the two
/// test-fixture lists the standalone file carries. The production reader
/// never uses this type — it deserializes through [`StreamRuleConfig`],
/// whose serde treats the fixture fields as unknown keys and ignores them.
/// Only the test harness deserializes through this type, to drive the
/// fixtures through the matcher.
///
/// The standalone document wraps the list in the same `[[stream_rules.entry]]`
/// table key `jinn.toml` uses, so an entry pastes in either direction without
/// learning a second key name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamRuleFileEntry {
    /// The rule's identity, unique across the set, and the name the
    /// interruption reports. Fixtures assert against it by name.
    #[serde(default)]
    pub name: String,
    /// Prose description of what the rule forbids.
    #[serde(default)]
    pub description: String,
    /// Regexes tested against the accumulated stream buffer.
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Scope tokens naming where the rule applies.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The guidance injected when the rule fires.
    #[serde(default)]
    pub body: String,
    /// Same field as [`StreamRuleConfig::project`] — see there for the
    /// anchored-glob spelling rules and the fatal-at-boot policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Command strings that MUST trip this rule. Ignored by the runtime.
    #[serde(default)]
    pub positive: Vec<String>,
    /// Command strings that MUST NOT trip this rule. Ignored by the runtime.
    #[serde(default)]
    pub negative: Vec<String>,
}

impl StreamRuleFileEntry {
    /// The live rule this entry contributes, with its fixtures stripped.
    ///
    /// This is the projection the production reader effectively performs by
    /// deserializing through [`StreamRuleConfig`] instead; it exists for the
    /// harness, which checks that the projection is faithful.
    #[must_use]
    pub fn live_rule(&self) -> StreamRuleConfig {
        StreamRuleConfig {
            name: self.name.clone(),
            description: self.description.clone(),
            conditions: self.conditions.clone(),
            scopes: self.scopes.clone(),
            body: self.body.clone(),
            project: self.project.clone(),
        }
    }
}

/// The standalone `stream_rules.toml` document shape.
///
/// The file carries the same `[[stream_rules.entry]]` key `jinn.toml` uses,
/// which in document terms is a top-level `stream_rules` table holding an
/// `entry` array-of-tables — hence the nesting here.
///
/// It carries no `disabled_rules`: the name array is a `jinn.toml`-only
/// shape, so a stray one in a standalone rules file is just an ignored
/// unknown key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StreamRulesFile {
    /// The `stream_rules` table: this file's only content.
    #[serde(default)]
    pub stream_rules: StreamRulesTable,
}

/// The `stream_rules` table of a standalone rules document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StreamRulesTable {
    /// The rules, in file order.
    #[serde(default)]
    pub entry: Vec<StreamRuleFileEntry>,
}

impl StreamRulesFile {
    /// The rules, in file order.
    #[must_use]
    pub fn entries(&self) -> &[StreamRuleFileEntry] {
        &self.stream_rules.entry
    }
}

/// Parses a standalone `stream_rules.toml` document into its entries.
///
/// The document carries the same `[[stream_rules.entry]]` key `jinn.toml`
/// uses. Fixture fields (`positive`, `negative`) survive here and are the
/// caller's to strip via [`StreamRuleFileEntry::live_rule`].
///
/// # Errors
///
/// Returns the TOML error when `text` is not a parseable rules document.
pub fn parse_stream_rules_file(text: &str) -> Result<StreamRulesFile, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn a_file_carrying_the_old_key_is_still_read() {
        // Given a document written before the budget moved.

        // When the document is read.
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

        // When the document is read.
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

        // When the rules are read.
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

    #[rstest::rstest]
    #[test]
    fn an_absent_disabled_rules_array_reads_empty() {
        // Given a document with rules but no `disabled_rules` key.

        // When the document is read.
        let config: StreamRulesConfig = jinn_config::testutil::config_layer(
            r#"
            [[stream_rules.entry]]
            name = 'no-todo'
            conditions = ['TODO']
            body = 'Finish it.'
        "#,
        )
        .get()
        .expect("read");

        // Then every rule is armed: absence reads empty, never an error.
        assert_eq!(config.disabled_rules, [] as [std::string::String; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn a_written_disabled_rules_array_round_trips() {
        // Given a document carrying the name array beside its rules.

        // When the document is read.
        let config: StreamRulesConfig = jinn_config::testutil::config_layer(
            r#"
            [stream_rules]
            disabled_rules = ['no-todo', 'no-yelling']

            [[stream_rules.entry]]
            name = 'no-todo'
            conditions = ['TODO']
            body = 'Finish it.'
        "#,
        )
        .get()
        .expect("read");

        // Then both names survive the table sharing the section with the
        // entry list.
        assert_eq!(
            config.disabled_rules,
            vec!["no-todo".to_owned(), "no-yelling".to_owned()]
        );
    }

    #[rstest::rstest]
    #[test]
    fn an_entry_carrying_the_retired_disabled_key_still_deserializes() {
        // Given an entry written by a build that persisted the per-entry
        // flag, beside the name array that replaced it.

        // When the rules are read.
        let rules: Vec<StreamRuleConfig> = jinn_config::testutil::config_layer(
            r#"
            [[stream_rules.entry]]
            name = 'no-todo'
            conditions = ['TODO']
            body = 'Finish it.'
            disabled = true
        "#,
        )
        .get_list()
        .expect("read");

        // Then the rule reads intact and armed: the stale key is the user's
        // own unknown key — ignored, not honored, and never migrated.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "no-todo");
    }
}
