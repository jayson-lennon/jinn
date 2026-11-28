//! The browser → server messages.
//!
//! One enum, one imperative per variant. The server dispatches every
//! frame through a single exhaustive `match`; there is no wildcard arm,
//! so a new variant is a compile error until it has a handler.

use jinn_core_types::{ChatEntryId, SessionId};

use crate::payload::{DetailViewBody, ImageUploadBody, MarketViewBody};
use serde::{Deserialize, Serialize};

/// A message from the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The first frame on every connect: announces the page and names
    /// the newest entry it already holds, if any.
    Hello {
        /// The newest transcript entry the page holds, or `None` on a
        /// fresh load.
        last_entry_id: Option<ChatEntryId>,
        /// The history revision the page last saw, or `None` on a fresh
        /// load. Incremental resume needs both this and the cursor to
        /// match; anything else is a full snapshot.
        revision: Option<u64>,
    },
    /// Ask for one page of entries older than the ones the page holds.
    ///
    /// `before` names the oldest entry the page holds; the page returned
    /// ends just before it. Refused when `before` is not an entry of the
    /// active session or is its first entry.
    MoreEntries {
        /// The oldest entry the page holds.
        before: ChatEntryId,
    },
    /// A dash opened on the page. While any page has one open, the tick
    /// pushes the statistics when they change; nothing is pushed while
    /// none does.
    DashOpen,
    /// The dash on the page closed.
    DashClose,
    /// Load history until `entry_id` is held. The reply streams the page's
    /// own older-entry pages, oldest first, until one includes the entry.
    /// Refused when `entry_id` is not an entry of the active session.
    JumpToEntry {
        /// The entry to scroll the transcript to.
        entry_id: ChatEntryId,
    },
    /// Stop the jump this page asked for. The server stops sending the pages
    /// it has not yet sent; the ones already sent stay. A cancel that arrives
    /// after the jump finished does nothing.
    CancelJump,
    /// Send a message as a fresh turn (rejected while a turn runs).
    Send {
        /// The message text.
        text: String,
        /// The pending images, in chip order. A send may carry images and
        /// no text.
        images: Vec<ImageUploadBody>,
    },
    /// Queue a message behind the running turn.
    Queue {
        /// The message text.
        text: String,
        /// The pending images, in chip order.
        images: Vec<ImageUploadBody>,
    },
    /// Cancel the running turn.
    Cancel,
    /// Make a session the instance's active one.
    ActivateSession {
        /// The session to activate.
        session_id: SessionId,
    },
    /// Switch the active session's model.
    SwitchModel {
        /// The full row id the model picker rendered.
        provider_id: String,
    },
    /// Rename the active session.
    Rename {
        /// The new title.
        title: String,
    },
    /// Ask for the stored-session picker's rows.
    PickerRows {
        /// The title filter (empty for the unfiltered list).
        filter: String,
    },
    /// Ask for the model picker's rows.
    ModelRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Ask for the skills modal's rows for the active session.
    SkillRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Toggle one skill's enablement on the active session, applied at
    /// once to its skill filter; the reply carries the fresh rows.
    ToggleSkill {
        /// The skill's name.
        skill: String,
    },
    /// Load one skill's body into the active session's context as a
    /// pinned tool call/result pair; the reply carries the pair.
    LoadSkill {
        /// The skill's name.
        skill: String,
    },
    /// Ask for the tools modal's rows for the active session.
    ToolRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Toggle one tool's enablement on the active session, committed at
    /// once to its tool filter; the reply carries the fresh rows.
    ToggleTool {
        /// The tool's name.
        tool: String,
    },
    /// Ask for the MCP servers modal's rows for the active session.
    McpRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Toggle one MCP server's enablement on the active session; the reply
    /// carries the fresh rows.
    ToggleMcp {
        /// The server's name.
        server: String,
    },
    /// Ask for the persona modal's rows for the active session.
    PersonaRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Switch the active session's persona.
    SwitchPersona {
        /// The persona's name.
        persona: String,
    },
    /// Pin the entry at its relative position, or unpin it when it is
    /// already pinned. The entry is in the active session, and a pin
    /// takes its whole tool loop — the same command the terminal's `p`
    /// publishes.
    TogglePin {
        /// The entry to toggle.
        entry_id: ChatEntryId,
    },
    /// Flip one entry between forced-include and forced-exclude — the same
    /// override the terminal's `x` resolves to, and never back to the
    /// default. The entry is in the active session and takes its whole
    /// tool loop. A pinned entry is refused: a pin overrides exclusion.
    ToggleContext {
        /// The entry to toggle.
        entry_id: ChatEntryId,
    },
    /// Fork the active session at one entry: a new session holding the
    /// history up to and including it, which becomes the active session.
    /// The entry is in the active session — the same command the
    /// terminal's `f` publishes.
    Fork {
        /// The entry to fork at.
        entry_id: ChatEntryId,
    },
    /// Ask for the applets modal's rows: rescan the saved-app catalog and
    /// wake the applets server, then answer with the rows as they stand.
    /// An open modal receives fresh rows when either lands.
    AppletRows {
        /// The filter (empty for the unfiltered list).
        filter: String,
    },
    /// Ask the provider registry to rediscover its models.
    RefreshModels,
    /// Set the page's color mode.
    SetTheme {
        /// The mode: `light`, `dark`, or `system`.
        theme: String,
    },
    /// Add a session to, or remove it from, the library.
    SetLibrary {
        /// The session.
        session_id: SessionId,
        /// The membership to write.
        in_library: bool,
    },
    /// Archive a session: persisted as archived and removed from memory,
    /// without running its teardown lifecycle.
    ArchiveSession {
        /// The session to archive.
        session_id: SessionId,
    },
    /// Close a session the way the terminal's `x` does: run its teardown
    /// lifecycle, then archive it — or archive it at once when no teardown
    /// is due.
    TeardownSession {
        /// The session to tear down and archive.
        session_id: SessionId,
    },
    /// Ask for the new-session form's choices.
    NewSessionOptions,
    /// Open the OpenRouter model market and answer its rows for `view`.
    /// On an empty catalog this also publishes the models fetch; the rows
    /// land as a push when it does.
    MarketOpen {
        /// The view the rows answer.
        view: MarketViewBody,
    },
    /// Ask for the market's rows in a new view (a filter, sort, or mix
    /// change). Answers with the rows for exactly that view.
    MarketRows {
        /// The view the rows answer.
        view: MarketViewBody,
    },
    /// Ask for one model's endpoint detail. Answers at once from the
    /// cell when the detail is cached, else with `loading` while the
    /// fetch runs; the detail lands as a push when it does.
    MarketDetail {
        /// The full listing id (`openrouter/{author}/{slug}[:variant]`).
        model_id: String,
        /// The sort the endpoint rows answer.
        view: DetailViewBody,
    },
    /// Switch the active session's model to `model_id`, and pin — or
    /// with a `None` tag, clear — its OpenRouter routing endpoint.
    MarketUse {
        /// The full listing id.
        model_id: String,
        /// The routing tag to pin, or `None` for auto-route.
        tag: Option<String>,
    },
    /// Drop every market cache and refetch. The rows land as pushes.
    MarketRefresh,
    /// Create a session through the shared creation path.
    CreateSession {
        /// The chosen project entry's configured path string.
        project: String,
        /// The lifecycle name, or empty for the blank lifecycle.
        lifecycle: String,
        /// The raw args text; empty for none.
        args: String,
    },
}

#[must_use]
/// The variant's wire tag, for tests and logging.
pub const fn tag_of(msg: &ClientMsg) -> &'static str {
    match msg {
        ClientMsg::Hello { .. } => "hello",
        ClientMsg::MoreEntries { .. } => "more_entries",
        ClientMsg::DashOpen => "dash_open",
        ClientMsg::DashClose => "dash_close",
        ClientMsg::JumpToEntry { .. } => "jump_to_entry",
        ClientMsg::CancelJump => "cancel_jump",
        ClientMsg::Send { .. } => "send",
        ClientMsg::Queue { .. } => "queue",
        ClientMsg::Cancel => "cancel",
        ClientMsg::ActivateSession { .. } => "activate_session",
        ClientMsg::SwitchModel { .. } => "switch_model",
        ClientMsg::Rename { .. } => "rename",
        ClientMsg::PickerRows { .. } => "picker_rows",
        ClientMsg::ModelRows { .. } => "model_rows",
        ClientMsg::SkillRows { .. } => "skill_rows",
        ClientMsg::ToggleSkill { .. } => "toggle_skill",
        ClientMsg::LoadSkill { .. } => "load_skill",
        ClientMsg::ToolRows { .. } => "tool_rows",
        ClientMsg::ToggleTool { .. } => "toggle_tool",
        ClientMsg::McpRows { .. } => "mcp_rows",
        ClientMsg::ToggleMcp { .. } => "toggle_mcp",
        ClientMsg::PersonaRows { .. } => "persona_rows",
        ClientMsg::SwitchPersona { .. } => "switch_persona",
        ClientMsg::TogglePin { .. } => "toggle_pin",
        ClientMsg::ToggleContext { .. } => "toggle_context",
        ClientMsg::Fork { .. } => "fork",
        ClientMsg::AppletRows { .. } => "applet_rows",
        ClientMsg::RefreshModels => "refresh_models",
        ClientMsg::SetTheme { .. } => "set_theme",
        ClientMsg::SetLibrary { .. } => "set_library",
        ClientMsg::ArchiveSession { .. } => "archive_session",
        ClientMsg::TeardownSession { .. } => "teardown_session",
        ClientMsg::NewSessionOptions => "new_session_options",
        ClientMsg::CreateSession { .. } => "create_session",
        ClientMsg::MarketOpen { .. } => "market_open",
        ClientMsg::MarketRows { .. } => "market_rows",
        ClientMsg::MarketDetail { .. } => "market_detail",
        ClientMsg::MarketUse { .. } => "market_use",
        ClientMsg::MarketRefresh => "market_refresh",
    }
}
