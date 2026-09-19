//! Edit-read auto-prune worker.
//!
//! When a file is read, all prior `edit` and `write` ToolCall+ToolResult pairs
//! on the same file are marked [`ForcedExclude`]. The read output now represents
//! the current file state, making prior edits/writes stale.
//!
//! # Example
//!
//! ```text
//!  X  [Tool Call]: edit(/foo.rs)         ← pruned (stale before read)
//!  X  [Tool Result] (OK): edit applied   ← pruned
//!     [Tool Call]: read(/foo.rs)          ← triggers pruning of prior edits
//!     [Tool Result] (OK): <contents>
//! ```
//!
//! [`ForcedExclude`]: jinn_core_types::ContextOverride::ForcedExclude

pub use jinn_preferences_config::schemas::auto_prune::EditReadAutoPruneConfig;
// ── Configuration ─────────────────────────────────────────────────────

/// Default enabled state for edit-read auto-prune.
/// Default `min_age` for edit-read auto-prune.
///
/// Number of entries from the end of history within which prior
/// edit/write call+result pairs are protected from pruning
/// when a same-file read occurs.
/// Edit-read auto-prune configuration.
///
/// Serialized as `[auto_prune.edit_read]` in `jinn.toml`.
/// Controls the auto-prune worker that excludes stale edit/write tool calls
/// when a same-file read follows, since the read output represents the
/// current file state.
use std::sync::Arc;

use crate::worker::HistoryWorker;
use jinn_core_types::HistoryMutation;
use jinn_core_types::SessionId;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};

use super::is_within_min_age;

// ── Shared helpers (used by both edit-read and read-edit workers) ──────

/// Tool names that modify files.
pub(super) const MODIFY_TOOLS: &[&str] = &["edit", "write"];

/// Returns `true` if the tool name is a file-modifying tool (`edit` or `write`).
pub(super) fn is_modify_tool(name: &str) -> bool {
    MODIFY_TOOLS.contains(&name)
}

/// Extract the `path` field from a tool call's JSON arguments string.
///
/// Returns `None` if the arguments cannot be parsed or the `path` field is
/// missing or not a string.
pub(super) fn extract_path_from_arguments(arguments: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value
        .get("path")?
        .as_str()
        .map(std::borrow::ToOwned::to_owned)
}

/// Walk forward from a `ToolCall` at `call_idx` to find its matching `ToolResult`.
///
/// Returns `Some((result_entry_id, result_index))` if a match is found.
/// Returns `None` if no result exists (pending/orphaned).
pub(super) fn find_matching_result(
    history: &[ChatEntry],
    call_idx: usize,
    tool_call_id: &str,
) -> Option<(jinn_core_types::ChatEntryId, usize)> {
    // ToolResults appear after their ToolCall, so scan forward only.
    for (j, entry) in history.iter().enumerate().skip(call_idx + 1) {
        if let ChatEntryKind::ToolResult { id, .. } = &entry.kind
            && id == tool_call_id
        {
            return Some((entry.id.clone(), j));
        }
    }
    // No matching result found — the call is still pending or orphaned.
    None
}

// ── Worker ─────────────────────────��──────────────────────────────────

/// Edit-read auto-prune worker.
///
/// For each `read` tool call in history, walks backward to find all prior
/// `edit`/`write` ToolCall+ToolResult pairs on the same file and marks them
/// [`ForcedExclude`]. The read output represents current state, making those
/// edits/writes stale.
#[derive(Clone)]
pub struct EditReadAutoPruneWorker {
    /// Configuration for the edit-read auto-prune strategy.
    pub config: EditReadAutoPruneConfig,
}

#[async_trait::async_trait]
impl HistoryWorker for EditReadAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-edit-read"
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
            let read_path = match &entry.kind {
                ChatEntryKind::ToolCall {
                    name, arguments, ..
                } if name == "read" => {
                    let Some(path) = extract_path_from_arguments(arguments) else {
                        continue;
                    };
                    path
                }
                _ => continue,
            };

            // Walk backward from the read to find prior edit/write calls on the same file.
            prune_backward(
                &history,
                i,
                &read_path,
                self.config.min_age,
                &mut mutations,
                self.name(),
            );
        }

        mutations
    }
}

/// Walk backward from a read tool call at `read_index` and prune all
/// edit/write ToolCall+ToolResult pairs on the same file path.
///
/// Runs regardless of whether the read itself is excluded — stale edits are
/// noise even if the read is already pruned.
///
/// Entries whose age (`history.len() - entry_idx - 1`) is less than
/// `min_age` are protected: no mutations are emitted for either half of
/// the pair. This prevents the model from "forgetting" that it wrote a
/// file shortly before reading it back.
fn prune_backward(
    history: &[ChatEntry],
    read_index: usize,
    read_path: &str,
    min_age: usize,
    mutations: &mut Vec<HistoryMutation>,
    worker_name: &str,
) {
    let history_len = history.len();
    // Walk backward from the read to find prior edit/write calls on the same file.
    for j in (0..read_index).rev() {
        let Some(back_entry) = history.get(j) else {
            continue;
        };

        // Only interested in edit/write tool calls targeting the same file path.
        let back_call_id = match &back_entry.kind {
            ChatEntryKind::ToolCall {
                name,
                arguments,
                id,
                ..
            } if is_modify_tool(name)
                && extract_path_from_arguments(arguments).is_some_and(|p| p == read_path) =>
            {
                id.clone()
            }
            _ => continue,
        };

        // Skip young entries — `min_age` protection.
        if is_within_min_age(history_len, j, min_age) {
            continue;
        }

        let back_call_entry_id = back_entry.id.clone();
        let back_call_protected = back_entry.is_protected_from_prune();

        // Walk forward from this edit/write call to find its matching ToolResult.
        // The result may appear anywhere after the call (not necessarily right after).
        let back_result = find_matching_result(history, j, &back_call_id);

        let back_result_protected = back_result.as_ref().is_some_and(|(_, k)| {
            history
                .get(*k)
                .is_some_and(jinn_core_types::ChatEntry::is_protected_from_prune)
        });

        // Skip if both call and result are protected — nothing to do.
        if back_call_protected && back_result_protected {
            continue;
        }

        if !back_call_protected {
            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: back_call_entry_id,
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: worker_name.to_owned(),
                },
            });
        }
        if let Some((result_id, _)) = back_result.filter(|_| !back_result_protected) {
            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: result_id,
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: worker_name.to_owned(),
                },
            });
        }
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

    fn worker() -> EditReadAutoPruneWorker {
        worker_with_min_age(0)
    }

    /// Build a worker with a specific `min_age` floor.
    fn worker_with_min_age(min_age: usize) -> EditReadAutoPruneWorker {
        EditReadAutoPruneWorker {
            config: EditReadAutoPruneConfig {
                enabled: true,
                min_age,
            },
        }
    }

    /// Evaluate the default worker (`min_age = 0`) synchronously for tests.
    fn evaluate(history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        evaluate_with(&worker(), history)
    }

    /// Evaluate an arbitrary worker synchronously for tests.
    fn evaluate_with(w: &EditReadAutoPruneWorker, history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
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
    fn extract_path_from_valid_json() {
        let path = extract_path_from_arguments(r#"{"path": "/foo/bar.rs"}"#);
        assert_eq!(path, Some("/foo/bar.rs".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn extract_path_from_json_with_extra_fields() {
        let path = extract_path_from_arguments(r#"{"path": "/foo.rs", "offset": 1, "limit": 50}"#);
        assert_eq!(path, Some("/foo.rs".to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn extract_path_returns_none_for_missing_path() {
        let path = extract_path_from_arguments(r#"{"file": "/foo.rs"}"#);
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    #[test]
    fn extract_path_returns_none_for_malformed_json() {
        let path = extract_path_from_arguments("not json");
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    #[test]
    fn extract_path_returns_none_for_non_string_path() {
        let path = extract_path_from_arguments(r#"{"path": 42}"#);
        assert_eq!(path, None);
    }

    #[rstest::rstest]
    #[test]
    fn is_modify_tool_recognizes_edit_and_write() {
        assert!(is_modify_tool("edit"));
        assert!(is_modify_tool("write"));
        assert!(!is_modify_tool("read"));
        assert!(!is_modify_tool("bash"));
    }

    #[rstest::rstest]
    #[test]
    fn no_edit_read_pattern_produces_no_mutations() {
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
    fn backward_prunes_prior_edits_on_same_file() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/foo.rs", "edit 1 applied");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let edit2 = edit_call_result("tc-2", "/foo.rs", "edit 2 applied");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());
        let read = read_call_result("tc-3", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        // edit1 call+result + edit2 call+result = 4 backward mutations.
        assert_eq!(
            mutations.len(),
            4,
            "both prior edits should be backward-pruned"
        );

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&edit1[0].id));
        assert!(ids.contains(&edit1[1].id));
        assert!(ids.contains(&edit2[0].id));
        assert!(ids.contains(&edit2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn backward_prunes_prior_writes_on_same_file() {
        let mut history = Vec::new();
        let write1 = write_call_result("tc-1", "/foo.rs", "written 1");
        history.push(write1[0].clone());
        history.push(write1[1].clone());
        let write2 = write_call_result("tc-2", "/foo.rs", "written 2");
        history.push(write2[0].clone());
        history.push(write2[1].clone());
        let read = read_call_result("tc-3", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        assert_eq!(
            mutations.len(),
            4,
            "both prior writes should be backward-pruned"
        );

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&write1[0].id));
        assert!(ids.contains(&write1[1].id));
        assert!(ids.contains(&write2[0].id));
        assert!(ids.contains(&write2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn backward_prunes_mixed_prior_edits_and_writes() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/foo.rs", "edit applied");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let write1 = write_call_result("tc-2", "/foo.rs", "written");
        history.push(write1[0].clone());
        history.push(write1[1].clone());
        let read = read_call_result("tc-3", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        assert_eq!(
            mutations.len(),
            4,
            "prior edit and write should both be backward-pruned"
        );

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&edit1[0].id));
        assert!(ids.contains(&edit1[1].id));
        assert!(ids.contains(&write1[0].id));
        assert!(ids.contains(&write1[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn backward_no_mutation_when_no_prior_edits_or_writes() {
        let mut history = Vec::new();
        let read = read_call_result("tc-1", "/foo.rs", "file contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        assert!(mutations.is_empty(), "nothing to backward-prune");
    }

    #[rstest::rstest]
    #[test]
    fn backward_does_not_prune_different_files() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/bar.rs", "edit on bar");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let read = read_call_result("tc-2", "/foo.rs", "foo contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        assert!(
            mutations.is_empty(),
            "edit on different file should not be pruned"
        );
    }

    #[rstest::rstest]
    #[test]
    fn backward_does_not_prune_reads() {
        // Given a read followed by another read on the same file.
        let mut history = Vec::new();
        let read1 = read_call_result("tc-1", "/foo.rs", "contents v1");
        history.push(read1[0].clone());
        history.push(read1[1].clone());
        let read2 = read_call_result("tc-2", "/foo.rs", "contents v2");
        history.push(read2[0].clone());
        history.push(read2[1].clone());

        let mutations = evaluate(history);
        // Reads should never be pruned by edit-read worker.
        assert!(
            mutations.is_empty(),
            "reads should never be pruned by edit-read worker"
        );
    }

    #[rstest::rstest]
    #[test]
    fn backward_already_excluded_edit_no_duplicate() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/foo.rs", "edit applied");
        let mut edit_call = edit1[0].clone();
        edit_call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let mut edit_result = edit1[1].clone();
        edit_result.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(edit_call);
        history.push(edit_result);
        let read = read_call_result("tc-2", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        assert!(
            mutations.is_empty(),
            "already-excluded edit should not produce duplicate mutations"
        );
    }

    #[rstest::rstest]
    #[test]
    fn backward_runs_even_when_read_already_excluded() {
        let mut history = Vec::new();
        let edit1 = edit_call_result("tc-1", "/foo.rs", "edit applied");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());
        let read = read_call_result("tc-2", "/foo.rs", "contents");
        let mut read_call = read[0].clone();
        read_call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let mut read_result = read[1].clone();
        read_result.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(read_call);
        history.push(read_result);

        let mutations = evaluate(history);
        // Even though the read is fully excluded, backward pruning should still run.
        assert_eq!(
            mutations.len(),
            2,
            "prior edit should still be backward-pruned"
        );

        let ids = mutation_ids(&mutations);
        assert!(ids.contains(&edit1[0].id));
        assert!(ids.contains(&edit1[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn backward_skips_edit_without_result() {
        let mut history = Vec::new();
        let orphan_entry = ChatEntry::tool_call("tc-orphan", "edit", r#"{"path": "/foo.rs"}"#);
        let orphan_id = orphan_entry.id.clone();
        history.push(orphan_entry);
        let read = read_call_result("tc-2", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        // The orphan edit call has no result to find. The call itself should be pruned.
        assert_eq!(
            mutations.len(),
            1,
            "orphan edit call should still be pruned"
        );

        let ids = mutation_ids(&mutations);
        assert!(
            ids.contains(&orphan_id),
            "orphan edit call should be pruned"
        );
    }

    #[rstest::rstest]
    #[test]
    fn backward_prunes_five_prior_edits() {
        let mut history = Vec::new();
        let mut prior_edits = Vec::new();
        for i in 0..5 {
            let edit = edit_call_result(&format!("tc-{i}"), "/foo.rs", &format!("edit {i}"));
            history.push(edit[0].clone());
            history.push(edit[1].clone());
            prior_edits.push(edit);
        }
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let mutations = evaluate(history);
        // 5 edits × 2 (call+result) = 10 backward mutations.
        assert_eq!(mutations.len(), 10);

        let ids = mutation_ids(&mutations);
        for edit in &prior_edits {
            assert!(ids.contains(&edit[0].id), "edit call should be pruned");
            assert!(ids.contains(&edit[1].id), "edit result should be pruned");
        }
    }

    /// Write at the end of a short history is protected by `min_age`: no
    /// `ForcedExclude` mutation is emitted for the write's call or result,
    /// even though a same-file read appears later in history.
    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_write_from_backward_prune() {
        let mut history = Vec::new();
        let write = write_call_result("tc-write", "/foo.rs", "written");
        history.push(write[0].clone());
        history.push(write[1].clone());
        // Two non-tool entries to pad age without adding candidates.
        history.push(ChatEntry::user("ok"));
        history.push(ChatEntry::assistant("ack"));
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        // history.len() = 6; write call is at idx 0, age = 5.
        // With min_age = 10, age 5 < 10 → write is protected.

        let w = worker_with_min_age(10);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            !ids.contains(&write[0].id),
            "write call should be protected by min_age"
        );
        assert!(
            !ids.contains(&write[1].id),
            "write result should be protected by min_age"
        );
    }

    /// `min_age = 0` reproduces pre-fix behavior: the same write from the
    /// `min_age_protects_recent_write_from_backward_prune` test is now
    /// pruned together with its result.
    #[rstest::rstest]
    #[test]
    fn min_age_zero_backward_prunes_as_before() {
        let mut history = Vec::new();
        let write = write_call_result("tc-write", "/foo.rs", "written");
        history.push(write[0].clone());
        history.push(write[1].clone());
        history.push(ChatEntry::user("ok"));
        history.push(ChatEntry::assistant("ack"));
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());

        let w = worker_with_min_age(0);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            ids.contains(&write[0].id),
            "write call should be pruned with min_age = 0"
        );
        assert!(
            ids.contains(&write[1].id),
            "write result should be pruned with min_age = 0"
        );
    }

    /// A write well past the `min_age` floor is pruned as before — the
    /// protection only applies to entries near the end of history.
    #[rstest::rstest]
    #[test]
    fn old_write_still_pruned_in_long_history() {
        let mut history = Vec::new();
        let write = write_call_result("tc-write", "/foo.rs", "written");
        history.push(write[0].clone());
        history.push(write[1].clone());
        // Pad with 100 entries to push the write well past min_age = 10.
        for i in 0..50 {
            history.push(ChatEntry::user(format!("u-{i}")));
            history.push(ChatEntry::assistant(format!("a-{i}")));
        }
        let read = read_call_result("tc-read", "/foo.rs", "contents");
        history.push(read[0].clone());
        history.push(read[1].clone());
        // history.len() = 104; write call at idx 0, age = 103.
        // With min_age = 10, age 103 ≥ 10 → write is pruned.

        let w = worker_with_min_age(10);
        let mutations = evaluate_with(&w, history);

        let ids = mutation_ids(&mutations);
        assert!(
            ids.contains(&write[0].id),
            "old write call should be pruned regardless of min_age"
        );
        assert!(
            ids.contains(&write[1].id),
            "old write result should be pruned regardless of min_age"
        );
    }
}
