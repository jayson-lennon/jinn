//! The HTTP wire shapes the web UI exchanges.
//!
//! Three families:
//!
//! - **Action forms** — what an action POST carries: [`SendForm`],
//!   [`CancelForm`], [`ModelForm`], [`PersonaForm`], [`SwitchForm`].
//!   Field-for-field the `application/x-www-form-urlencoded` bodies
//!   the Rust tests, curl, and the e2e bootstrap post; the Datastar
//!   browser posts JSON signal bodies whose keys are the camelCase
//!   aliases each field declares, and the server's dual extractor
//!   accepts either encoding. axum's `Form` extractor (or the dual
//!   wrapper) deserializes them.
//! - [`TranscriptQuery`] — the cursor grammar of the transcript pull:
//!   `?since=N` returns the entries rendered at index `N` and after.
//! - [`SseEnvelope`] — one fan-out push envelope: the region kind it
//!   belongs to, the session it belongs to, and the cursor it
//!   advances. The push seam translates it into a Datastar SDK event;
//!   no client dispatches on a name.

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// `POST /web/send` — deliver text into a session.
///
/// The camelCase aliases are the Datastar browser's JSON signal keys;
/// the snake_case fields are the urlencoded contract.
#[derive(Debug, Clone, Deserialize)]
pub struct SendForm {
    /// The session the text targets.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The text, delivered verbatim.
    pub text: String,
}

/// `POST /web/cancel` — cancel a session's live turn.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelForm {
    /// The session whose turn to cancel.
    #[serde(alias = "sessionId")]
    pub session_id: String,
}

/// `POST /web/model` — switch a session's model.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelForm {
    /// The session to switch.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The model id, as a `Single` selection.
    #[serde(alias = "providerId")]
    pub provider_id: String,
}

/// `POST /web/session-new` — create a session through the shared
/// creation path.
///
/// `project` names a `[[project.entry]]` entry by its configured
/// (raw, `~`-allowed) path string — the form's select renders one
/// option per entry with exactly this value, and the server matches
/// it against the configured list before expanding. `lifecycle` is a
/// `[[session_lifecycle.script]]` name, or the empty string for the
/// blank lifecycle (the form's first row). `args` is the quote-
/// preserving space-delimited text the TUI arg popup parses; the
/// server splits it with the same parser.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct CreateSessionForm {
    /// The chosen project entry's configured path string.
    #[serde(default)]
    pub project: String,
    /// The lifecycle name, or empty for the blank lifecycle.
    #[serde(default)]
    pub lifecycle: String,
    /// The raw args text; absent reads as no args at all.
    #[serde(default)]
    pub args: Option<String>,
}

/// `GET /web/session-new` — the new-session form fragment pull.
///
/// `lifecycle` names the lifecycle the form should render selected
/// and, when its setup declares parameters, render the args input
/// for; absent or empty selects the blank row. The value is echoed
/// back into the fragment, so a re-pull after a lifecycle change
/// shows that lifecycle's form.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SessionNewQuery {
    /// The lifecycle the form renders for; absent or empty → blank.
    #[serde(default)]
    pub lifecycle: Option<String>,
}

/// `POST /web/models/refresh` — the modal's refresh-models action.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsRefreshForm {
    /// The session whose button to re-render in the reply.
    #[serde(alias = "sessionId")]
    pub session_id: String,
}

/// `POST /web/persona` — switch the viewed session's persona.
#[derive(Debug, Clone, Deserialize)]
pub struct PersonaForm {
    /// The session to switch.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The persona name, exactly as a rendered row carries it.
    #[serde(alias = "persona")]
    pub persona: String,
}

/// `POST /web/mcp` — toggle one MCP server's enablement for a session.
///
/// The POST names the server only; the desired state is derived
/// server-side from the session's current enabled set (one XOR per
/// request, mirroring the TUI inspector's `<tab>`), so the wire never
/// carries a desired-state field that could disagree with the state
/// the server actually holds. The response is the re-rendered modal
/// fragment, so the row's mark and badge are correct the moment the
/// response lands.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct McpToggleForm {
    /// The session whose enablement set to toggle.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The server name, exactly as a rendered row carries it (the
    /// `[mcp.<name>]` table key).
    pub server: String,
    /// The client's current filter text, echoed into the response's
    /// re-rendered modal (the stale-fragment rule extends to the
    /// filtered view). Absent or empty renders unfiltered.
    #[serde(default, alias = "filter")]
    pub filter: Option<String>,
}

/// `POST /web/skills/toggle` — toggle one skill's enablement for a
/// session.
///
/// The POST names the skill only; the desired state is derived
/// server-side from the session's current skill filter (a mode-
/// preserving `permit`/`withhold`, mirroring the TUI picker's toggle),
/// so the wire never carries a desired-state field that could
/// disagree with the filter the server actually holds. The response is
/// the re-rendered modal fragment, so the row's mark is correct the
/// moment the response lands. Publishes nothing: skill enablement has
/// no runtime counterpart — the gate reads the session filter live.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SkillToggleForm {
    /// The session whose skill filter to toggle.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The skill name, exactly as a rendered row carries it (the
    /// frontmatter `name`).
    pub skill: String,
    /// The client's current filter text, echoed into the response's
    /// re-rendered modal (the stale-fragment rule extends to the
    /// filtered view). Absent or empty renders unfiltered.
    #[serde(default, alias = "filter")]
    pub filter: Option<String>,
}

/// `POST /web/skills/load` — force-load a skill into a session's
/// context.
///
/// The web mirror of the TUI picker's `<c-l>`: a pinned `skill`
/// tool-call/result pair appends to the session's history, a disabled
/// skill is enabled durably through the filter first, and an
/// already-loaded skill appends the same transient notice the TUI
/// emits instead of a duplicate pair. `MarkSessionInteracted` is the
/// only publish.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SkillLoadForm {
    /// The session whose context to load the skill into.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The skill name, exactly as a rendered row carries it (the
    /// frontmatter `name`).
    pub skill: String,
    /// The client's current filter text, echoed into the response's
    /// re-rendered modal (the stale-fragment rule extends to the
    /// filtered view). Absent or empty renders unfiltered.
    #[serde(default, alias = "filter")]
    pub filter: Option<String>,
}

/// `POST /web/session/{id}/activate` — activate a session.
///
/// The path carries the session id. The urlencoded contract posts an
/// empty body; the Datastar browser posts a signal JSON object, and
/// a unit struct cannot deserialize from one — so the type carries an
/// optional session id the JSON path may name (ignored; the path is
/// authoritative).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SwitchForm {
    /// The JSON body's optional session echo; the path wins.
    #[serde(default, alias = "sessionId")]
    pub session_id: Option<String>,
}

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

/// `POST /web/theme` — set the webui's color mode.
///
/// The body names only the chosen mode (`light`/`dark`/`system`);
/// anything else is a 400 refusal — an unknown value was never
/// rendered by the menu control, so it cannot be accepted. The mode
/// value is the exact vocabulary the CSS `data-theme` selectors and
/// the persisted `AppStateFile::web_theme` share.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ThemeForm {
    /// The chosen mode: `light`, `dark`, or `system`.
    pub theme: String,
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

/// The sessions pull's query: `GET /web/sessions?q=<text>&viewed=<id>`.
///
/// The filter narrows the merged list to the sessions whose titles
/// fuzzy-match `q` — matched server-side with the TUI session picker's
/// matcher, so exactly one matching implementation exists. An absent
/// or empty `q` reads as "no filter"; the full grouped list returns.
///
/// `viewed` names the session the pulling page is viewing — the list's
/// blue row is the *viewed* session, and two pages viewing different
/// sessions each show their own blue, so the viewer travels with the
/// pull instead of the server guessing. Absent or empty reads as "no
/// viewer" (the landing page), which renders no active row at all.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SessionsQuery {
    /// The title fragment sessions must fuzzy-match. Absent or empty →
    /// every session.
    #[serde(default)]
    pub q: Option<String>,
    /// The session id the pulling page views. Absent or empty → the
    /// landing page's no-viewer render: no row carries the active
    /// styling. An unparseable id reads the same way — a page pull
    /// must never 500 because its viewer string went stale.
    #[serde(default, alias = "viewedSession")]
    pub viewed: Option<String>,
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

    /// The viewed session's id, or `None` when nothing is viewed.
    ///
    /// The same "absent and empty are equivalent" rule as
    /// [`Self::filter_text`], plus the stale-viewer rule: an id that
    /// is not a UUID resolves to no viewer rather than an error — the
    /// pull renders a list, not a validation surface.
    #[must_use]
    pub fn viewed_session(&self) -> Option<SessionId> {
        self.viewed
            .as_deref()
            .filter(|text| !text.is_empty())
            .and_then(SessionId::try_from_string)
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
    /// Whether this row is the *viewed* session — the session the
    /// rendering page has open, not the instance's active one. Two
    /// pages viewing different sessions each paint their own row.
    pub active: bool,
    /// Whether the session is a bot run (rows render tinted, and sends
    /// to them are refused).
    pub is_bot: bool,
    /// Whether the session is loaded in memory (vs archived-only).
    pub loaded: bool,
    /// Whether the session carries an unseen settle — work that
    /// finished while the user was viewing another session. Rows
    /// wearing it render marked until their session is viewed (by
    /// activation anywhere in the instance, or by a page opening it).
    pub unread: bool,
    /// Whether the session's live phase is working — a turn is in
    /// flight (streaming, running tools, or cancelling). Rows wearing
    /// it render the working spinner ahead of the title. Derived from
    /// the same [`jinn_slices::PhaseView::is_working`] the TUI
    /// throbber reads, so the two surfaces cannot disagree about busy.
    /// Archived rows never carry it.
    pub working: bool,
    /// Tree depth under this row's parent session — 0 for a root, 1
    /// for its subagent children. The webui renders the picker's tree,
    /// so children indent under the session that spawned them.
    pub depth: usize,
}

/// The kind of SSE push — the event name the client dispatches on.
///
/// No client source tree mirrors [`SseKind::event_name`]: the
/// declarative Datastar attributes in the askama templates carry the
/// ids and routes, and the server-side contract tests hold the
/// vocabulary's integrity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SseKind {
    /// The in-flight turn's rendered tail, wholesale-replaced into the
    /// transcript's streaming container at each flush tick.
    Streaming,
    /// Re-rendered composer controls — the send↔cancel button.
    Controls,
    /// Re-rendered sessions list.
    Sessions,
    /// Signal that the discovered-model cache changed — the model
    /// picker's rows. The envelope is a signal only: an open modal
    /// re-pulls its rows from the picker's GET route (with its current
    /// filter), which remains the correctness dependency.
    Models,
    /// Signal that the MCP runtime cell's status projection changed —
    /// the MCP servers modal's rows. The envelope is a signal only: an
    /// open modal re-pulls its rows from the modal's GET route, which
    /// remains the correctness dependency. Subscribed from
    /// `McpRuntimeStored` (the coordinator's stored event, emitted
    /// strictly after each cell write), never the raw status events —
    /// those have two publishers against one cell writer.
    McpStatus,
    /// Signal that a skills discovery scan reported — the skills
    /// modal's rows. The envelope is a signal only: an open modal
    /// re-pulls its rows from the modal's GET route, which remains
    /// the correctness dependency. Subscribed from `SkillsLoaded`
    /// (the discovery worker's event, broadcast after each scan),
    /// so a modal opened before a slow scan finishes fills itself
    /// without a reload — discovery lands async after the modal can
    /// be opened.
    Skills,
    /// Signal that MCP connections changed — the tools modal's rows,
    /// whose MCP-served tools appear and disappear with the session's
    /// connections. The envelope is a signal only: an open modal
    /// re-pulls its **rows fragment** (never the modal skeleton — the
    /// revert stamp's `data-init` must not re-run mid-open, or the
    /// open-time snapshot the dismiss-revert restores would reset).
    /// Subscribed from `McpRuntimeStored`, the same stored event the
    /// MCP modal's signal rides: connection change is the only thing
    /// that moves a tools list (the registry, provider, and bot gates
    /// are static per session).
    Tools,
    /// Full transcript replace — the convergence push after a turn
    /// settles (or anything else invalidates the incremental cursor).
    TranscriptFull,
}

impl SseKind {
    /// The event name the client's stream listeners dispatch on.
    #[must_use]
    pub const fn event_name(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Controls => "controls",
            Self::Sessions => "sessions",
            Self::Models => "models",
            Self::McpStatus => "mcp-status",
            Self::Skills => "skills",
            Self::Tools => "tools",
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
/// pull exactly where the push left off. A [`SseKind::Streaming`]
/// envelope's cursor is its session's **settled boundary** — the count
/// of entries the settled transcript already rendered when the turn
/// began — never the live history's growing length: a replace-shaped
/// push with a growing cursor reintroduces the reconnect re-append bug
/// class the no-id rule on [`SseKind::TranscriptFull`] fixed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SseEnvelope {
    /// Which swap the payload lands under.
    pub kind: SseKind,
    /// The session the payload belongs to.
    pub session_id: SessionId,
    /// The rendered-entry cursor after this payload (streaming pushes
    /// carry the settled boundary; other kinds carry the current
    /// count).
    pub cursor: usize,
    /// The server-rendered HTML fragment.
    pub html: String,
}

/// A web page opened a session — the arrival fact.
///
/// Published by the session-page render handler (`GET /web/session/{id}`)
/// after the page's HTML is built: the user has put a session on a
/// screen, which is "the user looked" as much as an activation is. The
/// webui fan-out answers it by clearing that session's unread mark —
/// the arrival path that a click's activation already covers, and the
/// path a reload or a deep link (an archived session's page, opened
/// without activating it) has no other way through.
///
/// An Event, and the webui's only self-owned schema: exactly one
/// producer (the page handler) and one handler (the fan-out) declare
/// it, so the broadcast contract holds however the transport resolves.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A web page opened a session.")]
pub struct SessionPageOpened {
    /// The session the page opened.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for SessionPageOpened {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

    use super::{McpToggleForm, SkillLoadForm, SkillToggleForm, SseKind, TranscriptQuery};

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
    #[case(SseKind::Streaming, "streaming")]
    #[case(SseKind::Controls, "controls")]
    #[case(SseKind::Sessions, "sessions")]
    #[case(SseKind::Models, "models")]
    #[case(SseKind::McpStatus, "mcp-status")]
    #[case(SseKind::Skills, "skills")]
    #[case(SseKind::Tools, "tools")]
    #[case(SseKind::TranscriptFull, "transcript-full")]
    fn sse_kind_names_its_event(#[case] kind: SseKind, #[case] expected: &str) {
        assert_eq!(kind.event_name(), expected);
    }

    #[rstest::rstest]
    #[test]
    fn mcp_toggle_form_deserializes_snake_and_camel_session_keys() {
        // Given the urlencoded shape the browser posts (camelCase
        // alias) and the shape curl/tests post (snake_case field).
        let camel: McpToggleForm =
            serde_json::from_str("{\"sessionId\":\"abc\",\"server\":\"excalimate\"}")
                .expect("camelCase parses");
        let snake: McpToggleForm =
            serde_json::from_str("{\"session_id\":\"abc\",\"server\":\"excalimate\"}")
                .expect("snake_case parses");

        // Then both encodings land in the same form — the dual
        // extractor accepts either body shape.
        assert_eq!(camel, snake);
        assert_eq!(camel.session_id, "abc");
        assert_eq!(camel.server, "excalimate");
    }

    #[rstest::rstest]
    #[test]
    fn skill_forms_deserialize_snake_and_camel_session_keys() {
        // Given the urlencoded shape the browser posts (camelCase
        // alias) and the shape curl/tests post (snake_case field), for
        // both the toggle and the load form.
        let toggle_camel: SkillToggleForm =
            serde_json::from_str("{\"sessionId\":\"abc\",\"skill\":\"rust-programming\"}")
                .expect("toggle camelCase parses");
        let toggle_snake: SkillToggleForm =
            serde_json::from_str("{\"session_id\":\"abc\",\"skill\":\"rust-programming\"}")
                .expect("toggle snake_case parses");
        let load_camel: SkillLoadForm =
            serde_json::from_str("{\"sessionId\":\"abc\",\"skill\":\"rust-programming\"}")
                .expect("load camelCase parses");
        let load_snake: SkillLoadForm =
            serde_json::from_str("{\"session_id\":\"abc\",\"skill\":\"rust-programming\"}")
                .expect("load snake_case parses");

        // Then both encodings land in the same form for each — the
        // dual extractor accepts either body shape.
        assert_eq!(toggle_camel, toggle_snake);
        assert_eq!(toggle_camel.skill, "rust-programming");
        assert_eq!(load_camel, load_snake);
        assert_eq!(load_camel.session_id, "abc");
    }
}
