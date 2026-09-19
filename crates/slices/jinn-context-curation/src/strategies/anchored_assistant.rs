//! Anchored-assistant auto-prune worker.
//!
//! Targets large (`> 80` token) `Assistant` text entries that are far from
//! any **anchor** entry in either direction. These are typically planning
//! narration, status updates, or mid-chain commentary the model emits to
//! itself during autonomous coding — entries that the
//! [`TrivialAssistantAutoPruneWorker`] leaves alone (it only handles
//! `<= 80` tokens).
//!
//! # Anchors
//!
//! An entry is an anchor if any of:
//! - It is a `User` entry (any position).
//! - It is the **first** entry in history (regardless of type).
//! - It is the **last** entry in history (regardless of type).
//!
//! The first/last anchors guarantee that the opening message and the most recent wrap-up summary
//! are never pruned by this worker, even when a long tool-call chain pushes them far from any
//! `User` entry.
//!
//! # Semantics
//!
//! For each `Assistant` entry:
//! 1. Skip if the entry is empty (placeholder for tool-call-only responses).
//! 2. Skip if already [`ForcedExclude`]d.
//! 3. Skip if the entry is pinned.
//! 4. Skip if the entry is within `min_age` of the end of history.
//! 5. Look up (or compute and cache) the token count via the shared
//!    [`HistoryWorkerChatEntryTokenCache`].
//! 6. Skip if `tokens <= 80` (owned by `TrivialAssistantAutoPruneWorker`).
//! 7. Compute the distance to the nearest anchor on each side.
//! 8. Prune if **both** sides exceed `radius`.
//!
//! `min_age` is a raw-distance protection floor: a candidate entry is only
//! pruned when it is at least `min_age` slots from the end of history. With
//! `min_age = 0` no entry is protected.

use std::sync::Arc;

use super::min_age::is_within_min_age;
use crate::worker::HistoryWorker;
use jinn_core_types::HistoryMutation;
use jinn_core_types::SessionId;
use jinn_core_types::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};
use jinn_domain::feat::context::strategy::token_estimator::{TiktokenCounter, TokenCounter};
pub use jinn_preferences_config::schemas::auto_prune::AnchoredAssistantAutoPruneConfig;

/// Anchored-assistant auto-prune worker.
///
/// See the [module docs](self) for full semantics.
#[derive(Clone)]
pub struct AnchoredAssistantAutoPruneWorker {
    /// Configuration for the anchor-radius strategy. The working
    /// [`radius`](AnchoredAssistantAutoPruneConfig::radius) is sourced from
    /// this config's own `radius` field at wiring time.
    pub config: AnchoredAssistantAutoPruneConfig,
    /// The anchor radius applied at evaluation time.
    pub radius: usize,
    /// Minimum token count for an entry to be considered a pruning candidate.
    /// Entries at or below this threshold are owned by
    /// [`TrivialAssistantAutoPruneWorker`](super::TrivialAssistantAutoPruneWorker).
    /// Derived from `trivial_assistant.max_tokens + 1` at wiring time.
    pub min_candidate_tokens: u32,
    /// Shared per-session, per-entry token-count cache. Cheap clone (inner is
    /// `Arc`-shared).
    pub token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
    /// Long-lived tiktoken counter. Cheap copy (`Copy` type with `&'static`
    /// encoder reference).
    pub counter: TiktokenCounter,
}

/// Collect the indices of every anchor entry in history, sorted and deduped.
///
/// Anchors are: every `User` entry, plus the first and last history indices
/// (regardless of entry kind). The first and last anchors are always
/// present when `history` is non-empty, so this returns a non-empty `Vec`
/// for any non-empty history and an empty `Vec` only for empty history.
pub(crate) fn collect_anchor_indices(history: &[ChatEntry]) -> Vec<usize> {
    if history.is_empty() {
        return Vec::new();
    }

    let last_idx = history.len() - 1;
    let mut anchors: Vec<usize> = history
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e.kind {
            ChatEntryKind::User { .. } => Some(i),
            _ => None,
        })
        .collect();
    // The `User`-only list is already sorted ascending. Insert the boundary
    // anchors in their natural positions; the early-exit branches below keep
    // the result sorted without a full sort.
    if anchors.first() != Some(&0) {
        anchors.insert(0, 0);
    }
    if anchors.last() != Some(&last_idx) {
        anchors.push(last_idx);
    }
    // Dedup defensive (should be a no-op given the branches above).
    anchors.dedup();
    anchors
}
/// Compute `(d_back, d_fwd)` — index distances to the nearest preceding and
/// following anchors.
///
/// `anchor_indices` must be sorted ascending and non-empty (the natural
/// output of [`collect_anchor_indices`] for non-empty history). `idx` is the
/// position of the candidate `Assistant` entry.
///
/// Returns `(None, None)` only when there are no anchors at all. Otherwise
/// returns distances where `None` means "no anchor on that side" (i.e.,
/// distance is `∞`).
#[expect(clippy::unreachable, reason = "infallible")]
pub(crate) fn distances_to_nearest_anchors(
    idx: usize,
    anchor_indices: &[usize],
) -> (Option<usize>, Option<usize>) {
    if anchor_indices.is_empty() {
        return (None, None);
    }

    // Binary search: Ok means `idx` itself is an anchor (e.g., first or last
    // entry when the entry being evaluated is also one of the boundary
    // anchors). Err gives the insertion point — the count of anchor indices
    // strictly less than `idx`.
    if let Ok(insertion) = anchor_indices.binary_search(&idx) {
        // idx is itself an anchor → both distances are 0.
        // (Callers skip pruning anchors directly, but be safe.)
        let _ = insertion;
        return (Some(0), Some(0));
    }
    // binary_search returned Err, so insertion fits.
    let Err(insertion) = anchor_indices.binary_search(&idx) else {
        unreachable!("handled above");
    };

    // Preceding anchor = anchor_indices[insertion - 1] if insertion > 0.
    let d_back = insertion
        .checked_sub(1)
        .and_then(|i| anchor_indices.get(i).map(|&a| idx - a));
    // Following anchor = anchor_indices[insertion] if insertion < len.
    let d_fwd = anchor_indices
        .get(insertion)
        .map(|&anchor_idx| anchor_idx - idx);

    (d_back, d_fwd)
}

/// Build the list of `SetContextOverride::ForcedExclude` mutations for a
/// single snapshot.
///
/// Pure function (no `&self`) so unit tests can call it directly without
/// spinning up a tokio runtime.
///
/// # Algorithm
///
/// 1. If history is empty, return `Vec::new()` — no candidates exist.
/// 2. Pre-compute `anchor_indices` via [`collect_anchor_indices`].
/// 3. For each `Assistant` entry:
///    a. Skip empty, pinned, or already-excluded entries.
///    b. Look up or compute token count via the shared cache.
///    c. Skip if `tokens <= 80`.
///    d. Compute `(d_back, d_fwd)` via [`distances_to_nearest_anchors`].
///    e. Prune if both distances are present and both strictly exceed
///    `radius`.
///    f. Skip otherwise (including the case where only one side is
///    missing — that side acts as ∞, and we keep the entry if it's within
///    radius).
///
/// Context bundle for [`build_prune_mutations`] — groups the seven parameters
/// that vary per invocation into a single struct so the function signature stays
/// under clippy's argument-count threshold.
struct PruneCtx<'a> {
    history: &'a [ChatEntry],
    radius: usize,
    min_age: usize,
    min_candidate_tokens: u32,
    session_id: &'a SessionId,
    token_cache: &'a jinn_token_count_msg::HistoryWorkerChatEntryTokenCache,
    counter: &'a TiktokenCounter,
    worker_name: &'a str,
}

fn build_prune_mutations(ctx: &PruneCtx<'_>) -> Vec<HistoryMutation> {
    let radius = ctx.radius.max(1);

    if ctx.history.is_empty() {
        return Vec::new();
    }
    let anchor_indices = collect_anchor_indices(ctx.history);

    let mut mutations = Vec::new();
    let history_len = ctx.history.len();

    for (idx, entry) in ctx.history.iter().enumerate() {
        // Only Assistant entries are candidates.
        let text = match &entry.kind {
            ChatEntryKind::Assistant(t) => t.as_str(),
            _ => continue,
        };

        // Empty assistant entries are placeholders for tool-call-only
        // responses. Skip defensively.
        if text.is_empty() {
            continue;
        }

        // Skip pinned entries — pin beats ForcedExclude.
        if entry.is_pinned() {
            continue;
        }

        // Skip protected entries (ForcedInclude or ForcedExclude) —
        // ForcedInclude stays by user intent; ForcedExclude is already done.
        if entry.is_protected_from_prune() {
            continue;
        }

        // Protection floor: never prune entries within `min_age` of the
        // end of history. `min_age = 0` disables protection entirely.
        if is_within_min_age(history_len, idx, ctx.min_age) {
            continue;
        }

        // Look up or compute token count. The closure only fires on first
        // miss for this (session, entry) pair.
        let tokens = ctx
            .token_cache
            .get_or_insert_with(ctx.session_id, &entry.id, || ctx.counter.count(text) as u32);

        // Skip small entries — owned by TrivialAssistantAutoPruneWorker.
        if tokens < ctx.min_candidate_tokens {
            continue;
        }

        let (d_back, d_fwd) = distances_to_nearest_anchors(idx, &anchor_indices);

        // Prune if both sides have anchors and both exceed radius.
        let prune = match (d_back, d_fwd) {
            (Some(db), Some(df)) => db > radius && df > radius,
            // One side has no anchor (distance = ∞). The other side is
            // finite; keep if within radius, prune if outside.
            (Some(db), None) => db > radius,
            (None, Some(df)) => df > radius,
            // No anchors at all — already ruled out by the empty-check
            // above, but defensive: skip.
            (None, None) => continue,
        };

        if prune {
            tracing::debug!(
                entry_id = %entry.id,
                tokens,
                radius,
                d_back = ?d_back,
                d_fwd = ?d_fwd,
                "anchored_assistant: excluding stale large assistant entry",
            );
            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: entry.id.clone(),
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: ctx.worker_name.to_owned(),
                },
            });
        }
    }

    mutations
}

#[async_trait::async_trait]
impl HistoryWorker for AnchoredAssistantAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-anchored-assistant"
    }

    async fn evaluate(
        &self,
        session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let mutations = build_prune_mutations(&PruneCtx {
            history: &history,
            radius: self.radius,
            min_age: self.config.min_age,
            min_candidate_tokens: self.min_candidate_tokens,
            session_id,
            token_cache: &self.token_cache,
            counter: &self.counter,
            worker_name: self.name(),
        });
        tracing::debug!(
            mutations = mutations.len(),
            radius = self.radius,
            history_len = history.len(),
        );
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
    use jinn_core_types::ChatEntry;
    use jinn_core_types::ChatEntryId;

    /// Tests use 81 as the hard-coded threshold (trivial_assistant default max_tokens=80 + 1).
    const TEST_MIN_CANDIDATE_TOKENS: u32 = 81;

    use jinn_core_types::SessionId;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    /// Build a worker with the given radius (enabled = true).
    /// Build a worker with the given radius and `min_age = 0` (back-compat baseline).
    fn worker(radius: usize) -> AnchoredAssistantAutoPruneWorker {
        worker_with_min_age(radius, 0)
    }

    /// Build a worker with the given radius and `min_age`.
    fn worker_with_min_age(radius: usize, min_age: usize) -> AnchoredAssistantAutoPruneWorker {
        AnchoredAssistantAutoPruneWorker {
            config: AnchoredAssistantAutoPruneConfig {
                enabled: true,
                radius,
                min_age,
            },
            radius,
            min_candidate_tokens: 81,
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::new(),
            counter: TiktokenCounter::o200k_base(),
        }
    }

    /// Build a trivial (≤80 tiktoken tokens) assistant entry.
    fn trivial_assistant(text: &str) -> ChatEntry {
        ChatEntry::assistant(text)
    }

    /// Build an assistant entry whose token count is reliably greater than
    /// 80 under the `o200k_base` encoding.
    fn large_assistant() -> ChatEntry {
        // ~500 chars of varied English prose → comfortably >80 o200k_base
        // tokens. Mirrors the helper in trivial_assistant.rs.
        let body = "This is a substantial assistant response with multiple sentences and \
             enough vocabulary to comfortably exceed eighty tiktoken tokens under \
             the o200k_base encoding used throughout the codebase. ";
        ChatEntry::assistant(body.repeat(4))
    }

    /// Evaluate the worker on a history snapshot.
    fn evaluate(
        w: &AnchoredAssistantAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let history: Arc<[ChatEntry]> = history.into();
        rt.block_on(async { w.evaluate(&SessionId::new(), history).await })
    }

    /// Collect entry ids targeted by `SetContextOverride::ForcedExclude`.
    fn excluded_ids(mutations: &[HistoryMutation]) -> std::collections::HashSet<ChatEntryId> {
        let mut out = std::collections::HashSet::new();
        for m in mutations {
            if let HistoryMutation::SetContextOverride {
                entry_id,
                value: ContextOverride::ForcedExclude,
                ..
            } = m
            {
                out.insert(entry_id.clone());
            }
        }
        out
    }

    // ------------------------------------------------------------------
    // 1. empty_history_produces_no_mutations
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn empty_history_produces_no_mutations() {
        let w = worker(100);
        assert!(evaluate(&w, Vec::new()).is_empty());
    }

    // ------------------------------------------------------------------
    // 2. single_entry_history_produces_no_mutations
    //
    // The only entry is simultaneously first-anchor and last-anchor.
    // There are no other candidates, so no mutations.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn single_entry_history_produces_no_mutations() {
        let w = worker(100);
        let history = vec![large_assistant()];
        assert!(evaluate(&w, history).is_empty());
    }

    // ------------------------------------------------------------------
    // 3. last_entry_is_anchor_protects_wrap_up
    //
    // Bug being fixed: long tool-call chain pushes the wrap-up assistant
    // far from the originating User. Pre-fix this was pruned. Post-fix the
    // last-entry anchor keeps it.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn last_entry_is_anchor_protects_wrap_up() {
        let w = worker(100);
        let mut history = vec![ChatEntry::user("plan this feature")];
        // 200 trivial padding entries (tool-call style chain).
        history.extend(std::iter::repeat_n(trivial_assistant("step"), 200));
        // The wrap-up assistant lands well beyond radius=100 from the User.
        let wrap_up = large_assistant();
        let wrap_up_id = wrap_up.id.clone();
        history.push(wrap_up);

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&wrap_up_id),
            "last entry must be an anchor — wrap-up summary must be kept"
        );
    }

    // ------------------------------------------------------------------
    // 4. first_entry_is_anchor_protects_leading_assistant
    //
    // Symmetric to #3: a leading assistant at index 0 must be kept even
    // when far from any subsequent User.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn first_entry_is_anchor_protects_leading_assistant() {
        let w = worker(100);
        let leading = large_assistant();
        let leading_id = leading.id.clone();
        let mut history = vec![leading];
        history.extend(std::iter::repeat_n(trivial_assistant("step"), 200));
        history.push(ChatEntry::user("now do something"));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&leading_id),
            "first entry must be an anchor — leading assistant must be kept"
        );
    }

    // ------------------------------------------------------------------
    // 5. large_assistant_far_from_all_anchors_is_pruned
    //
    // Anchors at first (User), middle (User), and last (trivial) positions.
    // A large assistant sits between them, beyond radius from each.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn large_assistant_far_from_all_anchors_is_pruned() {
        let w = worker(5);
        let mut history = vec![ChatEntry::user("start")];
        // Lead padding so the candidate is far from the first-anchor (index 0).
        history.extend(std::iter::repeat_n(trivial_assistant("lead"), 50));
        let middle = large_assistant();
        let middle_id = middle.id.clone();
        history.push(middle);
        // Push the candidate well beyond radius from the lead anchor.
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 50));
        // A second User anchor — but the candidate is far from it too.
        history.push(ChatEntry::user("new turn"));
        // Tail padding so the last anchor isn't the candidate's neighbor.
        history.extend(std::iter::repeat_n(trivial_assistant("y"), 50));

        let mutations = evaluate(&w, history);
        assert!(
            excluded_ids(&mutations).contains(&middle_id),
            "middle entry beyond radius from all anchors must be pruned"
        );
    }

    // ------------------------------------------------------------------
    // 6. radius_boundary_at_exact_radius_is_kept
    //
    // First+User anchor at 0, large assistant at index R. Distance = R.
    // Rule is "strictly greater than R" → kept.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn radius_boundary_at_exact_radius_is_kept() {
        let radius = 10;
        let w = worker(radius);
        let mut history = vec![ChatEntry::user("anchor")];
        history.extend(std::iter::repeat_n(trivial_assistant("x"), radius - 1));
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        history.push(asst);
        // Tail padding so the last anchor isn't the candidate itself.
        history.push(trivial_assistant("tail"));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "assistant at d_back = R must be kept"
        );
    }

    // ------------------------------------------------------------------
    // 7. radius_boundary_at_radius_plus_one_is_pruned
    //
    // First+User anchor at 0, large assistant at index R+1. Now also far
    // from any tail anchor because of trailing padding.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn radius_boundary_at_radius_plus_one_is_pruned() {
        let radius = 10;
        let w = worker(radius);
        let mut history = vec![ChatEntry::user("anchor")];
        history.extend(std::iter::repeat_n(trivial_assistant("x"), radius));
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        history.push(asst);
        // Tail padding: need more than radius trivials so d_fwd > radius.
        history.extend(std::iter::repeat_n(trivial_assistant("y"), radius + 1));

        let mutations = evaluate(&w, history);
        assert!(
            excluded_ids(&mutations).contains(&asst_id),
            "assistant at d_back = R+1 and far from tail must be pruned"
        );
    }

    // ------------------------------------------------------------------
    // 8. forward_anchor_protects_wrap_up
    //
    // User at 0, candidate just before a mid-history User → kept.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn forward_anchor_protects_wrap_up() {
        let radius = 5;
        let w = worker(radius);
        let mut history = vec![ChatEntry::user("start")];
        history.extend(std::iter::repeat_n(trivial_assistant("x"), radius + 5));
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        history.push(asst);
        // Now add a user 1 entry later — candidate is the wrap-up.
        history.push(ChatEntry::user("next"));
        // Tail padding so the last anchor isn't artificially close.
        history.extend(std::iter::repeat_n(trivial_assistant("z"), 50));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "wrap-up assistant near forward user must be kept"
        );
    }

    // ------------------------------------------------------------------
    // 9. no_preceding_anchor_only_following_within_radius_kept
    //
    // Large assistant at index 0 (also first-anchor), user at index 3.
    // Either way the candidate is within radius.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn no_preceding_anchor_only_following_within_radius_kept() {
        let w = worker(5);
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        let mut history = vec![asst];
        history.push(trivial_assistant("a"));
        history.push(trivial_assistant("b"));
        // User at index 3.
        history.push(ChatEntry::user("anchor"));
        history.extend(std::iter::repeat_n(trivial_assistant("pad"), 20));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "assistant within forward radius (and as first anchor) must be kept"
        );
    }

    // ------------------------------------------------------------------
    // 10. assistant_far_from_all_anchors_pruned
    //
    // First+User anchor at 0, candidate in the middle, tail anchor far
    // away. The candidate is beyond radius from both anchors.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn assistant_far_from_all_anchors_pruned() {
        let radius = 3;
        let w = worker(radius);
        let mut history = vec![ChatEntry::user("anchor at 0")];
        // 100 entries of padding so candidate is far from any anchor.
        history.extend(std::iter::repeat_n(large_assistant(), 100));
        let small = trivial_assistant("ok");
        let small_id = small.id.clone();
        history.push(small);

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        // The small entry must NOT be pruned by this worker.
        assert!(
            !excluded.contains(&small_id),
            "small (<=80 token) assistant must not be pruned by this worker"
        );
    }

    // ------------------------------------------------------------------
    // 11. already_excluded_entry_is_skipped
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn already_excluded_entry_is_skipped() {
        let w = worker(1);
        let mut history = vec![ChatEntry::user("anchor")];
        let mut asst = large_assistant();
        asst.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let asst_id = asst.id.clone();
        history.push(asst);
        // Push the assistant outside radius.
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 5));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&asst_id),
            "already-excluded entry must not receive duplicate mutation"
        );
    }

    // ------------------------------------------------------------------
    // 11b. forced_included_entry_is_skipped
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn forced_included_entry_is_skipped() {
        let w = worker(1);
        let mut history = vec![ChatEntry::user("anchor")];
        let mut asst = large_assistant();
        asst.context_override = ContextOverride::ForcedInclude;
        let asst_id = asst.id.clone();
        history.push(asst);
        // Push the assistant outside radius.
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 5));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&asst_id),
            "ForcedInclude entry must not receive ForcedExclude mutation"
        );
    }

    // ------------------------------------------------------------------
    // 12. pinned_entry_is_skipped
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn pinned_entry_is_skipped() {
        use jinn_core_types::PinPosition;
        let w = worker(1);
        let mut history = vec![ChatEntry::user("anchor")];
        let mut asst = large_assistant();
        asst.pin_position = Some(PinPosition::Top);
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 5));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "pinned entry must not be pruned"
        );
    }

    // ------------------------------------------------------------------
    // 13. empty_assistant_is_skipped
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn empty_assistant_is_skipped() {
        let w = worker(1);
        let mut history = vec![ChatEntry::user("anchor")];
        history.push(trivial_assistant(""));
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 5));

        // No mutations at all — the empty assistant is skipped, the trivial
        // ones are filtered by token count.
        let mutations = evaluate(&w, history);
        assert!(mutations.is_empty());
    }

    // ------------------------------------------------------------------
    // 13b. small_assistant_far_from_user_is_not_pruned
    //
    // Small (<=80 tokens) assistant far from any anchor must NOT be
    // pruned by this worker — that's the trivial_assistant worker's
    // job. Verifies the disjoint-candidate-set contract.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn small_assistant_far_from_user_is_not_pruned() {
        let w = worker(2);
        let mut history = vec![ChatEntry::user("anchor at 0")];
        // 100 entries of padding so the small assistant is far from any anchor.
        history.extend(std::iter::repeat_n(large_assistant(), 100));
        let small = trivial_assistant("ok");
        let small_id = small.id.clone();
        history.push(small);

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&small_id),
            "small (<=80 token) assistant must not be pruned by this worker"
        );
    }

    // ------------------------------------------------------------------
    // 14. non_assistant_entries_in_prune_region_not_targeted
    //
    // ToolCall, ToolResult, System, etc. must not receive mutations.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn non_assistant_entries_in_prune_region_not_targeted() {
        use jinn_core_types::ToolResultStatus;
        let w = worker(1);
        let mut history = vec![ChatEntry::user("start")];
        history.push(ChatEntry::system("sys"));
        history.push(ChatEntry::error("err"));
        history.push(ChatEntry::tool_call("c1", "bash", r#"{"command":"ls"}"#));
        history.push(ChatEntry::tool_result(
            "c1",
            "bash",
            "out",
            ToolResultStatus::Success,
        ));
        history.push(large_assistant()); // <- candidate, far from user
        history.push(large_assistant()); // <- candidate
        // Push them beyond radius with trivial padding.
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 5));

        let mutations = evaluate(&w, history.clone());
        // The only excluded ids must belong to Assistant entries.
        for m in &mutations {
            if let HistoryMutation::SetContextOverride { entry_id, .. } = m {
                let found = history
                    .iter()
                    .find(|e| e.id == *entry_id)
                    .expect("mutation targets entry in history");
                assert!(
                    matches!(found.kind, ChatEntryKind::Assistant(_)),
                    "non-assistant entry must not be targeted: {:?}",
                    found.kind
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // 15. token_cache_populated_after_evaluate
    //
    // First evaluate populates the cache; second evaluate uses cache.
    // Verify by intercepting with a known token-count and observing the
    // cached value.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn token_cache_populated_after_evaluate() {
        let w = worker(1);
        let session_id = SessionId::new();
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        let history: Arc<[ChatEntry]> = vec![ChatEntry::user("anchor"), asst].into();

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&session_id, history.clone()).await });

        let cached = w.token_cache.get(&session_id, &asst_id);
        assert!(
            cached.is_some(),
            "assistant entry must be cached after evaluate"
        );
        let cached_count = cached.expect("checked Some");
        assert!(
            cached_count >= TEST_MIN_CANDIDATE_TOKENS,
            "large_assistant helper must produce >80 tokens, got {cached_count}"
        );
    }

    // ------------------------------------------------------------------
    // 15b. second_evaluate_uses_cached_tokens_not_recomputed
    //
    // If the second call recomputes, it will get >80 and emit a prune
    // mutation. If it reads from cache (now sabotaged to 10), it skips.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn second_evaluate_uses_cached_tokens_not_recomputed() {
        let w = worker(1);
        let session_id = SessionId::new();
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        // Use a history where the candidate CAN be pruned (i.e., far from
        // all anchors). To do that without the last-entry anchor saving
        // it, we put tail padding after the candidate.
        let mut history = vec![ChatEntry::user("anchor")];
        history.push(asst);
        history.extend(std::iter::repeat_n(trivial_assistant("pad"), 50));
        let history: Arc<[ChatEntry]> = history.into();

        let rt = tokio::runtime::Runtime::new().expect("runtime");

        // First call populates the cache and emits a prune mutation.
        let _ = rt.block_on(async { w.evaluate(&session_id, history.clone()).await });

        // Sanity: cache now holds a real count >80.
        let cached = w
            .token_cache
            .get(&session_id, &asst_id)
            .expect("first evaluate must populate cache");
        assert!(
            cached >= TEST_MIN_CANDIDATE_TOKENS,
            "real token count must be >80, got {cached}"
        );

        // Sabotage the cache so the cached value is now below threshold.
        w.token_cache.insert(
            session_id.clone(),
            asst_id.clone(),
            TEST_MIN_CANDIDATE_TOKENS - 1,
        );

        // Reset context_override on a fresh history copy so the worker
        // doesn't take the idempotency path.
        let mut history2 = (*history).to_vec();
        for e in &mut history2 {
            e.apply_context_override(
                ContextOverride::Default,
                ChangeSource::Internal {
                    label: "test".into(),
                },
            );
        }

        let mutations = rt.block_on(async { w.evaluate(&session_id, history2.into()).await });
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&asst_id),
            "second evaluate must read sabotaged cache and skip the entry; \
             if it recomputed, the entry would be pruned again"
        );
    }

    // ------------------------------------------------------------------
    // 16. radius_clamped_to_minimum_one
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn radius_clamped_to_minimum_one() {
        let w = worker(0);
        let mut history = vec![ChatEntry::user("anchor")];
        // Two large assistants: at d=1 (kept even at R=1) and d=2 (pruned).
        let near = large_assistant();
        let near_id = near.id.clone();
        history.push(near);
        let far = large_assistant();
        let far_id = far.id.clone();
        history.push(far);
        // Tail padding so the last anchor is far from the candidates.
        history.push(trivial_assistant("padding"));
        history.push(trivial_assistant("tail"));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&near_id),
            "d=1 at clamped R=1 must be kept"
        );
        assert!(
            excluded.contains(&far_id),
            "d=2 at clamped R=1 must be pruned"
        );
    }

    // ------------------------------------------------------------------
    // 17. multiple_large_assistants_far_from_anchors_all_pruned
    //
    // With trailing padding past the last large assistant, all the middle
    // large assistants are beyond radius from every anchor.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn multiple_large_assistants_far_from_anchors_all_pruned() {
        let w = worker(5);
        let mut history = vec![ChatEntry::user("start")];
        // First response: kept (d=1).
        let kept = large_assistant();
        let kept_id = kept.id.clone();
        history.push(kept);
        // 50 trivial entries → push the next large assistants far from user.
        history.extend(std::iter::repeat_n(trivial_assistant("x"), 50));
        // 5 large assistants — all far.
        let mut pruned_ids = Vec::new();
        for _ in 0..5 {
            let a = large_assistant();
            pruned_ids.push(a.id.clone());
            history.push(a);
        }
        // Tail padding past the last large assistant.
        history.extend(std::iter::repeat_n(trivial_assistant("y"), 50));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(!excluded.contains(&kept_id));
        for id in &pruned_ids {
            assert!(excluded.contains(id), "expected {id} to be pruned");
        }
    }

    // ------------------------------------------------------------------
    // 18. idempotency_second_evaluate_no_new_mutations
    //
    // After applying mutations to history, second evaluate must produce
    // zero new mutations.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn idempotency_second_evaluate_no_new_mutations() {
        let w = worker(2);
        let mut history = vec![ChatEntry::user("anchor")];
        let asst1 = large_assistant();
        let asst1_id = asst1.id.clone();
        history.push(asst1);
        history.push(trivial_assistant("filler"));
        let asst2 = large_assistant();
        let asst2_id = asst2.id.clone();
        history.push(asst2);
        // Tail padding so the last anchor is not the candidate itself,
        // and asst2 is beyond radius from both anchors.
        history.push(trivial_assistant("more filler"));
        history.push(trivial_assistant("more filler 2"));
        history.push(trivial_assistant("tail"));

        let first = evaluate(&w, history.clone());
        assert!(excluded_ids(&first).contains(&asst2_id));
        assert!(!excluded_ids(&first).contains(&asst1_id));

        // Apply mutations to a copy of history.
        let mut applied = history.clone();
        for m in &first {
            if let HistoryMutation::SetContextOverride {
                entry_id, value, ..
            } = m
                && let Some(e) = applied.iter_mut().find(|e| e.id == *entry_id)
            {
                e.apply_context_override(
                    *value,
                    ChangeSource::Internal {
                        label: "test".into(),
                    },
                );
            }
        }

        let second = evaluate(&w, applied);
        assert!(
            second.is_empty(),
            "second evaluate after applying mutations must produce no new mutations"
        );
    }

    // ------------------------------------------------------------------
    // 19. distances_to_nearest_anchors_unit_tests
    //
    // Direct unit tests for the helper. The first and last index are
    // always anchors when present.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn distances_helper_empty_anchors_returns_none_none() {
        let (a, b) = distances_to_nearest_anchors(5, &[]);
        assert_eq!((a, b), (None, None));
    }

    #[rstest::rstest]
    #[test]
    fn distances_helper_anchor_before_returns_back_only() {
        // anchors at 0 (first) and 10.
        let anchors = vec![0_usize, 10];
        let (a, b) = distances_to_nearest_anchors(3, &anchors);
        assert_eq!((a, b), (Some(3), Some(7)));
    }

    #[rstest::rstest]
    #[test]
    fn distances_helper_anchor_after_returns_fwd_only() {
        // anchors at 0 (first) and 5.
        let anchors = vec![0_usize, 5];
        let (a, b) = distances_to_nearest_anchors(2, &anchors);
        assert_eq!((a, b), (Some(2), Some(3)));
    }

    #[rstest::rstest]
    #[test]
    fn distances_helper_anchors_both_sides_returns_both() {
        // anchors at 0 (first), 10, 20.
        let anchors = vec![0_usize, 10, 20];
        let (a, b) = distances_to_nearest_anchors(12, &anchors);
        assert_eq!((a, b), (Some(2), Some(8)));
    }

    #[rstest::rstest]
    #[test]
    fn distances_helper_idx_is_anchor_returns_zero_zero() {
        // Candidate idx is itself an anchor.
        let anchors = vec![0_usize, 5, 10];
        let (a, b) = distances_to_nearest_anchors(5, &anchors);
        assert_eq!((a, b), (Some(0), Some(0)));
    }

    // ------------------------------------------------------------------
    // 20. collect_anchor_indices_unit_tests
    //
    // Verify the anchor-collection invariant: first, last, and every User
    // are anchors; the result is sorted and deduped.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn collect_anchors_empty_history() {
        assert!(collect_anchor_indices(&[]).is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn collect_anchors_single_entry_is_both_first_and_last() {
        let history = vec![ChatEntry::assistant("only")];
        let anchors = collect_anchor_indices(&history);
        assert_eq!(anchors, vec![0]);
    }

    #[rstest::rstest]
    #[test]
    fn collect_anchors_no_user_first_and_last_are_anchors() {
        let history = vec![
            ChatEntry::assistant("a"),
            ChatEntry::assistant("b"),
            ChatEntry::assistant("c"),
        ];
        let anchors = collect_anchor_indices(&history);
        assert_eq!(anchors, vec![0, 2]);
    }

    #[rstest::rstest]
    #[test]
    fn collect_anchors_first_is_user_deduped() {
        let history = vec![
            ChatEntry::user("first"),
            ChatEntry::assistant("mid"),
            ChatEntry::assistant("last"),
        ];
        let anchors = collect_anchor_indices(&history);
        // Index 0 is both first-anchor and user-anchor — deduped to a
        // single entry. Index 2 is the last-anchor.
        assert_eq!(anchors, vec![0, 2]);
    }

    #[rstest::rstest]
    #[test]
    fn collect_anchors_last_is_user_deduped() {
        let history = vec![
            ChatEntry::assistant("first"),
            ChatEntry::assistant("mid"),
            ChatEntry::user("last"),
        ];
        let anchors = collect_anchor_indices(&history);
        // First anchor at 0, last+user anchor at 2 (deduped).
        assert_eq!(anchors, vec![0, 2]);
    }

    #[rstest::rstest]
    #[test]
    fn collect_anchors_mid_user_included() {
        let history = vec![
            ChatEntry::assistant("first"),
            ChatEntry::user("mid"),
            ChatEntry::assistant("last"),
        ];
        let anchors = collect_anchor_indices(&history);
        assert_eq!(anchors, vec![0, 1, 2]);
    }
    // ------------------------------------------------------------------
    // 21. no_user_entries_first_and_last_still_anchor
    //
    // With no User entries, first and last anchors still apply. A middle
    // large assistant beyond radius from both ends is pruned; the
    // boundary assistants are kept.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn no_user_entries_first_and_last_still_anchor() {
        let w = worker(5);
        let leading = large_assistant();
        let leading_id = leading.id.clone();
        let mut history = vec![leading];
        // Padding between first anchor and middle candidate.
        history.extend(std::iter::repeat_n(trivial_assistant("pad"), 50));
        let middle = large_assistant();
        let middle_id = middle.id.clone();
        history.push(middle);
        // Padding between middle and last anchor.
        history.extend(std::iter::repeat_n(trivial_assistant("pad"), 50));
        let trailing = large_assistant();
        let trailing_id = trailing.id.clone();
        history.push(trailing);

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&leading_id),
            "first entry must be kept (first anchor)"
        );
        assert!(
            excluded.contains(&middle_id),
            "middle entry beyond radius from all anchors must be pruned"
        );
        assert!(
            !excluded.contains(&trailing_id),
            "last entry must be kept (last anchor)"
        );
    }

    // ------------------------------------------------------------------
    // min_age protection tests
    // ------------------------------------------------------------------

    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_large_assistant() {
        // Layout: idx 0 = User anchor; idx 1..=100 = padding; idx 101 = candidate;
        // idx 102..=151 = trivial_assistant padding (no trailing User).
        // Anchors = [0, 151]. candidate d_back=101, d_fwd=50 → both > radius(3) → pruned.
        let radius = 3;
        let w = worker(radius); // min_age = 0
        let mut history = vec![ChatEntry::user("anchor at 0")];
        history.extend(std::iter::repeat_n(large_assistant(), 100));
        let middle = large_assistant();
        let middle_id = middle.id.clone();
        history.push(middle);
        history.extend(std::iter::repeat_n(trivial_assistant("tail"), 50));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            excluded.contains(&middle_id),
            "with min_age = 0, candidate beyond radius from all anchors must be pruned"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_large_assistant() {
        // Same layout as above (history_len=152, candidate idx=101, age=51).
        // With min_age=200, candidate is inside the floor and must be kept
        // even though both anchor distances exceed the radius.
        let radius = 3;
        let w = worker_with_min_age(radius, 200);
        let mut history = vec![ChatEntry::user("anchor at 0")];
        history.extend(std::iter::repeat_n(large_assistant(), 100));
        let middle = large_assistant();
        let middle_id = middle.id.clone();
        history.push(middle);
        history.extend(std::iter::repeat_n(trivial_assistant("tail"), 50));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&middle_id),
            "recent large assistant inside min_age floor must be protected"
        );
    }

    #[rstest::rstest]
    #[test]
    fn min_age_boundary_strict_less_than_anchored_assistant_radius() {
        // Layout: idx 0 = User anchor; idx 1..=100 = padding; idx 101 = candidate;
        // idx 102..=151 = trivial_assistant padding. history_len = 152.
        // candidate idx = 101 → age = 152 - 101 - 1 = 50.
        //
        // is_within_min_age returns true when age < min_age (strict less-than).
        //
        // At min_age = 51: age=50 < 51 → protected.
        // At min_age = 50: age=50 < 50 is false → NOT protected.
        let radius = 3;
        let mut history = vec![ChatEntry::user("anchor at 0")];
        history.extend(std::iter::repeat_n(large_assistant(), 100));
        let middle = large_assistant();
        let middle_id = middle.id.clone();
        history.push(middle);
        history.extend(std::iter::repeat_n(trivial_assistant("tail"), 50));

        // Protected: age = 50 < min_age = 51.
        let w = worker_with_min_age(radius, 51);
        let mutations = evaluate(&w, history.clone());
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&middle_id),
            "age = min_age - 1 must be protected"
        );

        // Not protected: age = 50 = min_age.
        let w = worker_with_min_age(radius, 50);
        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            excluded.contains(&middle_id),
            "age = min_age must NOT be protected (strict less-than)"
        );
    }

    #[rstest::rstest]
    #[test]
    fn anchored_assistant_radius_defaults_to_twenty() {
        // Given the default anchored-assistant config.
        let config = AnchoredAssistantAutoPruneConfig::default();

        // Then the radius defaults to 20 — the same effective value the
        // wiring sourced from the removed anchor-shield config.
        assert_eq!(config.radius, 20);
    }
}
