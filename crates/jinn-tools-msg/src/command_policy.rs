//! Command policy types — the compiled matcher the bash tool consults.
//!
//! Advisory-strength by design: rules exist to stop well-trained habits
//! (like per-package test invocations in a whole-workspace repo), not to
//! resist a determined actor. Resolution from project config happens in the
//! tools slice (which owns the preferences read); this module holds only the
//! vocabulary: the rule source type and the compiled matcher.

use regex::Regex;

/// A rule pairs a user-authored regex with the corrective message returned
/// when the regex matches a command. Rules are advisory-strength by design:
/// they exist to stop well-trained habits, not to resist a determined actor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandPolicyRule {
    /// Regex matched against the full command string.
    pub pattern: String,
    /// Message returned in the failed tool result when [`Self::pattern`] matches.
    pub message: String,
}

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
        {
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
