//! Reading `[[stream_rules.entry]]` from `jinn.toml`.
//!
//! One function, because the section is read once at activation and never
//! cached: a reload is observed by the next launch, and a rule the user
//! deleted is not left armed by a stale copy.
//!
//! This is also where the removed `[[tools.bash_command_policy]]` section is
//! read. Its rules became stream rules carrying `on_trigger = "fail_tool"`,
//! which is the same behaviour under a name that also covers the rest of the
//! rule surface — so a user's existing patterns keep working without their
//! file being rewritten.

use jinn_config::ConfigLayer;
use jinn_preferences_config::schemas::{
    FAIL_TOOL_TRIGGER, LegacyCommandPolicyRule, LegacyProjectConfig, StreamRuleConfig,
};

/// Reads the configured stream rules, in file order.
///
/// A malformed section reads as no rules rather than failing activation: a
/// typo in the rules list must not stop jinn from starting.
pub fn read_rules(config: &ConfigLayer) -> Vec<StreamRuleConfig> {
    let mut rules = match config.get_list::<StreamRuleConfig>() {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "stream rules section is malformed, continuing with no stream rules"
            );
            Vec::new()
        }
    };
    rules.extend(read_legacy_command_policy(config));
    rules
}

/// Converts a removed `[[tools.bash_command_policy]]` entry into the stream
/// rule that replaced it.
///
/// The conversion is at read time on purpose. Rewriting the user's file would
/// discard the comments explaining why each pattern is shaped the way it is —
/// the shipped patterns carry several paragraphs of exactly that — and this
/// project preserves comments on save for the same reason.
///
/// A `command_policy` list nested under a `[[project.entry]]` is handled the
/// same way, scoped to that project, so a project's own rules survive too.
fn read_legacy_command_policy(config: &ConfigLayer) -> Vec<StreamRuleConfig> {
    let global = config.get_list::<LegacyCommandPolicyRule>();
    let projects = config.get_list::<LegacyProjectConfig>();

    if global.is_err() && projects.is_err() {
        return Vec::new();
    }

    tracing::warn!(
        key = "tools.bash_command_policy",
        replacement = "stream_rules.entry with on_trigger = \"fail_tool\"",
        "`[[tools.bash_command_policy]]` is now a stream rule with on_trigger = \
         \"fail_tool\"; your patterns still apply, and [[project.entry]].command_policy \
         becomes a rule's `project` field"
    );

    let mut converted = Vec::new();
    if let Ok(rules) = global {
        converted.extend(rules.iter().map(|rule| convert(rule, None)));
    }
    converted.extend(convert_project_rules(&projects.unwrap_or_default()));
    converted
}

/// Converts the `command_policy` list each legacy project entry carried.
///
/// Each becomes a rule carrying that project's path in its `project` field,
/// which is what a rule's project scope means today — so a project's own rules
/// keep applying in that project and nowhere else.
fn convert_project_rules(projects: &[LegacyProjectConfig]) -> Vec<StreamRuleConfig> {
    projects
        .iter()
        .flat_map(|project| {
            project
                .command_policy
                .iter()
                .map(|rule| convert(rule, Some(&project.path)))
        })
        .collect()
}

/// The stream rule equivalent of one legacy command-policy rule.
fn convert(rule: &LegacyCommandPolicyRule, project: Option<&std::path::Path>) -> StreamRuleConfig {
    StreamRuleConfig {
        name: format!("bash-policy: {}", rule.pattern),
        description: String::new(),
        conditions: vec![rule.pattern.clone()],
        // A legacy rule was about one tool, so it must be scoped to that tool
        // or the `fail_tool` requirement rejects it and the rule does nothing.
        scopes: vec!["tool:bash".to_owned()],
        body: rule.message.clone(),
        on_trigger: Some(FAIL_TOOL_TRIGGER.to_owned()),
        project: project.map(|p| p.to_string_lossy().into_owned()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::read_rules;
    use jinn_config::ConfigLayer;
    use jinn_preferences_config::schemas::FAIL_TOOL_TRIGGER;

    /// Reads `doc` as a config layer.
    fn layer(doc: &str) -> ConfigLayer {
        jinn_config::testutil::config_layer(doc)
    }

    #[rstest::rstest]
    #[test]
    fn a_legacy_command_policy_rule_becomes_a_failing_tool_rule() {
        // Given the removed section, with one rule.
        let config = layer(
            r#"
            [[tools.bash_command_policy]]
            pattern = 'rm -rf /'
            message = 'That is the whole filesystem.'
        "#,
        );

        // When the rules are read.
        let rules = read_rules(&config);

        // Then it is a stream rule that denies the bash tool.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].conditions, vec!["rm -rf /".to_owned()]);
        assert_eq!(
            rules[0].on_trigger.as_deref(),
            Some(FAIL_TOOL_TRIGGER),
            "a legacy rule blocked the command, so it must deny"
        );
        assert_eq!(rules[0].scopes, vec!["tool:bash".to_owned()]);
    }

    #[rstest::rstest]
    #[test]
    fn a_legacy_command_policy_keeps_its_message() {
        // Given the removed section, with a rule carrying guidance.
        let config = layer(
            r#"
            [[tools.bash_command_policy]]
            pattern = 'chmod 777'
            message = 'That makes the file world-writable.'
        "#,
        );

        // When the rules are read.
        let rules = read_rules(&config);

        // Then the guidance the author wrote is what the model will be told.
        assert_eq!(rules[0].body, "That makes the file world-writable.");
    }

    #[rstest::rstest]
    #[test]
    fn a_projects_own_command_policy_becomes_a_project_scoped_rule() {
        // Given two projects, the second carrying its own blocked-command rule.
        let config = layer(
            r#"
            [[project.entry]]
            path = "/home/dev/code/myapp"

            [[project.entry]]
            path = "/home/dev/code/other"
            [[project.entry.command_policy]]
            pattern = 'cargo test --workspace'
            message = 'Run the package test, not the whole workspace.'
        "#,
        );

        // When the rules are read.
        let rules = read_rules(&config);

        // Then the rule is scoped to the project that declared it.
        assert!(
            rules.iter().any(|r| {
                r.conditions == vec!["cargo test --workspace".to_owned()]
                    && r.project.as_deref() == Some("/home/dev/code/other")
            }),
            "a project's own rule must keep its project scope, got: {:?}",
            rules
                .iter()
                .map(|r| (&r.conditions, &r.project))
                .collect::<Vec<_>>()
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_file_with_no_command_policy_reads_only_its_stream_rules() {
        // Given a file with a stream rule and nothing legacy.
        let config = layer(
            r#"
            [[stream_rules.entry]]
            name = "no-todo"
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it or remove it.'
        "#,
        );

        // When the rules are read.
        let rules = read_rules(&config);

        // Then the rule is there and the migration contributed nothing.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "no-todo");
        assert_eq!(rules[0].on_trigger, None);
    }
}
