//! Entry-kind → (role, body) extraction for the FTS index.
//!
//! One mapping, used by both the reindex stage and tests: which entry kinds
//! are searchable, what role label they get, and what body text goes into the
//! index. Kinds with no prose worth searching (`Actor`, `Thinking`,
//! `Transient`, `Annotation`) yield `None` and never get an FTS row.

use crate::protocol::ChatEntryKind;
use crate::feat::session_search::model::SearchableRole;

/// A flattened, searchable view of one persisted entry.
#[derive(Debug, Clone)]
pub struct SearchableEntry {
    /// Stable entry id (the FTS row's `entry_id` column).
    pub entry_id: String,
    /// The role label (FTS `role` column).
    pub role: SearchableRole,
    /// The searchable prose (FTS `body` column).
    pub body: String,
    /// The entry's primary timestamp (FTS `entry_ts` column).
    pub entry_ts: String,
}

/// Extracts the searchable view of an entry kind, or `None` for kinds that
/// are never indexed.
///
/// Bodies mirror the tool-facing text conventions: tool calls/results are
/// prefixed with the tool name, tool results prefer `full_content` (recall
/// beats index size here — the untruncated output is the interesting part),
/// and user entries are indexed from `expanded` (what the LLM actually saw).
#[must_use]
pub fn extract_searchable(kind: &ChatEntryKind) -> Option<(SearchableRole, String)> {
    let (role, body) = match kind {
        ChatEntryKind::User { expanded, .. } => (SearchableRole::User, expanded.clone()),
        ChatEntryKind::Assistant(text) => (SearchableRole::Assistant, text.clone()),
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => (SearchableRole::ToolCall, format!("{name}: {arguments}")),
        ChatEntryKind::ToolResult {
            name,
            content,
            full_content,
            ..
        } => {
            let output = full_content.clone().unwrap_or_else(|| content.clone());
            (SearchableRole::ToolResult, format!("{name}: {output}"))
        }
        ChatEntryKind::System(text) => (SearchableRole::System, text.clone()),
        ChatEntryKind::Error(text) => (SearchableRole::Error, text.clone()),
        ChatEntryKind::Compaction { summary, .. } => (SearchableRole::Compaction, summary.clone()),
        // Never indexed: test-era chatter, hidden reasoning, UI-only hints,
        // and display-only citation lists.
        ChatEntryKind::Actor { .. }
        | ChatEntryKind::Thinking(_)
        | ChatEntryKind::Transient(_)
        | ChatEntryKind::Annotation { .. } => return None,
    };
    Some((role, body))
}

/// Renders an [`EntryTiming`](crate::protocol::EntryTiming)'s primary
/// timestamp as the RFC3339 key stored in the FTS `entry_ts` column.
///
/// RFC3339 UTC timestamps compare correctly as plain strings, which is what
/// the `since`/`until` filters rely on.
#[must_use]
pub fn entry_ts_key(timing: &crate::protocol::EntryTiming) -> String {
    timing.at().to_string()
}
