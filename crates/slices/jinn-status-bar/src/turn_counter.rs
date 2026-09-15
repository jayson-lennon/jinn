//! Turn counter - counts conversation turns for the status bar.
//!
//! A "turn" is when a participant gives up control:
//! - User sends a message → 1 turn
//! - Model finishes a response (not tool-use) → 1 turn
//!
//! Always recomputes from the active session's chat history so the count
//! is correct after session switches, reloads, etc.

use jinn_domain::protocol::{ChatEntry, ChatEntryKind};

#[cfg(test)]
use jinn_domain::protocol::ToolResultStatus;

/// Computes the number of conversation turns in the given chat history.
///
/// A user entry always counts as a turn. An assistant entry counts as a turn
/// only if it is the last entry or is NOT followed by a `ToolCall` - intermediate
/// tool-loop assistant messages are part of the same turn.
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
                if !followed_by_tool_call {
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
    use jinn_domain::protocol::ChatEntry;

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
}
