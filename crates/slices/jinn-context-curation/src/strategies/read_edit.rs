//! Read-edit auto-prune worker.
//!
//! After a file is read, once enough subsequent `edit` or `write` calls to the same
//! file have occurred (configurable via `threshold`, default 2), the read
//! ToolCall+ToolResult are marked [`ForcedExclude`]. The read contents are now
//! stale — the file has changed enough that the cached output no longer
//! represents current state.
//!
//! # Example
//!
//! ```text
//!     [Tool Call]: read(/foo.rs)          ← triggers forward count
//!     [Tool Result] (OK): <contents>
//!     [Tool Call]: edit(/foo.rs)
//!     [Tool Result] (OK): edit applied
//!  X  [Tool Call]: write(/foo.rs)        ← 2nd edit/write → triggers read pruning
//!     [Tool Result] (OK): written        ← (this write is NOT pruned)
//! ```
//!
//! The write itself is never pruned by this worker — only the stale read.
//! The [`EditReadAutoPruneWorker`] handles the reverse direction (pruning old
//! edits when a file is re-read).
//!
//! [`ForcedExclude`]: jinn_core_types::ContextOverride::ForcedExclude
//! [`EditReadAutoPruneWorker`]: super::edit_read::EditReadAutoPruneWorker

use std::sync::Arc;

pub use jinn_preferences_config::schemas::auto_prune::ReadEditAutoPruneConfig;

use crate::worker::HistoryWorker;
use jinn_core_types::HistoryMutation;
use jinn_core_types::SessionId;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};

use super::edit_read::{extract_path_from_arguments, find_matching_result, is_modify_tool};
use super::is_within_min_age;

/// Default enabled state for read-edit auto-prune.
/// Default `min_age` for read-edit auto-prune.
///
/// Number of entries from the end of history within which
/// read call+result pairs are protected from pruning.
/// Default threshold for read-edit auto-prune.
///
/// Number of edit/write operations on the same file required before
/// pruning the prior read call+result pair.
/// Read-edit auto-prune configuration.
///
/// Serialized as `[auto_prune.read_edit]` in `jinn.toml`.
/// Controls the auto-prune worker that excludes stale read tool calls
/// after the file has been edited a configurable number of times.
/// Count how many edit/write ToolCalls on the same file path appear after
/// the given index. Stops early once the count reaches `threshold`.
///
/// This determines whether the read's contents are stale — once enough
/// modifications have happened after the read, the read output no longer
/// represents the file's current state.
fn count_subsequent_modifications(
    history: &[ChatEntry],
    after_idx: usize,
    file_path: &str,
    threshold: usize,
) -> usize {
    let mut count: usize = 0;
    for entry in history.iter().skip(after_idx + 1) {
        if let ChatEntryKind::ToolCall {
            name, arguments, ..
        } = &entry.kind
            && is_modify_tool(name)
            && extract_path_from_arguments(arguments).is_some_and(|p| p == file_path)
        {
            count += 1;
            if count >= threshold {
                break;
            }
        }
    }
    count
}

/// Read-edit auto-prune worker.
///
/// For each `read` tool call, counts subsequent `edit`/`write` calls on the same
/// file. Once the count reaches `config.threshold`, the read call+result are marked
/// [`ForcedExclude`] — the file has changed enough that the read contents are stale.
///
/// [`ForcedExclude`]: jinn_core_types::ContextOverride::ForcedExclude
#[derive(Clone)]
pub struct ReadEditAutoPruneWorker {
    /// Configuration for the read-edit auto-prune strategy.
    pub config: ReadEditAutoPruneConfig,
}

#[async_trait::async_trait]
impl HistoryWorker for ReadEditAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-read-edit"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let mut mutations = Vec::new();
        let history_len = history.len();

        for i in 0..history_len {
            let Some(entry) = history.get(i) else {
                continue;
            };

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

            let read_call_entry_id = entry.id.clone();
            let call_protected = entry.is_protected_from_prune();

            // ── Forward pruning ──────────────────────────────────────────
            // Find the read's corresponding ToolResult. If none found,
            // skip forward pruning — an orphaned read has no pair to prune.
            let Some((result_entry_id, result_idx)) =
                find_matching_result(&history, i, &tool_call_id)
            else {
                continue;
            };

            let result_protected = history
                .get(result_idx)
                .is_some_and(jinn_core_types::ChatEntry::is_protected_from_prune);

            // Count how many edit/write calls to the same file appear after
            // this read. Once the threshold is reached, the read is stale.
            let modify_count =
                count_subsequent_modifications(&history, i, &read_path, self.config.threshold);

            if modify_count >= self.config.threshold {
                // min_age protection: don't prune reads that are too close to
                // the end of history.
                let call_within_min_age = is_within_min_age(history_len, i, self.config.min_age);
                let result_within_min_age =
                    is_within_min_age(history_len, result_idx, self.config.min_age);

                if !call_protected && !call_within_min_age {
                    mutations.push(HistoryMutation::SetContextOverride {
                        entry_id: read_call_entry_id,
                        value: ContextOverride::ForcedExclude,
                        source: ChangeSource::Worker {
                            name: self.name().to_owned(),
                        },
                    });
                }
                if !result_protected && !result_within_min_age {
                    mutations.push(HistoryMutation::SetContextOverride {
                        entry_id: result_entry_id,
                        value: ContextOverride::ForcedExclude,
                        source: ChangeSource::Worker {
                            name: self.name().to_owned(),
                        },
                    });
                }
            }
        }

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
        clippy::similar_names,
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

    /// Helper: create an edit ToolCall + ToolResult pair.
    fn edit_call_result(call_id: &str, path: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "edit", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "edit", content, ToolResultStatus::Success),
        ]
    }

    /// Helper: create a write ToolCall + ToolResult pair.
    fn write_call_result(call_id: &str, path: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "write", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "write", content, ToolResultStatus::Success),
        ]
    }

    fn worker() -> ReadEditAutoPruneWorker {
        worker_with_min_age(0)
    }

    /// Build a worker with a specific `min_age` floor.
    fn worker_with_min_age(min_age: usize) -> ReadEditAutoPruneWorker {
        ReadEditAutoPruneWorker {
            config: ReadEditAutoPruneConfig {
                enabled: true,
                min_age,
                threshold: 2,
            },
        }
    }

    /// Build a worker with a specific `threshold`.
    fn worker_with_threshold(threshold: usize) -> ReadEditAutoPruneWorker {
        ReadEditAutoPruneWorker {
            config: ReadEditAutoPruneConfig {
                enabled: true,
                min_age: 0,
                threshold,
            },
        }
    }

    /// Evaluate the default worker (`min_age = 0`) synchronously for tests.
    fn evaluate(history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        evaluate_with(&worker(), history)
    }

    /// Evaluate an arbitrary worker synchronously for tests.
    fn evaluate_with(w: &ReadEditAutoPruneWorker, history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    /// Collect mutation entry IDs from a list of mutations.
    fn mutation_ids(mutations: &[HistoryMutation]) -> Vec<jinn_core_types::ChatEntryId> {
        mutations
            .iter()
            .filter_map(|m| match m {
                HistoryMutation::SetContextOverride {
                    entry_id, value, ..
                } => {
                    assert_eq!(*value, ContextOverride::ForcedExclude);
                    Some(entry_id.clone())
                }
                _ => None,
            })
            .collect()
    }

    #[rstest::rstest]
    #[test]
    fn no_read_edit_pattern_produces_no_mutations() {
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("what is 2+2?"),
            ChatEntry::assistant("4"),
        ];
        let mutations = evaluate(history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn read_edit_does_not_prune_edits() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        let read = read_call_result("tc-3", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        // read_edit only prunes reads, never edits. This read has 0 subsequent
        // edits, so it shouldn't be pruned either.
        assert!(
            mutations.is_empty(),
            "read_edit worker should never prune edits"
        );
    }

    #[rstest::rstest]
    #[test]
    fn read_then_one_edit_no_prune() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit = edit_call_result("tc-2", "/foo.rs", "edit applied");
        history.push(edit[0].clone());
        history.push(edit[1].clone());

        let mutations = evaluate(history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn read_then_two_edits_prunes_both_call_and_result() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1 applied");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2 applied");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 2, "should prune both call and result");

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read[0].id), "read ToolCall should be pruned");
        assert!(
            ids.contains(&read[1].id),
            "read ToolResult should be pruned"
        );
    }

    #[rstest::rstest]
    #[test]
    fn read_edit_different_files_no_prune() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/bar.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/bar.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_call_no_duplicate_mutation() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        let mut call = read[0].clone();
        call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(call);
        history.push(read[1].clone()); // result NOT excluded
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 1, "only result should be pruned");

        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, read[1].id, "should target read ToolResult");
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_result_no_duplicate_mutation() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone()); // call NOT excluded
        let mut result = read[1].clone();
        result.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(result);
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 1, "only call should be pruned");

        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, read[0].id, "should target read ToolCall");
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_call_no_forward_mutation() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        let mut call = read[0].clone();
        call.context_override = ContextOverride::ForcedInclude;
        history.push(call);
        history.push(read[1].clone()); // result NOT protected
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 1, "only result should be pruned");

        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, read[1].id, "should target read ToolResult");
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn both_already_excluded_no_forward_mutations() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        let mut call = read[0].clone();
        call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let mut result = read[1].clone();
        result.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(call);
        history.push(result);
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert!(
            mutations.is_empty(),
            "should not produce forward-pruning mutations for already-excluded entries"
        );
    }

    #[rstest::rstest]
    #[test]
    fn multiple_reads_same_file_both_forward_pruned() {
        let mut history = Vec::new();
        // First read of /foo.rs
        let read1 = read_call_result("tc-1", "/foo.rs", "contents v1");
        history.push(read1[0].clone());
        history.push(read1[1].clone());
        // Second read of /foo.rs
        let read2 = read_call_result("tc-2", "/foo.rs", "contents v2");
        history.push(read2[0].clone());
        history.push(read2[1].clone());
        // Two edits of /foo.rs
        let edit1 = edit_call_result("tc-3", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-4", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        // 2 mutations per read (call+result) × 2 reads = 4 forward-pruning mutations.
        assert_eq!(
            mutations.len(),
            4,
            "both reads should be pruned (call+result each)"
        );

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read1[0].id));
        assert!(ids.contains(&read1[1].id));
        assert!(ids.contains(&read2[0].id));
        assert!(ids.contains(&read2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn interleaved_files_tracked_independently() {
        let mut history = Vec::new();
        let read_a = read_call_result("tc-1", "/a.rs", "contents a");
        history.push(read_a[0].clone());
        history.push(read_a[1].clone());
        let read_b = read_call_result("tc-2", "/b.rs", "contents b");
        history.push(read_b[0].clone());
        history.push(read_b[1].clone());
        let edit_a1 = edit_call_result("tc-3", "/a.rs", "edit a1");
        history.push(edit_a1[0].clone());
        history.push(edit_a1[1].clone());
        let edit_b1 = edit_call_result("tc-4", "/b.rs", "edit b1");
        history.push(edit_b1[0].clone());
        history.push(edit_b1[1].clone());
        let edit_a2 = edit_call_result("tc-5", "/a.rs", "edit a2");
        history.push(edit_a2[0].clone());
        history.push(edit_a2[1].clone());
        let edit_b2 = edit_call_result("tc-6", "/b.rs", "edit b2");
        history.push(edit_b2[0].clone());
        history.push(edit_b2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 4, "both reads should be pruned");

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read_a[0].id));
        assert!(ids.contains(&read_a[1].id));
        assert!(ids.contains(&read_b[0].id));
        assert!(ids.contains(&read_b[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn orphan_read_no_result_no_mutation() {
        let mut history = Vec::new();
        // Read tool call but no corresponding tool result.
        history.push(ChatEntry::tool_call(
            "tc-orphan",
            "read",
            r#"{"path": "/foo.rs"}"#,
        ));
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let mutations = evaluate(history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn edit_before_read_not_counted_forward() {
        let mut history = Vec::new();
        // Edit before the read — should not count toward forward threshold.
        let edit0 = edit_call_result("tc-0", "/foo.rs", "edit 0");
        history.push(edit0[0].clone());
        history.push(edit0[1].clone());
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        // Only 1 edit after the read.
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());

        let mutations = evaluate(history);
        // The read should NOT be forward-pruned (only 1 edit after it).
        // This worker does not backward-prune edits — that's EditReadAutoPruneWorker.
        let ids = mutation_ids(&mutations);
        assert!(
            !ids.contains(&read[0].id),
            "read should not be forward-pruned with only 1 subsequent edit"
        );
        assert!(
            !ids.contains(&read[1].id),
            "read result should not be forward-pruned with only 1 subsequent edit"
        );
        // Edits are never pruned by this worker.
        assert!(
            !ids.contains(&edit0[0].id),
            "edits are never pruned by read_edit worker"
        );
    }

    #[rstest::rstest]
    #[test]
    fn three_edits_still_prunes() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        let edit3 = edit_call_result("tc-4", "/foo.rs", "edit 3");
        history.push(edit3[0].clone());
        history.push(edit3[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 2, "should still prune read with 3 edits");
    }

    #[rstest::rstest]
    #[test]
    fn read_then_two_writes_prunes_read() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let write1 = write_call_result("tc-2", "/foo.rs", "written 1");
        history.push(write1[0].clone());
        history.push(write1[1].clone());
        let write2 = write_call_result("tc-3", "/foo.rs", "written 2");
        history.push(write2[0].clone());
        history.push(write2[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 2, "should prune read after 2 writes");

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read[0].id));
        assert!(ids.contains(&read[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn read_then_mixed_edit_and_write_prunes_read() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit applied");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let write1 = write_call_result("tc-3", "/foo.rs", "written");
        history.push(write1[0].clone());
        history.push(write1[1].clone());

        let mutations = evaluate(history);
        assert_eq!(mutations.len(), 2, "1 edit + 1 write = threshold met");

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read[0].id));
        assert!(ids.contains(&read[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn read_then_one_write_no_prune() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let write1 = write_call_result("tc-2", "/foo.rs", "written");
        history.push(write1[0].clone());
        history.push(write1[1].clone());

        let mutations = evaluate(history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn threshold_3_requires_3_edits() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        // With threshold = 3, only 2 edits is not enough.
        let w = worker_with_threshold(3);
        let mutations = evaluate_with(&w, history);
        assert!(
            mutations.is_empty(),
            "2 edits should not meet threshold of 3"
        );
    }

    #[rstest::rstest]
    #[test]
    fn threshold_3_met_with_3_edits() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-2", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        let edit3 = edit_call_result("tc-4", "/foo.rs", "edit 3");
        history.push(edit3[0].clone());
        history.push(edit3[1].clone());

        // With threshold = 3, 3 edits is enough.
        let w = worker_with_threshold(3);
        let mutations = evaluate_with(&w, history);
        assert_eq!(mutations.len(), 2, "3 edits should meet threshold of 3");

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&read[0].id));
        assert!(ids.contains(&read[1].id));
    }

    /// Read near the end of history is protected by `min_age`: no
    /// `ForcedExclude` mutation is emitted, even though 2 edits follow.
    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_read() {
        let mut history = Vec::new();
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-e1", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-e2", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        // history.len() = 6; read call at idx 0, age = 5.
        // With min_age = 10, age 5 < 10 → read is protected.

        let w = worker_with_min_age(10);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            !ids.contains(&read[0].id),
            "read call should be protected by min_age"
        );
        assert!(
            !ids.contains(&read[1].id),
            "read result should be protected by min_age"
        );
    }

    /// `min_age = 0` reproduces pre-fix behavior: the read from the
    /// `min_age_protects_recent_read` test is now pruned.
    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_as_before() {
        let mut history = Vec::new();
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        let edit1 = edit_call_result("tc-e1", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-e2", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        let w = worker_with_min_age(0);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            ids.contains(&read[0].id),
            "read call should be pruned with min_age = 0"
        );
        assert!(
            ids.contains(&read[1].id),
            "read result should be pruned with min_age = 0"
        );
    }

    /// A read well past the `min_age` floor is pruned as before — the
    /// protection only applies to entries near the end of history.
    #[rstest::rstest]
    #[test]
    fn old_read_still_pruned_in_long_history() {
        let mut history = Vec::new();
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        // Pad with 100 entries to push the read well past min_age = 10.
        for i in 0..50 {
            history.push(ChatEntry::user(format!("u-{i}")));
            history.push(ChatEntry::assistant(format!("a-{i}")));
        }
        let edit1 = edit_call_result("tc-e1", "/foo.rs", "edit 1");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-e2", "/foo.rs", "edit 2");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        // history.len() = 106; read call at idx 0, age = 105.
        // With min_age = 10, age 105 ≥ 10 → read is pruned.

        let w = worker_with_min_age(10);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            ids.contains(&read[0].id),
            "old read call should be pruned regardless of min_age"
        );
        assert!(
            ids.contains(&read[1].id),
            "old read result should be pruned regardless of min_age"
        );
    }
}
