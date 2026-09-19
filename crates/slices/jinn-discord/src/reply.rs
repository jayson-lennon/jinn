//! Extract the final reply from a session's history for Discord delivery.
//!
//! When a turn ends (`PhaseKind::Idle`), the gateway scans the history backward
//! to find the last meaningful entry: an `Assistant` message (the answer) or an
//! `Error` (a failed turn). User/System/Tool entries are skipped so the Discord
//! user only sees the model's final response, never the intermediate scratch.

use jinn_domain::protocol::{ChatEntry, ChatEntryKind};

/// The final reply to forward to Discord, or `None` if the history has no
/// assistant or error entry (e.g. a session that errored before the model ran).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalReply {
    /// The model's final answer text.
    Assistant(String),
    /// An error produced during the turn.
    Error(String),
}

/// Scan `history` backward and return the first non-`User` entry that carries
/// deliverable text.
///
/// Tool calls, tool results, thinking, annotations, compaction, and actor
/// messages are skipped — they are intermediate scaffolding, not the final
/// answer. The scan stops at the first `Assistant` or `Error` entry; if a user
/// message is encountered first (history ends with the user's own input, e.g.
/// a turn that produced no model output before going idle), `None` is returned.
#[must_use]
pub fn read_final_reply(history: &[ChatEntry]) -> Option<FinalReply> {
    for entry in history.iter().rev() {
        match &entry.kind {
            ChatEntryKind::Assistant(text) => return Some(FinalReply::Assistant(text.clone())),
            ChatEntryKind::Error(text) => return Some(FinalReply::Error(text.clone())),
            // The user's own input is a hard barrier — history ended at the
            // user's turn with no model output before going idle.
            ChatEntryKind::User { .. } => return None,
            // Skip intermediate scaffolding entries.
            ChatEntryKind::System(_)
            | ChatEntryKind::Actor { .. }
            | ChatEntryKind::Thinking(_)
            | ChatEntryKind::ToolCall { .. }
            | ChatEntryKind::ToolResult { .. }
            | ChatEntryKind::Transient(_)
            | ChatEntryKind::Compaction { .. }
            | ChatEntryKind::Annotation { .. } => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{FinalReply, read_final_reply};
    use jinn_domain::protocol::ChatEntry;

    #[rstest::rstest]
    #[test]
    fn history_ending_in_assistant_returns_its_text() {
        // Given a history ending in an assistant message.
        let history = vec![ChatEntry::user("hi"), ChatEntry::assistant("hello there")];
        // When reading the final reply.
        let reply = read_final_reply(&history);
        // Then it returns the assistant text.
        assert_eq!(reply, Some(FinalReply::Assistant("hello there".to_owned())));
    }

    #[rstest::rstest]
    #[test]
    fn history_ending_in_error_returns_the_error_text() {
        // Given a history ending in an error.
        let history = vec![ChatEntry::user("hi"), ChatEntry::error("boom")];
        // When reading the final reply.
        let reply = read_final_reply(&history);
        // Then it returns the error text.
        assert_eq!(reply, Some(FinalReply::Error("boom".to_owned())));
    }

    #[rstest::rstest]
    #[test]
    fn history_ending_in_user_returns_none() {
        // Given a history that ends with the user's own message (no model reply).
        let history = vec![ChatEntry::assistant("prev"), ChatEntry::user("again")];
        // When reading the final reply.
        let reply = read_final_reply(&history);
        // Then nothing is returned.
        assert_eq!(reply, None);
    }
}
