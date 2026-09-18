//! Broken-edit auto-prune worker.
//!
//! Detects `edit` tool calls whose `ToolResult` has `status: Failure` and marks
//! both the `ToolCall` and `ToolResult` as [`ForcedExclude`] once enough
//! conversation has accumulated after the failed edit. This removes
//! useless failed-edit noise from the LLM context window.
//!
//! The `min_age` field (default: 10) is a raw-distance protection floor:
//! a failed-edit pair is only pruned when its position in history is at
//! least `min_age` slots from the end of history. A `min_age` of 0
//! disables protection entirely (everything is eligible, back-compat
//! baseline).
//!
//! # Example
//!
//! ```text
//!    [User]: fix the bug in /foo.rs
//! X  [Tool Call]: edit(/foo.rs)
//! X  [Tool Result] (Failure): stale anchor
//!    [Tool Call]: edit(/foo.rs)
//!    [Tool Result] (OK): edit applied
//!    [Assistant]: I've fixed the bug.
//!
//! [`ForcedExclude`]: crate::feat::session::chat_entry::ContextOverride::ForcedExclude

use std::sync::Arc;

pub use jinn_preferences_config::schemas::auto_prune::BrokenEditAutoPruneConfig;

use crate::feat::auto_prune_worker::is_within_min_age;
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::feat::session::chat_entry::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};
use crate::feat::session::history_mutation::HistoryMutation;
use crate::feat::session::tool_result_status::ToolResultStatus;
use crate::protocol::SessionId;

/// Default minimum age for broken-edit auto-prune.
/// Default enabled state for broken-edit auto-prune.
/// Broken-edit auto-prune configuration.
///
/// Serialized as `[auto_prune.broken_edit]` in `jinn.toml`.
/// Controls the auto-prune worker that excludes failed edit tool call+result pairs
/// from the LLM context once enough conversation has moved on.
/// Broken-edit auto-prune worker.
/// Inspects history for `edit` tool calls whose results failed. Once the
/// failed `ToolCall` is at least `min_age` entries from the end of history,
/// both the call and its result are excluded from context.
#[derive(Clone)]
pub struct BrokenEditAutoPruneWorker {
    /// Configuration for the broken-edit auto-prune strategy.
    pub config: BrokenEditAutoPruneConfig,
}

/// Walk forward from an edit ToolCall to find its matching ToolResult.
///
/// Returns `Some((entry_id, status))` if a matching result is found.
/// Returns `None` if no result exists (pending/orphaned) or the result
/// is already excluded (the pair is already handled).
fn find_failed_edit_result(
    history: &[ChatEntry],
    call_idx: usize,
    tool_call_id: &str,
) -> Option<(
    crate::feat::session::chat_entry::ChatEntryId,
    ToolResultStatus,
)> {
    // ToolResults appear after their ToolCall, so scan forward only.
    for entry in history.iter().skip(call_idx + 1) {
        if let ChatEntryKind::ToolResult { id, status, .. } = &entry.kind
            && id == tool_call_id
        {
            // Found the matching result — return its ID and status.
            return Some((entry.id.clone(), *status));
        }
    }
    // No matching result found — the call is still pending or orphaned.
    None
}

#[async_trait::async_trait]
impl HistoryWorker for BrokenEditAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-broken-edit"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let mut mutations = Vec::new();

        for i in 0..history.len() {
            let Some(entry) = history.get(i) else {
                continue;
            };

            // Only interested in "edit" tool calls.
            let tool_call_id = match &entry.kind {
                ChatEntryKind::ToolCall { name, id, .. } if name == "edit" => id.clone(),
                _ => continue,
            };

            // Skip if the call is already protected from prune.
            if entry.is_protected_from_prune() {
                continue;
            }

            let edit_call_entry_id = entry.id.clone();

            // Walk forward to find the ToolResult for this edit call.
            // If none found (pending/orphaned), skip — incomplete pairs
            // can't be pruned.
            let Some((result_id, status)) = find_failed_edit_result(&history, i, &tool_call_id)
            else {
                continue;
            };

            // Only prune failed edits — successful ones are useful context.
            if status != ToolResultStatus::Failure {
                continue;
            }

            // Skip if the result is protected from prune — the pair is already handled.
            let result_protected = history
                .iter()
                .skip(i + 1)
                .find(|e| e.id == result_id)
                .is_some_and(ChatEntry::is_protected_from_prune);
            if result_protected {
                continue;
            }

            // Skip if the failed-edit call is within the protection floor.
            // A `min_age` of 0 disables protection entirely.
            if is_within_min_age(history.len(), i, self.config.min_age) {
                continue;
            }

            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: edit_call_entry_id,
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: self.name().to_owned(),
                },
            });
            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: result_id,
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: self.name().to_owned(),
                },
            });
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
        reason = "test code"
    )]

    use super::*;
    use crate::feat::session::chat_entry::ChatEntry;
    use crate::feat::session::tool_result_status::ToolResultStatus;
    use crate::protocol::SessionId;

    /// Helper: create a failed edit ToolCall + ToolResult pair.
    fn failed_edit_call_result(call_id: &str, path: &str, error_msg: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "edit", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "edit", error_msg, ToolResultStatus::Failure),
        ]
    }

    /// Helper: create a successful edit ToolCall + ToolResult pair.
    fn successful_edit_call_result(call_id: &str, path: &str, msg: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "edit", format!(r#"{{"path": "{path}"}}"#)),
            ChatEntry::tool_result(call_id, "edit", msg, ToolResultStatus::Success),
        ]
    }

    /// Build a history with a failed edit pair followed by N user entries (in-context).
    fn history_with_failed_edit_and_tail(path: &str, tail_count: usize) -> Vec<ChatEntry> {
        let mut history = Vec::new();
        let edit = failed_edit_call_result("tc-1", path, "edit failed: stale anchor");
        history.push(edit[0].clone());
        history.push(edit[1].clone());
        for i in 0..tail_count {
            history.push(ChatEntry::user(format!("tail message {i}")));
        }
        history
    }

    /// Build a worker with the given `min_age`.
    fn worker_with_min_age(min_age: usize) -> BrokenEditAutoPruneWorker {
        BrokenEditAutoPruneWorker {
            config: BrokenEditAutoPruneConfig {
                enabled: true,
                min_age,
            },
        }
    }

    async fn run_evaluate(
        worker: &BrokenEditAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        worker.evaluate(&SessionId::new(), Arc::from(history)).await
    }

    fn block_on_evaluate(
        worker: &BrokenEditAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { run_evaluate(worker, history).await })
    }

    #[rstest::rstest]
    #[test]
    fn no_edit_produces_no_mutations() {
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("what is 2+2?"),
            ChatEntry::assistant("4"),
        ];
        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn successful_edit_produces_no_mutations() {
        let mut history = Vec::new();
        let edit = successful_edit_call_result("tc-1", "/foo.rs", "edit applied");
        history.push(edit[0].clone());
        history.push(edit[1].clone());
        for i in 0..100 {
            history.push(ChatEntry::user(format!("tail message {i}")));
        }
        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_failed_edit() {
        // With min_age = 0, the failed edit is pruned even when it is recent.
        let history = history_with_failed_edit_and_tail("/foo.rs", 1);
        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(mutations.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_failed_edit() {
        // Failed edit at idx 0, history len = 12, age = 11. With min_age = 50, protected.
        let history = history_with_failed_edit_and_tail("/foo.rs", 10);
        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_failed_edit_with_long_history() {
        // Long history, failed edit far back, min_age = 0 → prune.
        let history = history_with_failed_edit_and_tail("/foo.rs", 100);
        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(mutations.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn failed_edit_at_boundary_protected() {
        // Failed edit at idx 0. history.len() = 51 (edit + result + 49 user).
        // age = 51 - 0 - 1 = 50. min_age = 50 → 50 < 50 false → NOT protected → prunes.
        let history = history_with_failed_edit_and_tail("/foo.rs", 49);
        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(mutations.len(), 2, "age == min_age is not protected");
    }

    #[rstest::rstest]
    #[test]
    fn failed_edit_one_below_boundary_not_protected() {
        // history.len() = 50, age = 49. min_age = 50 → 49 < 50 → protected.
        let history = history_with_failed_edit_and_tail("/foo.rs", 48);
        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty(), "age = min_age - 1 is protected");
    }

    // ------------------------------------------------------------------
    // config_alias_min_tail_entries_still_parses
    //
    // Legacy `min_tail_entries` field must still deserialize via serde
    // alias and populate the new `min_age` field. Back-compat for users
    // with existing `jinn.toml` files.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn config_alias_min_tail_entries_still_parses() {
        // Given a TOML fragment using the legacy `min_tail_entries` field.
        let toml_src = "
            enabled = true
            min_tail_entries = 10
        ";

        // When deserializing.
        let config: BrokenEditAutoPruneConfig = toml::from_str(toml_src).expect("parse");

        // Then the legacy field populates the new min_age field via serde alias.
        assert_eq!(config.min_age, 10);
        assert!(config.enabled);
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_call_produces_no_duplicate_mutation() {
        let mut history = history_with_failed_edit_and_tail("/foo.rs", 100);
        // Mark the edit ToolCall as already excluded.
        history[0].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );

        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_result_produces_no_duplicate_mutation() {
        let mut history = history_with_failed_edit_and_tail("/foo.rs", 100);
        // Mark the edit ToolResult as already excluded.
        history[1].apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );

        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_call_produces_no_mutation() {
        let mut history = history_with_failed_edit_and_tail("/foo.rs", 100);
        // Mark the edit ToolCall as force-included.
        history[0].context_override = ContextOverride::ForcedInclude;
        let call_id = history[0].id.clone();
        let result_id = history[1].id.clone();

        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        // broken_edit is pair-atomic: if either half is protected, neither mutates.
        // So protecting the call protects the entire pair.
        assert!(
            mutations.is_empty(),
            "pair-atomic: protecting call protects result"
        );
        let _ = (call_id, result_id);
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_result_produces_no_mutation() {
        let mut history = history_with_failed_edit_and_tail("/foo.rs", 100);
        // Mark the edit ToolResult as force-included.
        history[1].context_override = ContextOverride::ForcedInclude;
        let call_id = history[0].id.clone();
        let result_id = history[1].id.clone();

        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        // pair-atomic: protecting the result also protects the call.
        assert!(
            mutations.is_empty(),
            "pair-atomic: protecting result protects call"
        );
        let _ = (call_id, result_id);
    }

    #[rstest::rstest]
    #[test]
    fn multiple_failed_edits_all_pruned_when_old_enough() {
        let mut history = Vec::new();

        // First failed edit.
        let edit1 = failed_edit_call_result("tc-1", "/a.rs", "failed a");
        history.push(edit1[0].clone());
        history.push(edit1[1].clone());

        // Second failed edit.
        let edit2 = failed_edit_call_result("tc-2", "/b.rs", "failed b");
        history.push(edit2[0].clone());
        history.push(edit2[1].clone());

        // Tail entries after last edit.
        for i in 0..100 {
            history.push(ChatEntry::user(format!("tail {i}")));
        }

        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            4,
            "both failed edits should be pruned (2 entries each)"
        );
    }

    #[rstest::rstest]
    #[test]
    fn edit_without_result_produces_no_mutation() {
        let mut history = Vec::new();
        // Edit tool call but no corresponding tool result.
        history.push(ChatEntry::tool_call(
            "tc-orphan",
            "edit",
            r#"{"path": "/foo.rs"}"#,
        ));
        for i in 0..100 {
            history.push(ChatEntry::user(format!("tail {i}")));
        }

        let worker = worker_with_min_age(0);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn only_failed_edits_are_pruned_not_successful_ones() {
        let mut history = Vec::new();

        // Successful edit.
        let ok_edit = successful_edit_call_result("tc-1", "/a.rs", "ok");
        history.push(ok_edit[0].clone());
        history.push(ok_edit[1].clone());

        // Failed edit.
        let fail_edit = failed_edit_call_result("tc-2", "/b.rs", "failed");
        history.push(fail_edit[0].clone());
        history.push(fail_edit[1].clone());

        // Tail entries.
        for i in 0..100 {
            history.push(ChatEntry::user(format!("tail {i}")));
        }

        let worker = worker_with_min_age(50);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            2,
            "only the failed edit pair should be pruned"
        );
    }
}
