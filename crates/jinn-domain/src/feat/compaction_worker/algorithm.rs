//! Compaction algorithm - boundary finding, token accumulation, entry gathering.
//!
//! Extracted from the old `CompactionActor` for reuse by `CompactionWorker`.

use crate::feat::context::strategy::token_estimator::CharRatioEstimator;
use crate::feat::context::strategy::token_estimator::estimate_entry_tokens;
use crate::protocol::{ChatEntry, ChatEntryKind};

/// Whether a chat entry kind is a self-sufficient opener for the kept
/// (recent) region after compaction.
///
/// The kept region must stand on its own as a valid message sequence — see
/// [`adjust_cut_to_boundary`] Pass 3. Only `User` and `System` qualify: every
/// other kind structurally depends on a preceding message to render validly
/// in `entries_to_messages` (`Assistant` needs a preceding user turn; `ToolCall`
/// attaches to a preceding `Assistant`; `ToolResult` needs a matching preceding
/// `assistant.tool_calls[].id`). `Actor`/`Thinking`/`Transient`/`Error` render as
/// prefixed `User` messages but are context-optional, not a guaranteed first turn.
///
/// This is the predicate for Pass 3 of [`adjust_cut_to_boundary`].
fn is_valid_kept_opener(kind: &ChatEntryKind) -> bool {
    matches!(kind, ChatEntryKind::User { .. } | ChatEntryKind::System(..))
}

/// Adjust the token-based cut index forward to the next valid boundary.
///
/// Three passes compose to guarantee the kept (recent) region — everything
/// from the returned cut index onward — is a structurally valid LLM message
/// sequence on its own, independent of any preceding (compacted) context.
/// This makes the compaction summary non-load-bearing: excluding it can never
/// produce an invalid request.
///
/// - **Pass 1**: if the cut lands on a `ToolCall`/`ToolResult`, walk forward
///   past them. These have structural dependencies on preceding messages:
///   `ToolCall` merges into the preceding `Assistant` (orphaning if absent);
///   `ToolResult` needs a matching preceding `assistant.tool_calls[].id`.
/// - **Pass 2**: if the cut lands on an `Assistant`, advance past any
///   *incomplete* tool loop it begins (a complete loop is safe to cut on
///   structurally, but see Pass 3).
/// - **Pass 3**: walk the cut forward until the leading kept entry is a valid
///   self-sufficient opener ([`is_valid_kept_opener`] — only `User`/`System`).
///   This absorbs entries Pass 1/2 left behind that would still render as an
///   invalid first message: any `Assistant` (empty *or* non-empty standalone),
///   or a stray `ToolResult` whose partner was excluded.
///
/// Composition: Pass 1/2 are inlined in `inner` (whose recursion keeps them
///   self-consistent); Pass 3 is a single linear walk applied once at the top
///   level. Pass 3 runs last so it catches the empty/text `Assistant` opener
///   that Pass 2 leaves when a complete tool loop begins with one.
///
/// Returns the adjusted cut index (>= `cut_index`, <= `history.len()`).
///
/// This function never panics: the opener walk is total and `cut_index` is
/// bounded by `history.len()`.
pub fn adjust_cut_to_boundary(history: &[ChatEntry], cut_index: usize) -> usize {
    // Pass 1 + Pass 2 (recursively self-consistent).
    let cut_index = adjust_cut_inner(history, cut_index);

    // Pass 3: advance to the first entry that is a valid self-sufficient opener.
    // Linear walk; Pass 1/2 already ran, so this only mops up invalid openers
    // (any Assistant, or a stray ToolResult whose partner was excluded).
    let mut cut_index = cut_index;
    while cut_index < history.len() {
        let Some(entry) = history.get(cut_index) else {
            break;
        };
        if is_valid_kept_opener(&entry.kind) {
            break;
        }
        cut_index += 1;
    }
    cut_index
}

/// Pass 1 and Pass 2 of [`adjust_cut_to_boundary`].
///
/// Kept private so the public entry point always applies Pass 3 afterwards.
#[expect(clippy::expect_used, reason = "infallible")]
fn adjust_cut_inner(history: &[ChatEntry], cut_index: usize) -> usize {
    if cut_index >= history.len() {
        return cut_index;
    }

    // Pass 1: If cut lands on a ToolCall or ToolResult, walk forward past them.
    let cut_index = if matches!(
        history.get(cut_index).map(|e| &e.kind),
        Some(ChatEntryKind::ToolCall { .. } | ChatEntryKind::ToolResult { .. })
    ) {
        history
            .get(cut_index..)
            .expect("cut_index < history.len() checked above")
            .iter()
            .position(|entry| {
                !matches!(
                    entry.kind,
                    ChatEntryKind::ToolCall { .. } | ChatEntryKind::ToolResult { .. }
                )
            })
            .map_or(history.len(), |offset| cut_index + offset)
    } else {
        cut_index
    };

    if cut_index >= history.len() {
        return cut_index;
    }

    // Pass 2: If the cut lands on an Assistant, check for incomplete tool loops.
    if !matches!(
        history.get(cut_index).map(|e| &e.kind),
        Some(ChatEntryKind::Assistant(..))
    ) {
        return cut_index;
    }

    // Scan the tool loop group starting from this Assistant.
    let mut tool_call_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: Vec<String> = Vec::new();
    let mut group_end = cut_index + 1;

    let tail = history.get(cut_index + 1..).map_or(&[][..], |s| s);
    for (offset, entry) in tail.iter().enumerate() {
        match &entry.kind {
            ChatEntryKind::ToolCall { id, .. } => {
                tool_call_ids.push(id.clone());
                group_end = cut_index + 1 + offset + 1;
            }
            ChatEntryKind::ToolResult { id, .. } => {
                tool_result_ids.push(id.clone());
                group_end = cut_index + 1 + offset + 1;
            }
            // Stop at any entry that isn't part of this tool loop group.
            _ => break,
        }
    }

    // If all tool calls have matching results, the loop is complete - safe to cut here.
    if tool_call_ids.is_empty()
        || tool_call_ids
            .iter()
            .all(|id| tool_result_ids.iter().any(|r| r == id))
    {
        return cut_index;
    }

    // Incomplete tool loop - walk forward past the entire group and re-check.
    adjust_cut_inner(history, group_end)
}

/// Compute the cut index by walking backwards accumulating tokens.
///
/// Returns the index where recent entries begin (those fitting within the reserve).
/// If `compact_all` is true, returns `history.len()`.
/// If all tokens fit within the reserve, returns `start_index`.
pub fn compute_cut_index(
    history: &[ChatEntry],
    start_index: usize,
    reserve_tokens: usize,
    compact_all: bool,
) -> usize {
    let estimator = CharRatioEstimator;

    if compact_all {
        return history.len();
    }

    let mut accumulated_tokens = 0usize;
    let mut cut_index = start_index;

    for i in (start_index..history.len()).rev() {
        let Some(entry) = history.get(i) else {
            continue;
        };
        let tokens = estimate_entry_tokens(&estimator, entry);
        accumulated_tokens += tokens;
        if accumulated_tokens > reserve_tokens {
            cut_index = i + 1;
            break;
        }
    }

    cut_index
}

/// Gather compactable entries between `start_index` and `cut_index`,
/// excluding System and Compaction entries.
///
/// Returns `(gathered_indices, tokens_before)` where `tokens_before` is
/// the total estimated tokens of the gathered entries.
pub fn gather_compactable_entries(
    history: &[ChatEntry],
    start_index: usize,
    cut_index: usize,
) -> (Vec<usize>, usize) {
    let estimator = CharRatioEstimator;
    let mut gathered_indices: Vec<usize> = Vec::new();
    let mut tokens_before: usize = 0;
    for (i, entry) in history.iter().enumerate().take(cut_index).skip(start_index) {
        if matches!(entry.kind, ChatEntryKind::System(_)) || entry.is_compaction() {
            continue;
        }
        tokens_before += estimate_entry_tokens(&estimator, entry);
        gathered_indices.push(i);
    }
    (gathered_indices, tokens_before)
}

/// Resolve the effective context window size.
///
/// Uses the provider-reported `context_length` if available,
/// otherwise falls back to the configured `fallback`.
pub fn resolve_context_window(context_length: Option<u32>, fallback: usize) -> usize {
    context_length.map_or(fallback, |v| v as usize)
}

/// Estimate total tokens for all entries from `start_index` to end.
///
/// Excludes `Compaction` entries (they are boundaries, not content).
/// Includes System entries - they consume context window budget even though
/// they're excluded from compaction by `gather_compactable_entries()`.
pub fn estimate_total_tokens(history: &[ChatEntry], start_index: usize) -> usize {
    let estimator = CharRatioEstimator;
    let mut total = 0usize;
    for entry in history.iter().take(history.len()).skip(start_index) {
        if entry.is_compaction() {
            continue;
        }
        total += estimate_entry_tokens(&estimator, entry);
    }
    total
}

/// Find the start boundary: index after the last Compaction entry.
pub fn find_start_boundary(history: &[ChatEntry]) -> usize {
    history
        .iter()
        .rposition(ChatEntry::is_compaction)
        .map_or(0, |i| i + 1)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::{
        ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride,
    };

    #[rstest::rstest]
    #[test]
    fn is_valid_kept_opener_accepts_only_user_and_system() {
        // Given every kind of entry.
        let user = ChatEntry::user("hello");
        let system = ChatEntry::system("ready");
        let empty_assistant = ChatEntry::assistant("");
        let text_assistant = ChatEntry::assistant("done");
        let tool_call = ChatEntry::tool_call("tc", "bash", "{}");
        let tool_result = ChatEntry::tool_result(
            "tc",
            "bash",
            "out",
            crate::protocol::ToolResultStatus::Success,
        );
        let actor = ChatEntry::actor("src", "text");
        let thinking = ChatEntry::thinking("reasoning");
        let transient = ChatEntry::transient("welcome");
        let error = ChatEntry::error("boom");
        let compaction = ChatEntry {
            id: ChatEntryId::new(),
            timing: crate::protocol::EntryTiming::instant_now(),
            kind: ChatEntryKind::Compaction {
                summary: "summary".to_owned(),
                tokens_before: 0,
                tokens_after: 0,
                entries_compacted: 0,
                model_used: "model".to_owned(),
            },
            pin_position: None,
            context_override: ContextOverride::Default,
            context_history: Vec::new(),
            token_count: None,
        };

        // Then only User and System are valid openers.
        assert!(
            is_valid_kept_opener(&user.kind),
            "User should be valid opener"
        );
        assert!(
            is_valid_kept_opener(&system.kind),
            "System should be valid opener"
        );

        // And every other kind is rejected, including a non-empty Assistant.
        assert!(
            !is_valid_kept_opener(&empty_assistant.kind),
            "empty Assistant must be invalid"
        );
        assert!(
            !is_valid_kept_opener(&text_assistant.kind),
            "non-empty Assistant must be invalid"
        );
        assert!(
            !is_valid_kept_opener(&tool_call.kind),
            "ToolCall must be invalid"
        );
        assert!(
            !is_valid_kept_opener(&tool_result.kind),
            "ToolResult must be invalid"
        );
        assert!(!is_valid_kept_opener(&actor.kind), "Actor must be invalid");
        assert!(
            !is_valid_kept_opener(&thinking.kind),
            "Thinking must be invalid"
        );
        assert!(
            !is_valid_kept_opener(&transient.kind),
            "Transient must be invalid"
        );
        assert!(!is_valid_kept_opener(&error.kind), "Error must be invalid");
        assert!(
            !is_valid_kept_opener(&compaction.kind),
            "Compaction must be invalid"
        );
    }

    #[rstest::rstest]
    #[test]
    fn find_start_boundary_returns_zero_when_no_compaction() {
        let entries = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi")];
        assert_eq!(find_start_boundary(&entries), 0);
    }

    #[rstest::rstest]
    #[test]
    fn find_start_boundary_returns_after_last_compaction() {
        let entries = vec![
            ChatEntry::user("hello"),
            ChatEntry {
                id: ChatEntryId::new(),
                timing: crate::protocol::EntryTiming::instant_now(),
                kind: ChatEntryKind::Compaction {
                    summary: "summary1".to_owned(),
                    tokens_before: 0,
                    tokens_after: 0,
                    entries_compacted: 0,
                    model_used: "model".to_owned(),
                },
                pin_position: None,
                context_override: ContextOverride::Default,
                context_history: Vec::new(),
                token_count: None,
            },
            ChatEntry::user("world"),
        ];
        assert_eq!(find_start_boundary(&entries), 2);
    }

    #[rstest::rstest]
    #[test]
    fn compute_cut_index_returns_len_for_compact_all() {
        let entries = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi")];
        assert_eq!(compute_cut_index(&entries, 0, 100, true), 2);
    }

    #[rstest::rstest]
    #[test]
    fn compute_cut_index_returns_start_when_all_fit() {
        let entries = vec![ChatEntry::user("hi")];
        // A short entry fits in 10000 tokens reserve.
        assert_eq!(compute_cut_index(&entries, 0, 10000, false), 0);
    }

    #[rstest::rstest]
    #[test]
    fn gather_compactable_excludes_system() {
        let entries = vec![ChatEntry::system("ready"), ChatEntry::user("hello")];
        let (indices, _) = gather_compactable_entries(&entries, 0, 2);
        assert_eq!(indices.len(), 1);
        assert_eq!(indices[0], 1); // Only the user entry
    }

    #[rstest::rstest]
    #[test]
    fn adjust_cut_walks_past_tool_call_group() {
        // Given history: [User, Assistant, ToolCall, ToolResult, Assistant, ToolCall]
        // Cut at index 2 lands on ToolCall - should walk forward past entire tool loop group.
        // The Assistant at index 4 starts an incomplete tool loop (ToolCall at 5 has no result),
        // so the adjustment walks past it to index 6 (history.len()).
        let entries = vec![
            ChatEntry::user("do something"),
            ChatEntry::assistant("let me check"),
            ChatEntry::tool_call("tc1", "bash", r#"{"command":"ls"}"#),
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "file.txt",
                crate::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::assistant("here is the result"),
            ChatEntry::tool_call("tc2", "read", r#"{"path":"file.txt"}"#),
        ];

        // When adjusting cut at index 2 (lands on ToolCall).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then it walks forward past both tool groups. The first group (tc1)
        // is complete, but the second group (tc2, no result) is incomplete.
        // The result is past the end of history (6 = len).
        assert_eq!(
            adjusted, 6,
            "should skip past both tool groups to end of history"
        );
    }

    #[rstest::rstest]
    #[test]
    fn adjust_cut_advances_past_complete_tool_loop_to_valid_opener() {
        // Given history: [User, Assistant, ToolCall, ToolResult, Assistant("done")]
        // Cut at index 2 lands on ToolCall - the tool loop IS complete (result present).
        // Pass 1 walks to index 4 (Assistant). Pass 2 finds no tool calls, so the
        // Assistant at 4 is structurally clean - BUT Pass 3 rejects it as an opener
        // (Assistant needs a preceding user turn), advancing to index 5 (len).
        let entries = vec![
            ChatEntry::user("do something"),
            ChatEntry::assistant("let me check"),
            ChatEntry::tool_call("tc1", "bash", r#"{"command":"ls"}"#),
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "file.txt",
                crate::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::assistant("done"),
        ];

        // When adjusting cut at index 2.
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then Pass 3 advances past the Assistant opener to end of history.
        assert_eq!(
            adjusted, 5,
            "complete tool loop whose opener is an Assistant must advance to a valid opener"
        );
    }

    #[rstest::rstest]
    #[test]
    fn pass3_advances_past_empty_assistant_opener_complete_tool_loop() {
        // Given history ending: [empty Assistant, ToolCall, ToolResult, User("next")].
        // Cut lands on the empty Assistant (index 2). The tool loop is complete, but
        // an empty Assistant is not a valid opener - Pass 3 advances to the User.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::assistant(""),
            ChatEntry::tool_call("tc1", "bash", "{}"),
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "out",
                crate::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::user("next"),
        ];

        // When adjusting cut at index 2 (the empty Assistant).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then it advances past the empty-Assistant tool loop to the User at index 5.
        assert_eq!(adjusted, 5, "should advance to the User opener");
    }

    #[rstest::rstest]
    #[test]
    fn pass3_advances_past_dangling_tool_result_whose_call_is_excluded() {
        // Given history: the cut lands on a ToolResult whose matching ToolCall is
        // on the excluded (pre-cut) side. Pass 1 walks past the ToolResult, then
        // Pass 3 mops up any further invalid opener. Here the entry after the
        // ToolResult is a User, so the cut lands on it.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::tool_call("tc1", "bash", "{}"), // excluded side
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "out",
                crate::protocol::ToolResultStatus::Success,
            ), // <- cut lands here (index 3)
            ChatEntry::user("next"),
        ];

        // When adjusting cut at index 3 (the dangling ToolResult).
        let adjusted = adjust_cut_to_boundary(&entries, 3);

        // Then it advances past the ToolResult to the User at index 4.
        assert_eq!(
            adjusted, 4,
            "should advance past dangling ToolResult to User"
        );
    }

    #[rstest::rstest]
    #[test]
    fn pass3_advances_past_tool_call_whose_result_is_kept_group_split() {
        // Given history where the cut lands on a ToolCall whose ToolResult is in
        // the kept region (group split). Pass 1 walks past the whole ToolCall/
        // ToolResult run, landing on the User after it.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::tool_call("tc1", "bash", "{}"), // <- cut lands here (index 2)
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "out",
                crate::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::user("next"),
        ];

        // When adjusting cut at index 2 (the split ToolCall).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then it advances past the split tool group to the User at index 4.
        assert_eq!(adjusted, 4, "should advance past split tool group to User");
    }

    #[rstest::rstest]
    #[test]
    fn pass3_leaves_cut_unchanged_when_opener_is_real_user() {
        // Given history where the cut already lands on a User entry - a valid opener.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::user("next"),
            ChatEntry::assistant("reply"),
        ];

        // When adjusting cut at index 2 (a User).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then the cut is unchanged - no false advancement.
        assert_eq!(adjusted, 2, "valid User opener must not be advanced");
    }

    #[rstest::rstest]
    #[test]
    fn pass3_advances_past_nonempty_standalone_assistant_opener() {
        // Given history where the cut lands on a non-empty standalone Assistant
        // (text only, no tool calls). Per the refined invariant this is an invalid
        // opener - the kept region must open with User/System.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::assistant("here is a standalone reply"), // <- cut (index 2)
            ChatEntry::user("next"),
        ];

        // When adjusting cut at index 2 (non-empty standalone Assistant).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then it advances past the Assistant to the User at index 3.
        assert_eq!(
            adjusted, 3,
            "non-empty standalone Assistant is an invalid opener and must advance"
        );
    }

    #[rstest::rstest]
    #[test]
    fn pass3_consumes_entire_remaining_history_when_no_valid_opener() {
        // Given history whose entire kept tail is invalid openers (Assistants and
        // tool entries) with no User/System after the cut.
        let entries = vec![
            ChatEntry::user("earlier"),
            ChatEntry::assistant("excluded"),
            ChatEntry::assistant("done"),
            ChatEntry::tool_call("tc1", "bash", "{}"),
            ChatEntry::tool_result(
                "tc1",
                "bash",
                "out",
                crate::protocol::ToolResultStatus::Success,
            ),
        ];

        // When adjusting cut at index 2 (no valid opener anywhere after).
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then Pass 3 consumes the entire remaining history (5 = len).
        assert_eq!(
            adjusted,
            entries.len(),
            "should consume entire remaining history when no valid opener exists"
        );
    }

    #[rstest::rstest]
    #[test]
    fn adjust_cut_walks_past_incomplete_tool_loop() {
        // Given history: [User, Assistant, ToolCall] (no ToolResult yet)
        // Cut at index 2 lands on ToolCall - incomplete loop should skip it.
        let entries = vec![
            ChatEntry::user("do something"),
            ChatEntry::assistant("let me check"),
            ChatEntry::tool_call("tc1", "bash", r#"{"command":"ls"}"#),
        ];

        // When adjusting cut at index 2.
        let adjusted = adjust_cut_to_boundary(&entries, 2);

        // Then it walks past the incomplete tool loop to the end of history.
        assert_eq!(adjusted, 3, "should skip past incomplete tool loop to end");
    }

    #[rstest::rstest]
    #[test]
    fn compute_cut_index_walks_backwards_from_reserve() {
        // Given entries with enough text that they won't all fit in a tiny reserve.
        // Each entry ~70 chars = ~18 tokens via char-ratio (0.25 ratio).
        let entries = vec![
            ChatEntry::user("msg 0 is about twenty tokens long enough to be more than ten"),
            ChatEntry::assistant("resp 0 about twenty tokens long enough to be more than ten"),
            ChatEntry::user("msg 1 is about twenty tokens long enough to be more than ten"),
            ChatEntry::assistant("resp 1 about twenty tokens long enough to be more than ten"),
            ChatEntry::user("msg 2 is about twenty tokens long enough to be more than ten"),
            ChatEntry::assistant("resp 2 about twenty tokens long enough to be more than ten"),
        ];

        // When computing cut with a tiny reserve that can only hold ~2 entries.
        let cut = compute_cut_index(&entries, 0, 30, false);

        // Then the cut is past the start - older entries don't fit in reserve.
        assert!(
            cut > 0,
            "cut should be past start when entries exceed reserve"
        );
        assert!(
            cut < entries.len(),
            "cut should be before end when some entries fit"
        );
    }
}
