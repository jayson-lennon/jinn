//! Regex-based auto-prune worker.
//!
//! Matches tool calls by regex pattern and prunes all but the most recent
//! `keep_last` matching call+result pairs. Rules are configured via
//! `[[auto_prune.regex.rules]]` in `jinn.toml`.
//!
//! Regex patterns are compiled once at construction time via
//! [`RegexAutoPruneWorker::from_config`] and never recompiled during evaluation.
//!
//! The top-level `min_age` field (default: 50) is a raw-distance protection
//! floor: matching pairs whose `ToolCall` is within `min_age` slots of the
//! end of history are never pruned. With `min_age = 0` no pair is protected
//! (back-compat baseline).
//!
//! [`ForcedExclude`]: jinn_core_types::ContextOverride::ForcedExclude

use super::min_age::is_within_min_age;

use crate::worker::HistoryWorker;
use jinn_core_types::HistoryMutation;
use jinn_core_types::SessionId;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride};
pub use jinn_preferences_config::schemas::auto_prune::{RegexAutoPruneConfig, RegexPruneRule};

/// Default regex prune rule tool name.
/// Default regex prune rule keep_last.
/// Default enabled state for regex auto-prune.
/// Default minimum age for regex auto-prune.
/// A single regex-based auto-prune rule.
///
/// Serialized as `[[auto_prune.regex]]` in `jinn.toml`.
/// Each rule matches tool calls by name and content, keeping only the
/// most recent `keep_last` matching call+result pairs in context.
/// Regex-based auto-prune configuration.
///
/// Serialized as `[auto_prune.regex]` in `jinn.toml`.
/// Contains a list of regex rules that identify tool calls to prune.
///
/// Created from [`RegexPruneRule`](jinn_preferences_config::schemas::RegexPruneRule)
/// during worker construction. The regex is compiled exactly once.
struct CompiledRegexRule {
    /// The compiled regex pattern.
    regex: regex::Regex,
    /// Tool name to filter by (e.g., "bash").
    tool_name: String,
    /// Number of most recent matching pairs to keep (minimum 1).
    keep_last: usize,
    /// Raw-distance protection floor: pairs whose call is within `min_age`
    /// slots of the end of history are never pruned.
    min_age: usize,
}

/// Regex-based auto-prune worker.
///
/// Holds a set of compiled regex rules. On each `HistoryAppended` event,
/// scans history for matching tool calls and prunes older ones by emitting
/// `SetContextOverride::ForcedExclude` for both the call and its result.
#[derive(Clone)]
pub struct RegexAutoPruneWorker {
    /// Compiled regex rules (empty if disabled or no rules configured).
    rules: Vec<CompiledRegexRule>,
}

impl RegexAutoPruneWorker {
    /// Construct a worker from config, compiling all regex patterns once.
    ///
    /// Returns `Err(regex::Error)` if any pattern is invalid.
    /// Clamps `keep_last` to a minimum of 1.
    /// Returns an empty worker if disabled or no rules configured.
    ///
    /// # Errors
    ///
    /// Returns `regex::Error` if any pattern string fails to compile.
    pub fn from_config(config: &RegexAutoPruneConfig) -> Result<Self, regex::Error> {
        if !config.enabled || config.rules.is_empty() {
            return Ok(Self { rules: Vec::new() });
        }

        let mut compiled = Vec::with_capacity(config.rules.len());
        for rule in &config.rules {
            let regex = regex::Regex::new(&rule.pattern)?;
            compiled.push(CompiledRegexRule {
                regex,
                tool_name: rule.tool_name.clone(),
                keep_last: rule.keep_last.max(1),
                min_age: rule.min_age,
            });
        }

        Ok(Self { rules: compiled })
    }
}

// Manual Clone impl because regex::Regex doesn't derive Clone.
// (Actually it does implement Clone, but the struct isn't Clone by default.)
impl Clone for CompiledRegexRule {
    fn clone(&self) -> Self {
        Self {
            regex: self.regex.clone(),
            tool_name: self.tool_name.clone(),
            keep_last: self.keep_last,
            min_age: self.min_age,
        }
    }
}

/// Walk forward from a ToolCall to find its matching ToolResult by tool call ID.
///
/// Returns `None` if no matching result exists (pending/orphaned call).
fn find_matching_result(
    history: &[ChatEntry],
    call_idx: usize,
    tool_call_id: &str,
) -> Option<ChatEntryId> {
    // ToolResults appear after their ToolCall, so scan forward only.
    for entry in history.iter().skip(call_idx + 1) {
        if let ChatEntryKind::ToolResult { id, .. } = &entry.kind
            && id == tool_call_id
        {
            return Some(entry.id.clone());
        }
    }
    // No matching result found — the call is still pending or orphaned.
    None
}

/// Scan history for ToolCalls matching a single regex rule and collect
/// `(call_idx, call_entry_id, result_entry_id)` tuples.
///
/// Matches regardless of exclusion status — already-excluded entries still
/// count toward `keep_last` positioning so that the "most recent N" window
/// is stable regardless of prior pruning.
fn collect_matching_pairs(
    history: &[ChatEntry],
    rule: &CompiledRegexRule,
) -> Vec<(usize, ChatEntryId, ChatEntryId)> {
    let mut matched_pairs: Vec<(usize, ChatEntryId, ChatEntryId)> = Vec::new();

    for (i, entry) in history.iter().enumerate() {
        // Only interested in ToolCall entries matching the rule's tool_name.
        let tool_call_id = match &entry.kind {
            ChatEntryKind::ToolCall { id, name, .. } if name == &rule.tool_name => id.clone(),
            _ => continue,
        };

        // Run regex against the full text() output: "{name}: {arguments}".
        // Match regardless of current exclusion status so that already-excluded
        // entries still count toward keep_last positioning. The text itself is
        // never logged (can be thousands of chars); entry_id is enough to
        // recover it.
        let text = entry.text();
        let matched = rule.regex.is_match(&text);
        tracing::debug!(
            entry_id = %entry.id,
            matched,
            "regex match attempt"
        );
        if !matched {
            continue;
        }

        // Walk forward to find the ToolResult for this matching call.
        // If none found (pending/orphaned), skip — incomplete pairs don't
        // count toward keep_last positioning.
        if let Some(result_id) = find_matching_result(history, i, &tool_call_id) {
            tracing::debug!(
                call_id = %entry.id,
                result_id = %result_id,
                "matched pair"
            );
            matched_pairs.push((i, entry.id.clone(), result_id));
        } else {
            tracing::warn!(call_id = %entry.id, "no matching result found");
        }
    }

    matched_pairs
}

/// For each rule, prune all but the last `keep_last` matching pairs.
///
/// Pairs are in history order (oldest first), so `.take()` selects the
/// oldest pairs to prune. Only emits mutations for entries not already excluded.
fn build_prune_mutations(
    history: &[ChatEntry],
    rules: &[CompiledRegexRule],
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let mut mutations = Vec::new();

    for rule in rules {
        let matched_pairs = collect_matching_pairs(history, rule);

        tracing::debug!(
            rule = %rule.regex,
            matched_count = matched_pairs.len(),
            keep_last = rule.keep_last,
            "pruning decision",
        );

        if matched_pairs.len() <= rule.keep_last {
            continue;
        }

        // Pairs are oldest-first. Prune the oldest ones beyond keep_last.
        let prune_count = matched_pairs.len() - rule.keep_last;
        let rule_ident = rule.regex.as_str();
        let history_len = history.len();
        for (idx, (call_idx, call_id, result_id)) in
            matched_pairs.iter().take(prune_count).enumerate()
        {
            // Protection floor: never prune pairs whose call is within
            // the rule's `min_age` slots of the end of history.
            if is_within_min_age(history_len, *call_idx, rule.min_age) {
                continue;
            }

            let call_protected = history
                .iter()
                .any(|e| e.id == *call_id && e.is_protected_from_prune());
            let result_protected = history
                .iter()
                .any(|e| e.id == *result_id && e.is_protected_from_prune());

            if !call_protected {
                tracing::debug!(
                    rule = %rule_ident,
                    pair_index = idx,
                    entry_id = %call_id,
                    "emitting ForcedExclude for call"
                );
                mutations.push(HistoryMutation::SetContextOverride {
                    entry_id: call_id.clone(),
                    value: ContextOverride::ForcedExclude,
                    source: ChangeSource::Worker {
                        name: worker_name.to_owned(),
                    },
                });
            }
            if !result_protected {
                tracing::debug!(
                    rule = %rule_ident,
                    pair_index = idx,
                    entry_id = %result_id,
                    "emitting ForcedExclude for result"
                );
                mutations.push(HistoryMutation::SetContextOverride {
                    entry_id: result_id.clone(),
                    value: ContextOverride::ForcedExclude,
                    source: ChangeSource::Worker {
                        name: worker_name.to_owned(),
                    },
                });
            }
        }
    }

    mutations
}

#[async_trait::async_trait]
impl HistoryWorker for RegexAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-regex"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: std::sync::Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let mutations = build_prune_mutations(&history, &self.rules, self.name());

        tracing::debug!(total_mutations = mutations.len(), "regex worker done");
        mutations
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
    use jinn_core_types::SessionId;
    use jinn_core_types::ToolResultStatus;
    use jinn_core_types::{ChangeSource, ChatEntry, ContextOverride};

    /// Helper: create a bash ToolCall + ToolResult pair.
    fn bash_call_result(call_id: &str, command: &str, output: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "bash", format!(r#"{{"command": "{command}"}}"#)),
            ChatEntry::tool_result(call_id, "bash", output, ToolResultStatus::Success),
        ]
    }

    /// Helper: create a read ToolCall + ToolResult pair.
    fn read_call_result(call_id: &str, path: &str, output: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "read", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "read", output, ToolResultStatus::Success),
        ]
    }

    /// Helper: build a worker from rules with given keep_last for "cargo check" pattern.
    fn worker_for_cargo_check(keep_last: usize) -> RegexAutoPruneWorker {
        RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "cargo check".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last,
                min_age: 0,
            }],
        })
        .expect("valid config")
    }

    /// Helper: evaluate a worker on a history snapshot.
    fn evaluate(worker: &RegexAutoPruneWorker, history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let history: std::sync::Arc<[ChatEntry]> = history.into();
        rt.block_on(async { worker.evaluate(&SessionId::new(), history).await })
    }

    #[rstest::rstest]
    #[test]
    fn from_config_clamps_keep_last_to_minimum_1() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "cargo check".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last: 0,
                min_age: 0,
            }],
        })
        .expect("valid config");

        // Verify by checking behavior: with 1 match and keep_last clamped to 1, no pruning.
        let history = vec![
            bash_call_result("tc-1", "cargo check", "ok")[0].clone(),
            bash_call_result("tc-1", "cargo check", "ok")[1].clone(),
        ];
        let mutations = evaluate(&worker, history);
        assert!(
            mutations.is_empty(),
            "keep_last clamped to 1, single match should not prune"
        );
    }

    #[rstest::rstest]
    #[test]
    fn from_config_returns_error_for_invalid_regex() {
        let result = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "[".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last: 1,
                min_age: 0,
            }],
        });
        assert!(result.is_err(), "invalid regex should return error");
    }

    #[rstest::rstest]
    #[test]
    fn from_config_disabled_returns_empty_worker() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: false,
            rules: vec![RegexPruneRule {
                pattern: "cargo check".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last: 1,
                min_age: 0,
            }],
        })
        .expect("valid config");

        let history = vec![
            bash_call_result("tc-1", "cargo check", "ok")[0].clone(),
            bash_call_result("tc-1", "cargo check", "ok")[1].clone(),
        ];
        let mutations = evaluate(&worker, history);
        assert!(
            mutations.is_empty(),
            "disabled worker should produce no mutations"
        );
    }

    #[rstest::rstest]
    #[test]
    fn no_matching_tool_calls_produces_no_mutations() {
        let history = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi")];
        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn single_match_with_keep_last_1_produces_no_mutations() {
        let pair = bash_call_result("tc-1", "cargo check 2>&1", "all good");
        let history = vec![pair[0].clone(), pair[1].clone()];
        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn three_matches_keep_last_1_prunes_two_oldest() {
        let mut history = Vec::new();

        // First pair
        let p1 = bash_call_result("tc-1", "cargo check", "errors: 0");
        history.push(p1[0].clone());
        history.push(p1[1].clone());

        // Second pair
        let p2 = bash_call_result("tc-2", "cargo check", "errors: 0");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        // Third pair (most recent)
        let p3 = bash_call_result("tc-3", "cargo check", "errors: 0");
        history.push(p3[0].clone());
        history.push(p3[1].clone());

        // Save entry IDs before move.
        let id_0 = history[0].id.clone();
        let id_1 = history[1].id.clone();
        let id_2 = history[2].id.clone();
        let id_3 = history[3].id.clone();
        let id_4 = history[4].id.clone();
        let id_5 = history[5].id.clone();

        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);

        // Should prune 2 oldest pairs = 4 mutations (2 calls + 2 results).
        assert_eq!(mutations.len(), 4);

        // Verify the first two calls and their results are targeted.
        let mut excluded_ids = std::collections::HashSet::new();
        for m in &mutations {
            if let HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } = m
            {
                assert_eq!(*value, ContextOverride::ForcedExclude);
                excluded_ids.insert(entry_id);
            }
        }

        // First two calls and first two results should be excluded.
        assert!(excluded_ids.contains(&id_0), "tc-1 call should be excluded");
        assert!(
            excluded_ids.contains(&id_1),
            "tc-1 result should be excluded"
        );
        assert!(excluded_ids.contains(&id_2), "tc-2 call should be excluded");
        assert!(
            excluded_ids.contains(&id_3),
            "tc-2 result should be excluded"
        );
        // Third pair should NOT be excluded.
        assert!(!excluded_ids.contains(&id_4), "tc-3 call should be kept");
        assert!(!excluded_ids.contains(&id_5), "tc-3 result should be kept");
    }

    #[rstest::rstest]
    #[test]
    fn three_matches_keep_last_2_prunes_one_oldest() {
        let mut history = Vec::new();

        let p1 = bash_call_result("tc-1", "cargo check", "errors: 0");
        history.push(p1[0].clone());
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "errors: 0");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        let p3 = bash_call_result("tc-3", "cargo check", "errors: 0");
        history.push(p3[0].clone());
        history.push(p3[1].clone());

        let worker = worker_for_cargo_check(2);
        let mutations = evaluate(&worker, history);

        // Should prune 1 oldest pair = 2 mutations.
        assert_eq!(mutations.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn keep_last_3_with_only_2_matches_produces_no_mutations() {
        let mut history = Vec::new();

        let p1 = bash_call_result("tc-1", "cargo check", "errors: 0");
        history.push(p1[0].clone());
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "errors: 0");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        let worker = worker_for_cargo_check(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_entries_are_not_re_pruned() {
        let mut history = Vec::new();

        let p1 = bash_call_result("tc-1", "cargo check", "errors: 0");
        let mut call1 = p1[0].clone();
        call1.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        ); // already excluded
        history.push(call1);
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "errors: 0");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        // Clone ID before move.
        let result1_id = history[1].id.clone();
        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);

        // With the new logic, excluded entries still count for positioning.
        // Both pairs match, keep_last=1, so the first pair is pruned.
        // The first call is already excluded, so only the first result gets a mutation.
        assert_eq!(mutations.len(), 1);
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, result1_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_entries_are_not_pruned() {
        let mut history = Vec::new();

        let p1 = bash_call_result("tc-1", "cargo check", "errors: 0");
        let mut call1 = p1[0].clone();
        call1.context_override = ContextOverride::ForcedInclude; // force-included
        history.push(call1);
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "errors: 0");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        // Clone ID before move.
        let result1_id = history[1].id.clone();
        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);

        // Force-included call is protected; only the result mutates.
        assert_eq!(mutations.len(), 1);
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, result1_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn multiple_rules_apply_independently() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![
                RegexPruneRule {
                    pattern: "cargo check".to_owned(),
                    tool_name: "bash".to_owned(),
                    keep_last: 1,
                    min_age: 0,
                },
                RegexPruneRule {
                    pattern: "cargo test".to_owned(),
                    tool_name: "bash".to_owned(),
                    keep_last: 1,
                    min_age: 0,
                },
            ],
        })
        .expect("valid config");

        let mut history = Vec::new();

        // Two cargo check calls
        let c1 = bash_call_result("tc-1", "cargo check", "ok");
        history.push(c1[0].clone());
        history.push(c1[1].clone());

        let c2 = bash_call_result("tc-2", "cargo check", "ok");
        history.push(c2[0].clone());
        history.push(c2[1].clone());

        // Two cargo test calls
        let t1 = bash_call_result("tc-3", "cargo test", "passed");
        history.push(t1[0].clone());
        history.push(t1[1].clone());

        let t2 = bash_call_result("tc-4", "cargo test", "passed");
        history.push(t2[0].clone());
        history.push(t2[1].clone());

        let mutations = evaluate(&worker, history);

        // Each rule prunes 1 oldest pair = 2 rules * 2 mutations = 4 total.
        assert_eq!(mutations.len(), 4);
    }

    #[rstest::rstest]
    #[test]
    fn rules_filter_by_tool_name() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "foo".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last: 1,
                min_age: 0,
            }],
        })
        .expect("valid config");

        let mut history = Vec::new();

        // "read" tool call that contains "foo" — should NOT match.
        let r1 = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(r1[0].clone());
        history.push(r1[1].clone());

        let mutations = evaluate(&worker, history);
        assert!(
            mutations.is_empty(),
            "read tool should not match bash-only rule"
        );
    }

    #[rstest::rstest]
    #[test]
    fn regex_matches_against_tool_call_text() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "bash:.*cargo check".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last: 1,
                min_age: 0,
            }],
        })
        .expect("valid config");

        let mut history = Vec::new();

        let p1 = bash_call_result("tc-1", "cargo check", "ok");
        history.push(p1[0].clone());
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "ok");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        let mutations = evaluate(&worker, history);

        // Pattern matches "bash: {"command": "cargo check"}".
        assert_eq!(mutations.len(), 2, "should prune the older pair");
    }

    #[rstest::rstest]
    #[test]
    fn empty_history_produces_no_mutations() {
        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, vec![]);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn tool_call_without_matching_result_is_skipped() {
        let history = vec![ChatEntry::tool_call(
            "tc-orphan",
            "bash",
            r#"{"command": "cargo check"}"#,
        )];

        let worker = worker_for_cargo_check(1);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn multiple_rules_same_tool_different_patterns() {
        let worker = RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![
                RegexPruneRule {
                    pattern: "cargo check".to_owned(),
                    tool_name: "bash".to_owned(),
                    keep_last: 1,
                    min_age: 0,
                },
                RegexPruneRule {
                    pattern: "cargo clippy".to_owned(),
                    tool_name: "bash".to_owned(),
                    keep_last: 1,
                    min_age: 0,
                },
            ],
        })
        .expect("valid config");

        let mut history = Vec::new();

        // Two cargo check calls
        let c1 = bash_call_result("tc-1", "cargo check", "ok");
        history.push(c1[0].clone());
        history.push(c1[1].clone());

        let c2 = bash_call_result("tc-2", "cargo check", "ok");
        history.push(c2[0].clone());
        history.push(c2[1].clone());

        // Two cargo clippy calls
        let cl1 = bash_call_result("tc-3", "cargo clippy", "ok");
        history.push(cl1[0].clone());
        history.push(cl1[1].clone());

        let cl2 = bash_call_result("tc-4", "cargo clippy", "ok");
        history.push(cl2[0].clone());
        history.push(cl2[1].clone());

        let mutations = evaluate(&worker, history);

        // Each rule prunes 1 oldest pair = 2 rules * 2 mutations = 4 total.
        assert_eq!(mutations.len(), 4);
    }

    /// Helper: like `worker_for_cargo_check` but with explicit `min_age`.
    fn worker_with_min_age(keep_last: usize, min_age: usize) -> RegexAutoPruneWorker {
        RegexAutoPruneWorker::from_config(&RegexAutoPruneConfig {
            enabled: true,
            rules: vec![RegexPruneRule {
                pattern: "cargo check".to_owned(),
                tool_name: "bash".to_owned(),
                keep_last,
                min_age,
            }],
        })
        .expect("valid config")
    }

    /// Build a history containing `n` cargo-check pairs and tail padding.
    ///
    /// Layout: indices 0..2n are the n pairs; indices 2n..history_len are
    /// `trivial_assistant("tail")` padding so history_len is fixed at 52.
    /// With 2 pairs the oldest call_idx is 0 → age = 51.
    fn history_with_two_check_pairs_and_tail() -> (Vec<ChatEntry>, ChatEntryId, ChatEntryId) {
        let mut history = Vec::new();
        let p1 = bash_call_result("tc-1", "cargo check", "ok");
        let p1_call_id = p1[0].id.clone();
        let p1_result_id = p1[1].id.clone();
        history.push(p1[0].clone());
        history.push(p1[1].clone());

        let p2 = bash_call_result("tc-2", "cargo check", "ok");
        history.push(p2[0].clone());
        history.push(p2[1].clone());

        // Pad to history_len = 52 with trivial assistant entries.
        // history.len() is currently 4; need 48 more.
        history.extend(std::iter::repeat_n(ChatEntry::assistant("tail"), 48));
        (history, p1_call_id, p1_result_id)
    }

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_matching_pair_regex() {
        // Given a history with two cargo-check pairs (oldest at idx 0)
        // padded to history_len = 52, and keep_last=1.
        // With min_age=0, the older pair should be pruned (back-compat baseline).
        let (history, _call_id, _result_id) = history_with_two_check_pairs_and_tail();
        let worker = worker_with_min_age(1, 0);

        // When evaluating the worker.
        let mutations = evaluate(&worker, history);

        // Then exactly 2 mutations are emitted (call + result of older pair).
        assert_eq!(
            mutations.len(),
            2,
            "min_age=0 must prune the older pair (back-compat baseline)"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_matching_pair_regex() {
        // Given a history with two cargo-check pairs padded to 52 entries,
        // where the oldest call_idx is 0 (age = 51).
        // With min_age = 60, age 51 < 60 → the pair is protected.
        let (history, call_id, result_id) = history_with_two_check_pairs_and_tail();
        let worker = worker_with_min_age(1, 60);

        // When evaluating the worker.
        let mutations = evaluate(&worker, history);

        // Then no mutations are emitted — the pair within min_age is protected.
        assert!(
            mutations.is_empty(),
            "min_age must protect recent matching pair"
        );
        // And specifically, the call_id and result_id we know are protected.
        for m in &mutations {
            if let HistoryMutation::SetContextOverride { entry_id, .. } = m {
                assert!(
                    entry_id != &call_id && entry_id != &result_id,
                    "protected pair must not appear in mutations"
                );
            }
        }
    }

    #[rstest::rstest]
    #[test]
    fn min_age_boundary_strict_less_than_regex() {
        // history_len = 52, oldest call_idx = 0, age = 51.
        //
        // is_within_min_age returns true when age < min_age (strict less-than).
        //
        // At min_age = 52: age=51 < 52 → protected.
        // At min_age = 51: age=51 < 51 is false → NOT protected.
        let (history, _, _) = history_with_two_check_pairs_and_tail();

        // Protected: age = 51 < min_age = 52.
        let worker = worker_with_min_age(1, 52);
        let mutations = evaluate(&worker, history.clone());
        assert!(mutations.is_empty(), "age = min_age - 1 must be protected");

        // Not protected: age = 51 = min_age.
        let worker = worker_with_min_age(1, 51);
        let mutations = evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            2,
            "age = min_age must NOT be protected (strict less-than)"
        );
    }

    use jinn_domain::common::app_info::PREFS_FILE_NAME;
    use jinn_preferences_config::load_preferences_from;
    use jinn_preferences_config::schemas::auto_prune::{
        default_regex_keep_last, default_regex_min_age, default_regex_tool_name,
    };
    use jinn_preferences_config::user_preferences::{AutoPruneConfig, UserPreferences};
    use tempfile::TempDir;

    #[rstest::rstest]
    fn default_regex_config_rules_are_valid_patterns() {
        // Given the default regex prune config.
        let config = RegexAutoPruneConfig::default();

        // Then every built-in rule pattern compiles as a regex.
        for rule in &config.rules {
            let _compiled = regex::Regex::new(&rule.pattern).expect("pattern compiles");
        }
    }

    #[rstest::rstest]
    fn regex_prune_rule_defaults_are_usable() {
        // Given a rule built from the serde default functions.
        let rule = RegexPruneRule {
            pattern: "cargo check".to_owned(),
            tool_name: default_regex_tool_name(),
            keep_last: default_regex_keep_last(),
            min_age: default_regex_min_age(),
        };

        // Then the tool name default is a usable tool name.
        assert!(!rule.tool_name.is_empty());
        // And keep_last is at least 1 (a rule must keep something).
        assert!(rule.keep_last >= 1);
    }

    #[rstest::rstest]
    fn load_parses_multiple_regex_rules() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[[auto_prune.regex.rules]]
pattern = "cargo check"
tool_name = "bash"
keep_last = 1

[[auto_prune.regex.rules]]
pattern = "cargo test"
tool_name = "bash"
keep_last = 2
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.regex.rules.len(), 2);
        assert_eq!(prefs.auto_prune.regex.rules[0].pattern, "cargo check");
        assert_eq!(prefs.auto_prune.regex.rules[1].pattern, "cargo test");
    }

    #[rstest::rstest]
    fn load_parses_regex_rules_with_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[[auto_prune.regex.rules]]
pattern = "cargo check"
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.regex.rules.len(), 1);
        assert_eq!(prefs.auto_prune.regex.rules[0].pattern, "cargo check");
        assert_eq!(prefs.auto_prune.regex.rules[0].tool_name, "bash");
        assert_eq!(prefs.auto_prune.regex.rules[0].keep_last, 1);
    }

    #[rstest::rstest]
    fn load_without_auto_prune_regex_section_uses_defaults() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"last_model = "ollama/llama3"
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert_eq!(prefs.auto_prune.regex, RegexAutoPruneConfig::default());
    }

    #[rstest::rstest]
    fn load_parses_regex_rules_with_header_section() {
        // Mirrors the real user config: [auto_prune.regex] header + [[auto_prune.regex.rules]] entries.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(PREFS_FILE_NAME);
        std::fs::write(
            &path,
            r#"[auto_prune.regex]
enabled = true

[[auto_prune.regex.rules]]
pattern = "ls"
tool_name = "bash"
keep_last = 1

[[auto_prune.regex.rules]]
pattern = "cargo check"
tool_name = "bash"
keep_last = 1
"#,
        )
        .expect("write");

        let prefs = load_preferences_from(&path).expect("load");
        assert!(prefs.auto_prune.regex.enabled);
        assert_eq!(prefs.auto_prune.regex.rules.len(), 2);
        assert_eq!(prefs.auto_prune.regex.rules[0].pattern, "ls");
        assert_eq!(prefs.auto_prune.regex.rules[1].pattern, "cargo check");
    }

    #[rstest::rstest]
    fn serialize_regex_rules_produces_correct_toml() {
        let prefs = UserPreferences {
            auto_prune: AutoPruneConfig {
                regex: RegexAutoPruneConfig {
                    enabled: true,
                    rules: vec![
                        RegexPruneRule {
                            pattern: "ls".to_owned(),
                            tool_name: "bash".to_owned(),
                            keep_last: 1,
                            min_age: 0,
                        },
                        RegexPruneRule {
                            pattern: "cargo check".to_owned(),
                            tool_name: "bash".to_owned(),
                            keep_last: 1,
                            min_age: 0,
                        },
                    ],
                },
                ..AutoPruneConfig::default()
            },
            ..UserPreferences::default()
        };

        let toml_str = toml::to_string_pretty(&prefs).expect("serialize");

        // Round-trip back
        let reloaded: UserPreferences = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(reloaded.auto_prune.regex.rules.len(), 2);
        assert_eq!(reloaded.auto_prune.regex.rules[0].pattern, "ls");
        assert_eq!(reloaded.auto_prune.regex.rules[1].pattern, "cargo check");
    }
}
