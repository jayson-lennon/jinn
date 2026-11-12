//! Turn counter - counts conversation turns in a session's history.
//!
//! A "turn" is when a participant gives up control:
//! - User sends a message → 1 turn
//! - Model finishes a response (not tool-use) → 1 turn
//!
//! A model response immediately following a `RuleInterrupt` (harness steering,
//! such as a handoff compaction nudge) is part of the steering turn, not a new
//! one: the agent never gave up control, so it is not counted.
//!
//! Always recomputes from the active session's chat history so the count
//! is correct after session switches, reloads, etc.

use jinn_core_types::{ChatEntry, ChatEntryKind};

#[cfg(test)]
use jinn_core_types::{ChatEntryId, EntryTiming, ToolResultStatus};

/// Computes the number of conversation turns in the given chat history.
///
/// A user entry always counts as a turn. An assistant entry counts as a turn
/// only if it is the last entry or is NOT followed by a `ToolCall` - intermediate
/// tool-loop assistant messages are part of the same turn. An assistant entry
/// immediately preceded by a `RuleInterrupt` is likewise part of the steering
/// turn, not a new one — the agent never gave up control.
///
/// When `fork_ordinal` is `Some(n)`, entries at indices 0..=n are treated as inherited
/// from a parent session and are skipped — only entries after the fork point count.
#[must_use]
pub fn compute_turn_count(history: &[ChatEntry], fork_ordinal: Option<usize>) -> u32 {
    let len = history.len();
    let mut count = 0u32;
    for (i, entry) in history.iter().enumerate() {
        // Skip inherited entries — they belong to a parent session.
        if let Some(ordinal) = fork_ordinal
            && i <= ordinal
        {
            continue;
        }
        match &entry.kind {
            ChatEntryKind::User { .. } => count += 1,
            ChatEntryKind::Assistant(..) => {
                let is_last = i == len - 1;
                let followed_by_tool_call = !is_last
                    && history
                        .get(i + 1)
                        .is_some_and(|h| matches!(h.kind, ChatEntryKind::ToolCall { .. }));
                // A response to harness steering (RuleInterrupt) is same-turn
                // work — the agent never gave up control — so it is not counted.
                let preceded_by_rule_interrupt = i > 0
                    && matches!(
                        history.get(i - 1).map(|h| &h.kind),
                        Some(ChatEntryKind::RuleInterrupt { .. })
                    );
                if !followed_by_tool_call && !preceded_by_rule_interrupt {
                    count += 1;
                }
            }
            _ => {}
        }
    }
    count
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

    #[rstest::rstest]
    fn empty_history_returns_zero() {
        // Given no chat entries.
        let history: Vec<ChatEntry> = vec![];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the count is zero.
        assert_eq!(count, 0);
    }

    #[rstest::rstest]
    fn user_entry_counts_as_one_turn() {
        // Given a single user entry.
        let history = vec![ChatEntry::user("hello")];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the count is one.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn assistant_entry_counts_as_one_turn() {
        // Given a single assistant entry.
        let history = vec![ChatEntry::assistant("hi there")];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the count is one.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn assistant_followed_by_tool_call_is_not_a_turn() {
        // Given an assistant entry followed by a tool call.
        let history = vec![
            ChatEntry::user("list files"),
            ChatEntry::assistant("let me check"),
            ChatEntry::tool_call("id-1", "bash", r#"{"command":"ls"}"#),
            ChatEntry::tool_result(
                "id-1",
                "bash",
                "file1.txt\nfile2.txt",
                ToolResultStatus::Success,
            ),
            ChatEntry::assistant("here are the files"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the intermediate assistant (followed by tool_call) is not a turn.
        // User = 1, final assistant = 1. Total = 2.
        assert_eq!(count, 2);
    }

    #[rstest::rstest]
    fn assistant_as_last_entry_is_a_turn() {
        // Given history ending with an assistant entry.
        let history = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi there")];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then both entries count as turns.
        assert_eq!(count, 2);
    }

    #[rstest::rstest]
    fn system_and_other_entries_are_ignored() {
        // Given entries that are not user or assistant.
        let history = vec![
            ChatEntry::system("welcome"),
            ChatEntry::error("something broke"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the count is zero.
        assert_eq!(count, 0);
    }

    #[rstest::rstest]
    fn tool_loop_skips_intermediate_assistant_entries() {
        // Given a tool loop with two rounds of tool calls.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::assistant("let me read the file"),
            ChatEntry::tool_call("id-1", "bash", r#"{"command":"cat main.rs"}"#),
            ChatEntry::tool_result("id-1", "bash", "fn main() {}", ToolResultStatus::Success),
            ChatEntry::assistant("now I'll edit it"),
            ChatEntry::tool_call("id-2", "bash", r#"{"command":"sed ..."}"#),
            ChatEntry::tool_result("id-2", "bash", "done", ToolResultStatus::Success),
            ChatEntry::assistant("the bug is fixed"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then only the user entry and final assistant entry count.
        // The two intermediate assistants (followed by tool_call) are skipped.
        assert_eq!(count, 2);
    }

    #[rstest::rstest]
    fn forked_session_turn_count_is_zero() {
        // Given a history with 5 entries (3 user + 2 assistant) and a fork_ordinal covering all of them.
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("how are you"),
            ChatEntry::assistant("good"),
            ChatEntry::user("great"),
        ];

        // When computing the turn count with fork_ordinal = 4 (all entries inherited).
        let count = compute_turn_count(&history, Some(4));

        // Then the count is zero — no turns taken in this forked session.
        assert_eq!(count, 0);
    }

    #[rstest::rstest]
    fn forked_session_counts_only_new_turns() {
        // Given a history with 5 inherited entries and 1 new user entry added after the fork.
        let history = vec![
            ChatEntry::user("hello"),
            ChatEntry::assistant("hi"),
            ChatEntry::user("how are you"),
            ChatEntry::assistant("good"),
            ChatEntry::user("great"),
            ChatEntry::user("new question"),
        ];

        // When computing the turn count with fork_ordinal = 4 (entries 0-4 inherited).
        let count = compute_turn_count(&history, Some(4));

        // Then only the new user entry counts as a turn.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn fork_ordinal_with_none_counts_all() {
        // Given a history with 2 user entries.
        let history = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi")];

        // When computing the turn count with fork_ordinal = None (root session).
        let count = compute_turn_count(&history, None);

        // Then all entries are counted.
        assert_eq!(count, 2);
    }

    #[rstest::rstest]
    fn assistant_after_rule_interrupt_is_not_a_turn() {
        // Given a user message followed by harness steering and the model's
        // response to it.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
            ChatEntry::assistant("recap: fixed half of it"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then only the user turn counts — the response to steering is
        // same-turn work.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn handoff_cycle_sequence_counts_one_turn() {
        // Given a full handoff compaction cycle: nudge, recap, summary.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
            ChatEntry::assistant("recap: fixed half of it"),
            ChatEntry::new_with_kind(
                ChatEntryId::new(),
                EntryTiming::instant_now(),
                ChatEntryKind::Compaction {
                    summary: "summary of the session".to_owned(),
                    tokens_before: 100,
                    tokens_after: 50,
                    entries_compacted: 2,
                    model_used: "test-model".to_owned(),
                },
                None,
            ),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the compaction cycle is invisible: only the user turn counts.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn assistant_after_rule_interrupt_followed_by_tool_call_not_counted() {
        // Given steering whose response continues straight into a tool call.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
            ChatEntry::assistant("recap, then continuing"),
            ChatEntry::tool_call("id-1", "bash", r#"{"command":"ls"}"#),
            ChatEntry::tool_result("id-1", "bash", "main.rs", ToolResultStatus::Success),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then neither skip fires a count: only the user turn counts.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn plain_assistant_still_counts() {
        // Given an ordinary response with no steering in front of it.
        let history = vec![ChatEntry::user("hello"), ChatEntry::assistant("hi there")];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then both entries count — the steering rule changed nothing here.
        assert_eq!(count, 2);
    }

    #[rstest::rstest]
    fn rule_interrupt_alone_counts_nothing() {
        // Given steering that fired with no response recorded yet.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then only the user turn counts — steering itself is not a turn.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn assistant_after_consecutive_rule_interrupts_is_not_a_turn() {
        // Given two rules fired back-to-back before one response.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
            ChatEntry::rule_interrupt("no-cat-command", "halt"),
            ChatEntry::assistant("recap: fixed half of it"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then it is still one steering episode: only the user turn counts.
        assert_eq!(count, 1);
    }

    #[rstest::rstest]
    fn response_after_steering_then_user_message_counts_normally() {
        // Given steering, a response, and then a fresh user message answered.
        let history = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::rule_interrupt("handoff", "summarize progress"),
            ChatEntry::assistant("recap: fixed half of it"),
            ChatEntry::user("keep going"),
            ChatEntry::assistant("done"),
        ];

        // When computing the turn count.
        let count = compute_turn_count(&history, None);

        // Then the steering cycle is invisible but the later exchange counts
        // in full: user(1), recap(0), user(2), response(3).
        assert_eq!(count, 3);
    }
}
