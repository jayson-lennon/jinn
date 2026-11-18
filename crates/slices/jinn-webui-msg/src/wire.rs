//! The HTTP wire shapes the web UI exchanges.
//!
//! Three families:
//!
//! - **Action forms** — what the browser posts: [`SendForm`],
//!   [`CancelForm`], [`ModelForm`], [`SwitchForm`]. Field-for-field the
//!   `application/x-www-form-urlencoded` bodies the web client's
//!   `api.ts` posts; axum's `Form` extractor deserializes them.
//! - [`TranscriptQuery`] — the cursor grammar of the transcript pull:
//!   `?since=N` returns the entries rendered at index `N` and after.
//! - [`SseEnvelope`] — one SSE push: the event name the client
//!   dispatches on (mirrored by the TypeScript `wire.ts`), the session
//!   it belongs to, and the cursor the client should acknowledge (it
//!   rides the SSE `id:` line and returns as `Last-Event-ID` on
//!   reconnect).

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// `POST /web/send` — deliver text into a session.
#[derive(Debug, Clone, Deserialize)]
pub struct SendForm {
    /// The session the text targets.
    pub session_id: String,
    /// The text, delivered verbatim.
    pub text: String,
}

/// `POST /web/cancel` — cancel a session's live turn.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelForm {
    /// The session whose turn to cancel.
    pub session_id: String,
}

/// `POST /web/model` — switch a session's model.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelForm {
    /// The session to switch.
    pub session_id: String,
    /// The model id, as a `Single` selection.
    pub provider_id: String,
}

/// `POST /web/session/{id}/activate` — activate a session.
///
/// The path carries the session id, so the form body is empty; the
/// type exists so the handler can accept (and ignore) an
/// `application/x-www-form-urlencoded` body uniformly with the other
/// actions.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SwitchForm;

/// `POST /web/session/{id}/rename` — rename a session.
///
/// The path carries the session id (the activate precedent); the body
/// names only the new title.
#[derive(Debug, Clone, Deserialize)]
pub struct RenameForm {
    /// The new title, applied trimmed (an empty-after-trim value is a
    /// 400 refusal, mirroring the TUI rename's own validation).
    pub title: String,
}

/// The transcript pull's query: `GET /web/transcript/{id}?since=N`.
///
/// The cursor is an index into the session's rendered entry sequence.
/// An absent or unparseable `since` reads as "from the start"; a
/// `since` at or past the entry count also resolves to a full render,
/// because history can shrink under a rewind-retry and the client
/// must re-sync rather than receive nothing.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct TranscriptQuery {
    /// Render entries from this index onward. Absent → the whole
    /// transcript.
    #[serde(default)]
    pub since: Option<usize>,
}

impl TranscriptQuery {
    /// The cursor the pull starts at, after the shrink-re-read rule.
    ///
    /// `Some(cursor)` when the client names a position strictly inside
    /// the current entry count; `None` (full render) otherwise. The
    /// length check lives here so every caller applies the same rule.
    #[must_use]
    pub fn cursor_within(&self, entry_count: usize) -> Option<usize> {
        match self.since {
            Some(cursor) if cursor < entry_count => Some(cursor),
            _ => None,
        }
    }
}

/// The sessions pull's query: `GET /web/sessions?q=<text>`.
///
/// The filter narrows the merged list to the sessions whose titles
/// fuzzy-match `q` — matched server-side with the TUI session picker's
/// matcher, so exactly one matching implementation exists. An absent
/// or empty `q` reads as "no filter"; the full grouped list returns.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SessionsQuery {
    /// The title fragment sessions must fuzzy-match. Absent or empty →
    /// every session.
    #[serde(default)]
    pub q: Option<String>,
}

impl SessionsQuery {
    /// The filter text, or `None` when no filter is in force.
    ///
    /// The empty-string check lives here so every caller applies the
    /// same "absent and empty are equivalent" rule — the matcher's
    /// empty-pattern behavior never defines the unfiltered case.
    #[must_use]
    pub fn filter_text(&self) -> Option<&str> {
        self.q.as_deref().filter(|text| !text.is_empty())
    }
}

/// One session row of the sessions list fragment.
///
/// The merge of what `AppState` knows about live sessions and what the
/// store knows about archived ones — one row shape for both, so the
/// template has one list to render.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionListItem {
    /// The session's id.
    pub session_id: SessionId,
    /// Display title; the store's untitled fallback when empty.
    pub title: String,
    /// Whether this row is the instance's active session.
    pub active: bool,
    /// Whether the session is a bot run (rows render tinted, and sends
    /// to them are refused).
    pub is_bot: bool,
    /// Whether the session is loaded in memory (vs archived-only).
    pub loaded: bool,
    /// Tree depth under this row's parent session — 0 for a root, 1
    /// for its subagent children. The webui renders the picker's tree,
    /// so children indent under the session that spawned them.
    pub depth: usize,
}

/// The kind of SSE push — the event name the client dispatches on.
/// The TypeScript mirror of [`SseKind::event_name`] lives in
/// `webui/client/src/wire.ts`, and the drift test in this crate's
/// `wire_tests` (plus the webui crate's `contract_tests`) fails when
/// the two sides disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SseKind {
    /// Appended transcript HTML (`sse-swap="chat"`, `beforeend`).
    Chat,
    /// Re-rendered composer controls — the send↔cancel button.
    Controls,
    /// Re-rendered sessions list.
    Sessions,
    /// Full transcript replace — the convergence push after a turn
    /// settles (or anything else invalidates the incremental cursor).
    TranscriptFull,
}

impl SseKind {
    /// The event name the client's stream listeners dispatch on.
    #[must_use]
    pub const fn event_name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Controls => "controls",
            Self::Sessions => "sessions",
            Self::TranscriptFull => "transcript-full",
        }
    }
}

/// One SSE push, serialized per line into the response stream.
///
/// `session_id` is the filter key: a connection subscribed to one
/// session never receives another session's fragment. `cursor` is the
/// rendered-entry index the payload advances the client to — it rides
/// the SSE `id:` line, so a reconnect's `Last-Event-ID` resumes the
/// pull exactly where the push left off.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SseEnvelope {
    /// Which swap the payload lands under.
    pub kind: SseKind,
    /// The session the payload belongs to.
    pub session_id: SessionId,
    /// The rendered-entry cursor after this payload (chat pushes only;
    /// other kinds carry the current count).
    pub cursor: usize,
    /// The server-rendered HTML fragment.
    pub html: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::SseKind;
    use super::TranscriptQuery;

    #[rstest::rstest]
    #[test]
    fn absent_since_resolves_to_a_full_render() {
        // Given a query with no `since`.
        let query = TranscriptQuery { since: None };

        // When resolving the cursor against any entry count.
        let cursor = query.cursor_within(7);

        // Then there is no cursor: the pull renders everything.
        assert_eq!(cursor, None);
    }

    #[rstest::rstest]
    #[test]
    fn cursor_inside_the_history_starts_there() {
        // Given a query pointing into a seven-entry history.
        let query = TranscriptQuery { since: Some(3) };

        // When resolving against that history's length.
        let cursor = query.cursor_within(7);

        // Then the pull starts at the named index.
        assert_eq!(cursor, Some(3));
    }

    #[rstest::rstest]
    #[test]
    fn cursor_at_or_past_the_length_rereads_everything() {
        // Given a query whose cursor names the end of a shrunken history.
        let query = TranscriptQuery { since: Some(7) };

        // When resolving against a history that now holds fewer entries.
        let cursor = query.cursor_within(5);

        // Then the pull re-syncs with a full render.
        assert_eq!(cursor, None);
    }

    #[rstest::rstest]
    #[case(SseKind::Chat, "chat")]
    #[case(SseKind::Controls, "controls")]
    #[case(SseKind::Sessions, "sessions")]
    #[case(SseKind::TranscriptFull, "transcript-full")]
    fn sse_kind_names_its_event(#[case] kind: SseKind, #[case] expected: &str) {
        assert_eq!(kind.event_name(), expected);
    }
}
