//! Consecutive-reads auto-prune worker.
//!
//! Detects repeated `read` tool calls on the same file path and keeps only the
//! last `keep_last` (default: 3) call+result pairs per path, marking all older
//! pairs as [`ForcedExclude`]. This removes stale file contents from the LLM
//! context window.
//!
//! The `min_age` field (default: 50) is a raw-distance protection floor:
//! pairs whose `ToolCall` is within `min_age` slots of the end of history are
//! never pruned. With `min_age = 0` no pair is protected (back-compat baseline).
//!
//! Pruning is immediate — no threshold or delay.
//! Path matching is exact string comparison (no normalization).
//!
//! # Example (keep_last = 2)
//!
//! ```text
//! X  [Tool Call]: read("/foo.rs")       ← pruned (3rd oldest)
//! X  [Tool Result] (OK): <old contents>
//! X  [Tool Call]: read("/foo.rs")       ← pruned (2nd oldest)
//! X  [Tool Result] (OK): <old contents>
//!    [Tool Call]: read("/foo.rs")       ← kept (most recent)
//!    [Tool Result] (OK): <current contents>
//!    [Assistant]: based on the latest read...
//! ```
//!
//! [`ForcedExclude`]: jinn_core_types::ContextOverride::ForcedExclude

use std::collections::HashMap;

pub use jinn_preferences_config::schemas::auto_prune::ConsecutiveReadsAutoPruneConfig;
use std::sync::Arc;

use super::min_age::is_within_min_age;
use crate::worker::HistoryWorker;
use jinn_core_types::HistoryMutation;
use jinn_core_types::SessionId;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride};

/// Default number of consecutive read pairs to keep per file path.
/// Default enabled state for consecutive-reads auto-prune.
/// Default minimum age for consecutive-reads auto-prune.
/// Consecutive-reads auto-prune configuration.
///
/// Serialized as `[auto_prune.consecutive_reads]` in `jinn.toml`.
/// Controls the auto-prune worker that caps the number of `read`
/// tool call+result pairs per file path, keeping only the most recent ones.
/// Consecutive-reads auto-prune worker.
///
/// For each unique file path, keeps only the last `keep_last` `read` tool
/// call+result pairs. Older pairs are excluded from LLM context.
#[derive(Clone)]
pub struct ConsecutiveReadsAutoPruneWorker {
    /// Configuration for the consecutive-reads auto-prune strategy.
    pub config: ConsecutiveReadsAutoPruneConfig,
}

/// Extract the `path` field from a tool call's JSON arguments string.
///
/// Returns `None` if the arguments cannot be parsed or the `path` field is
/// missing or not a string.
fn extract_path_from_arguments(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value
        .get("path")?
        .as_str()
        .map(std::borrow::ToOwned::to_owned)
}

/// Walk forward from a read ToolCall to find its matching ToolResult.
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

/** A matched read pair (ToolCall + its corresponding ToolResult). */
struct ReadPair {
    call_idx: usize,
    call_entry_id: ChatEntryId,
    result_entry_id: ChatEntryId,
}

/// Scan history for read ToolCalls, resolve each to its result,
/// and group into pairs by file path.
///
/// Skips ToolCalls with no parseable path or no matching ToolResult.
fn collect_read_pairs_by_path(history: &[ChatEntry]) -> HashMap<String, Vec<ReadPair>> {
    let mut pairs_by_path: HashMap<String, Vec<ReadPair>> = HashMap::new();

    for (i, entry) in history.iter().enumerate() {
        // Only interested in "read" tool calls with a parseable file path.
        let (tool_call_id, read_path) = match &entry.kind {
            ChatEntryKind::ToolCall {
                name,
                arguments,
                id,
                ..
            } if name == "read" => {
                let Some(path) = extract_path_from_arguments(arguments) else {
                    continue;
                };
                (id.clone(), path)
            }
            _ => continue,
        };

        // Walk forward to find the ToolResult for this read call.
        // If none found (pending/orphaned), skip — incomplete pairs
        // don't count toward the file's total.
        let Some(result_id) = find_matching_result(history, i, &tool_call_id) else {
            continue;
        };

        pairs_by_path.entry(read_path).or_default().push(ReadPair {
            call_idx: i,
            call_entry_id: entry.id.clone(),
            result_entry_id: result_id,
        });
    }

    pairs_by_path
}

fn build_prune_mutations(
    history: &[ChatEntry],
    groups: &HashMap<String, Vec<ReadPair>>,
    keep_last: usize,
    min_age: usize,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let mut mutations = Vec::new();
    let history_len = history.len();

    for pairs in groups.values() {
        if pairs.len() <= keep_last {
            continue;
        }

        // Pairs are oldest-first. Prune the oldest ones beyond keep_last.
        let prune_count = pairs.len() - keep_last;
        for pair in pairs.iter().take(prune_count) {
            // Protection floor: never prune pairs whose call is within
            // `min_age` slots of the end of history.
            if is_within_min_age(history_len, pair.call_idx, min_age) {
                continue;
            }

            // Only emit mutations for entries not protected from prune.
            let call_protected = history
                .iter()
                .any(|e| e.id == pair.call_entry_id && e.is_protected_from_prune());
            let result_protected = history
                .iter()
                .any(|e| e.id == pair.result_entry_id && e.is_protected_from_prune());

            if !call_protected {
                mutations.push(HistoryMutation::SetContextOverride {
                    entry_id: pair.call_entry_id.clone(),
                    value: ContextOverride::ForcedExclude,
                    source: ChangeSource::Worker {
                        name: worker_name.to_owned(),
                    },
                });
            }
            if !result_protected {
                mutations.push(HistoryMutation::SetContextOverride {
                    entry_id: pair.result_entry_id.clone(),
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
impl HistoryWorker for ConsecutiveReadsAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-consecutive-reads"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let keep_last = self.config.keep_last.max(1);

        let groups = collect_read_pairs_by_path(&history);
        build_prune_mutations(
            &history,
            &groups,
            keep_last,
            self.config.min_age,
            self.name(),
        )
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
    use jinn_core_types::ChatEntry;
    use jinn_core_types::SessionId;
    use jinn_core_types::ToolResultStatus;

    /// Helper: create a read ToolCall + ToolResult pair.
    fn read_call_result(call_id: &str, path: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "read", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "read", content, ToolResultStatus::Success),
        ]
    }

    /// Build a history with N read pairs of the same file.
    fn history_with_n_reads(path: &str, count: usize) -> Vec<ChatEntry> {
        let mut history = Vec::new();
        for i in 0..count {
            let pair = read_call_result(&format!("tc-{i}"), path, &format!("contents v{i}"));
            history.push(pair[0].clone());
            history.push(pair[1].clone());
        }
        history
    }

    fn worker_with_keep_last(keep_last: usize) -> ConsecutiveReadsAutoPruneWorker {
        worker_with(keep_last, 0)
    }

    fn worker_with(keep_last: usize, min_age: usize) -> ConsecutiveReadsAutoPruneWorker {
        ConsecutiveReadsAutoPruneWorker {
            config: ConsecutiveReadsAutoPruneConfig {
                enabled: true,
                keep_last,
                min_age,
            },
        }
    }

    fn evaluate(
        worker: &ConsecutiveReadsAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    #[rstest::rstest]
    #[test]
    fn no_read_calls_produces_no_mutations() {
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("what is 2+2?"),
            ChatEntry::assistant("4"),
        ];
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn single_read_produces_no_mutations() {
        let history = history_with_n_reads("/foo.rs", 1);
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn exact_keep_last_produces_no_mutations() {
        let history = history_with_n_reads("/foo.rs", 3);
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn one_over_keep_last_prunes_oldest_pair() {
        let history = history_with_n_reads("/foo.rs", 4);
        let expected_call_id = history[0].id.clone();
        let expected_result_id = history[1].id.clone();
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);

        assert_eq!(mutations.len(), 2);

        let mut pruned_ids: Vec<_> = mutations
            .iter()
            .map(|m| match m {
                HistoryMutation::SetContextOverride {
                    entry_id, value, ..
                } => {
                    assert_eq!(*value, ContextOverride::ForcedExclude);
                    entry_id.clone()
                }
                other => panic!("expected SetContextOverride, got {other:?}"),
            })
            .collect();
        pruned_ids.sort_by_key(std::string::ToString::to_string);

        let mut expected = vec![expected_call_id, expected_result_id];
        expected.sort_by_key(std::string::ToString::to_string);

        assert_eq!(pruned_ids, expected);
    }

    #[rstest::rstest]
    #[test]
    fn two_over_keep_last_prunes_two_oldest_pairs() {
        let history = history_with_n_reads("/foo.rs", 5);
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);

        // 2 oldest pairs × 2 entries each = 4 mutations.
        assert_eq!(mutations.len(), 4);
    }

    #[rstest::rstest]
    #[test]
    fn multiple_files_pruned_independently() {
        let mut history = Vec::new();

        // 4 reads of /a.rs (exceeds keep_last=2 by 2 → 4 mutations)
        for i in 0..4 {
            let pair = read_call_result(&format!("tc-a-{i}"), "/a.rs", &format!("a v{i}"));
            history.push(pair[0].clone());
            history.push(pair[1].clone());
        }

        // 2 reads of /b.rs (exactly keep_last=2 → 0 mutations)
        for i in 0..2 {
            let pair = read_call_result(&format!("tc-b-{i}"), "/b.rs", &format!("b v{i}"));
            history.push(pair[0].clone());
            history.push(pair[1].clone());
        }

        let worker = worker_with_keep_last(2);
        let mutations = evaluate(&worker, history);

        // 2 pruned pairs for /a.rs × 2 entries each = 4 mutations
        assert_eq!(mutations.len(), 4);
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_call_and_result_produces_no_duplicate() {
        let mut history = history_with_n_reads("/foo.rs", 4);
        // Mark the oldest pair as already excluded.
        history[0].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history[1].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_call_only_prunes_result() {
        let mut history = history_with_n_reads("/foo.rs", 4);
        // Mark only the call as excluded.
        history[0].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let expected_result_id = history[1].id.clone();

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);

        // Only the result should get a mutation.
        assert_eq!(mutations.len(), 1);
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, expected_result_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_result_only_prunes_call() {
        let mut history = history_with_n_reads("/foo.rs", 4);
        // Mark only the result as excluded.
        history[1].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let expected_call_id = history[0].id.clone();

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);

        // Only the call should get a mutation.
        assert_eq!(mutations.len(), 1);
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, expected_call_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_call_only_prunes_result() {
        let mut history = history_with_n_reads("/foo.rs", 4);
        // Mark only the call as force-included.
        history[0].context_override = ContextOverride::ForcedInclude;
        let expected_result_id = history[1].id.clone();

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);

        // Only the result should get a mutation.
        assert_eq!(mutations.len(), 1);
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, expected_result_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn orphaned_call_without_result_is_skipped() {
        let history = vec![ChatEntry::tool_call(
            "tc-orphan",
            "read",
            r#"{"path": "/foo.rs"}"#,
        )];

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn read_without_path_argument_is_skipped() {
        let history = vec![
            ChatEntry::tool_call("tc-1", "read", r#"{"offset": 1}"#),
            ChatEntry::tool_result("tc-1", "read", "some output", ToolResultStatus::Success),
        ];

        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn different_paths_tracked_separately() {
        let mut history = Vec::new();

        let pair1 = read_call_result("tc-1", "/abs/path/foo.rs", "abs contents");
        history.push(pair1[0].clone());
        history.push(pair1[1].clone());

        let pair2 = read_call_result("tc-2", "foo.rs", "rel contents");
        history.push(pair2[0].clone());
        history.push(pair2[1].clone());

        let worker = worker_with_keep_last(1);
        let mutations = evaluate(&worker, history);

        // Each path has 1 pair, keep_last=1, so no pruning.
        assert!(
            mutations.is_empty(),
            "different string paths should be tracked independently"
        );
    }

    #[rstest::rstest]
    #[test]
    fn keep_last_1_prunes_all_but_last() {
        let history = history_with_n_reads("/foo.rs", 5);
        let worker = worker_with_keep_last(1);
        let mutations = evaluate(&worker, history);

        // 4 pruned pairs × 2 entries each = 8 mutations.
        assert_eq!(mutations.len(), 8);
    }

    #[rstest::rstest]
    #[test]
    fn empty_history_produces_no_mutations() {
        let worker = worker_with_keep_last(3);
        let mutations = evaluate(&worker, vec![]);
        assert!(mutations.is_empty());
    }

    // ------------------------------------------------------------------
    // min_age protection tests
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_read_pair() {
        // With min_age = 0, old read pairs are pruned even when recent.
        let history = history_with_n_reads("/foo.rs", 4);
        let worker = worker_with(3, 0);
        let mutations = evaluate(&worker, history);
        // 1 pair pruned × 2 entries = 2 mutations.
        assert_eq!(mutations.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_read_pair() {
        // 4 reads of same file, keep_last = 3 → oldest pair normally pruned.
        // History length = 8, oldest pair at idx 0 → age = 7.
        // With min_age = 50, age 7 < 50 → protected → not pruned.
        let history = history_with_n_reads("/foo.rs", 4);
        let worker = worker_with(3, 50);
        let mutations = evaluate(&worker, history);
        assert!(
            mutations.is_empty(),
            "recent read pair must be protected by min_age"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_boundary_strict_less_than_consecutive_reads() {
        // Build history so oldest pair has age exactly at the floor.
        // 4 reads of /foo.rs (8 entries, idx 0..7), then N user entries.
        // history_len = 8 + N. Oldest call at idx 0 → age = 8 + N - 1.
        // For age = N + 7 = min_age = N + 7: NOT protected (strict <).
        // For age = N + 7 < min_age = N + 8: protected.
        const N: usize = 10;
        let mut history = history_with_n_reads("/foo.rs", 4);
        for i in 0..N {
            history.push(ChatEntry::user(format!("padding {i}")));
        }
        // history.len() = 18, oldest call at idx 0, age = 17.

        // Protected case: age 17 < min_age 18.
        let worker = worker_with(3, N + 8);
        let mutations = evaluate(&worker, history.clone());
        assert!(mutations.is_empty(), "age = min_age - 1 must be protected");

        // Not-protected case: age 17 = min_age 17.
        let worker = worker_with(3, N + 7);
        let mutations = evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            2,
            "age = min_age must NOT be protected (strict less-than)"
        );
    }
}
