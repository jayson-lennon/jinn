//! Todo auto-prune worker.
//!
//! Detects all `todo_`-prefixed tool calls in conversation history and treats
//! them as one unified group. Keeps only the most recent `ToolCall` +
//! `ToolResult` pair and marks all older pairs as [`ForcedExclude`].
//! This removes stale todo state from the LLM context window.
//!
//! Pruning is immediate — no threshold or delay.
//!
//! # `protect_latest`
//!
//! The most recent pair is additionally force-included (worker-sourced, so it
//! applies immediately and sticks against other workers' excludes). Without
//! this, a later pruner such as `tool_age_window` can silently drop the
//! current task list from context once enough history elapses after the last
//! todo call. When a newer pair arrives, the superseded include is demoted to
//! `Default` and the old pair excluded in the same batch. User pins and `x`
//! toggles outrank the protection. Disabling `protect_latest` in config
//! restores the legacy exclude-only behavior.
//!
//! # Example
//!
//! ```text
//! X  [Tool Call]: todo_add_task("Write code")
//! X  [Tool Result] (OK): <stale task list>
//!    [Tool Call]: todo_complete_task("t1")
//! *  [Tool Result] (OK): <current task list>  (force-included)
//!    [Assistant]: all done
//! ```
//!
//!
//! # `min_age` protection
//!
//! Each call's `call_info.index` (raw position in history) is checked against
//! `min_age`: any pair whose call sits within `min_age` slots of the end of
//! history is left in place. `min_age = 0` disables the floor and reproduces
//! the pre-fix behavior (only the last pair is kept). The `protect_latest`
//! include is not gated by `min_age` — the latest pair is protected even
//! when it is the only pair.

use crate::feat::auto_prune_worker::is_within_min_age;

use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::protocol::HistoryMutation;
use crate::protocol::SessionId;
use crate::protocol::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};
pub use jinn_preferences_config::schemas::auto_prune::TodoAutoPruneConfig;
use std::collections::HashMap;
use std::sync::Arc;

/// Returns true if a tool name belongs to the todo tool group.
fn is_todo_tool(name: &str) -> bool {
    name.starts_with("todo_")
}

/// A matched ToolCall with its index and the tool_call_id used to find its ToolResult.
struct CallInfo {
    /// Index of the ToolCall in history.
    index: usize,
    /// The entry ID of the ToolCall.
    entry_id: crate::protocol::ChatEntryId,
    /// The tool_call_id used to match ToolCall → ToolResult.
    tool_call_id: String,
}

/// Todo auto-prune worker.
///
/// Scans history for all `todo_`-prefixed tool calls as one unified group.
/// Marks every pair except the most recent one as
/// [`ContextOverride::ForcedExclude`]. Pruning is immediate — no delay
/// threshold.
#[derive(Clone)]
pub struct TodoAutoPruneWorker {
    /// Configuration for the todo auto-prune strategy.
    pub config: TodoAutoPruneConfig,
}

/// Collect all ToolCalls and ToolResults for any `todo_` tool from history.
///
/// Returns a list of call info (in history order) and a map from tool_call_id
/// to (result_index, result_entry_id). This single-pass approach avoids the
/// forward-scan loops used by other workers — ToolResults are collected into
/// a HashMap by their `id` field, which directly matches the ToolCall's `id`.
fn collect_all_todo_pairs(
    history: &[ChatEntry],
) -> (
    Vec<CallInfo>,
    HashMap<String, (usize, crate::protocol::ChatEntryId)>,
) {
    // ToolCalls in history order — their position determines which are "oldest".
    let mut calls: Vec<CallInfo> = Vec::new();
    // ToolResults keyed by tool_call_id — one result per call.
    let mut result_map: HashMap<String, (usize, crate::protocol::ChatEntryId)> = HashMap::new();

    for (i, entry) in history.iter().enumerate() {
        match &entry.kind {
            ChatEntryKind::ToolCall { name, id, .. } if is_todo_tool(name) => {
                calls.push(CallInfo {
                    index: i,
                    entry_id: entry.id.clone(),
                    tool_call_id: id.clone(),
                });
            }
            ChatEntryKind::ToolResult { id, name, .. } if is_todo_tool(name) => {
                // Each ToolResult's `id` matches exactly one ToolCall's `id`.
                result_map.insert(id.clone(), (i, entry.id.clone()));
            }
            _ => {}
        }
    }

    (calls, result_map)
}

/// Build prune mutations for the todo tool group.
///
/// With `protect_latest` on, the most recent pair is force-included (so no
/// other pruner can remove the current task list from context) and the pair
/// it supersedes is demoted-then-excluded; all older pairs go through the
/// legacy exclusion pass. With `protect_latest` off, only the legacy pass
/// runs (excluding every pair except the most recent) — identical to the
/// pre-`protect_latest` behavior.
fn build_prune_mutations(
    history: &[ChatEntry],
    calls: &[CallInfo],
    result_map: &HashMap<String, (usize, crate::protocol::ChatEntryId)>,
    min_age: usize,
    worker_name: &str,
    protect_latest: bool,
) -> Vec<HistoryMutation> {
    // No todo calls in history — nothing to include or prune.
    if calls.is_empty() {
        return Vec::new();
    }

    // How many of the trailing calls are claimed by the protect-latest pass
    // (latest + superseded); the legacy pass only sees the older ones.
    let claimed = if protect_latest { 2 } else { 1 };
    let older_count = calls.len().saturating_sub(claimed);

    let mut mutations = Vec::new();
    if protect_latest {
        mutations.extend(include_latest_pair(history, calls, result_map, worker_name));
        if let Some(previous) = calls.len().checked_sub(2).and_then(|i| calls.get(i)) {
            mutations.extend(prune_superseded_pair(
                history,
                previous,
                result_map,
                worker_name,
            ));
        }
    }
    mutations.extend(prune_older_pairs(
        history,
        calls,
        result_map,
        min_age,
        worker_name,
        older_count,
    ));
    mutations
}

/// Force-include the most recent todo call+result pair.
///
/// Worker-sourced includes apply immediately (they are never buffered) and
/// stick against other workers' excludes, so the current task list always
/// stays in context. Halves that are pinned, user-excluded, or already
/// forced-included are skipped — user intent outranks the protection. An
/// orphaned call (no matching result) gets no include; pairs are protected
/// as a unit.
fn include_latest_pair(
    history: &[ChatEntry],
    calls: &[CallInfo],
    result_map: &HashMap<String, (usize, crate::protocol::ChatEntryId)>,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let Some(latest) = calls.last() else {
        return Vec::new();
    };

    let mut mutations = Vec::new();
    if let Some(call) = history.get(latest.index)
        && may_force_include(call)
    {
        mutations.push(include_mutation(call.id.clone(), worker_name));
    }
    if let Some((result_idx, result_entry_id)) = result_map.get(&latest.tool_call_id)
        && let Some(result) = history.get(*result_idx)
        && may_force_include(result)
    {
        mutations.push(include_mutation(result_entry_id.clone(), worker_name));
    }
    mutations
}

/// Whether a worker include may be emitted for this entry.
///
/// Pins and user exclusions always win; an existing `ForcedInclude` (user or
/// worker) makes the mutation redundant.
fn may_force_include(entry: &ChatEntry) -> bool {
    !entry.is_pinned()
        && !entry.is_user_force_excluded()
        && entry.context_override() != ContextOverride::ForcedInclude
}

/// Demote and exclude the pair superseded by the latest one.
///
/// The demote must precede the exclude within the same batch: a worker
/// `ForcedInclude` is sticky against a worker exclude at apply time, so the
/// superseded include is first dropped to [`ContextOverride::Default`] and
/// only then excluded. A half the user or another worker included is left
/// untouched — demoting it would override an intent this worker does not
/// own, and the apply-time guard would refuse the exclusion anyway.
fn prune_superseded_pair(
    history: &[ChatEntry],
    previous: &CallInfo,
    result_map: &HashMap<String, (usize, crate::protocol::ChatEntryId)>,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let call_half = history
        .get(previous.index)
        .map(|entry| (&previous.entry_id, entry));
    let result_half = result_map
        .get(&previous.tool_call_id)
        .and_then(|(idx, id)| history.get(*idx).map(|entry| (id, entry)));

    let mut mutations = Vec::new();
    for (id, entry) in call_half.into_iter().chain(result_half) {
        if entry.context_override() == ContextOverride::ForcedInclude {
            if !todo_worker_include(entry, worker_name) {
                // User (or foreign worker) include — leave the half alone.
                continue;
            }
            mutations.push(demote_mutation(id.clone(), worker_name));
        }
        mutations.push(exclude_mutation(id.clone(), worker_name));
    }
    mutations
}

/// Whether the entry's most recent context change is a `ForcedInclude`
/// emitted by this todo worker. False for user includes and for includes
/// from any other worker.
fn todo_worker_include(entry: &ChatEntry, worker_name: &str) -> bool {
    matches!(entry.context_history.last(), Some(event)
        if event.to == ContextOverride::ForcedInclude
        && matches!(&event.source, ChangeSource::Worker { name } if name.as_str() == worker_name))
}

/// The legacy exclusion pass: exclude all-but-the-last `count` pairs whose
/// call is older than `min_age` and not protected from pruning.
fn prune_older_pairs(
    history: &[ChatEntry],
    calls: &[CallInfo],
    result_map: &HashMap<String, (usize, crate::protocol::ChatEntryId)>,
    min_age: usize,
    worker_name: &str,
    count: usize,
) -> Vec<HistoryMutation> {
    let mut mutations = Vec::new();

    for call_info in calls.iter().take(count) {
        // Protection floor: never prune pairs whose call sits within
        // `min_age` slots of the end of history.
        if is_within_min_age(history.len(), call_info.index, min_age) {
            continue;
        }

        // Prune the ToolCall if not protected from prune.
        if !history
            .get(call_info.index)
            .is_some_and(crate::protocol::ChatEntry::is_protected_from_prune)
        {
            mutations.push(exclude_mutation(call_info.entry_id.clone(), worker_name));
        }

        // Prune the corresponding ToolResult if it exists and isn't protected.
        if let Some((result_idx, result_entry_id)) = result_map.get(&call_info.tool_call_id)
            && !history
                .get(*result_idx)
                .is_some_and(crate::protocol::ChatEntry::is_protected_from_prune)
        {
            mutations.push(exclude_mutation(result_entry_id.clone(), worker_name));
        }
    }

    mutations
}

/// A worker-sourced `ForcedInclude` mutation.
fn include_mutation(entry_id: crate::protocol::ChatEntryId, worker_name: &str) -> HistoryMutation {
    HistoryMutation::SetContextOverride {
        entry_id,
        value: ContextOverride::ForcedInclude,
        source: ChangeSource::Worker {
            name: worker_name.to_owned(),
        },
    }
}

/// A worker-sourced `Default` mutation (demotes an include this worker owns).
fn demote_mutation(entry_id: crate::protocol::ChatEntryId, worker_name: &str) -> HistoryMutation {
    HistoryMutation::SetContextOverride {
        entry_id,
        value: ContextOverride::Default,
        source: ChangeSource::Worker {
            name: worker_name.to_owned(),
        },
    }
}

/// A worker-sourced `ForcedExclude` mutation.
fn exclude_mutation(entry_id: crate::protocol::ChatEntryId, worker_name: &str) -> HistoryMutation {
    HistoryMutation::SetContextOverride {
        entry_id,
        value: ContextOverride::ForcedExclude,
        source: ChangeSource::Worker {
            name: worker_name.to_owned(),
        },
    }
}

#[async_trait::async_trait]
impl HistoryWorker for TodoAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-todo"
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let (calls, result_map) = collect_all_todo_pairs(&history);
        build_prune_mutations(
            &history,
            &calls,
            &result_map,
            self.config.min_age,
            self.name(),
            self.config.protect_latest,
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
    use crate::protocol::SessionId;
    use crate::protocol::ToolResultStatus;
    use crate::protocol::{ChatEntry, ChatEntryId};

    /// Helper: create a `todo_get_task_list` ToolCall + ToolResult pair.
    fn get_task_list_call_result(call_id: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "todo_get_task_list", r#"{"phase_id":"p1"}"#),
            ChatEntry::tool_result(
                call_id,
                "todo_get_task_list",
                content,
                ToolResultStatus::Success,
            ),
        ]
    }

    /// Helper: create a `todo_complete_task` ToolCall + ToolResult pair.
    fn complete_task_call_result(call_id: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "todo_complete_task", r#"{"task_id":"t1"}"#),
            ChatEntry::tool_result(
                call_id,
                "todo_complete_task",
                content,
                ToolResultStatus::Success,
            ),
        ]
    }

    /// Helper: create a `todo_add_phase` ToolCall + ToolResult pair.
    fn add_phase_call_result(call_id: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(call_id, "todo_add_phase", r#"{"description":"Build"}"#),
            ChatEntry::tool_result(
                call_id,
                "todo_add_phase",
                content,
                ToolResultStatus::Success,
            ),
        ]
    }

    /// Helper: create a `todo_add_task` ToolCall + ToolResult pair.
    fn add_task_call_result(call_id: &str, content: &str) -> [ChatEntry; 2] {
        [
            ChatEntry::tool_call(
                call_id,
                "todo_add_task",
                r#"{"phase_id":"p1","description":"Write code"}"#,
            ),
            ChatEntry::tool_result(call_id, "todo_add_task", content, ToolResultStatus::Success),
        ]
    }

    fn worker() -> TodoAutoPruneWorker {
        worker_with_protect_latest(true)
    }

    /// Build a worker with `min_age = 0` and the given `protect_latest` flag.
    fn worker_with_protect_latest(protect_latest: bool) -> TodoAutoPruneWorker {
        TodoAutoPruneWorker {
            config: TodoAutoPruneConfig {
                enabled: true,
                min_age: 0,
                protect_latest,
            },
        }
    }

    /// Evaluate the worker synchronously for tests.
    fn evaluate(history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        let w = worker();
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    /// Evaluate with an explicit `protect_latest` flag.
    fn evaluate_with_protect_latest(
        history: Vec<ChatEntry>,
        protect_latest: bool,
    ) -> Vec<HistoryMutation> {
        let w = worker_with_protect_latest(protect_latest);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    /// Whether the mutation targets the entry with a `ForcedInclude`.
    fn targets_include(m: &HistoryMutation, id: &ChatEntryId) -> bool {
        matches!(
            m,
            HistoryMutation::SetContextOverride {
                entry_id,
                value: ContextOverride::ForcedInclude,
                ..
            } if entry_id == id
        )
    }

    /// Whether the mutation targets the entry with a `ForcedExclude`.
    fn targets_exclude(m: &HistoryMutation, id: &ChatEntryId) -> bool {
        matches!(
            m,
            HistoryMutation::SetContextOverride {
                entry_id,
                value: ContextOverride::ForcedExclude,
                ..
            } if entry_id == id
        )
    }

    /// Whether the mutation targets the entry with a `Default` (demote).
    fn targets_demote(m: &HistoryMutation, id: &ChatEntryId) -> bool {
        matches!(
            m,
            HistoryMutation::SetContextOverride {
                entry_id,
                value: ContextOverride::Default,
                ..
            } if entry_id == id
        )
    }

    /// Whether any mutation includes the entry.
    fn any_include(mutations: &[HistoryMutation], id: &ChatEntryId) -> bool {
        mutations.iter().any(|m| targets_include(m, id))
    }

    /// Whether any mutation excludes the entry.
    fn any_exclude(mutations: &[HistoryMutation], id: &ChatEntryId) -> bool {
        mutations.iter().any(|m| targets_exclude(m, id))
    }

    /// Whether any mutation demotes the entry.
    fn any_demote(mutations: &[HistoryMutation], id: &ChatEntryId) -> bool {
        mutations.iter().any(|m| targets_demote(m, id))
    }

    #[rstest::rstest]
    #[test]
    fn no_todo_calls_produces_no_mutations() {
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
    fn single_get_task_list_is_force_included() {
        // Given a history with exactly one todo call+result pair.
        let call_result = get_task_list_call_result("tc-1", "task list here");
        let history = vec![call_result[0].clone(), call_result[1].clone()];

        // When evaluating with protect_latest on (the default).
        let mutations = evaluate(history);

        // Then both halves of the pair are force-included.
        assert!(any_include(&mutations, &call_result[0].id));
        assert!(any_include(&mutations, &call_result[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn single_complete_task_is_force_included() {
        // Given a history with exactly one todo_complete_task pair.
        let call_result = complete_task_call_result("tc-1", "task completed");
        let history = vec![call_result[0].clone(), call_result[1].clone()];

        // When evaluating with protect_latest on (the default).
        let mutations = evaluate(history);

        // Then both halves of the pair are force-included.
        assert!(any_include(&mutations, &call_result[0].id));
        assert!(any_include(&mutations, &call_result[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn multiple_get_task_list_prunes_older() {
        let mut history = Vec::new();
        // First call (older — should be pruned).
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        // Second call (most recent — should be kept).
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        let mutations = evaluate(history);
        // Should emit: 2 includes for tc-2 (the latest) + 2 excludes for tc-1.
        assert_eq!(mutations.len(), 4);

        // tc-1 (superseded) is excluded; tc-2 (latest) is included.
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn multiple_complete_task_prunes_older() {
        let mut history = Vec::new();
        let cr1 = complete_task_call_result("tc-1", "completed t1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = complete_task_call_result("tc-2", "completed t2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        let mutations = evaluate(history);
        // 2 includes for tc-2 (latest) + 2 excludes for tc-1.
        assert_eq!(mutations.len(), 4);

        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn interleaved_todo_tools_pruned_as_group() {
        let mut history = Vec::new();
        // get_task_list v1 (older — should be pruned)
        let g1 = get_task_list_call_result("g-1", "list v1");
        history.push(g1[0].clone());
        history.push(g1[1].clone());
        // complete_task v1 (older — should be pruned)
        let c1 = complete_task_call_result("c-1", "completed t1");
        history.push(c1[0].clone());
        history.push(c1[1].clone());
        // get_task_list v2 (older — should be pruned)
        let g2 = get_task_list_call_result("g-2", "list v2");
        history.push(g2[0].clone());
        history.push(g2[1].clone());
        // complete_task v2 (most recent — kept)
        let c2 = complete_task_call_result("c-2", "completed t2");
        history.push(c2[0].clone());
        history.push(c2[1].clone());

        let mutations = evaluate(history);
        // Unified pruning: only c-2 survives. g-1, c-1, g-2 are pruned
        // (6 excludes) and c-2 is force-included (2 includes) = 8.
        assert_eq!(mutations.len(), 8);

        // g-1, c-1, and g-2 should be pruned.
        assert!(any_exclude(&mutations, &g1[0].id));
        assert!(any_exclude(&mutations, &g1[1].id));
        assert!(any_exclude(&mutations, &c1[0].id));
        assert!(any_exclude(&mutations, &c1[1].id));
        assert!(any_exclude(&mutations, &g2[0].id));
        assert!(any_exclude(&mutations, &g2[1].id));
        // c-2 (most recent) should be included, not pruned.
        assert!(any_include(&mutations, &c2[0].id));
        assert!(any_include(&mutations, &c2[1].id));
        assert!(!any_exclude(&mutations, &c2[0].id));
        assert!(!any_exclude(&mutations, &c2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn already_excluded_no_duplicate_mutation() {
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        // Mark both as already excluded.
        let mut call = cr1[0].clone();
        call.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let mut result = cr1[1].clone();
        result.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        history.push(call);
        history.push(result);

        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        let mutations = evaluate(history);
        // tc-1 halves are already excluded → their re-excludes are emitted
        // but are apply-time no-ops; tc-2 gets its 2 includes.
        assert_eq!(mutations.len(), 4);
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn user_included_previous_pair_is_not_demoted_or_excluded() {
        // Given an older pair whose halves were force-included by the user.
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        let mut call = cr1[0].clone();
        call.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
        let mut result = cr1[1].clone();
        result.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
        history.push(call);
        history.push(result);
        // A newer pair arrives — tc-1 is now superseded.
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        // When evaluating with protect_latest on.
        let mutations = evaluate(history);

        // Then the user's include on tc-1 is respected: no demote, no exclude.
        assert!(!any_demote(&mutations, &cr1[0].id));
        assert!(!any_demote(&mutations, &cr1[1].id));
        assert!(!any_exclude(&mutations, &cr1[0].id));
        assert!(!any_exclude(&mutations, &cr1[1].id));
        // And the latest pair is still included.
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn tool_call_without_result_still_prunes_call() {
        let mut history = Vec::new();
        // Orphan ToolCall with no corresponding ToolResult.
        history.push(ChatEntry::tool_call(
            "tc-orphan",
            "todo_get_task_list",
            "{}",
        ));
        let orphan_id = history[0].id.clone();
        // Most recent call with result.
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        let mutations = evaluate(history);
        // 3 mutations: 2 includes for tc-2 (latest, protected) + the orphan
        // ToolCall exclude (it has no result half to prune).
        assert_eq!(mutations.len(), 3);
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
        let orphan_excludes: Vec<_> = mutations
            .iter()
            .filter(|m| targets_exclude(m, &orphan_id))
            .collect();
        match orphan_excludes.first() {
            Some(HistoryMutation::SetContextOverride {
                entry_id, value, ..
            }) => {
                assert_eq!(entry_id, &orphan_id);
                assert_eq!(*value, ContextOverride::ForcedExclude);
            }
            other => panic!("expected orphan exclude mutation, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn three_calls_prunes_first_two() {
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());
        let cr3 = get_task_list_call_result("tc-3", "v3");
        history.push(cr3[0].clone());
        history.push(cr3[1].clone());

        let mutations = evaluate(history);
        // tc-1 + tc-2 excluded (4) and tc-3 included (2) = 6 mutations.
        assert_eq!(mutations.len(), 6);

        // tc-1 and tc-2 pruned.
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
        assert!(any_exclude(&mutations, &cr2[0].id));
        assert!(any_exclude(&mutations, &cr2[1].id));
        // tc-3 (most recent) included, not pruned.
        assert!(any_include(&mutations, &cr3[0].id));
        assert!(any_include(&mutations, &cr3[1].id));
        assert!(!any_exclude(&mutations, &cr3[0].id));
        assert!(!any_exclude(&mutations, &cr3[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn other_tool_calls_not_affected() {
        let mut history = Vec::new();
        // A read tool call (should not be touched).
        history.push(ChatEntry::tool_call(
            "rc-1",
            "read",
            r#"{"path": "/foo.rs"}"#,
        ));
        history.push(ChatEntry::tool_result(
            "rc-1",
            "read",
            "contents",
            ToolResultStatus::Success,
        ));
        // A todo_get_task_list call.
        let cr = get_task_list_call_result("tc-1", "list");
        history.push(cr[0].clone());
        history.push(cr[1].clone());

        let mutations = evaluate(history);
        // The single todo pair is the latest → 2 includes. Non-todo tools
        // are untouched.
        assert_eq!(mutations.len(), 2);
        assert!(any_include(&mutations, &cr[0].id));
        assert!(any_include(&mutations, &cr[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn different_todo_tools_prunes_older() {
        let mut history = Vec::new();
        // add_task (older — should be pruned)
        let a1 = add_task_call_result("a-1", "created t1");
        history.push(a1[0].clone());
        history.push(a1[1].clone());
        // add_phase (newer — should be kept)
        let a2 = add_phase_call_result("a-2", "created phase");
        history.push(a2[0].clone());
        history.push(a2[1].clone());

        let mutations = evaluate(history);
        // a-1 excluded (2) + a-2 included (2) = 4 mutations.
        assert_eq!(mutations.len(), 4);

        assert!(any_exclude(&mutations, &a1[0].id));
        assert!(any_exclude(&mutations, &a1[1].id));
        assert!(any_include(&mutations, &a2[0].id));
        assert!(any_include(&mutations, &a2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn mixed_todo_tools_keeps_only_last() {
        let mut history = Vec::new();
        // add_task (oldest)
        let a1 = add_task_call_result("a-1", "created t1");
        history.push(a1[0].clone());
        history.push(a1[1].clone());
        // add_phase (middle)
        let a2 = add_phase_call_result("a-2", "created phase");
        history.push(a2[0].clone());
        history.push(a2[1].clone());
        // get_task_list (newest — should be kept)
        let g1 = get_task_list_call_result("g-1", "list v1");
        history.push(g1[0].clone());
        history.push(g1[1].clone());

        let mutations = evaluate(history);
        // a-1 + a-2 excluded (4) and g-1 included (2) = 6 mutations.
        assert_eq!(mutations.len(), 6);

        // a-1 and a-2 pruned.
        assert!(any_exclude(&mutations, &a1[0].id));
        assert!(any_exclude(&mutations, &a1[1].id));
        assert!(any_exclude(&mutations, &a2[0].id));
        assert!(any_exclude(&mutations, &a2[1].id));
        // g-1 (most recent) included, not pruned.
        assert!(any_include(&mutations, &g1[0].id));
        assert!(any_include(&mutations, &g1[1].id));
        assert!(!any_exclude(&mutations, &g1[0].id));
        assert!(!any_exclude(&mutations, &g1[1].id));
    }

    /// Evaluate with explicit min_age (protect_latest on, the default).
    fn evaluate_with_min_age(history: Vec<ChatEntry>, min_age: usize) -> Vec<HistoryMutation> {
        let w = TodoAutoPruneWorker {
            config: TodoAutoPruneConfig {
                enabled: true,
                min_age,
                protect_latest: true,
            },
        };
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&SessionId::new(), Arc::from(history)).await })
    }

    /// Build a history with two get_task_list pairs (oldest at idx 0) plus
    /// padding to history_len = 52. With 2 pairs the oldest call_idx is 0 →
    /// age = 51.
    fn history_with_two_todo_pairs_and_tail() -> Vec<ChatEntry> {
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());
        // Pad to history_len = 52 with trivial assistant entries.
        // history.len() is currently 4; need 48 more.
        history.extend(std::iter::repeat_n(ChatEntry::assistant("tail"), 48));
        history
    }

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_older_todo_pair() {
        // Given a history with two get_task_list pairs padded to history_len = 52.
        // With min_age=0, the older pair should be pruned (back-compat baseline).
        let history = history_with_two_todo_pairs_and_tail();

        // When evaluating with min_age=0.
        let mutations = evaluate_with_min_age(history, 0);

        // Then 4 mutations are emitted: 2 excludes for the older pair
        // (min_age=0 baseline) + 2 includes for the latest pair.
        assert_eq!(
            mutations.len(),
            4,
            "min_age=0 must prune the older todo pair and include the latest"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_todo_pair() {
        // Given a history with three get_task_list pairs padded to 52 entries:
        // call_idx 0 (age 51), call_idx 2 (age 49), call_idx 4 (age 47).
        // With protect_latest on, the superseded pair (idx 2) bypasses min_age
        // — only two pairs may ever share context — so the boundary applies
        // to the oldest pair alone.
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());
        let cr3 = get_task_list_call_result("tc-3", "list v3");
        history.push(cr3[0].clone());
        history.push(cr3[1].clone());
        history.extend(std::iter::repeat_n(ChatEntry::assistant("tail"), 46));

        // When evaluating with min_age=60 (every age < 60).
        let mutations = evaluate_with_min_age(history, 60);

        // Then tc-1 is protected by min_age, but the superseded pair tc-2
        // bypasses it (exactly two pairs may share context) and tc-3 gets
        // its includes: 2 excludes for tc-2 + 2 includes for tc-3 = 4.
        assert_eq!(mutations.len(), 4);
        assert!(any_exclude(&mutations, &cr2[0].id));
        assert!(any_exclude(&mutations, &cr2[1].id));
        assert!(any_include(&mutations, &cr3[0].id));
        assert!(any_include(&mutations, &cr3[1].id));
        assert!(!any_exclude(&mutations, &cr1[0].id));
        assert!(!any_exclude(&mutations, &cr1[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn min_age_boundary_strict_less_than_todo() {
        // Three pairs padded to 52 entries: oldest call_idx 0 (age 51),
        // superseded call_idx 2 (age 49), latest call_idx 4 (age 47).
        //
        // is_within_min_age returns true when age < min_age (strict
        // less-than). The superseded pair bypasses min_age (protect_latest);
        // the boundary is observable on the oldest pair:
        //   min_age = 52: age 51 < 52 → protected (no exclude for tc-1).
        //   min_age = 51: age 51 < 51 is false → NOT protected (tc-1 excluded).
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());
        let cr3 = get_task_list_call_result("tc-3", "list v3");
        history.push(cr3[0].clone());
        history.push(cr3[1].clone());
        history.extend(std::iter::repeat_n(ChatEntry::assistant("tail"), 46));

        // Protected: age = 51 < min_age = 52 → only the 2 latest includes
        // (the superseded pair's demote+exclude bypasses min_age).
        let mutations = evaluate_with_min_age(history.clone(), 52);
        assert_eq!(
            mutations.len(),
            4,
            "age = min_age - 1 must protect only the oldest pair"
        );
        assert!(mutations.iter().all(|m| !targets_exclude(m, &cr1[0].id)));
        assert!(mutations.iter().all(|m| !targets_exclude(m, &cr1[1].id)));

        // Not protected: age = 51 = min_age → oldest pair excluded too
        // (2 excludes for tc-1 + 2 excludes for tc-2 + 2 includes = 6).
        let mutations = evaluate_with_min_age(history, 51);
        assert_eq!(
            mutations.len(),
            6,
            "age = min_age must NOT be protected (strict less-than)"
        );
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
    }

    // ------------------------------------------------------------------
    // protect_latest tests
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[test]
    fn protect_latest_force_includes_most_recent_todo_pair() {
        // Given a history with two todo pairs (min_age = 0).
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        // When evaluating.
        let mutations = evaluate(history);

        // Then the most recent pair carries ForcedInclude mutations.
        assert!(any_include(&mutations, &cr2[0].id));
        assert!(any_include(&mutations, &cr2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn superseded_pair_is_demoted_then_excluded() {
        // Given two todo pairs where the older one was previously included
        // by this worker (the state left by the prior evaluation).
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        let mut call = cr1[0].clone();
        call.apply_context_override(
            ContextOverride::ForcedInclude,
            ChangeSource::Worker {
                name: "auto-prune-todo".to_owned(),
            },
        );
        let mut result = cr1[1].clone();
        result.apply_context_override(
            ContextOverride::ForcedInclude,
            ChangeSource::Worker {
                name: "auto-prune-todo".to_owned(),
            },
        );
        history.push(call);
        history.push(result);
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        // When evaluating.
        let mutations = evaluate(history);

        // Then the superseded pair is demoted (Default) and excluded.
        assert!(any_demote(&mutations, &cr1[0].id));
        assert!(any_demote(&mutations, &cr1[1].id));
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
        // And the demote precedes the exclude within the batch — the
        // apply-time sticky-include guard would refuse a bare exclude.
        let positions = |pred: &dyn Fn(&HistoryMutation) -> bool| {
            mutations
                .iter()
                .position(pred)
                .expect("mutation must exist")
        };
        let demote_pos = positions(&|m| targets_demote(m, &cr1[0].id));
        let exclude_pos = positions(&|m| targets_exclude(m, &cr1[0].id));
        assert!(demote_pos < exclude_pos, "demote must precede exclude");
    }

    #[rstest::rstest]
    #[test]
    fn older_pairs_excluded_unchanged() {
        // Given three todo pairs.
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());
        let cr3 = get_task_list_call_result("tc-3", "v3");
        history.push(cr3[0].clone());
        history.push(cr3[1].clone());

        // When evaluating.
        let mutations = evaluate(history);

        // Then the oldest pair gets exactly the same exclude treatment the
        // legacy pass always applied: ForcedExclude, worker-sourced.
        let excludes: Vec<_> = mutations
            .iter()
            .filter(|m| targets_exclude(m, &cr1[0].id))
            .collect();
        assert_eq!(excludes.len(), 1);
        match &excludes[0] {
            HistoryMutation::SetContextOverride {
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker { name },
                ..
            } => assert_eq!(name, "auto-prune-todo"),
            other => panic!("expected worker exclude, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn user_excluded_latest_pair_is_not_force_included() {
        // Given a latest pair whose halves the user excluded with `x`.
        let mut history = Vec::new();
        let cr = get_task_list_call_result("tc-1", "list");
        let mut call = cr[0].clone();
        call.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        let mut result = cr[1].clone();
        result.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        history.push(call);
        history.push(result);

        // When evaluating.
        let mutations = evaluate(history);

        // Then no include is emitted — user intent wins.
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn pinned_latest_pair_receives_no_include() {
        use crate::protocol::PinPosition;
        // Given a latest pair whose halves are pinned.
        let mut history = Vec::new();
        let cr = get_task_list_call_result("tc-1", "list");
        let mut call = cr[0].clone();
        call.pin_position = Some(PinPosition::Top);
        let mut result = cr[1].clone();
        result.pin_position = Some(PinPosition::Top);
        history.push(call);
        history.push(result);

        // When evaluating.
        let mutations = evaluate(history);

        // Then the pin already guarantees inclusion — no include emitted.
        assert!(mutations.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn protect_latest_false_preserves_legacy_behavior() {
        // Given two todo pairs and the protect_latest flag off.
        let mut history = Vec::new();
        let cr1 = get_task_list_call_result("tc-1", "list v1");
        history.push(cr1[0].clone());
        history.push(cr1[1].clone());
        let cr2 = get_task_list_call_result("tc-2", "list v2");
        history.push(cr2[0].clone());
        history.push(cr2[1].clone());

        // When evaluating.
        let mutations = evaluate_with_protect_latest(history, false);

        // Then only the legacy excludes exist: 2 mutations, no includes.
        assert_eq!(mutations.len(), 2);
        assert!(any_exclude(&mutations, &cr1[0].id));
        assert!(any_exclude(&mutations, &cr1[1].id));
        assert!(!any_include(&mutations, &cr2[0].id));
        assert!(!any_include(&mutations, &cr2[1].id));
    }

    #[rstest::rstest]
    #[test]
    fn protect_latest_false_keeps_single_pair_untouched() {
        // Given a single todo pair and the flag off.
        let call_result = get_task_list_call_result("tc-1", "list");
        let history = vec![call_result[0].clone(), call_result[1].clone()];

        // When evaluating.
        let mutations = evaluate_with_protect_latest(history, false);

        // Then nothing happens — exactly the pre-protect_latest behavior.
        assert!(mutations.is_empty());
    }
}
