//! Project command policy - resolution and matching for the bash tool.
//!
//! Resolves the blocked-command rules of the configured project containing a
//! session's cwd (`~`-expanded lexical longest-prefix match) and compiles them
//! into a matcher consulted by the bash tool before any child process spawns.
//!
//! Advisory-strength by design: rules exist to stop well-trained habits
//! (like `cargo test -p` in a whole-workspace repo), not to resist a
//! determined actor.

use std::path::{Path, PathBuf};

use regex::Regex;

use jinn_tools_msg::CommandPolicyRule;

use jinn_domain::feat::project::ProjectConfig;

/// Compiled blocked-command rules for one project. Empty matches nothing.
#[derive(Debug, Clone, Default)]
pub struct CompiledCommandPolicy {
    /// Compiled regexes paired with their messages, in config order.
    rules: Vec<(Regex, String)>,
}

impl CompiledCommandPolicy {
    /// Compiles user-authored rules. An invalid regex is skipped with a
    /// `tracing::warn!` naming the pattern — one bad rule is inert, never
    /// fatal for the project or the tool call.
    #[must_use]
    pub fn compile(rules: &[CommandPolicyRule]) -> Self {
        let compiled: Vec<(Regex, String)> = rules
            .iter()
            .filter_map(|rule| match Regex::new(&rule.pattern) {
                Ok(regex) => Some((regex, rule.message.clone())),
                Err(err) => {
                    tracing::warn!(
                        pattern = %rule.pattern,
                        %err,
                        "command_policy: skipping rule with invalid regex"
                    );
                    None
                }
            })
            .collect();
        Self { rules: compiled }
    }

    /// Returns `(pattern_as_written, message)` for the first rule matching
    /// `command`. Config order is precedence: first match wins.
    #[must_use]
    pub fn matched_message(&self, command: &str) -> Option<(&str, &str)> {
        self.rules
            .iter()
            .find(|(regex, _)| regex.is_match(command))
            .map(|(regex, message)| (regex.as_str(), message.as_str()))
    }

    /// True when no rules are compiled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Returns the command-policy rules of the configured project containing
/// `cwd`, or an empty vec.
///
/// Matching is a lexical, component-wise prefix between each `~`-expanded
/// project path and `cwd`; the longest matching project path wins (nesting).
#[must_use]
pub fn resolve_project_rules(
    projects: &[ProjectConfig],
    cwd: &Path,
    home: &Path,
) -> Vec<CommandPolicyRule> {
    matching_project(projects, cwd, home)
        .map_or_else(Vec::new, |project| project.command_policy.clone())
}

/// Returns the configured project with the longest `~`-expanded path that is
/// a lexical prefix of (or equal to) `cwd`.
fn matching_project<'a>(
    projects: &'a [ProjectConfig],
    cwd: &Path,
    home: &Path,
) -> Option<&'a ProjectConfig> {
    projects
        .iter()
        .filter_map(|project| {
            let expanded = expand_tilde(&project.path, home);
            cwd.starts_with(&expanded)
                .then_some((expanded.as_os_str().len(), project))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, project)| project)
}

/// Expands a leading `~` (and `~/`) in a configured project path against `home`.
fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    let Some(first) = path.components().next() else {
        return path.to_path_buf();
    };
    match first {
        std::path::Component::Normal(marker) if marker == "~" => {
            let suffix: PathBuf = path.components().skip(1).collect();
            if suffix.as_os_str().is_empty() {
                home.to_path_buf()
            } else {
                home.join(suffix)
            }
        }
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    fn rule(pattern: &str, message: &str) -> CommandPolicyRule {
        CommandPolicyRule {
            pattern: pattern.to_owned(),
            message: message.to_owned(),
        }
    }

    fn project(path: &str, rules: Vec<CommandPolicyRule>) -> ProjectConfig {
        ProjectConfig {
            path: PathBuf::from(path),
            command_policy: rules,
        }
    }

    #[rstest::rstest]
    #[case("/home/me/w/repo", true)]
    #[case("/home/me/w/repo/sub/dir", true)]
    #[case("/home/me/w/repo-sibling", false)]
    #[case("/home/me/w", false)]
    #[case("/elsewhere", false)]
    fn cwd_inside_project_matches(#[case] cwd: &str, #[case] expected: bool) {
        // Given a project configured with a tilde-prefixed path and a home dir.
        let projects = [project("~/w/repo", vec![rule("a", "m")])];
        let home = Path::new("/home/me");

        // When resolving rules for a cwd.
        let rules = resolve_project_rules(&projects, Path::new(cwd), home);

        // Then membership follows the lexical prefix (component-wise).
        assert_eq!(rules.is_empty(), !expected);
    }

    #[rstest::rstest]
    #[test]
    #[rstest::rstest]
    fn tilde_only_path_expands_to_home_itself() {
        // Given a project configured as bare `~`.
        let projects = [project("~", vec![rule("a", "m")])];
        let home = Path::new("/home/me");

        // When resolving rules for a cwd directly inside home.
        let rules = resolve_project_rules(&projects, Path::new("/home/me/notes"), home);

        // Then the tilde expanded to home and the rules apply.
        assert_eq!(rules.len(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn longest_prefix_project_wins_over_ancestor() {
        // Given a nested pair of configured projects, each with a distinct rule.
        let projects = [
            project("/w", vec![rule("outer", "outer msg")]),
            project("/w/repo", vec![rule("inner", "inner msg")]),
        ];
        let cwd = Path::new("/w/repo/src");

        // When resolving rules for a cwd inside the inner project.
        let rules = resolve_project_rules(&projects, cwd, Path::new("/"));

        // Then the inner (longest prefix) project's rules win.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "inner");
    }

    #[rstest::rstest]
    #[case("cargo test -p jinn-domain", true)]
    #[case("cargo t -p foo", true)]
    #[case("cargo test", false)]
    #[case("rg \"cargo test\" notes.md", false)]
    fn dash_p_policy_matches_only_dash_p_commands(#[case] command: &str, #[case] expected: bool) {
        // Given a policy with the canonical `-p` guard regex.
        let policy =
            CompiledCommandPolicy::compile(&[rule(r"cargo\s+(test|t)\b.*\s-p\b", "use just test")]);

        // When matching a command.
        let matched = policy.matched_message(command);

        // Then matches follow the pattern's intent: `-p` forms are blocked,
        // plain and quoted forms are not.
        assert_eq!(matched.is_some(), expected, "command: {command}");
    }

    #[rstest::rstest]
    #[test]
    fn first_matching_rule_wins_in_config_order() {
        // Given a policy whose two rules both match the command.
        let policy = CompiledCommandPolicy::compile(&[
            rule("first", "first message"),
            rule("second", "second message"),
        ]);

        // When matching a command both rules match.
        let matched = policy.matched_message("first and second");

        // Then the first rule (config order) supplies pattern and message.
        assert_eq!(matched, Some(("first", "first message")));
    }

    #[rstest::rstest]
    #[test]
    fn empty_policy_matches_nothing() {
        // Given a policy compiled from no rules.
        let policy = CompiledCommandPolicy::default();

        // When matching any command.
        let matched = policy.matched_message("rm -rf /");

        // Then nothing matches and the policy is empty.
        assert!(matched.is_none());
        assert!(policy.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn invalid_regex_rule_is_inert_but_sibling_still_enforces() {
        // Given a policy with one invalid regex followed by one valid rule.
        let policy = CompiledCommandPolicy::compile(&[
            rule("([unclosed", "never compiles"),
            rule("forbidden", "blocked"),
        ]);

        // When matching a command the invalid rule would have caught.
        let invalid_hit = policy.matched_message("([unclosed thing");
        // And a command the valid rule catches.
        let valid_hit = policy.matched_message("run forbidden now");

        // Then the invalid rule is silently inert (warned at compile).
        assert!(invalid_hit.is_none());
        // And the valid rule still enforces.
        assert_eq!(valid_hit, Some(("forbidden", "blocked")));
    }
}
