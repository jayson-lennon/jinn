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
    use crate::matcher::CompiledSet;
    use jinn_config::ConfigLayer;
    use jinn_preferences_config::schemas::FAIL_TOOL_TRIGGER;
    use jinn_slices::StreamRuleSet;

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
            pattern = 'npm run release'
            message = 'Run the package test, not the whole workspace.'
        "#,
        );

        // When the rules are read.
        let rules = read_rules(&config);

        // Then the rule is scoped to the project that declared it.
        assert!(
            rules.iter().any(|r| {
                r.conditions == vec!["npm run release".to_owned()]
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

    /// A config carrying a global rule and a project-scoped one.
    fn mixed_project_config() -> ConfigLayer {
        layer(
            r#"
            [[stream_rules.entry]]
            name = 'global-rule'
            conditions = ['rm -rf']
            scopes = ['tool:bash']
            on_trigger = 'fail_tool'
            body = 'Never.'

            [[stream_rules.entry]]
            name = 'myapp-rule'
            conditions = ['npm\s+run\s+release']
            scopes = ['tool:bash']
            on_trigger = 'fail_tool'
            project = 'myapp'
            body = 'Run the checked-in script instead.'
        "#,
        )
    }

    #[rstest::rstest]
    #[test]
    fn a_global_and_a_project_rule_both_read_from_one_list() {
        // Given a file with a global rule and a project-scoped one.
        let config = mixed_project_config();

        // When the rules are read.
        let rules = read_rules(&config);

        // Then both are present, each with its own scope intact.
        assert_eq!(rules.len(), 2);
        let global = rules.iter().find(|r| r.name == "global-rule");
        assert_eq!(global.and_then(|r| r.project.as_deref()), None);
        let scoped = rules.iter().find(|r| r.name == "myapp-rule");
        assert_eq!(scoped.and_then(|r| r.project.as_deref()), Some("myapp"));
    }

    #[rstest::rstest]
    #[test]
    fn a_global_rule_merged_with_a_project_rule_keeps_both_blocks() {
        // Given the same two rules, compiled for the scoped project.
        let set = crate::matcher::CompiledSet::build(&read_rules(&mixed_project_config()))
            .for_project(std::path::Path::new("/home/dev/code/myapp"));

        // When each command is offered to the executor in that project.
        let global = set.deny_tool_call("bash", r#"{"command":"rm -rf /"}"#);
        let scoped = set.deny_tool_call("bash", r#"{"command":"npm run release"}"#);

        // Then both are denied: merging adds the project rule, it does not
        // displace the global one.
        assert_eq!(global.map(|hit| hit.name), Some("global-rule".to_owned()));
        assert_eq!(scoped.map(|hit| hit.name), Some("myapp-rule".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn a_project_rule_merged_with_a_global_rule_is_silent_outside_its_project() {
        // Given the same two rules, resolved for a project that is not myapp.
        let set = crate::matcher::CompiledSet::build(&read_rules(&mixed_project_config()))
            .for_project(std::path::Path::new("/home/dev/code/other"));

        // When the global rule's command is offered there.
        let global = set.deny_tool_call("bash", r#"{"command":"rm -rf /"}"#);

        // Then it is still denied: a project-scoped rule cannot lift a global.
        assert_eq!(global.map(|hit| hit.name), Some("global-rule".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn a_legacy_and_a_stream_rule_merge_into_one_set() {
        // Given a file with the removed section beside a real stream rule.
        let config = layer(
            r#"
            [[tools.bash_command_policy]]
            pattern = 'chmod 777'
            message = 'World-writable.'

            [[stream_rules.entry]]
            name = 'no-todo'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it.'
        "#,
        );

        // When the rules are read and compiled.
        let rules = read_rules(&config);
        let set = crate::matcher::CompiledSet::build(&rules);

        // Then both are live: the legacy one denies, the stream one interrupts.
        let denied = set.deny_tool_call("bash", r#"{"command":"chmod 777 x"}"#);
        assert!(
            denied.is_some(),
            "the converted legacy rule must still deny"
        );
        assert!(
            rules.iter().any(|r| r.name == "no-todo"),
            "the stream rule must survive the merge"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_legacy_project_rule_merges_with_a_global_one() {
        // Given a legacy global rule beside a legacy project rule.
        let config = layer(
            r#"
            [[tools.bash_command_policy]]
            pattern = 'drop\s+table'
            message = 'No.'

            [[project.entry]]
            path = "/home/dev/code/myapp"
            [[project.entry.command_policy]]
            pattern = 'truncate'
            message = 'Not here.'
        "#,
        );

        // When the rules are read and compiled for that project.
        let set = crate::matcher::CompiledSet::build(&read_rules(&config))
            .for_project(std::path::Path::new("/home/dev/code/myapp"));

        // Then both deny: the project rule adds its block beside the global.
        let global = set.deny_tool_call("bash", r#"{"command":"drop table t"}"#);
        let scoped = set.deny_tool_call("bash", r#"{"command":"truncate t"}"#);
        assert!(global.is_some(), "the legacy global rule must still deny");
        assert!(
            scoped.is_some(),
            "the legacy project rule must deny in its project"
        );
    }
}
