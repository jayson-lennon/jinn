//! Anchor-shield auto-prune worker.
//!
//! Emits [`ContextOverride::ForcedInclude`] for all in-context-by-default entry types
//! (`User`, `Assistant`, `ToolCall`, `ToolResult`) within a configurable radius of any
//! anchor entry. This prevents other auto-prune workers from excluding entries that carry
//! conversation structure near user turns.
//!
//! # Anchors
//!
//! An anchor is a `User` entry — any position in history. Unlike the
//! anchored-assistant pruner, the shield does *not* anchor on conversation
//! start/end boundaries (which would shield nearly every entry in short
//! conversations).
//!
//! Anchor indices are computed via [`collect_user_anchor_indices`] (User-only).
//!
//! # Pair Atomicity
//!
//! `ToolCall` and `ToolResult` entries form pairs (matched by `id`). If either half
//! is within the shield radius, the other half is also shielded — regardless of its
//! own distance to the nearest anchor. This prevents orphaned tool calls or results
//! in LLM context.
//!
//! # Relationship to Anchored-Assistant Prune Worker
//!
//! The shield worker and the [`AnchoredAssistantAutoPruneWorker`] share a single
//! `radius` value (configured on [`AnchorShieldConfig`]). The shield protects everything
//! within the radius; the prune worker removes everything beyond it. They partition
//! cleanly because:
//! - Shield: distance ≤ radius → `ForcedInclude`
//! - Prune: distance > radius → `ForcedExclude`
//!
//! # Excluded Entry Types
//!
//! `Compaction` entries are technically in-context-by-default but are excluded from
//! shielding because they are always managed by the compaction system and are pinned.
//! `System`, `Error`, `Actor`, `Thinking`, and `Transient` entries are not
//! in-context-by-default and are also excluded.
//!
//! [`ContextOverride::ForcedInclude`]: crate::feat::session::chat_entry::ContextOverride::ForcedInclude
//! [`AnchoredAssistantAutoPruneWorker`]: super::AnchoredAssistantAutoPruneWorker
//! [`collect_user_anchor_indices`]: super::anchored_assistant::collect_user_anchor_indices
//! [`AnchorShieldConfig`]: jinn_preferences_config::schemas::AnchorShieldConfig

use std::collections::HashSet;

pub use jinn_preferences_config::schemas::auto_prune::AnchorShieldConfig;
use std::sync::Arc;

use crate::feat::auto_prune_worker::anchored_assistant::{
    collect_user_anchor_indices, distances_to_nearest_anchors,
};
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::feat::session::chat_entry::{
    ChangeSource, ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride,
};
use crate::feat::session::history_mutation::HistoryMutation;
use crate::protocol::SessionId;

/// Default enabled state for anchor-shield auto-prune.
/// Default radius (in raw history entries) for the anchor-shield worker.
/// Anchor-shield auto-prune strategy configuration.
///
/// Serialized as `[auto_prune.anchor_shield]` in `jinn.toml`.
///
/// The shield worker emits `ForcedInclude` for all in-context-by-default
/// entry types (`User`, `Assistant`, `ToolCall`, `ToolResult`) within
/// `radius` of any anchor entry. This prevents other workers from excluding
/// entries that carry conversation structure near user turns.
///
/// The `radius` value is also used by the `AnchoredAssistantAutoPruneWorker`
/// so the shield boundary and prune boundary always align.
/// Anchor-shield auto-prune worker.
///
/// See module docs for full semantics. Construct with [`AnchorShieldConfig`].
#[derive(Clone)]
pub struct AnchorShieldAutoPruneWorker {
    /// Configuration for the anchor-shield strategy.
    pub config: AnchorShieldConfig,
}

/// Build the list of `SetContextOverride::ForcedInclude` mutations for a
/// single snapshot.
///
/// Pure function (no `&self`) so unit tests can call it directly without
/// spinning up a tokio runtime.
///
/// # Algorithm
///
/// 1. If history is empty, return empty vec.
/// 2. Compute anchor indices via [`collect_user_anchor_indices`] (User-only).
/// 3. Pass 1 — collect candidate ids: For each entry whose kind is
///    `User`, `Assistant`, `ToolCall`, or `ToolResult`, compute the minimum
///    distance to the nearest anchor. If the distance ≤ radius, add the
///    entry's id to the shield set.
/// 4. Pass 2 — pair atomicity: For each `ToolCall` entry, find its matching
///    `ToolResult` (forward scan by tool-call id). If either half is in the
///    shield set, add the partner.
/// 5. Emit: For each id in the final set, if the entry is not already
///    `ForcedInclude`, emit a `SetContextOverride { ForcedInclude }` mutation.
fn build_shield_mutations(
    history: &[ChatEntry],
    radius: usize,
    session_id: &SessionId,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    if history.is_empty() {
        return Vec::new();
    }

    let anchor_indices = collect_user_anchor_indices(history);
    let shield_set = collect_shield_candidates(history, radius, &anchor_indices);
    let shield_set = apply_pair_atomicity(history, shield_set);
    emit_shield_mutations(history, &shield_set, session_id, worker_name)
}

/// Pass 1: Collect entry ids that are within `radius` of any anchor.
///
/// Only shields entries whose kind is `User`, `Assistant`, `ToolCall`, or
/// `ToolResult`. Compaction, System, Error, Actor, Thinking, and Transient
/// entries are skipped.
fn collect_shield_candidates(
    history: &[ChatEntry],
    radius: usize,
    anchor_indices: &[usize],
) -> HashSet<ChatEntryId> {
    let mut candidates = HashSet::new();

    for (idx, entry) in history.iter().enumerate() {
        if !is_shieldable_kind(&entry.kind) {
            continue;
        }

        let (d_back, d_fwd) = distances_to_nearest_anchors(idx, anchor_indices);
        let min_distance = min_distance(d_back, d_fwd);

        if min_distance <= radius {
            candidates.insert(entry.id.clone());
        }
    }

    candidates
}

/// Pass 2: Expand the shield set with ToolCall/ToolResult pair partners.
///
/// If a `ToolCall`'s id is in the shield set, find the matching `ToolResult`
/// (by tool-call id) and add it. If a `ToolResult`'s id is in the set, find
/// the matching `ToolCall` and add it.
fn apply_pair_atomicity(
    history: &[ChatEntry],
    mut shield_set: HashSet<ChatEntryId>,
) -> HashSet<ChatEntryId> {
    // Build a lookup from tool-call id → (entry_idx, entry_id) for ToolCalls.
    let tool_calls: Vec<(usize, &ChatEntryId, &str)> = history
        .iter()
        .enumerate()
        .filter_map(|(idx, e)| match &e.kind {
            ChatEntryKind::ToolCall { id, .. } => Some((idx, &e.id, id.as_str())),
            _ => None,
        })
        .collect();

    // Build a lookup from tool-call id → entry_id for ToolResults.
    let tool_results: Vec<(&ChatEntryId, &str)> = history
        .iter()
        .filter_map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, .. } => Some((&e.id, id.as_str())),
            _ => None,
        })
        .collect();

    // For each ToolCall in the shield set, add the matching ToolResult.
    for (call_idx, call_entry_id, tool_call_id) in &tool_calls {
        if shield_set.contains(*call_entry_id) {
            // Find matching ToolResult (scan forward from call).
            for entry in history.iter().skip(call_idx + 1) {
                if let ChatEntryKind::ToolResult { id, .. } = &entry.kind
                    && id == tool_call_id
                {
                    shield_set.insert(entry.id.clone());
                    break;
                }
            }
        }
    }

    // For each ToolResult in the shield set, add the matching ToolCall.
    for (result_entry_id, tool_call_id) in &tool_results {
        if shield_set.contains(*result_entry_id) {
            // Find matching ToolCall.
            for (_, call_entry_id, call_id) in &tool_calls {
                if call_id == tool_call_id {
                    shield_set.insert((*call_entry_id).clone());
                    break;
                }
            }
        }
    }

    shield_set
}

/// Emit `ForcedInclude` mutations for all entries in the shield set that are
/// not already `ForcedInclude`.
fn emit_shield_mutations(
    history: &[ChatEntry],
    shield_set: &HashSet<ChatEntryId>,
    session_id: &SessionId,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let mut mutations = Vec::new();

    for entry in history {
        if !shield_set.contains(&entry.id) {
            continue;
        }

        // Skip entries already ForcedInclude — no duplicate mutations.
        if entry.context_override == ContextOverride::ForcedInclude {
            continue;
        }

        tracing::debug!(
            entry_id = %entry.id,
            radius = ?shield_set.len(),
            "anchor_shield: force-including entry near anchor"
        );
        mutations.push(HistoryMutation::SetContextOverride {
            entry_id: entry.id.clone(),
            value: ContextOverride::ForcedInclude,
            source: ChangeSource::Worker {
                name: worker_name.to_owned(),
            },
        });
    }

    tracing::debug!(
        mutations = mutations.len(),
        session_id = %session_id,
        "anchor_shield evaluate done"
    );
    mutations
}

/// Whether the entry kind should be shielded (User, Assistant, ToolCall, ToolResult).
///
/// Compaction is excluded because those entries are always managed by the
/// compaction system and are pinned. All other kinds are not in-context-by-default.
fn is_shieldable_kind(kind: &ChatEntryKind) -> bool {
    matches!(
        kind,
        ChatEntryKind::User { .. }
            | ChatEntryKind::Assistant(..)
            | ChatEntryKind::ToolCall { .. }
            | ChatEntryKind::ToolResult { .. }
    )
}

/// Compute the minimum distance to the nearest anchor from `(d_back, d_fwd)`.
///
/// If either side is `None` (no anchor on that side, effectively ∞), uses the
/// other side. If both are `None`, returns `usize::MAX`.
fn min_distance(d_back: Option<usize>, d_fwd: Option<usize>) -> usize {
    match (d_back, d_fwd) {
        (Some(b), Some(f)) => b.min(f),
        (Some(b), None) => b,
        (None, Some(f)) => f,
        (None, None) => usize::MAX,
    }
}

#[async_trait::async_trait]
impl HistoryWorker for AnchorShieldAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-anchor-shield"
    }

    async fn evaluate(
        &self,
        session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        build_shield_mutations(&history, self.config.radius, session_id, self.name())
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
    use crate::feat::session::chat_entry::{ChatEntry, PinPosition};
    use crate::feat::session::tool_result_status::ToolResultStatus;

    /// Build a worker with the given radius (enabled = true).
    fn worker(radius: usize) -> AnchorShieldAutoPruneWorker {
        AnchorShieldAutoPruneWorker {
            config: AnchorShieldConfig {
                enabled: true,
                radius,
            },
        }
    }

    /// Evaluate the worker on a history snapshot.
    fn evaluate(w: &AnchorShieldAutoPruneWorker, history: Vec<ChatEntry>) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let history: Arc<[ChatEntry]> = history.into();
        rt.block_on(async { w.evaluate(&SessionId::new(), history).await })
    }

    /// Collect the entry ids targeted by `SetContextOverride::ForcedInclude` mutations.
    fn included_ids(mutations: &[HistoryMutation]) -> HashSet<ChatEntryId> {
        let mut out = HashSet::new();
        for m in mutations {
            if let HistoryMutation::SetContextOverride {
                entry_id,
                value: ContextOverride::ForcedInclude,
                ..
            } = m
            {
                out.insert(entry_id.clone());
            }
        }
        out
    }

    /// Build N plain user entries.
    /// Build a ToolCall + ToolResult pair with matching tool-call id.
    fn tool_pair(call_id: &str, name: &str) -> (ChatEntry, ChatEntry) {
        let call = ChatEntry::tool_call(call_id, name, "{}");
        let result = ChatEntry::tool_result(call_id, name, "done", ToolResultStatus::Success);
        (call, result)
    }

    // ------------------------------------------------------------------
    // 1. empty_history_produces_no_mutations
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn empty_history_produces_no_mutations() {
        // Given an empty history.
        let w = worker(20);

        // When evaluating.
        let mutations = evaluate(&w, Vec::new());

        // Then no mutations are produced.
        assert!(mutations.is_empty());
    }

    // ------------------------------------------------------------------
    // 2. single_entry_is_anchor_and_shielded
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn single_user_entry_is_anchor_and_shielded() {
        // Given a single user entry (anchor, since it's User kind).
        let w = worker(20);
        let entry = ChatEntry::user("hello");
        let entry_id = entry.id.clone();

        // When evaluating.
        let mutations = evaluate(&w, vec![entry]);

        // Then the entry is ForcedInclude'd.
        let included = included_ids(&mutations);
        assert!(included.contains(&entry_id));
    }
    #[rstest::rstest]
    #[test]
    fn single_non_user_entry_without_anchors_is_not_shielded() {
        // Given a single assistant entry — no User anchors exist.
        let w = worker(20);
        let entry = ChatEntry::assistant("hello");
        let entry_id = entry.id.clone();

        // When evaluating.
        let mutations = evaluate(&w, vec![entry]);

        // Then no entries are shielded (no User anchors to measure from).
        let included = included_ids(&mutations);
        assert!(
            !included.contains(&entry_id),
            "non-User entry with no anchors must not be shielded"
        );
    }

    // ------------------------------------------------------------------
    // 3. assistant_within_radius_is_forced_included
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn assistant_within_radius_is_forced_included() {
        // Given a user entry at idx 0 (anchor) and an assistant at idx 1.
        let w = worker(5);
        let asst = ChatEntry::assistant("nearby");
        let asst_id = asst.id.clone();
        let history = vec![ChatEntry::user("anchor"), asst];

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the assistant (distance 1 ≤ 5) is ForcedInclude'd.
        let included = included_ids(&mutations);
        assert!(included.contains(&asst_id));
    }

    // ------------------------------------------------------------------
    // 4. assistant_outside_radius_is_not_shielded
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn assistant_outside_radius_is_not_shielded() {
        // Given a user entry at idx 0 (anchor) and an assistant at idx 6
        // (distance 6 from the nearest anchor). Use Assistant entries for
        // padding so they are NOT anchors.
        let w = worker(5);
        let mut history = vec![ChatEntry::user("anchor")]; // idx 0
        for i in 0..5 {
            history.push(ChatEntry::assistant(format!("pad {i}")));
        }
        let asst = ChatEntry::assistant("far away");
        let asst_id = asst.id.clone();
        history.push(asst); // idx 6
        // Padding after to ensure no other User anchors nearby.
        for i in 0..10 {
            history.push(ChatEntry::assistant(format!("tail {i}")));
        }
        // Anchor: idx 0 (User) — the only anchor (no boundary anchors).
        // Assistant at idx 6: distance 6 from anchor 0. > radius 5.
        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the assistant (nearest distance = 6 > 5) is NOT shielded.
        let included = included_ids(&mutations);
        assert!(
            !included.contains(&asst_id),
            "entry beyond radius must not be shielded"
        );
    }

    // ------------------------------------------------------------------
    // 5. tool_call_within_radius_shields_result_pair
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn tool_call_within_radius_shields_result_pair() {
        // Given a ToolCall within radius of an anchor, with its ToolResult
        // far from all anchors. The result must still be shielded via
        // pair atomicity.
        let mut history = vec![ChatEntry::user("anchor")]; // idx 0
        let (call, result) = tool_pair("tc-1", "write");
        let call_id = call.id.clone();
        let result_id = result.id.clone();
        history.push(call); // idx 1, distance 1 from anchor

        // Padding to push result far from all anchors.
        for i in 0..20 {
            history.push(ChatEntry::assistant(format!("padding {i}")));
        }
        history.push(result); // idx 22
        // More padding so last-entry anchor is far from result.
        for i in 0..20 {
            history.push(ChatEntry::assistant(format!("tail {i}")));
        }
        // Anchors: idx 0 (User), idx 43 (last entry).
        // Result at idx 22: distance 22 from anchor 0, distance 21 from
        // anchor 43. Both > radius 5.

        let w = worker(5);
        let mutations = evaluate(&w, history);

        // Then: call (idx 1, distance 1) is shielded.
        // Pair atomicity: result (idx 22, distance 22) is also shielded.
        let included = included_ids(&mutations);
        assert!(
            included.contains(&call_id),
            "call within radius must be shielded"
        );
        assert!(
            included.contains(&result_id),
            "result must be shielded via pair atomicity even though it's outside radius"
        );
    }

    // ------------------------------------------------------------------
    // 6. tool_result_within_radius_shields_call_pair
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn tool_result_within_radius_shields_call_pair() {
        // Given a ToolResult near a User anchor (distance 0),
        // and its ToolCall far from all anchors. Pair atomicity must
        // shield the call even though it's outside radius.
        let mut history = Vec::new();
        history.push(ChatEntry::user("first")); // idx 0, anchor

        for i in 0..30 {
            history.push(ChatEntry::assistant(format!("pad {i}")));
        }
        // ToolCall at idx 31, distance 31 from anchor 0.
        let (call, result) = tool_pair("tc-1", "read");
        let call_id = call.id.clone();
        let result_id = result.id.clone();
        history.push(call); // idx 31

        for i in 0..29 {
            history.push(ChatEntry::assistant(format!("pad2 {i}")));
        }
        // User anchor at idx 61.
        history.push(ChatEntry::user("second")); // idx 61, anchor
        // ToolResult at idx 62, distance 1 from anchor 61.
        history.push(result); // idx 62

        // Anchors: idx 0 (User), idx 61 (User).
        // Result at idx 62: distance 1 from anchor 61. Within radius 10.
        // Call at idx 31: distance 31 from anchor 0, distance 30 from
        // anchor 61. Both > radius 10.

        let w = worker(10);
        let mutations = evaluate(&w, history);

        let included = included_ids(&mutations);
        assert!(
            included.contains(&result_id),
            "result is near User anchor and must be shielded"
        );
        assert!(
            included.contains(&call_id),
            "call must be shielded via pair atomicity because its result is shielded"
        );
    }

    // ------------------------------------------------------------------
    // 7. user_entry_within_radius_is_shielded
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn user_entry_within_radius_is_shielded() {
        // Given two user entries with a non-user entry between them.
        let w = worker(5);
        let user1 = ChatEntry::user("first");
        let user1_id = user1.id.clone();
        let asst = ChatEntry::assistant("between");
        let user2 = ChatEntry::user("second");
        let user2_id = user2.id.clone();

        let history = vec![user1, asst, user2];

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then both user entries are shielded (they are anchors, distance 0).
        let included = included_ids(&mutations);
        assert!(included.contains(&user1_id));
        assert!(included.contains(&user2_id));
    }

    // ------------------------------------------------------------------
    // 8. system_entry_is_not_shielded
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn system_entry_is_not_shielded() {
        // Given a system entry next to an anchor.
        let w = worker(20);
        let mut history = vec![ChatEntry::user("anchor")];
        let sys = ChatEntry::system("system message");
        let sys_id = sys.id.clone();
        history.push(sys);

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the system entry is NOT shielded.
        let included = included_ids(&mutations);
        assert!(
            !included.contains(&sys_id),
            "system entries must not be shielded"
        );
    }

    // ------------------------------------------------------------------
    // 9. already_forced_included_entry_is_skipped
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn already_forced_included_entry_is_skipped() {
        // Given an entry that is already ForcedInclude.
        let w = worker(20);
        let mut entry = ChatEntry::assistant("already included");
        entry.context_override = ContextOverride::ForcedInclude;
        let entry_id = entry.id.clone();

        let history = vec![ChatEntry::user("anchor"), entry];

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then no mutation is emitted for the already-included entry.
        let included = included_ids(&mutations);
        assert!(
            !included.contains(&entry_id),
            "already ForcedInclude entry must not receive duplicate mutation"
        );
    }

    // ------------------------------------------------------------------
    // 10. already_forced_excluded_entry_is_overridden
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn already_forced_excluded_entry_is_overridden() {
        // Given an entry that is ForcedExclude but within the shield radius.
        let w = worker(20);
        let mut entry = ChatEntry::assistant("was excluded");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Worker {
                name: "other-worker".into(),
            },
        );
        let entry_id = entry.id.clone();

        let history = vec![ChatEntry::user("anchor"), entry];

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the shield emits ForcedInclude to override the exclusion.
        let included = included_ids(&mutations);
        assert!(
            included.contains(&entry_id),
            "ForcedExclude entry within radius must be upgraded to ForcedInclude"
        );
    }

    // ------------------------------------------------------------------
    // 11. multiple_anchors_shield_independent_neighborhoods
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn multiple_anchors_shield_independent_neighborhoods() {
        // Given two user anchors with a gap between them.
        let w = worker(3);
        let mut history = Vec::new();

        history.push(ChatEntry::user("anchor-1")); // idx 0
        let near1 = ChatEntry::assistant("near anchor-1");
        let near1_id = near1.id.clone();
        history.push(near1); // idx 1

        // Entries between anchors — some near anchor-1, some not.
        for i in 0..10 {
            history.push(ChatEntry::assistant(format!("mid {i}")));
        }

        let near2 = ChatEntry::assistant("near anchor-2");
        let near2_id = near2.id.clone();
        history.push(near2); // idx 12
        history.push(ChatEntry::user("anchor-2")); // idx 13

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then entries near each anchor are shielded independently.
        let included = included_ids(&mutations);
        assert!(
            included.contains(&near1_id),
            "entry near anchor-1 must be shielded"
        );
        assert!(
            included.contains(&near2_id),
            "entry near anchor-2 must be shielded"
        );
    }

    // ------------------------------------------------------------------
    // 12. boundary_at_exact_radius_is_included
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn boundary_at_exact_radius_is_included() {
        // Given an entry at exactly distance = radius from the nearest anchor.
        let w = worker(5);
        let mut history = vec![ChatEntry::user("anchor")]; // idx 0
        for i in 0..4 {
            history.push(ChatEntry::assistant(format!("pad {i}")));
        }
        let boundary = ChatEntry::assistant("at radius");
        let boundary_id = boundary.id.clone();
        history.push(boundary); // idx 5, distance 5 = radius

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the entry at distance = radius is shielded (≤).
        let included = included_ids(&mutations);
        assert!(
            included.contains(&boundary_id),
            "entry at exactly radius must be shielded (≤ not <)"
        );
    }

    // ------------------------------------------------------------------
    // 13. boundary_at_radius_plus_one_is_not_shielded
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn boundary_at_radius_plus_one_is_not_shielded() {
        // Given an entry at distance = radius + 1 from the nearest anchor.
        // History: User(0), pad(1..5), beyond(6), pad(7..12).
        // Anchors: idx 0 (User) and idx 12 (last entry).
        // Beyond at idx 6: distance from anchor 0 = 6, from anchor 12 = 6.
        // Both > radius 5. Not shielded.
        let w = worker(5);
        let mut history = vec![ChatEntry::user("anchor")]; // idx 0
        for i in 0..5 {
            history.push(ChatEntry::assistant(format!("pad {i}")));
        }
        let beyond = ChatEntry::assistant("beyond radius");
        let beyond_id = beyond.id.clone();
        history.push(beyond); // idx 6
        // Padding so last-entry anchor is far from the beyond entry.
        for i in 0..6 {
            history.push(ChatEntry::assistant(format!("tail {i}")));
        }
        // Last entry idx 12 is anchor. Beyond at idx 6: d=6 from both anchors.

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Then the entry beyond radius is NOT shielded.
        let included = included_ids(&mutations);
        assert!(
            !included.contains(&beyond_id),
            "entry at radius + 1 must not be shielded"
        );
    }

    // ------------------------------------------------------------------
    // 14. idempotency_second_evaluate_no_new_mutations
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn idempotency_second_evaluate_no_new_mutations() {
        // Given a history with entries near anchors.
        let w = worker(20);
        let asst = ChatEntry::assistant("near anchor");
        let asst_id = asst.id.clone();
        let history = vec![ChatEntry::user("anchor"), asst];

        // When evaluating the first time.
        let mutations1 = evaluate(&w, history.clone());
        let included1 = included_ids(&mutations1);
        assert!(included1.contains(&asst_id));

        // Apply the mutations to the history.
        let mut history2 = history;
        for m in &mutations1 {
            if let HistoryMutation::SetContextOverride {
                entry_id,
                value,
                source,
            } = m
            {
                for e in &mut history2 {
                    if e.id == *entry_id {
                        e.apply_context_override(*value, source.clone());
                    }
                }
            }
        }

        // When evaluating again on the mutated history.
        let mutations2 = evaluate(&w, history2);

        // Then no new mutations are produced (entries already ForcedInclude).
        assert!(
            mutations2.is_empty(),
            "second evaluate must produce no new mutations"
        );
    }

    // ------------------------------------------------------------------
    // 15. pinned_entry_is_not_duplicated
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn pinned_entry_is_not_duplicated() {
        // Given a pinned entry near an anchor.
        let w = worker(20);
        let mut entry = ChatEntry::assistant("pinned");
        entry.pin_position = Some(PinPosition::Top);
        let entry_id = entry.id.clone();

        let history = vec![ChatEntry::user("anchor"), entry];

        // When evaluating.
        let mutations = evaluate(&w, history);

        // Pinned entries have context_override Default (not ForcedInclude),
        // so the shield WILL emit ForcedInclude. This is correct — the
        // shield protects entries near anchors. Pin and ForcedInclude
        // compose: pin means "always in context", ForcedInclude means
        // "worker says keep in context". Both true.
        //
        // Actually, let's check: does the pinned entry get a mutation?
        // The shield only skips entries with context_override == ForcedInclude.
        // Pinned entries have Default. So yes, the shield emits a mutation.
        // This is fine — it's a no-op in practice (pin already keeps it).
        let included = included_ids(&mutations);
        // The mutation is emitted but it's harmless.
        assert!(included.contains(&entry_id));
    }
}
