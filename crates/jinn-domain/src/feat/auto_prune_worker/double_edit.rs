//! Double-edit auto-prune worker.
//!
//! Caps the number of `edit` and `write` tool call+result pairs per file path.
//! When the count for a file exceeds `max_file_edits`, the oldest pairs are
//! **immediately** excluded from context (both `ToolCall` and `ToolResult`).
//! No tail-entry delay.
//!
//! # Example
//!
//! ```text
//! X  [Tool Call]: edit(/foo.rs)     ← pruned (oldest)
//! X  [Tool Result] (OK): applied   ← pruned
//!    [Tool Call]: write(/foo.rs)    ← kept
//!    [Tool Result] (OK): written   ← kept
//!    [Tool Call]: edit(/foo.rs)     ← kept (newest)
//!    [Tool Result] (OK): applied   ← kept
//! ```
//!
//! With `max_file_edits = 2`, the oldest edit/write pair is pruned.
//!
//! [`ForcedExclude`]: crate::protocol::ContextOverride::ForcedExclude

use std::collections::HashMap;

pub use jinn_preferences_config::schemas::auto_prune::DoubleEditAutoPruneConfig;
use std::sync::Arc;

use crate::feat::auto_prune_worker::is_within_min_age;
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::protocol::HistoryMutation;
use crate::protocol::SessionId;
use crate::protocol::{ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride};

/// Default max file edits for double-edit auto-prune.
/// Default enabled state for double-edit auto-prune.
/// Default `min_age` for double-edit auto-prune.
///
/// Number of entries from the end of history within which edit/write
/// call+result pairs on a file are protected from pruning even when the
/// per-file cap (`max_file_edits`) would otherwise exclude them.
/// Double-edit auto-prune configuration.
///
/// Serialized as `[auto_prune.double_edit]` in `jinn.toml`.
/// Controls the auto-prune worker that caps the number of edit/write
/// tool call+result pairs per file path, keeping only the most recent ones.
/// Double-edit auto-prune worker.
///
/// Inspects history for `edit` and `write` tool calls grouped by file path.
/// When more than `max_file_edits` pairs exist for a single path, the oldest
/// pairs are excluded from context immediately (no tail-entry threshold).
#[derive(Clone)]
pub struct DoubleEditAutoPruneWorker {
    /// Configuration for the double-edit auto-prune strategy.
    pub config: DoubleEditAutoPruneConfig,
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

/// Walk forward from a ToolCall to find its matching ToolResult.
///
/// Returns `None` if no ToolResult with the given `tool_call_id` exists after
/// the call index (e.g. pending, orphaned, or pruned-from-history).
///
/// Override state is intentionally **not** consulted here — the helper is
/// purely a structural lookup. Mutation-emission guards that need to skip
/// protected entries happen in [`build_prune_mutations`], mirroring the other
/// auto-prune workers.
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

/// A collected edit/write tool call paired with its result.
struct EditWritePair {
    call_entry_id: ChatEntryId,
    result_entry_id: ChatEntryId,
    /// Index of the ToolCall in the history slice — used to apply
    /// the per-worker `min_age` floor before emitting a prune.
    call_idx: usize,
    /// True if the call half is already `ForcedInclude` or `ForcedExclude`.
    /// Captured at collection time so emission needs no `history` reference.
    call_protected: bool,
    /// True if the result half is already `ForcedInclude` or `ForcedExclude`.
    result_protected: bool,
}

#[async_trait::async_trait]
impl HistoryWorker for DoubleEditAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-double-edit"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        if self.config.max_file_edits == 0 {
            return Vec::new();
        }

        let groups = collect_edit_write_pairs_by_path(&history);
        build_prune_mutations(
            history.len(),
            groups,
            self.config.max_file_edits,
            self.config.min_age,
            self.name(),
        )
    }
}
/// Scan history for edit/write ToolCalls, resolve each to its result,
/// and group into pairs by file path.
///
/// Skips ToolCalls with no matching ToolResult. **Does not** filter on
/// `context_override` — protected entries (`ForcedInclude` or
/// `ForcedExclude`) are still counted toward the per-file total so the prune
/// window is stable. The suppression of mutations for those entries happens
/// in [`build_prune_mutations`].
fn collect_edit_write_pairs_by_path(history: &[ChatEntry]) -> HashMap<String, Vec<EditWritePair>> {
    // First pass: collect every edit/write ToolCall with its history index
    // and file path. We record indices so the second pass can scan forward
    // from each call to find its matching ToolResult.
    let mut candidates: Vec<(usize, String)> = Vec::new();
    for (i, entry) in history.iter().enumerate() {
        if let ChatEntryKind::ToolCall {
            name, arguments, ..
        } = &entry.kind
            && (name == "edit" || name == "write")
            && let Some(path) = extract_path_from_arguments(arguments)
        {
            candidates.push((i, path));
        }
    }

    // Second pass: for each candidate, resolve it to a complete call+result
    // pair and bucket by file path.
    let mut groups: HashMap<String, Vec<EditWritePair>> = HashMap::new();

    for (call_idx, path) in &candidates {
        let Some(call_entry) = history.get(*call_idx) else {
            continue;
        };

        let ChatEntryKind::ToolCall {
            id: tool_call_id, ..
        } = &call_entry.kind
        else {
            continue;
        };

        // Walk forward to find the ToolResult that matches this call.
        // If none found (pending or orphaned), skip it — incomplete pairs
        // don't count toward the file's total.
        let Some(result_id) = find_matching_result(history, *call_idx, tool_call_id) else {
            continue;
        };

        let call_protected = call_entry.is_protected_from_prune();
        let result_protected = history
            .iter()
            .find(|e| e.id == result_id)
            .is_some_and(ChatEntry::is_protected_from_prune);

        groups.entry(path.clone()).or_default().push(EditWritePair {
            call_entry_id: call_entry.id.clone(),
            result_entry_id: result_id,
            call_idx: *call_idx,
            call_protected,
            result_protected,
        });
    }

    groups
}

/// For each path group exceeding the limit, emit `SetContextOverride` mutations
/// for the oldest pairs.
///
/// Pairs whose halves are already protected (`ForcedInclude` or
/// `ForcedExclude`) are still **counted** toward the per-file total (so the
/// prune window is stable), but their individual halves are skipped at
/// emission time — no no-op duplicate is emitted, and user-pinned-in
/// entries are not silently flipped to `ForcedExclude`.
fn build_prune_mutations(
    history_len: usize,
    groups: HashMap<String, Vec<EditWritePair>>,
    max_file_edits: usize,
    min_age: usize,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let mut mutations = Vec::new();

    for (_path, pairs) in groups {
        if pairs.len() <= max_file_edits {
            continue;
        }

        // Pairs are in history order (oldest first), so .take() selects the
        // oldest pairs to prune. Both the call and its result must be excluded
        // so the pruned edit/write disappears entirely from context.
        let to_prune = pairs.len() - max_file_edits;
        for pair in pairs.iter().take(to_prune) {
            // Per-worker `min_age` floor: do not prune writes younger than
            // `min_age` entries from the end of history. Both halves of the
            // pair are skipped together (pair atomicity).
            if is_within_min_age(history_len, pair.call_idx, min_age) {
                continue;
            }
            let call_protected = pair.call_protected;
            let result_protected = pair.result_protected;

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
    use crate::protocol::ChatEntry;
    use crate::protocol::SessionId;
    use crate::protocol::ToolResultStatus;

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

    fn worker_with_max(max: usize) -> DoubleEditAutoPruneWorker {
        DoubleEditAutoPruneWorker {
            config: DoubleEditAutoPruneConfig {
                enabled: true,
                max_file_edits: max,
                min_age: 0,
            },
        }
    }

    fn worker_with_max_and_min_age(max: usize, min_age: usize) -> DoubleEditAutoPruneWorker {
        DoubleEditAutoPruneWorker {
            config: DoubleEditAutoPruneConfig {
                enabled: true,
                max_file_edits: max,
                min_age,
            },
        }
    }

    fn block_on_evaluate(
        worker: &DoubleEditAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { worker.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    fn collect_pruned_ids(mutations: Vec<HistoryMutation>) -> Vec<ChatEntryId> {
        let mut ids: Vec<_> = mutations
            .into_iter()
            .map(|m| match m {
                HistoryMutation::SetContextOverride {
                    entry_id, value, ..
                } => {
                    assert_eq!(value, ContextOverride::ForcedExclude);
                    entry_id
                }
                other => panic!("expected SetContextOverride, got {other:?}"),
            })
            .collect();
        ids.sort_by_key(std::string::ToString::to_string);
        ids
    }

    #[rstest::rstest]
    #[test]
    fn no_edit_write_produces_no_mutations() {
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("what is 2+2?"),
            ChatEntry::assistant("4"),
        ];
        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn under_max_produces_no_mutations() {
        let mut history = Vec::new();
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn over_max_prunes_oldest() {
        let mut history = Vec::new();
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());
        let e3 = edit_call_result("tc-3", "/foo.rs", "edit 3");
        history.push(e3[0].clone());
        history.push(e3[1].clone());

        let expected_call_id = history[0].id.clone();
        let expected_result_id = history[1].id.clone();

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        let pruned_ids = collect_pruned_ids(mutations);

        let mut expected = vec![expected_call_id, expected_result_id];
        expected.sort_by_key(std::string::ToString::to_string);
        assert_eq!(pruned_ids, expected);
    }

    #[rstest::rstest]
    #[test]
    fn mixed_edit_and_write_count_together() {
        let mut history = Vec::new();
        // edit (oldest) + write + write = 3, max=2 → prune the edit
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let w1 = write_call_result("tc-2", "/foo.rs", "write 1");
        history.push(w1[0].clone());
        history.push(w1[1].clone());
        let w2 = write_call_result("tc-3", "/foo.rs", "write 2");
        history.push(w2[0].clone());
        history.push(w2[1].clone());

        let expected_call_id = history[0].id.clone();
        let expected_result_id = history[1].id.clone();

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        let pruned_ids = collect_pruned_ids(mutations);

        let mut expected = vec![expected_call_id, expected_result_id];
        expected.sort_by_key(std::string::ToString::to_string);
        assert_eq!(pruned_ids, expected);
    }

    #[rstest::rstest]
    #[test]
    fn different_files_independent() {
        let mut history = Vec::new();
        // 3 edits to /foo.rs, 1 edit to /bar.rs, max=2
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let b1 = edit_call_result("tc-2", "/bar.rs", "bar edit 1");
        history.push(b1[0].clone());
        history.push(b1[1].clone());
        let e2 = edit_call_result("tc-3", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());
        let e3 = edit_call_result("tc-4", "/foo.rs", "edit 3");
        history.push(e3[0].clone());
        history.push(e3[1].clone());

        // Only the oldest /foo.rs pair should be pruned.
        let expected_call_id = history[0].id.clone();
        let expected_result_id = history[1].id.clone();

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        let pruned_ids = collect_pruned_ids(mutations);

        let mut expected = vec![expected_call_id, expected_result_id];
        expected.sort_by_key(std::string::ToString::to_string);
        assert_eq!(pruned_ids, expected);
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_skipped() {
        // 3 pairs total, max=2 -> prune oldest 1.
        // The oldest call is already ForcedExclude -> only its non-excluded
        // result gets a mutation.
        let mut history = Vec::new();
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        let mut e1_call = e1[0].clone();
        e1_call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(e1_call);
        let oldest_result_id = e1[1].id.clone();
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());
        let e3 = edit_call_result("tc-3", "/foo.rs", "edit 3");
        history.push(e3[0].clone());
        history.push(e3[1].clone());

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);

        assert_eq!(mutations.len(), 1, "only the non-excluded result mutates");
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, oldest_result_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn forced_included_skipped() {
        // Same shape as `already_excluded_skipped` but with ForcedInclude on
        // the oldest call. Proves ForcedInclude is treated symmetrically at
        // the mutation-emission step.
        let mut history = Vec::new();
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        let mut e1_call = e1[0].clone();
        e1_call.context_override = ContextOverride::ForcedInclude;
        history.push(e1_call);
        let oldest_result_id = e1[1].id.clone();
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());
        let e3 = edit_call_result("tc-3", "/foo.rs", "edit 3");
        history.push(e3[0].clone());
        history.push(e3[1].clone());

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);

        assert_eq!(mutations.len(), 1, "only the non-protected result mutates");
        match &mutations[0] {
            HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } => {
                assert_eq!(*entry_id, oldest_result_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected SetContextOverride, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn tool_call_without_result_ignored() {
        let mut history = Vec::new();
        // Orphan edit ToolCall (no result).
        history.push(ChatEntry::tool_call(
            "tc-orphan",
            "edit",
            r#"{"path": "/foo.rs"}"#,
        ));
        // Two complete edits.
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());

        // Only 2 complete pairs, max=2 → nothing pruned.
        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn exact_max_no_prune() {
        let mut history = Vec::new();
        let e1 = edit_call_result("tc-1", "/foo.rs", "edit 1");
        history.push(e1[0].clone());
        history.push(e1[1].clone());
        let e2 = edit_call_result("tc-2", "/foo.rs", "edit 2");
        history.push(e2[0].clone());
        history.push(e2[1].clone());

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn max_zero_means_no_limit() {
        let mut history = Vec::new();
        for i in 0..5 {
            let e = edit_call_result(&format!("tc-{i}"), "/foo.rs", &format!("edit {i}"));
            history.push(e[0].clone());
            history.push(e[1].clone());
        }

        let worker = worker_with_max(0);
        let mutations = block_on_evaluate(&worker, history);
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn five_edits_prune_three_oldest() {
        let mut history = Vec::new();
        let mut oldest_ids = Vec::new();
        for i in 0..5 {
            let e = edit_call_result(&format!("tc-{i}"), "/foo.rs", &format!("edit {i}"));
            if i < 3 {
                oldest_ids.push(e[0].id.clone());
                oldest_ids.push(e[1].id.clone());
            }
            history.push(e[0].clone());
            history.push(e[1].clone());
        }

        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(mutations.len(), 6, "3 oldest pairs × 2 = 6 mutations");

        let pruned_ids = collect_pruned_ids(mutations);
        oldest_ids.sort_by_key(std::string::ToString::to_string);
        assert_eq!(pruned_ids, oldest_ids);
    }

    #[rstest::rstest]
    #[test]
    fn multiple_files_both_overflow() {
        let mut history = Vec::new();
        // 4 edits to /a.rs
        for i in 0..4 {
            let e = edit_call_result(&format!("tc-a{i}"), "/a.rs", &format!("a{i}"));
            history.push(e[0].clone());
            history.push(e[1].clone());
        }
        // 3 edits to /b.rs
        for i in 0..3 {
            let e = edit_call_result(&format!("tc-b{i}"), "/b.rs", &format!("b{i}"));
            history.push(e[0].clone());
            history.push(e[1].clone());
        }

        // /a.rs: 4 entries, keep 2 → prune 2 oldest (4 mutations)
        // /b.rs: 3 entries, keep 2 → prune 1 oldest (2 mutations)
        // Total: 6 mutations
        let worker = worker_with_max(2);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(mutations.len(), 6);
    }

    // ------------------------------------------------------------------
    // `min_age` protection tests
    //
    // The `min_age` floor suppresses mutation emission for pairs whose
    // call_idx is within `min_age` of history.len(). Pair-atomicity is
    // preserved: a protected pair emits no mutations for either half.
    // Suppressed pairs still count toward the per-file total, so a file
    // may temporarily exceed `max_file_edits` while young entries age out
    // — this matches the "be less aggressive" design intent.
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[test]
    fn min_age_protects_young_file_with_many_writes() {
        // 3 writes to the same file with max_file_edits=2.
        // Without min_age the oldest pair would be pruned. With min_age=20
        // and a short history (the writes themselves occupy indices 0..6,
        // so the oldest call is at age = 6 − 0 − 1 = 5 < 20) all three
        // pairs are protected.
        let mut history = Vec::new();
        for i in 0..3 {
            let w = write_call_result(&format!("tc-{i}"), "/file.rs", &format!("v{i}"));
            history.push(w[0].clone());
            history.push(w[1].clone());
        }
        // history.len() = 6; every call is age < 20.
        let worker = worker_with_max_and_min_age(2, 20);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            0,
            "young file with all writes within min_age should not be pruned"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_as_before() {
        // min_age=0 must preserve pre-fix behavior: oldest pair pruned when
        // the file exceeds max_file_edits.
        let mut history = Vec::new();
        for i in 0..3 {
            let w = write_call_result(&format!("tc-{i}"), "/file.rs", &format!("v{i}"));
            history.push(w[0].clone());
            history.push(w[1].clone());
        }
        let worker = worker_with_max_and_min_age(2, 0);
        let mutations = block_on_evaluate(&worker, history);
        // 3 writes / max 2 → prune oldest call+result pair (2 mutations).
        assert_eq!(mutations.len(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn mixed_young_and_old_writes() {
        // Oldest write at the start of a long history (age ≫ min_age, prunable);
        // 3 more writes near the end (age < min_age, protected).
        // max_file_edits=2 + min_age=20 means: we have 4 writes total,
        // would normally prune the 2 oldest, but only the very oldest is
        // outside the protection floor.
        let mut history = Vec::new();

        // Write #0 at indices 0,1 — age will be ≫ 20.
        let w0 = write_call_result("tc-0", "/file.rs", "v0");
        history.push(w0[0].clone());
        history.push(w0[1].clone());

        // Pad with 50 user entries → history.len() = 52; write #0 call age = 51.
        for i in 0..50 {
            history.push(ChatEntry::user(format!("padding {i}")));
        }

        // Writes #1, #2, #3 at the end — all young.
        for i in 1..=3 {
            let w = write_call_result(&format!("tc-{i}"), "/file.rs", &format!("v{i}"));
            history.push(w[0].clone());
            history.push(w[1].clone());
        }

        // history.len() = 60. Write #0 call at idx 0 has age 59 (≥ 20, prunable).
        // Writes #1..#3 have ages 7, 5, 3 (all < 20, protected).
        // With max_file_edits=2, the worker would prune 2 oldest if it could,
        // but only write #0 is eligible → exactly 2 mutations (call+result).
        let worker = worker_with_max_and_min_age(2, 20);
        let mutations = block_on_evaluate(&worker, history);
        assert_eq!(
            mutations.len(),
            2,
            "only the unprotected oldest write should be pruned; young writes stay"
        );

        let pruned_ids: std::collections::HashSet<_> = mutations
            .iter()
            .map(|m| match m {
                HistoryMutation::SetContextOverride { entry_id, .. } => entry_id.clone(),
                _ => panic!("expected SetContextOverride mutations"),
            })
            .collect();
        assert!(pruned_ids.contains(&w0[0].id), "oldest call must be pruned");
        assert!(
            pruned_ids.contains(&w0[1].id),
            "oldest result must be pruned"
        );
    }
}
