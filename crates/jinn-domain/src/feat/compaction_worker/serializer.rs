//! Serialize chat entries to labeled text for the compaction LLM prompt.
//!
//! Produces a human-readable representation of the conversation that the
//! summarization model can process. Tool calls and tool results are omitted
//! entirely to prevent the model from fixating on invocation noise or code/output
//! instead of producing narrative summaries.

use crate::protocol::{ChatEntry, ChatEntryKind};
/// Serialize a slice of chat entries into labeled text.
///
/// Each entry produces one or more labeled lines:
/// - `[User]: <text>`
/// - `[Assistant]: <text>`
/// - `[Actor: <source>]: <text>`
///
/// Tool calls, tool results, System, Error, Thinking, Transient, Skill, Annotation, and Compaction
/// entries are skipped - they are not relevant to the summarization prompt.
/// entries are skipped - they are not relevant to the summarization prompt.
pub fn serialize_entries_for_compaction(entries: &[ChatEntry]) -> String {
    let mut lines = Vec::new();

    for entry in entries {
        match &entry.kind {
            ChatEntryKind::User { display, .. } => {
                lines.push(format!("[User]: {display}"));
            }
            ChatEntryKind::Assistant(text) => {
                lines.push(format!("[Assistant]: {text}"));
            }
            ChatEntryKind::Actor { source, text } => {
                lines.push(format!("[Actor: {source}]: {text}"));
            }
            // Skip entries that don't contribute to the conversation summary.
            ChatEntryKind::System(_)
            | ChatEntryKind::Error(_)
            | ChatEntryKind::Thinking(_)
            | ChatEntryKind::Transient(_)
            | ChatEntryKind::Compaction { .. }
            | ChatEntryKind::Annotation { .. }
            | ChatEntryKind::ToolCall { .. }
            | ChatEntryKind::ToolResult { .. } => {}
        }
    }

    lines.join("\n")
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
    #[test]
    fn serializes_user_entry() {
        let entries = vec![ChatEntry::user("Hello world")];
        let result = serialize_entries_for_compaction(&entries);
        assert_eq!(result, "[User]: Hello world");
    }

    #[rstest::rstest]
    #[test]
    fn serializes_assistant_entry() {
        let entries = vec![ChatEntry::assistant("Hi there")];
        let result = serialize_entries_for_compaction(&entries);
        assert_eq!(result, "[Assistant]: Hi there");
    }

    #[rstest::rstest]
    #[test]
    fn skips_tool_call_entry() {
        // Given a tool call.
        let entries = vec![ChatEntry::tool_call("id1", "bash", r#"{"command":"ls"}"#)];
        let result = serialize_entries_for_compaction(&entries);
        // Then it produces no output (tool calls are skipped).
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn truncates_long_tool_result() {
        let long_content = "x".repeat(5000);
        let entries = vec![ChatEntry::tool_result(
            "id1",
            "bash",
            &long_content,
            crate::protocol::ToolResultStatus::Success,
        )];
        let result = serialize_entries_for_compaction(&entries);
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn skips_system_entry() {
        let entries = vec![ChatEntry::system("ready")];
        let result = serialize_entries_for_compaction(&entries);
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn skips_thinking_entry() {
        let entries = vec![ChatEntry::thinking("reasoning")];
        let result = serialize_entries_for_compaction(&entries);
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn serializes_mixed_entries() {
        let entries = vec![
            ChatEntry::user("fix the bug"),
            ChatEntry::assistant("let me check"),
            ChatEntry::tool_call("id1", "bash", r#"{"command":"ls"}"#),
            ChatEntry::tool_result(
                "id1",
                "bash",
                "file.rs",
                crate::protocol::ToolResultStatus::Success,
            ),
            ChatEntry::assistant("done"),
        ];
        let result = serialize_entries_for_compaction(&entries);
        let lines: Vec<&str> = result.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("[User]"));
        assert!(lines[1].starts_with("[Assistant]"));
        assert!(lines[2].starts_with("[Assistant]"));
    }

    #[rstest::rstest]
    #[test]
    fn truncates_tool_result_with_multibyte_at_boundary() {
        // Given content where byte 2000 falls in the middle of an em-dash (3 bytes).
        // 1999 ASCII 'x' chars + "-" (em-dash, 3 bytes) + more text.
        let mut content = "x".repeat(1999);
        content.push('-');
        content.push_str("more text");

        let entries = vec![ChatEntry::tool_result(
            "id1",
            "bash",
            &content,
            crate::protocol::ToolResultStatus::Success,
        )];

        // When serializing for compaction.
        let result = serialize_entries_for_compaction(&entries);

        // Then it produces no output (tool results are skipped).
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn truncates_tool_result_with_emoji() {
        // Given content with emoji exceeding 2000 bytes.
        let content = "🎉".repeat(1000); // Each 🎉 is 4 bytes, so 4000 bytes total.

        let entries = vec![ChatEntry::tool_result(
            "id1",
            "bash",
            &content,
            crate::protocol::ToolResultStatus::Success,
        )];

        // When serializing for compaction.
        let result = serialize_entries_for_compaction(&entries);

        // Then it produces no output (tool results are skipped).
        assert!(result.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn skips_tool_result_with_content() {
        // Given a tool result with sensitive output.
        let entries = vec![ChatEntry::tool_result(
            "id1",
            "bash",
            "sensitive output that should never reach the LLM",
            crate::protocol::ToolResultStatus::Success,
        )];

        // When serializing.
        let result = serialize_entries_for_compaction(&entries);

        // Then no output is produced.
        assert!(result.is_empty());
    }
}
