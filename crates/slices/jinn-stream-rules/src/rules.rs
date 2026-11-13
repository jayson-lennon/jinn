//! Reading the enforced stream rules: assembly plus the disabled-name filter.
//!
//! The set itself is assembled in
//! [`jinn_preferences_config::assemble_stream_rules`], which every consumer
//! of the ruleset shares — this slice's install and refresh, the
//! stream-rule picker's list, and the storage boundary's unknown-name
//! check. Reading it here answers one question only: *which of those rules
//! fire right now*, by applying the `[stream_rules] disabled_rules` name
//! list from `jinn.toml`.
//!
//! The filter sits here — after every source has contributed, not inside
//! any one reader — because names address rules from *any* source: a
//! shipped rule or a `stream_rules.toml` rule has no `jinn.toml` entry to
//! carry a flag, but every rule has a name. A name matching nothing is
//! inert; removing a name re-arms its rule on the next read or refresh.
//! The filter is deliberately nowhere else: the picker lists the
//! unfiltered set (a disabled rule stays visible, seeded ✗ from array
//! membership) and name validation accepts the unfiltered set (a disabled
//! rule can be re-armed).

use jinn_config::ConfigLayer;
use jinn_preferences_config::assemble_stream_rules;
use jinn_preferences_config::schemas::{StreamRuleConfig, StreamRulesConfig};
use marked_path::{Absolute, MarkedPathBuf};

/// Reads the configured stream rules that will fire for a run.
///
/// `preferences_path` is where this run's `jinn.toml` lives; the optional
/// user rules file is the `stream_rules.toml` sibling of that document.
///
/// The result is the assembled set — embedded set, replaced wholesale by an
/// existing user file (malformed → warn and keep the embedded set), then
/// everything `jinn.toml` and the legacy sections contribute appended
/// after, deduped by name — minus every rule `disabled_rules` names.
pub fn read_rules(
    config: &ConfigLayer,
    preferences_path: &MarkedPathBuf<Absolute>,
) -> Vec<StreamRuleConfig> {
    let mut rules = assemble_stream_rules(config, preferences_path);

    let disabled = config
        .get::<StreamRulesConfig>()
        .map_or_default(|section| section.disabled_rules);
    // Silent skip on purpose: a deliberate disable is the requested
    // behaviour, so a name that matches nothing is inert rather than a
    // warning the user would see on every read.
    rules.retain(|rule| !disabled.iter().any(|name| name == &rule.name));
    rules
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

    use super::read_rules;
    use crate::matcher::CompiledSet;

    /// Compiles through the production build, panicking on a project-glob
    /// failure — these tests all feed compiling configs.
    fn compiled(configs: &[super::super::rules::StreamRuleConfig]) -> CompiledSet {
        CompiledSet::build(configs).expect("a test config compiles")
    }
    use jinn_config::ConfigLayer;
    use jinn_preferences_config::STREAM_RULES_FILE_NAME;
    use jinn_slices::{StreamContext, StreamRuleSet};

    /// Reads `doc` as a config layer.
    fn layer(doc: &str) -> ConfigLayer {
        jinn_config::testutil::config_layer(doc)
    }

    /// A `jinn.toml` path in a temp dir with no sibling rules file, for the
    /// tests that only exercise the jinn.toml half of the read.
    ///
    /// Returns the owned dir so the caller keeps it alive as long as the
    /// path is in use.
    fn temp_prefs() -> (tempfile::TempDir, MarkedPathBuf<Absolute>) {
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare filename");
        (dir.dir, path)
    }

    /// A `jinn.toml` path inside `dir`, so the sibling rules file resolves
    /// inside the same temp directory.
    fn prefs_path_in(dir: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
        dir.join("jinn.toml")
            .expect("temp root joins a bare filename")
    }

    /// Writes a user `stream_rules.toml` beside `prefs`, returning its path.
    fn write_user_rules(prefs: &MarkedPathBuf<Absolute>, body: &str) -> MarkedPathBuf<Absolute> {
        let path = prefs
            .parent()
            .expect("prefs path is absolute, so it has a parent")
            .join(STREAM_RULES_FILE_NAME)
            .expect("absolute parent joins a bare filename");
        std::fs::write(&path, body).expect("write user rules file");
        path
    }

    /// The names of the rules a read assembles, in order.
    fn assembled_names(config: &ConfigLayer, prefs: &MarkedPathBuf<Absolute>) -> Vec<String> {
        read_rules(config, prefs)
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn a_rule_naming_the_removed_trigger_key_still_fires() {
        // Given a rule written against the removed `on_trigger` key.
        let config = layer(
            r#"
            [[stream_rules.entry]]
            name = "no-rm"
            conditions = ['LEGACY-TRIGGER-CMD']
            scopes = ['tool:bash']
            body = 'Never.'
        "#,
        );

        // When the rule is compiled and the offending command streams.
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&config, &prefs));
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        let fired = session
            .check(
                r#"{"command":"LEGACY-TRIGGER-CMD"}"#,
                StreamContext::tool(0, "bash"),
            )
            .map(|hit| hit.name.clone());

        // Then it fires as a plain interrupt: the key is simply unknown now,
        // and a file that still carries it keeps working.
        assert_eq!(fired, Some("no-rm".to_owned()));
    }

    /// A config carrying a global rule and a project-scoped one.
    fn mixed_project_config() -> ConfigLayer {
        layer(
            r#"
            [[stream_rules.entry]]
            name = 'global-rule'
            conditions = ['GLOBAL-MARKER-CMD']
            scopes = ['tool:bash']
            body = 'Never.'

            [[stream_rules.entry]]
            name = 'myapp-rule'
            conditions = ['npm\s+run\s+release']
            scopes = ['tool:bash']
            project = '**/myapp'
            body = 'Run the checked-in script instead.'
        "#,
        )
    }

    #[rstest::rstest]
    #[test]
    fn a_global_rule_merged_with_a_project_rule_keeps_both_blocks() {
        // Given the same two rules, compiled for the scoped project. The
        // session cwd sits in a tempdir root shared with nothing else.
        let dir = jinn_testutil::tempdir();
        let myapp_cwd = dir
            .path
            .join("dev/code/myapp")
            .expect("temp root joins a bare name");
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&mixed_project_config(), &prefs)).for_cwd(Some(&myapp_cwd));

        // When each command's arguments stream in that project.
        let global = {
            let mut session = set.new_session(&jinn_core_types::SessionId::new());
            session
                .check(
                    r#"{"command":"GLOBAL-MARKER-CMD"}"#,
                    StreamContext::tool(0, "bash"),
                )
                .map(|hit| hit.name.clone())
        };
        let scoped = {
            let mut session = set.new_session(&jinn_core_types::SessionId::new());
            session
                .check(
                    r#"{"command":"npm run release"}"#,
                    StreamContext::tool(0, "bash"),
                )
                .map(|hit| hit.name.clone())
        };

        // Then both fire: merging adds the project rule, it does not displace
        // the global one.
        assert_eq!(global, Some("global-rule".to_owned()));
        assert_eq!(scoped, Some("myapp-rule".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn a_project_rule_merged_with_a_global_rule_is_silent_outside_its_project() {
        // Given the same two rules, resolved for a project that is not myapp:
        // a sibling directory under the same tempdir root.
        let dir = jinn_testutil::tempdir();
        let other_cwd = dir
            .path
            .join("dev/code/other")
            .expect("temp root joins a bare name");
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&mixed_project_config(), &prefs)).for_cwd(Some(&other_cwd));

        // When the global rule's command's arguments stream there.
        let global = {
            let mut session = set.new_session(&jinn_core_types::SessionId::new());
            session
                .check(
                    r#"{"command":"GLOBAL-MARKER-CMD"}"#,
                    StreamContext::tool(0, "bash"),
                )
                .map(|hit| hit.name.clone())
        };

        // Then it still fires: a project-scoped rule cannot lift a global.
        assert_eq!(global, Some("global-rule".to_owned()));
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
        let (_prefs_dir, prefs) = temp_prefs();
        let rules = read_rules(&config, &prefs);
        let set = compiled(&rules);

        // Then both are live: the legacy one fires on the tool stream, the
        // stream one fires on prose.
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        let legacy = session.check(
            r#"{"command":"chmod 777 x"}"#,
            StreamContext::tool(0, "bash"),
        );
        let mut prose = set.new_session(&jinn_core_types::SessionId::new());
        let modern = prose.check("leave a TODO here", StreamContext::text());
        assert!(
            legacy.is_some(),
            "the converted legacy rule must still fire on the tool stream"
        );
        assert!(modern.is_some(), "the stream rule must survive the merge");
    }

    #[rstest::rstest]
    #[test]
    fn a_legacy_project_rule_merges_with_a_global_one() {
        // Given a legacy global rule beside a legacy project rule. The
        // project's path and the session cwd derive from the same tempdir
        // root, so the rule matches there and nowhere else. The path is
        // written with forward slashes — the matcher normalizes every cwd to
        // forward slashes before globbing, so a backslash pattern would
        // never match on Windows — and `toml_edit::Value` interpolates it as
        // a properly escaped TOML string.
        let dir = jinn_testutil::tempdir();
        let myapp_cwd = dir
            .path
            .join("dev/code/myapp")
            .expect("temp root joins a bare name");
        let config = {
            let project_glob = myapp_cwd.to_string_lossy().replace('\\', "/");
            let project = toml_edit::Value::from(project_glob);
            layer(&format!(
                r#"
                [[tools.bash_command_policy]]
                pattern = 'drop\s+table'
                message = 'No.'

                [[project.entry]]
                path = {project}
                [[project.entry.command_policy]]
                pattern = 'truncate'
                message = 'Not here.'
            "#
            ))
        };

        // When the rules are read and compiled for that project.
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&config, &prefs)).for_cwd(Some(&myapp_cwd));

        // Then both fire: the project rule adds its block beside the global.
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        let global = session.check(
            r#"{"command":"drop table t"}"#,
            StreamContext::tool(0, "bash"),
        );
        let mut scoped = set.new_session(&jinn_core_types::SessionId::new());
        let in_project = scoped.check(
            r#"{"command":"truncate t"}"#,
            StreamContext::tool(0, "bash"),
        );
        assert!(global.is_some(), "the legacy global rule must still fire");
        assert!(
            in_project.is_some(),
            "the legacy project rule must fire in its project"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_disabled_name_disarms_an_embedded_rule() {
        // Given a jinn.toml naming a shipped rule in the disabled array.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['no-unbounded-recursive-rm']
        "#,
        );

        // When the rules are read with no user file.
        let (_prefs_dir, prefs) = temp_prefs();
        let names = assembled_names(&config, &prefs);

        // Then the named shipped rule is gone and its siblings stay armed —
        // the array reaches a rule with no jinn.toml entry of its own.
        assert!(
            !names.contains(&"no-unbounded-recursive-rm".to_owned()),
            "a named shipped rule must not survive, got: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "no-mkfs"),
            "the other shipped rules must stay armed, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_disabled_name_disarms_a_user_file_rule() {
        // Given a user rules file and an array naming its rule.
        let dir = jinn_testutil::tempdir();
        let prefs = prefs_path_in(&dir.path);
        write_user_rules(
            &prefs,
            r#"
            [[stream_rules.entry]]
            name = 'user-only-rule'
            conditions = ['NEVER-TYPE-this']
            scopes = ['tool:bash']
            body = 'Mine.'
        "#,
        );
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['user-only-rule']
        "#,
        );

        // When the rules are read.
        let names = assembled_names(&config, &prefs);

        // Then the user-file rule is disarmed by name: the filter runs
        // after assembly, not inside the jinn.toml reader.
        assert!(
            names.is_empty(),
            "a named user-file rule must not survive, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_disabled_name_disarms_a_jinn_toml_rule() {
        // Given a jinn.toml rule and its own name in the array.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['from-jinn-toml']

            [[stream_rules.entry]]
            name = 'from-jinn-toml'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it.'
        "#,
        );

        // When the rules are read.
        let (_prefs_dir, prefs) = temp_prefs();
        let names = assembled_names(&config, &prefs);

        // Then only the named rule is gone; the base set stays armed.
        assert!(
            !names.contains(&"from-jinn-toml".to_owned()),
            "a named jinn.toml rule must not survive, got: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "no-unbounded-recursive-rm"),
            "the embedded base set must stay armed, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_disabled_name_disarms_a_legacy_converted_rule() {
        // Given the removed legacy section and an array naming the rule it
        // converts into.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['bash-policy: LEGACY-MARKER-CMD']

            [[tools.bash_command_policy]]
            pattern = 'LEGACY-MARKER-CMD'
            message = 'That is the whole filesystem.'
        "#,
        );

        // When the rules are read.
        let (_prefs_dir, prefs) = temp_prefs();
        let names = assembled_names(&config, &prefs);

        // Then the converted rule is disarmed by name too: conversions pass
        // through the same choke point as every other source.
        assert!(
            !names.contains(&"bash-policy: LEGACY-MARKER-CMD".to_owned()),
            "a named legacy rule must not survive, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_disabled_name_matching_nothing_is_inert() {
        // Given an array naming a rule that exists nowhere.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['never-existed']
        "#,
        );

        // When the rules are read.
        let (_prefs_dir, prefs) = temp_prefs();
        let names = assembled_names(&config, &prefs);

        // Then the set is unchanged: names match at read time against
        // whatever assembled, and a stale name is inert, not an error.
        assert!(
            names.iter().any(|n| n == "no-unbounded-recursive-rm"),
            "the base set must be untouched by an unknown name, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn removing_a_name_from_the_array_re_arms_the_rule() {
        // Given an armed jinn.toml rule — its name no longer in any array.
        let config = layer(
            r#"
            [[stream_rules.entry]]
            name = 'from-jinn-toml'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it.'
        "#,
        );

        // When the rules are read.
        let (_prefs_dir, prefs) = temp_prefs();
        let names = assembled_names(&config, &prefs);

        // Then the rule is back: a name's absence means armed, so removing
        // it re-arms on the next read or refresh.
        assert!(
            names.contains(&"from-jinn-toml".to_owned()),
            "the re-armed rule must be read, got: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_named_rule_never_fires_on_a_matching_stream() {
        // Given a rule the disabled array names.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['from-jinn-toml']

            [[stream_rules.entry]]
            name = 'from-jinn-toml'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Finish it.'
        "#,
        );

        // When the assembled set is compiled and prose matching its
        // pattern streams.
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&config, &prefs));
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        let fired = session
            .check("leave a TODO here", StreamContext::text())
            .map(|hit| hit.name.clone());

        // Then nothing fires: the rule is not in the compiled set.
        assert_eq!(fired, None);
    }

    #[rstest::rstest]
    #[test]
    fn an_enabled_rule_still_fires_beside_a_named_disabled_one() {
        // Given two matching rules, the first named in the array.
        let config = layer(
            r#"
            [stream_rules]
            disabled_rules = ['off']

            [[stream_rules.entry]]
            name = 'off'
            conditions = ['TODO']
            scopes = ['text']
            body = 'Off.'

            [[stream_rules.entry]]
            name = 'on'
            conditions = ['TODO']
            scopes = ['text']
            body = 'On.'
        "#,
        );

        // When the delta that matches both streams — probed the way
        // production probes it: `check` per delta, then `finalize` at the
        // stream's end, the pair a real response always performs.
        let (_prefs_dir, prefs) = temp_prefs();
        let set = compiled(&read_rules(&config, &prefs));
        let mut session = set.new_session(&jinn_core_types::SessionId::new());
        let fired = session
            .check("TODO", StreamContext::text())
            .or_else(|| session.finalize())
            .map(|hit| hit.name.clone());

        // Then the enabled one fires: disabling one rule disarms only that
        // rule, not the set around it.
        assert_eq!(fired, Some("on".to_owned()));
    }
}
