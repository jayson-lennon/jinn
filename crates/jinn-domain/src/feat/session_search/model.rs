//! Data model crossing the session-store boundary for search and fetch.
//!
//! These types are the contract between the tools (`session_search` /
//! `session_fetch`) and the store backends. Params are plain data; outcomes
//! carry everything the formatter needs so formatting stays a pure function.

use serde::{Deserialize, Serialize};

/// A role filter / role label for a searchable entry.
///
/// Mirrors the subset of [`ChatEntryKind`](crate::protocol::ChatEntryKind)
/// variants that carry prose worth indexing. `Actor`, `Thinking`, `Transient`,
/// and `Annotation` entries are never indexed and have no role here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchableRole {
    /// A user message (indexed from the token-expanded text).
    User,
    /// An assistant response.
    Assistant,
    /// A tool invocation (indexed as `name: arguments`).
    ToolCall,
    /// A tool result (indexed as `name: output`, preferring untruncated content).
    ToolResult,
    /// A system status message.
    System,
    /// An error message.
    Error,
    /// A compaction summary.
    Compaction,
}

impl SearchableRole {
    /// Every role, in the order presented in tool docs.
    ///
    /// `actor` and `thinking` are deliberately absent: those kinds are never
    /// indexed or presented.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::User,
            Self::Assistant,
            Self::ToolCall,
            Self::ToolResult,
            Self::System,
            Self::Error,
            Self::Compaction,
        ]
    }

    /// The stable snake_case label persisted in the FTS `role` column and
    /// echoed in tool output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::System => "system",
            Self::Error => "error",
            Self::Compaction => "compaction",
        }
    }

    /// Parses a role label (the `fields` values accepted by `session_search`).
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        let role = match label {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool_call" => Self::ToolCall,
            "tool_result" => Self::ToolResult,
            "system" => Self::System,
            "error" => Self::Error,
            "compaction" => Self::Compaction,
            _ => return None,
        };
        Some(role)
    }
}

impl std::fmt::Display for SearchableRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parameters for an FTS query against the session index.
///
/// Session scoping and role/date filtering are resolved by the caller (the
/// tool) into plain id/label lists; the store executes them as `WHERE`
/// post-filters around the FTS `MATCH`.
#[derive(Debug, Clone)]
pub struct SearchParams {
    /// The raw FTS5 `MATCH` expression, passed through unmodified.
    pub query: String,
    /// Session ids to search within. Empty means "no restriction" (all
    /// sessions), which keeps the SQL simple when scope is `all`.
    pub session_ids: Vec<String>,
    /// Roles to include. Empty means every indexed role.
    pub roles: Vec<SearchableRole>,
    /// Inclusive lower bound on entry timestamps (RFC3339).
    pub since: Option<jiff::Timestamp>,
    /// Inclusive upper bound on entry timestamps (RFC3339).
    pub until: Option<jiff::Timestamp>,
    /// Maximum number of ranked hits to return.
    pub limit: usize,
}

/// One ranked search hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// Session the hit belongs to.
    pub session_id: String,
    /// Stable entry id — the address `session_fetch` drills into.
    pub entry_id: String,
    /// Role label of the indexed entry.
    pub role: String,
    /// FTS5 `snippet()` fragment with matches wrapped in `<<` `>>`.
    pub snippet: String,
    /// The entry's timestamp (RFC3339) for display and ordering context.
    pub entry_ts: String,
    /// Whether the entry is currently excluded from LLM context.
    pub excluded: bool,
}

/// The result of a search: flat ranked hits plus rollup counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutcome {
    /// Total matching rows across the searched scope (before `limit`).
    pub total_matches: u64,
    /// Matches per session, descending by count. Reveals sessions beyond the
    /// returned hits so the agent can re-search with a narrower scope.
    pub per_session: Vec<(String, u64)>,
    /// The top-N ranked hits.
    pub hits: Vec<SearchHit>,
}

/// One transcript entry in a fetched window.
///
/// `ordinal` is the live position within the session's history — rendered
/// for orientation, never used as an address.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Live position of this entry in the session history.
    pub ordinal: usize,
    /// The deserialized entry.
    pub entry: crate::protocol::ChatEntry,
    /// Whether the entry is currently excluded from LLM context.
    pub excluded: bool,
}

/// A contiguous slice of a session's transcript.
#[derive(Debug, Clone)]
pub struct TranscriptWindow {
    /// The session this window came from.
    pub session_id: String,
    /// Session title at fetch time.
    pub title: Option<String>,
    /// Total entries in the session (for the `entries lo–hi of total` header).
    pub total_entries: usize,
    /// The window's entries, in history order.
    pub entries: Vec<TranscriptEntry>,
}
