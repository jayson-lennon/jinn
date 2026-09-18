//! Trivial-assistant auto-prune worker.
//!
//! Prunes any `Assistant` entry whose estimated token count is at most
//! `max_tokens` (default: 80) AND whose age (distance from the end of raw
//! history) is at least `min_age` (default: 100). Targets low-value
//! "narration" turns the model emits between tool calls during autonomous
//! coding.
//!
//! # Semantics
//!
//! - Age is computed against every entry in raw history — already-excluded,
//!   thinking, transient, system, error, and pending-result entries all
//!   count. This makes the protection floor independent of what other
//!   auto-prune workers have already `ForcedExclude`d, so multiple workers
//!   compose cleanly.
//! - An entry is **protected** when its age is strictly less than `min_age`.
//!   With `min_age = 0`, no entry is ever protected (back-compat baseline).
//! - Only `ChatEntryKind::Assistant(_)` entries are candidates. Non-assistant
//!   entries are never targeted.
//! - Assistant entries inside the protection floor are never pruned,
//!   regardless of token count.
//! - Assistant entries outside the protection floor are pruned only if their
//!   estimated token count is `<= max_tokens`. Larger entries survive
//!   (a separate future worker will address large stale entries).
//! - Empty assistant entries are skipped defensively — they are already
//!   out of context via `is_empty_assistant()`, so they cannot be pruning
//!   candidates anyway.
//! - Already-`ForcedExclude` entries do not receive duplicate
//!   `SetContextOverride` mutations.
//! - Pinned entries are never pruned. Pin beats `ForcedExclude`.
//! - Tokens are counted with the same `TiktokenCounter::o200k_base()`
//!   encoder used by the token-count actor and the UI minimap, so the
//!   `max_tokens` cutoff matches what users see.
//!
//! # Token-cache integration
//!
//! Per-entry counts are looked up via
//! [`HistoryWorkerChatEntryTokenCache::get_or_insert_with`]. The first
//! worker to evaluate a session pays the tiktoken cost; subsequent
//! evaluations and concurrent workers (e.g., `AnchoredAssistantAutoPruneWorker`)
//! hit the cache.
//!
//! [`HistoryWorkerChatEntryTokenCache::get_or_insert_with`]: crate::feat::auto_prune_worker::HistoryWorkerChatEntryTokenCache::get_or_insert_with
//!
//! # Safety: pruning `Assistant` is unconditionally safe
//!
//! A `ToolCall` entry in `entries_to_messages` auto-creates an empty
//! `LlmMessage::Assistant { content: "", tool_calls: Some(vec![...]) }`
//! when its preceding `Assistant` message is missing. Excluding an
//! `Assistant` entry therefore cannot orphan a `ToolCall` or produce an
//! invalid provider request.
//!
//! # Example (min_age = 4, max_tokens = 80)
//!
//! ```text
//! X  [User]                  ← age 6 (not Assistant anyway)
//! X  [Assistant: "ok"]       ← age 5 (NOT protected; 1 token → pruned)
//!    [Assistant: "done"]     ← age 4 (NOT protected: age >= min_age)
//!    [Assistant: long...]    ← age 3 (protected: age 3 < min_age 4)
//!    [Tool Call]: bash       ← age 2 (protected; not Assistant anyway)
//!    [Tool Result] (OK)      ← age 1 (protected; not Assistant anyway)
//!    [Assistant: "ok"]       ← age 0 (protected; last entry)
//! ```

use std::sync::Arc;

pub use jinn_preferences_config::schemas::auto_prune::TrivialAssistantAutoPruneConfig;

use crate::feat::auto_prune_worker::{HistoryWorkerChatEntryTokenCache, is_within_min_age};
use crate::feat::context::strategy::token_estimator::{TiktokenCounter, TokenCounter};
use crate::feat::history_worker::worker_trait::HistoryWorker;
use crate::feat::session::chat_entry::{ChangeSource, ChatEntry, ChatEntryKind, ContextOverride};
use crate::feat::session::history_mutation::HistoryMutation;
use crate::protocol::SessionId;

/// Default enabled state for trivial-assistant auto-prune.
/// Default minimum age for trivial-assistant auto-prune.
/// Default token threshold below which an assistant entry is considered
/// "trivial" (small enough to prune when it lands outside the window).
/// Trivial-assistant auto-prune configuration.
///
/// Serialized as `[auto_prune.trivial_assistant]` in `jinn.toml`.
/// Controls the auto-prune worker that excludes any `Assistant` entry that
/// (a) is older than `min_age` entries from the end of history and
/// (b) is at most `max_tokens` tokens long.
///
/// The window counts every entry in raw history regardless of in-context
/// status, so that multiple auto-prune workers compose cleanly: each
/// worker's prune region is fixed by raw history length alone, not by what
/// has already been `ForcedExclude`d by other workers. Tokens are counted
/// via the same tiktoken `o200k_base` encoder used by the token-count
/// actor and minimap.
/// Trivial-assistant auto-prune worker.
///
/// See module docs for full semantics. Construct with
/// [`TrivialAssistantAutoPruneConfig`]; `min_age` is unclamped (0 means
/// "protect nothing"), `max_tokens` is clamped to a minimum of 1.
#[derive(Clone)]
pub struct TrivialAssistantAutoPruneWorker {
    /// Configuration for the trivial-assistant auto-prune strategy.
    pub config: TrivialAssistantAutoPruneConfig,
    /// Shared per-session, per-entry token-count cache. Cheap clone (inner is
    /// `Arc`-shared).
    pub token_cache: HistoryWorkerChatEntryTokenCache,
    /// Long-lived tiktoken counter. Cheap copy (`Copy` type with `&'static`
    /// encoder reference). Kept as a field so swapping in a non-static
    /// counter later is a one-line wiring change.
    pub counter: TiktokenCounter,
}

/// Build the list of `SetContextOverride::ForcedExclude` mutations for a
/// single snapshot.
///
/// Pure function (no `&self`) so unit tests can call it directly without
/// spinning up a tokio runtime.
///
/// Algorithm:
/// 1. For every entry in raw history, compute its age (distance from the
///    end).
/// 2. Skip entries protected by `min_age` (age < min_age). `min_age = 0`
///    protects nothing.
/// 3. For every `Assistant` entry not protected, look up (or compute and
///    cache) the token count via the shared
///    `HistoryWorkerChatEntryTokenCache`. Skips empty, pinned, or
///    already-excluded entries.
/// 4. If the count is `<= max_tokens`, emit a `SetContextOverride::ForcedExclude` mutation.
fn build_trivial_assistant_mutations(
    history: &[ChatEntry],
    min_age: usize,
    max_tokens: u32,
    session_id: &SessionId,
    token_cache: &HistoryWorkerChatEntryTokenCache,
    counter: &TiktokenCounter,
    worker_name: &str,
) -> Vec<HistoryMutation> {
    let max_tokens = max_tokens.max(1);
    let history_len = history.len();

    let mut mutations = Vec::new();

    for (idx, entry) in history.iter().enumerate() {
        // Skip entries protected by min_age (age < min_age).
        if is_within_min_age(history_len, idx, min_age) {
            continue;
        }

        // Only Assistant entries are candidates.
        let text = match &entry.kind {
            ChatEntryKind::Assistant(t) => t.as_str(),
            _ => continue,
        };

        // Defensive: empty assistant entries are placeholders for
        // tool-call-only responses and carry no pruneable text. Skip
        // explicitly to avoid emitting useless mutations (they are also
        // already out of context via `is_empty_assistant()`).
        if text.is_empty() {
            continue;
        }

        // Skip pinned entries — pin beats ForcedExclude.
        if entry.is_pinned() {
            continue;
        }

        // Skip protected entries (ForcedInclude or ForcedExclude) to avoid
        // duplicate mutations and respect user intent.
        if entry.is_protected_from_prune() {
            continue;
        }

        // Look up or compute token count. The closure only fires on first
        // miss for this (session, entry) pair; sibling workers sharing the
        // cache hit on first sight.
        let tokens =
            token_cache.get_or_insert_with(session_id, &entry.id, || counter.count(text) as u32);
        if tokens <= max_tokens {
            tracing::debug!(
                entry_id = %entry.id,
                tokens,
                max_tokens,
                age = history_len - idx - 1,
                min_age,
                "trivial_assistant: excluding old trivial assistant entry",
            );
            mutations.push(HistoryMutation::SetContextOverride {
                entry_id: entry.id.clone(),
                value: ContextOverride::ForcedExclude,
                source: ChangeSource::Worker {
                    name: worker_name.to_owned(),
                },
            });
        }
    }

    mutations
}

#[async_trait::async_trait]
impl HistoryWorker for TrivialAssistantAutoPruneWorker {
    #[expect(
        clippy::unnecessary_literal_bound,
        reason = "lifetime elision makes bound redundant"
    )]
    fn name(&self) -> &str {
        "auto-prune-trivial-assistant"
    }

    async fn evaluate(
        &self,
        session_id: &SessionId,
        history: Arc<[ChatEntry]>,
    ) -> Vec<HistoryMutation> {
        let mutations = build_trivial_assistant_mutations(
            &history,
            self.config.min_age,
            self.config.max_tokens as u32,
            session_id,
            &self.token_cache,
            &self.counter,
            self.name(),
        );
        tracing::debug!(
            mutations = mutations.len(),
            min_age = self.config.min_age,
            max_tokens = self.config.max_tokens,
            history_len = history.len(),
            "trivial_assistant evaluate done"
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
    use crate::feat::session::chat_entry::ChatEntry;
    use crate::protocol::SessionId;

    /// Build a worker with the given thresholds (enabled = true).
    fn worker(min_age: usize, max_tokens: usize) -> TrivialAssistantAutoPruneWorker {
        TrivialAssistantAutoPruneWorker {
            config: TrivialAssistantAutoPruneConfig {
                enabled: true,
                min_age,
                max_tokens,
            },
            token_cache: HistoryWorkerChatEntryTokenCache::new(),
            counter: TiktokenCounter::o200k_base(),
        }
    }

    use crate::feat::session::chat_entry::ChatEntryId;

    /// Build N plain user entries (all in-context).
    fn users(n: usize) -> Vec<ChatEntry> {
        (0..n)
            .map(|i| ChatEntry::user(format!("user msg {i}")))
            .collect()
    }

    /// Build a trivial (≤80 tiktoken tokens) assistant entry.
    fn trivial_assistant(text: &str) -> ChatEntry {
        ChatEntry::assistant(text)
    }

    /// Build an assistant entry whose token count is reliably greater
    /// than 80 under the `o200k_base` encoding.
    fn large_assistant() -> ChatEntry {
        // ~500 chars of varied English prose → comfortably >80 o200k_base
        // tokens. Verified by the sanity assertion in
        // `large_assistant_outside_window_is_not_pruned`.
        let body = "This is a substantial assistant response with multiple sentences and \
             enough vocabulary to comfortably exceed eighty tiktoken tokens under \
             the o200k_base encoding used throughout the codebase. ";
        ChatEntry::assistant(body.repeat(4))
    }

    /// Evaluate the worker on a history snapshot.
    fn evaluate(
        w: &TrivialAssistantAutoPruneWorker,
        history: Vec<ChatEntry>,
    ) -> Vec<HistoryMutation> {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let history: Arc<[ChatEntry]> = history.into();
        rt.block_on(async { w.evaluate(&SessionId::new(), history).await })
    }

    /// Collect the entry ids targeted by `SetContextOverride::ForcedExclude` mutations.
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
        let w = worker(100, 80);
        assert!(evaluate(&w, Vec::new()).is_empty());
    }

    // ------------------------------------------------------------------
    // 2. history_under_threshold_produces_no_mutations
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn history_under_threshold_produces_no_mutations() {
        let w = worker(100, 80);
        let mut history = users(50);
        history.insert(0, trivial_assistant("ok"));
        assert!(evaluate(&w, history).is_empty());
    }

    // ------------------------------------------------------------------
    // 3. history_exactly_at_threshold_produces_no_mutations
    //
    // 100 entries total. Trivial assistant at idx 0 is inside the window
    // → not pruned.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn history_exactly_at_threshold_produces_no_mutations() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        history.push(trivial_assistant("ok"));
        history.extend(users(99));
        assert_eq!(history.len(), 100);
        assert!(evaluate(&w, history).is_empty());
    }

    // ------------------------------------------------------------------
    // 4. trivial_assistant_outside_window_is_pruned
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn trivial_assistant_outside_window_is_pruned() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        // 100 user entries → total 101. Window covers last 100
        // (positions 1..=100). Position 0 (the assistant) is outside.
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert_eq!(mutations.len(), 1);
        assert!(excluded.contains(&asst_id));
    }

    // ------------------------------------------------------------------
    // 5. large_assistant_outside_window_is_not_pruned
    //
    // Sanity-check the test helper: ensure the "large" assistant text
    // actually exceeds 80 tiktoken tokens, then verify the worker leaves
    // it alone.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn large_assistant_outside_window_is_not_pruned() {
        let counter = TiktokenCounter::o200k_base();
        let asst = large_assistant();
        let text = match &asst.kind {
            ChatEntryKind::Assistant(t) => t.clone(),
            _ => panic!("expected assistant"),
        };
        let tokens = counter.count(&text);
        assert!(
            tokens > 80,
            "test helper must produce >80 tokens, got {tokens}"
        );

        let w = worker(100, 80);
        let mut history = Vec::new();
        let asst = large_assistant();
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&asst_id),
            "large assistant outside window must NOT be pruned"
        );
        assert!(mutations.is_empty());
    }

    // ------------------------------------------------------------------
    // 6. trivial_assistant_inside_window_is_not_pruned
    //
    // Place the assistant AFTER 100 user entries so it is inside the
    // window.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn trivial_assistant_inside_window_is_not_pruned() {
        let w = worker(100, 80);
        let mut history = users(100);
        let asst = trivial_assistant("ok");
        let asst_id = asst.id.clone();
        history.push(asst);
        // total = 101 entries. Window is last 100 (positions 1..=100).
        // The assistant at idx 100 is inside the window.
        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(!excluded.contains(&asst_id));
        assert!(mutations.is_empty());
    }

    // ------------------------------------------------------------------
    // 7. empty_assistant_outside_window_is_not_targeted
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn empty_assistant_outside_window_is_not_targeted() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        history.push(trivial_assistant(""));
        history.extend(users(100));
        let mutations = evaluate(&w, history);
        assert!(mutations.is_empty(), "empty assistant must not be targeted");
    }

    // ------------------------------------------------------------------
    // 8. non_assistant_entries_in_prune_window_are_not_targeted
    //
    // Old user/tool entries plus one trivial assistant. Only the
    // assistant should be pruned.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn non_assistant_entries_in_prune_window_are_not_targeted() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        let old_user = ChatEntry::user("old user");
        let old_user_id = old_user.id.clone();
        history.push(old_user);
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert_eq!(mutations.len(), 1);
        assert!(excluded.contains(&asst_id));
        assert!(
            !excluded.contains(&old_user_id),
            "non-assistant entry must not be pruned"
        );
    }

    // ------------------------------------------------------------------
    // 9. already_excluded_assistant_does_not_get_duplicate_mutation
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn already_excluded_assistant_does_not_get_duplicate_mutation() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        let mut asst = trivial_assistant("done");
        asst.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        assert!(
            mutations.is_empty(),
            "already-excluded entry must not receive duplicate mutation"
        );
        // The assistant id should not appear in any mutation.
        let excluded = excluded_ids(&mutations);
        assert!(!excluded.contains(&asst_id));
    }

    // ------------------------------------------------------------------
    // 9b. forced_included_assistant_does_not_get_mutation
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn forced_included_assistant_does_not_get_mutation() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        let mut asst = trivial_assistant("done");
        asst.context_override = ContextOverride::ForcedInclude;
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        // No mutation for the ForcedInclude entry.
        let excluded = excluded_ids(&mutations);
        assert!(
            !excluded.contains(&asst_id),
            "ForcedInclude entry must not receive ForcedExclude mutation"
        );
    }

    // ------------------------------------------------------------------
    // ------------------------------------------------------------------
    // 10. min_age_zero_prunes_old_entries
    //
    // min_age = 0 means "protect nothing" — every entry is a candidate.
    // Two entries (trivial assistant, user). Trivial assistant at idx 0
    // is NOT protected → pruned.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn min_age_zero_prunes_old_entries() {
        let w = worker(0, 80);
        let mut history = Vec::new();
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        history.push(ChatEntry::user("after"));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(excluded.contains(&asst_id));
    }

    // ------------------------------------------------------------------
    // 10b. min_age_protects_recent_trivial_assistant
    //
    // Trivial assistant at idx 0 in a 2-entry history has age 1.
    // With min_age = 5, age 1 < 5 → protected → not pruned.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn min_age_protects_recent_trivial_assistant() {
        let w = worker(5, 80);
        let mut history = Vec::new();
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        history.push(ChatEntry::user("after"));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "recent trivial assistant must be protected by min_age"
        );
    }

    // ------------------------------------------------------------------
    // 10c. min_age_boundary_strict_less_than
    //
    // age = history_len - idx - 1.
    // history_len = 100, trivial_assistant at idx 95 → age = 4. With
    // min_age = 5: 4 < 5 → protected. With min_age = 4: 4 < 4 → not protected.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn min_age_boundary_strict_less_than() {
        // Protected case: age 4 < min_age 5.
        let w = worker(5, 80);
        let mut history = users(95); // 95 user entries
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(4)); // total 100, assistant at idx 95, age = 4

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "age = min_age - 1 must be protected"
        );

        // Not-protected case: age 4 = min_age 4.
        let w = worker(4, 80);
        let mut history = users(95);
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(4));

        let mutations = evaluate(&w, history);
        assert!(
            excluded_ids(&mutations).contains(&asst_id),
            "age = min_age must NOT be protected (strict less-than)"
        );
    }

    // ------------------------------------------------------------------
    // 11. max_tokens_clamped_to_1
    //
    // max_tokens = 0 → clamped to 1. "Prune if tokens <= 1". A
    // single-word assistant like "ok" is exactly 1 o200k_base token.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn max_tokens_clamped_to_1() {
        let counter = TiktokenCounter::o200k_base();
        let text = "ok";
        let tokens = counter.count(text);
        assert_eq!(tokens, 1, "test assumes 'ok' is 1 token under o200k_base");

        let w = worker(100, 0);
        let mut history = Vec::new();
        let asst = trivial_assistant(text);
        let asst_id = asst.id.clone();
        history.push(asst);
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert!(
            excluded.contains(&asst_id),
            "with max_tokens clamped to 1, 'ok' (1 token) is pruned"
        );
    }

    // ------------------------------------------------------------------
    // 12. multiple_trivial_assistants_all_pruned_when_old
    //
    // 5 trivial assistants scattered in the first 100 positions of a
    // 200-entry history. All 5 should be excluded.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn multiple_trivial_assistants_all_pruned_when_old() {
        let w = worker(100, 80);
        let mut history = Vec::new();
        let mut expected_ids = Vec::new();
        for i in 0..5 {
            let asst = trivial_assistant(&format!("narration {i}"));
            expected_ids.push(asst.id.clone());
            history.push(asst);
        }
        // 195 user entries → total 200 entries. Window covers last 100.
        history.extend(users(195));

        let mutations = evaluate(&w, history);
        let excluded = excluded_ids(&mutations);
        assert_eq!(mutations.len(), 5);
        for id in &expected_ids {
            assert!(excluded.contains(id), "expected {id} to be pruned");
        }
    }

    // ------------------------------------------------------------------
    // 13. token_cache_populated_after_evaluate
    //
    // First evaluate populates the shared cache; verify by direct read.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn token_cache_populated_after_evaluate() {
        let w = worker(100, 80);
        let session_id = SessionId::new();
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        let mut history = vec![asst];
        history.extend(users(100));

        let history: Arc<[ChatEntry]> = history.into();
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async { w.evaluate(&session_id, history).await });

        assert_eq!(
            w.token_cache.get(&session_id, &asst_id),
            Some(1),
            "'done' is 1 o200k_base token and must be cached after evaluate"
        );
    }

    // ------------------------------------------------------------------
    // 14. second_evaluate_uses_cached_tokens_not_recomputed
    //
    // Sabotage the cache between two evaluate calls. The sabotage value
    // is `max_tokens + 1` (81), which causes the worker to SKIP (since
    // `tokens <= max_tokens` is `81 <= 80` = false). If the worker
    // recomputed, it would see 1 token and PRUNE. So a passing test
    // proves the cache was consulted.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn second_evaluate_uses_cached_tokens_not_recomputed() {
        let w = worker(100, 80);
        let session_id = SessionId::new();
        let asst = trivial_assistant("done");
        let asst_id = asst.id.clone();
        let mut history = vec![asst];
        history.extend(users(100));
        let history: Arc<[ChatEntry]> = history.into();

        let rt = tokio::runtime::Runtime::new().expect("runtime");

        // Sanity: first call prunes the entry.
        let first = rt.block_on(async { w.evaluate(&session_id, history.clone()).await });
        assert!(
            excluded_ids(&first).contains(&asst_id),
            "first evaluate must prune 'done' (1 token, outside window)"
        );

        // Sabotage cache: 81 > 80 = max_tokens → "too large to be trivial".
        // If the worker reads from cache on second evaluate, it will skip;
        // if it recomputes, it will see 1 token and prune.
        w.token_cache
            .insert(session_id.clone(), asst_id.clone(), 81);

        // Reset context_override so the idempotency skip doesn't hide the result.
        let mut history2 = (*history).to_vec();
        for e in &mut history2 {
            e.apply_context_override(
                ContextOverride::Default,
                ChangeSource::Internal {
                    label: "test".into(),
                },
            );
        }

        let second = rt.block_on(async { w.evaluate(&session_id, history2.into()).await });
        assert!(
            !excluded_ids(&second).contains(&asst_id),
            "second evaluate must read sabotaged cache (81 > 80) and skip; \"
             if it recomputed it would see 1 token and prune"
        );
    }

    // ------------------------------------------------------------------
    // 15. pinned_trivial_assistant_outside_window_is_not_pruned
    //
    // The new pin-skip guard must prevent a pinned trivial assistant from
    // receiving a ForcedExclude mutation, even when the entry is well
    // outside the keep window and would otherwise qualify for pruning.
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn pinned_trivial_assistant_outside_window_is_not_pruned() {
        use crate::feat::session::chat_entry::PinPosition;
        let w = worker(100, 80);
        let mut asst = trivial_assistant("done");
        asst.pin_position = Some(PinPosition::Top);
        let asst_id = asst.id.clone();
        let mut history = vec![asst];
        history.extend(users(100));

        let mutations = evaluate(&w, history);
        assert!(
            !excluded_ids(&mutations).contains(&asst_id),
            "pinned trivial assistant must not be pruned even outside window"
        );
    }

    // ------------------------------------------------------------------
    // 16. anchored_assistant_worker_reads_external_cache_writes
    //
    // Cross-worker sharing test: the trivial worker and the
    // anchored-assistant worker share a cache handle. We don't need to
    // actually run the trivial worker — sabotaging the cache substitutes
    // for a first write. The point is to prove the anchored-assistant worker honors
    // cache writes from outside its own evaluate path.
    //
    // Proof strategy: target entry "ok" has 1 o200k_base token. Sabotage
    // the cache with 999. The anchored-assistant worker's min_candidate_tokens is 81,
    // so a recomputed count of 1 would be skipped, but a cached count of
    // 999 would be classified as a candidate (then pruned because d_back
    // = 101 > radius 5).
    // ------------------------------------------------------------------
    #[rstest::rstest]
    #[test]
    fn anchored_assistant_worker_reads_external_cache_writes() {
        use crate::feat::auto_prune_worker::anchored_assistant::AnchoredAssistantAutoPruneWorker;
        use jinn_preferences_config::schemas::auto_prune::AnchoredAssistantAutoPruneConfig;

        let shared_cache = HistoryWorkerChatEntryTokenCache::new();
        let session_id = SessionId::new();

        // Trivial worker constructed against the shared handle — proves
        // both workers can hold the same cache instance (type-check).
        let _trivial = TrivialAssistantAutoPruneWorker {
            config: TrivialAssistantAutoPruneConfig {
                enabled: true,
                min_age: 100,
                max_tokens: 80,
            },
            token_cache: shared_cache.clone(),
            counter: TiktokenCounter::o200k_base(),
        };

        let anchored = AnchoredAssistantAutoPruneWorker {
            config: AnchoredAssistantAutoPruneConfig {
                enabled: true,
                radius: 5,
                min_age: 0,
            },
            radius: 5,
            min_candidate_tokens: 81,
            token_cache: shared_cache.clone(),
            counter: TiktokenCounter::o200k_base(),
        };

        // History: User at index 0, 100 trivial-step padding entries,
        // target "ok" at index 101, then 6 more padding entries so the
        // last anchor is at index 107 (d_fwd = 6 > radius 5).
        let mut history = vec![ChatEntry::user("anchor")];
        history.extend(std::iter::repeat_n(trivial_assistant("step"), 100));
        let target = trivial_assistant("ok");
        let target_id = target.id.clone();
        history.push(target);
        history.extend(std::iter::repeat_n(trivial_assistant("step"), 6));

        // Sabotage: cache "ok" as 999 tokens.
        shared_cache.insert(session_id.clone(), target_id.clone(), 999);

        let history: Arc<[ChatEntry]> = history.into();
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mutations = rt.block_on(async { anchored.evaluate(&session_id, history).await });

        assert!(
            excluded_ids(&mutations).contains(&target_id),
            "anchored-assistant worker must read sabotaged cache value (999 > 81 -> candidate); \",
             if it recomputed it would see 1 token and skip",
        );

        // Belt-and-suspenders: the anchored-assistant worker's get_or_insert_with
        // must not overwrite an existing cache entry.
        assert_eq!(
            shared_cache.get(&session_id, &target_id),
            Some(999),
            "anchored-assistant worker must not overwrite an existing cache entry"
        );
    }
}
