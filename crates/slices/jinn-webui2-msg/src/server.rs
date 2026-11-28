//! The server → browser messages.
//!
//! One enum, one noun or past-tense fact per variant. The browser
//! dispatches every frame through a single exhaustive switch; a new
//! variant is a compile error until it has a handler.

use crate::payload::{
    AppletRowsBody, ContextUsageBody, CwdBody, CwdCheckBody, DashMcpRowsBody, DashPinRowsBody,
    DashTasksBody, EntriesPageBody, EntryDeltaBody, EntryUpsertBody, ErrorBody, MarketDetailBody,
    MarketRowsBody, MarketStaleBody, McpRowsBody, ModelBody, ModelRowsBody, NewSessionOptionsBody,
    PersonaRowsBody, PhaseBody, PickerRowsBody, PromptTemplatesBody, PushInfoBody,
    SessionStatsBody, SessionsBody, SkillRowsBody, SnapshotBody, ThemeBody, TitleBody,
    ToolRowsBody,
};
use serde::{Deserialize, Serialize};

/// A message from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The whole state, on a fresh connect or whenever the server cannot
    /// determine what to send incrementally.
    ///
    /// Boxed, like `EntryUpsert`: this is one of the two heaviest frames,
    /// and every broadcast clones the enum at the size of its largest
    /// variant. Boxing keeps a one-entry delta as cheap to send as it
    /// looks. Serde is transparent over the `Box`, so the wire is
    /// unchanged.
    Snapshot(Box<SnapshotBody>),
    /// A whole entry, added or rewritten.
    ///
    /// Boxed: a `ChatEntry` is large on its own, and this is the heaviest
    /// frame on the wire (see `Snapshot` for why that matters).
    EntryUpsert(Box<EntryUpsertBody>),
    /// One page of older entries, answering a `MoreEntries` request.
    ///
    /// Boxed, like `Snapshot`: a page is one of the heaviest frames, and
    /// every broadcast clones the enum at the size of its largest variant.
    EntriesPage(Box<EntriesPageBody>),
    /// A text chunk extending one streamed field.
    EntryDelta(EntryDeltaBody),
    /// The session's phase changed.
    Phase(PhaseBody),
    /// The session list changed.
    Sessions(SessionsBody),
    /// The current model changed.
    Model(ModelBody),
    /// The session's title changed.
    Title(TitleBody),
    /// The stored-session picker's rows.
    PickerRows(PickerRowsBody),
    /// The model picker's rows.
    ModelRows(ModelRowsBody),
    /// The skills modal's rows for one session.
    SkillRows(SkillRowsBody),
    /// The tools modal's rows for one session.
    ToolRows(ToolRowsBody),
    /// The MCP servers modal's rows for one session.
    McpRows(McpRowsBody),
    /// The persona modal's rows for one session.
    PersonaRows(PersonaRowsBody),
    /// The applets modal's rows, with the applets server's serving phase.
    AppletRows(AppletRowsBody),
    /// A session's prompt templates, after its scan finished. An open `#`
    /// popup re-filters against the new list in place.
    PromptTemplates(PromptTemplatesBody),
    /// The new-session form's choices.
    NewSessionOptions(NewSessionOptionsBody),
    /// The page's color mode changed.
    Theme(ThemeBody),
    /// The session's persona and context usage figure, when either changed.
    ContextUsage(ContextUsageBody),
    /// The dash's session statistics, pushed only while a dash is open and
    /// only when a displayed value changed.
    SessionStats(Box<SessionStatsBody>),
    /// The dash's pinned rows, when the session's pins changed.
    DashPinRows(Box<DashPinRowsBody>),
    /// The dash's task list, when the session's list changed.
    DashTasks(Box<DashTasksBody>),
    /// The dash's enabled MCP servers and their live status.
    DashMcpRows(DashMcpRowsBody),
    /// The answer to a `CheckCwd`: the verdict for exactly the text the
    /// page asked about, resolved the way the terminal's cwd popup
    /// resolves it.
    CwdCheck(CwdCheckBody),
    /// A session's working directory changed — from the page, the
    /// terminal, or another page. The dash's directory row follows it.
    SessionCwd(CwdBody),
    /// The model market's rows for the view the page last asked for.
    MarketRows(MarketRowsBody),
    /// One model's endpoint detail.
    MarketDetail(MarketDetailBody),
    /// A market refresh step failed; stale data stands.
    MarketStale(MarketStaleBody),
    /// What the page needs to subscribe to push: whether the server can
    /// send pushes, and the key it signs them with. Sent as its own frame
    /// after every `hello` reply, and nowhere else — the key pair never
    /// changes while the server runs.
    PushInfo(PushInfoBody),
    /// A request was refused.
    Error(ErrorBody),
    /// A `hello` answered with nothing to send: the page's cursor is the
    /// newest entry at the current revision, so it already holds
    /// everything.
    ///
    /// Carries nothing. It is sent explicitly rather than as an empty
    /// reply because the page cannot tell a server that answered with
    /// nothing from one that never answered at all — silence tripped its
    /// handshake wait and made it close a healthy connection.
    Current,
    /// A heartbeat: the server is alive and the connection is still open.
    ///
    /// Carries nothing. It exists so an idle connection still has traffic
    /// — a browser cannot see a dropped connection until a frame fails to
    /// arrive, so the page treats a silent connection as a dead one and
    /// reconnects.
    Ping,
}

#[must_use]
/// The variant's wire tag, for tests and logging.
pub const fn tag_of(msg: &ServerMsg) -> &'static str {
    match msg {
        ServerMsg::Snapshot(_) => "snapshot",
        ServerMsg::EntryUpsert(_) => "entry_upsert",
        ServerMsg::EntriesPage(_) => "entries_page",
        ServerMsg::EntryDelta(_) => "entry_delta",
        ServerMsg::Phase(_) => "phase",
        ServerMsg::Sessions(_) => "sessions",
        ServerMsg::Model(_) => "model",
        ServerMsg::Title(_) => "title",
        ServerMsg::PickerRows(_) => "picker_rows",
        ServerMsg::ModelRows(_) => "model_rows",
        ServerMsg::SkillRows(_) => "skill_rows",
        ServerMsg::ToolRows(_) => "tool_rows",
        ServerMsg::McpRows(_) => "mcp_rows",
        ServerMsg::PersonaRows(_) => "persona_rows",
        ServerMsg::AppletRows(_) => "applet_rows",
        ServerMsg::PromptTemplates(_) => "prompt_templates",
        ServerMsg::NewSessionOptions(_) => "new_session_options",
        ServerMsg::Theme(_) => "theme",
        ServerMsg::ContextUsage(_) => "context_usage",
        ServerMsg::SessionStats(_) => "session_stats",
        ServerMsg::DashPinRows(_) => "dash_pin_rows",
        ServerMsg::DashTasks(_) => "dash_tasks",
        ServerMsg::DashMcpRows(_) => "dash_mcp_rows",
        ServerMsg::CwdCheck(_) => "cwd_check",
        ServerMsg::SessionCwd(_) => "session_cwd",
        ServerMsg::MarketRows(_) => "market_rows",
        ServerMsg::MarketDetail(_) => "market_detail",
        ServerMsg::MarketStale(_) => "market_stale",
        ServerMsg::PushInfo(_) => "push_info",
        ServerMsg::Error(_) => "error",
        ServerMsg::Current => "current",
        ServerMsg::Ping => "ping",
    }
}
