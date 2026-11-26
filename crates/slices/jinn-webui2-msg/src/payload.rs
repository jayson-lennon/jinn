//! The payload types carried by the two message enums.
//!
//! Every field is plain data the browser can render without another round
//! trip: entries ride as a wire projection of the core `ChatEntry` (its
//! `kind` tag intact), the phase as the core `PhaseView`, and ids as the
//! core newtypes.

use jinn_core_types::{ChatEntry, ChatEntryId, SessionId};
use jinn_session_store_msg::SessionState;
use serde::{Deserialize, Serialize};

/// The most entries a snapshot's transcript tail, or one page of older
/// entries, carries.
pub const TAIL_MAX_ENTRIES: usize = 200;

/// The most wire bytes a snapshot's transcript tail, or one page of older
/// entries, carries — whichever limit is hit first ends the page.
pub const TAIL_MAX_BYTES: usize = 1_048_576;

/// Which entry field an [`EntryDeltaBody`]'s chunk extends.
///
/// Exactly the four fields the session grows incrementally: assistant
/// text, reasoning text, a tool call's argument JSON, and a tool result's
/// output. Every other entry change is a whole-entry
/// [`EntryUpsertBody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DeltaField {
    /// The assistant entry's text.
    Text,
    /// The thinking entry's text.
    Thinking,
    /// A tool-call entry's argument JSON.
    Args,
    /// A tool-result entry's output.
    Output,
}

/// One row of the session list the sidebar shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRowBody {
    /// The session's id.
    pub session_id: SessionId,
    /// The session's title (the "Untitled Session" fallback is the
    /// browser's to render).
    pub title: String,
    /// Whether this session is the instance's active one.
    pub is_active: bool,
    /// Whether a turn is running (any phase other than idle).
    pub is_busy: bool,
    /// Whether the session is a library member. A member is listed under
    /// the Library view and hidden from the Sessions view.
    pub in_library: bool,
}

/// The whole state a page needs on a fresh connect: the active session,
/// its phase, model and title, its transcript, and the session list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotBody {
    /// The protocol version the server speaks.
    pub protocol: u16,
    /// The instance's active session, or `None` when none is active.
    pub session_id: Option<SessionId>,
    /// The active session's phase (`Idle` when no session is active).
    pub phase: PhaseBody,
    /// The active session's current model label (empty when none).
    pub model_label: String,
    /// The active session's title (empty when no session is active).
    pub title: String,
    /// The active session's history revision. The page echoes it back
    /// in `Hello` so the server can tell a safe incremental resume from
    /// one that needs the whole transcript again.
    pub revision: u64,
    /// The newest part of the active session's transcript, in order: at
    /// most [`TAIL_MAX_ENTRIES`] entries or [`TAIL_MAX_BYTES`] of wire
    /// bytes, whichever comes first. Older entries load a page at a time
    /// through `MoreEntries`.
    pub entries: Vec<EntryEnvelope>,
    /// How many entries precede the oldest one carried — how much older
    /// history a scroll to the top can still load.
    pub entries_before: usize,
    /// The loaded sessions the sidebar shows.
    pub sessions: Vec<SessionRowBody>,
    /// The library members the sidebar's Library view shows.
    pub library: Vec<SessionRowBody>,
    /// The persisted color mode — `light`, `dark`, or `system` — applied
    /// before the transcript's first paint.
    pub theme: String,
}

/// One transcript entry on the wire.
///
/// The core entry's own serde form is the session store's **persisted**
/// format, and that format is deliberately lossy in one direction: a
/// `Transient` entry is never persisted, so its deserializer has no arm
/// for it and would read it back as `System`. The browser needs the kind
/// to survive, so the envelope names it explicitly with the core's own
/// `kind_str` vocabulary (`snake_case`) and the browser dispatches on
/// that name — never on the persisted map's tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryEnvelope {
    /// The kind's stable snake_case name, from the core `kind_str`.
    pub kind: String,
    /// The entry, in its wire projection (see [`wire_entry`]).
    pub entry: WireEntry,
}

impl EntryEnvelope {
    /// Wraps an entry, deriving its kind name from the core vocabulary and
    /// carrying its wire projection.
    #[must_use]
    pub fn new(entry: &ChatEntry) -> Self {
        Self {
            kind: entry.kind_str().to_owned(),
            entry: WireEntry::project(entry),
        }
    }

    /// The entry's id, read back out of the projection.
    ///
    /// A projected entry always carries its id: it is the persisted serde
    /// form, which has one for every kind. The fallback below is
    /// unreachable for an envelope this crate built, and is a nil id
    /// rather than a panic so a hand-built frame can still be inspected.
    #[must_use]
    pub fn id(&self) -> ChatEntryId {
        self.entry
            .0
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(ChatEntryId::try_from_string)
            .unwrap_or_default()
    }

    /// The entry's primary text — what a user or assistant entry says, or
    /// a tool result's (truncated) output. Empty for kinds with none.
    #[must_use]
    pub fn entry_text(&self) -> &str {
        self.entry
            .0
            .get("kind")
            .and_then(|kind| kind.as_object())
            .and_then(|kind| kind.values().next())
            .and_then(|body| {
                body.get("display")
                    .or_else(|| body.get("text"))
                    .or_else(|| body.get("content"))
                    .or_else(|| body.get("expanded"))
            })
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    }
}

/// The entry as the browser needs it: the persisted serde form with two
/// heavy fields removed — see [`wire_entry`] for which and why.
///
/// A newtype over the projected JSON rather than a bare `ChatEntry`, so
/// the projection cannot be bypassed by constructing an envelope from the
/// raw entry. It serializes as the JSON it holds (transparent), and
/// deserializes from the same shape, so every frame round-trips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEntry(serde_json::Value);

impl WireEntry {
    /// Projects an entry for the wire.
    #[must_use]
    pub fn project(entry: &ChatEntry) -> Self {
        Self(wire_entry(entry))
    }

    /// The projected entry as JSON, for a size measurement or a test.
    #[must_use]
    pub fn as_json(&self) -> &serde_json::Value {
        &self.0
    }
}

impl Serialize for WireEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WireEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(serde_json::Value::deserialize(deserializer)?))
    }
}

/// Projects one entry onto the `/web2` wire: the entry's persisted serde
/// form minus the two fields the browser never reads from the socket.
///
/// - `ToolResult.full_content` — a tool result's full untruncated output,
///   up to hundreds of kilobytes the page only shows inside a collapsed
///   disclosure. The page fetches it on demand from the tool-output route
///   when the disclosure opens; `truncation` stays, because its presence
///   is how the page knows there is a full output to fetch.
/// - `attachments[*].data` — an attachment's raw bytes, on a user entry or
///   a tool result alike. The page fetches each image from the attachment
///   route, because an `<img>` needs an address; the bytes on the socket
///   were dead weight, serialized as a JSON number array.
///
/// The projection is wire-only. The core types' serde form is the session
/// store's on-disk format and is never changed by this; the strip happens
/// in the JSON tree after serialization.
#[must_use]
pub fn wire_entry(entry: &ChatEntry) -> serde_json::Value {
    let mut value = serde_json::to_value(entry).unwrap_or(serde_json::Value::Null);
    let Some(kind) = value
        .get_mut("kind")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return value;
    };
    for body in kind
        .values_mut()
        .filter_map(serde_json::Value::as_object_mut)
    {
        if body.get("full_content").is_some() {
            body.remove("full_content");
        }
        if let Some(attachments) = body
            .get_mut("attachments")
            .and_then(serde_json::Value::as_array_mut)
        {
            for attachment in attachments
                .iter_mut()
                .filter_map(serde_json::Value::as_object_mut)
            {
                attachment.remove("data");
            }
        }
    }
    value
}

/// A whole entry, placed at its position in the transcript.
///
/// Sent on add and on any non-streaming rewrite (a tool result's status,
/// a model stamp, a context override). The browser replaces whatever it
/// holds at that id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryUpsertBody {
    /// The session the entry belongs to, so a page that has switched
    /// sessions can discard a late frame.
    pub session_id: SessionId,
    /// The entry's index in the session's history.
    pub index: usize,
    /// The entry.
    pub entry: EntryEnvelope,
}

/// One page of older entries, answering a `MoreEntries` request.
///
/// The entries end just before the entry the page named, in history
/// order, bounded by the same 200-entries-or-1-MB rule as a snapshot's
/// tail. `entries_before` counts what precedes the oldest entry in *this*
/// page, so the browser knows whether another scroll can load more.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntriesPageBody {
    /// The session the entries belong to: a page that has switched
    /// sessions drops the frame.
    pub session_id: SessionId,
    /// The older entries, in history order.
    pub entries: Vec<EntryEnvelope>,
    /// How many entries precede the oldest one carried.
    pub entries_before: usize,
}

/// A text chunk extending one of the four streamed entry fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryDeltaBody {
    /// The session the entry belongs to (see [`EntryUpsertBody::session_id`]).
    pub session_id: SessionId,
    /// The entry the chunk extends.
    pub entry_id: ChatEntryId,
    /// Which field the chunk extends.
    pub field: DeltaField,
    /// The text to append.
    pub chunk: String,
}

/// The session's phase, as the UI reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseBody {
    /// The session the phase belongs to.
    pub session_id: SessionId,
    /// The phase.
    pub phase: jinn_slices::PhaseView,
    /// How many messages are waiting behind the running turn. The page
    /// shows it so a queued message is not simply gone: a queued entry
    /// does not enter the transcript until its turn runs.
    pub queue_len: usize,
}

/// The current model label, corrected by the server when a switch lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBody {
    /// The session the model belongs to.
    pub session_id: SessionId,
    /// The human-readable model label.
    pub label: String,
}

/// The session's title, after a rename lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TitleBody {
    /// The session that was renamed.
    pub session_id: SessionId,
    /// The new title.
    pub title: String,
}

/// One row of the stored-session picker.
///
/// Richer than a sidebar row: the picker shows sessions that are not
/// loaded, so it carries the stored state, the parent, and the project.
/// The rows are a tree in display order: each names its `depth` so the
/// browser can indent a forked session under its parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRowBody {
    /// The session.
    pub session_id: SessionId,
    /// Its stored title.
    pub title: String,
    /// Whether it is loaded in memory or only archived on disk.
    pub state: SessionState,
    /// The session this one was forked from, when it was.
    pub parent_id: Option<SessionId>,
    /// The project directory's display name, when the session has one.
    pub project: Option<String>,
    /// How deep the row sits in the visible tree: 0 for a root, one more
    /// for each level under a parent.
    pub depth: usize,
    /// When the session was last modified, as the store holds it. The
    /// browser formats it in its own zone.
    pub updated_at: jiff::Timestamp,
    /// The title, split into matched and plain runs for the filter.
    pub title_runs: Vec<TextRunBody>,
}

/// The stored-session picker's rows for one filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRowsBody {
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// At most fifty rows.
    pub rows: Vec<PickerRowBody>,
}

/// The rows the model picker shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRowsBody {
    /// The session whose models these are. An open picker drops rows for
    /// a session the page has left — the same reason [`SkillRowsBody`]
    /// carries one.
    pub session_id: SessionId,
    /// The filter the rows answer.
    pub filter: String,
    /// The rows, available first, the active model first among them.
    pub rows: Vec<ModelRowBody>,
}

/// The loaded sessions the sidebar shows, re-sent whenever the list or a
/// row's phase changes.
///
/// A named struct rather than a bare `Vec` inside the tagged enum: an
/// internally tagged variant has nowhere to put its `type` tag when the
/// payload is a sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsBody {
    /// The rows, in the sidebar's order. Library members are excluded.
    pub rows: Vec<SessionRowBody>,
    /// The library members, in the Library view's order.
    pub library: Vec<SessionRowBody>,
}

/// A refused request, with the reason the browser shows the user.
///
/// A named struct for the same reason as [`SessionsBody`]: a bare string
/// cannot carry the internal `type` tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Why the request was refused.
    pub message: String,
}

/// One row of the model picker.
///
/// The model and provider names arrive split into matched and plain runs
/// so the browser can highlight what the filter hit — the same split the
/// terminal picker draws, from the same shared filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRowBody {
    /// The full row id — what a `SwitchModel` sends back.
    pub provider_id: String,
    /// The model identifier, the row's primary text.
    pub model: String,
    /// The provider block name.
    pub provider_name: String,
    /// Whether the provider is usable (a key is present or unneeded).
    pub is_available: bool,
    /// Whether this row is the session's current model.
    pub is_active: bool,
    /// The model name, split into runs the filter matched and plain runs.
    pub model_runs: Vec<TextRunBody>,
    /// The provider name, split the same way.
    pub provider_runs: Vec<TextRunBody>,
}

/// One run of a skill row's text: a stretch the filter matched, or did
/// not. The browser renders the matched runs highlighted and the rest
/// plain; the split is the shared filter's, cut on grapheme boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRunBody {
    /// The run's text.
    pub text: String,
    /// Whether the filter matched this run.
    pub matched: bool,
}

/// One row of the skills modal.
///
/// Everything the browser needs to draw the row and decide what a click
/// does: whether the skill is enabled (the marker), whether it is already
/// loaded into context (a loaded skill cannot be toggled), and where it
/// was discovered from (only a project-scoped skill shows a badge). The
/// skill's body is deliberately absent — the browser never needs it, and
/// a load pins it server-side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRowBody {
    /// The skill's name — what a `ToggleSkill` or `LoadSkill` sends back.
    pub name: String,
    /// The skill's description.
    pub description: String,
    /// Whether the skill passes the session's skill filter.
    pub enabled: bool,
    /// Whether the skill's body is already loaded into the session's context.
    pub loaded: bool,
    /// Whether the skill was discovered from a project-local directory,
    /// as opposed to the user's global skills directory.
    pub is_project: bool,
    /// The name, split into matched and plain runs for the filter the rows
    /// answer. One plain run when the filter is empty.
    pub name_runs: Vec<TextRunBody>,
    /// The description, split the same way.
    pub description_runs: Vec<TextRunBody>,
}

/// The skills modal's rows for one filter.
///
/// Carries the session the rows describe: an open modal must ignore rows
/// for a session the page has left. The filter is echoed so a page that
/// has typed past it can drop the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRowsBody {
    /// The session whose skills these are.
    pub session_id: SessionId,
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// The rows, sorted by skill name, filtered by the shared filter.
    pub rows: Vec<SkillRowBody>,
}

/// One row of the tools modal.
///
/// Everything the browser needs to draw the row and decide what a click
/// does. `enabled` is the session's live tool filter read at row build —
/// the same predicate the prompt's gate applies — so the mark the page
/// shows is the truth the model sees. The description is the definition's
/// first sentence, the scannable form the shared row builder truncates
/// to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRowBody {
    /// The tool's name — what a `ToggleTool` sends back.
    pub name: String,
    /// The description's first sentence.
    pub description: String,
    /// Whether the session's tool filter currently permits the tool.
    pub enabled: bool,
    /// The name, split into matched and plain runs for the filter the rows
    /// answer. One plain run when the filter is empty.
    pub name_runs: Vec<TextRunBody>,
    /// The description, split the same way.
    pub description_runs: Vec<TextRunBody>,
}

/// The tools modal's rows for one filter.
///
/// Carries the session and echoes the filter for the same reasons
/// [`SkillRowsBody`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRowsBody {
    /// The session whose tools these are.
    pub session_id: SessionId,
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// The rows, sorted case-insensitively by name, filtered by the shared
    /// filter.
    pub rows: Vec<ToolRowBody>,
}

/// One row of the applets modal.
///
/// Everything the browser needs to draw the row and decide what a click
/// does. `href` is the saved applet's URL on the applets server while it
/// is serving — `http://<bind>/applets/<id>/`, opening a new tab — and
/// `None` while the server is offline or the id fails the validity gate,
/// in which case the row renders disabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppletRowBody {
    /// The applet's directory id — its URL path segment.
    pub id: String,
    /// The applet's display title.
    pub title: String,
    /// The title, split into matched and plain runs for the filter the rows
    /// answer. One plain run when the filter is empty.
    pub title_runs: Vec<TextRunBody>,
    /// Where the row opens, or `None` for a disabled row.
    pub href: Option<String>,
}

/// The applets modal's rows for one filter, with the server's serving phase.
///
/// The applets catalog is instance-wide, so unlike the other rows bodies
/// this one names no session: a frame answers whichever page asked, and the
/// browser matches it to the filter it is holding. `bind` is the resolved
/// `host:port` while `serving`, and empty while offline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppletRowsBody {
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// Whether the applets server is bound and serving.
    pub serving: bool,
    /// The resolved bind the applets server accepted; empty while offline.
    pub bind: String,
    /// The rows, newest first, filtered by the shared fuzzy filter.
    pub rows: Vec<AppletRowBody>,
}

/// The display state of one MCP server row's status badge.
///
/// The shared derivation's four states, as the wire carries them. The
/// browser renders the word; it never derives the badge itself, because a
/// disabled server is `off` no matter what its connection last reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpBadgeBody {
    /// Disabled for the session — no connection wanted.
    Off,
    /// Enabled and coming up, or not yet reported.
    Starting,
    /// Enabled, live connection.
    Running,
    /// Enabled; the connection failed or was torn down.
    Dead,
}

/// One row of the MCP servers modal.
///
/// The name, the launch summary, the session's enablement mark, and the
/// badge the shared derivation produced from the runtime cell's effective
/// status projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRowBody {
    /// The server name — the `[mcp.<name>]` table key, and what a
    /// `ToggleMcp` sends back.
    pub name: String,
    /// The launch summary (command and args).
    pub description: String,
    /// Whether the session has this server enabled.
    pub enabled: bool,
    /// The status badge the row shows.
    pub badge: McpBadgeBody,
    /// The name, split into matched and plain runs for the filter the rows
    /// answer. One plain run when the filter is empty.
    pub name_runs: Vec<TextRunBody>,
    /// The description, split the same way.
    pub description_runs: Vec<TextRunBody>,
}

/// The MCP servers modal's rows for one filter.
///
/// Carries the session and echoes the filter for the same reasons
/// [`SkillRowsBody`] does. A push after a runtime-cell write answers the
/// filter that session's modal last asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRowsBody {
    /// The session whose MCP servers these are.
    pub session_id: SessionId,
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// The rows, sorted case-insensitively by name, filtered by the shared
    /// filter.
    pub rows: Vec<McpRowBody>,
}

/// One row of the persona modal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaRowBody {
    /// The persona's name — what a `SwitchPersona` sends back.
    pub name: String,
    /// Short description shown after the name.
    pub description: String,
    /// Whether this row is the session's current persona.
    pub is_active: bool,
    /// The name, split into matched and plain runs for the filter the rows
    /// answer. One plain run when the filter is empty.
    pub name_runs: Vec<TextRunBody>,
    /// The description, split the same way.
    pub description_runs: Vec<TextRunBody>,
}

/// The persona modal's rows for one filter.
///
/// Carries the session and echoes the filter for the same reasons
/// [`SkillRowsBody`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaRowsBody {
    /// The session whose persona choice these rows mark.
    pub session_id: SessionId,
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// The rows, in the personas' own (name-sorted) order, filtered by the
    /// shared filter.
    pub rows: Vec<PersonaRowBody>,
}

/// The page's color mode after a theme change lands.
///
/// Carries no session: the mode is the instance's, not one session's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeBody {
    /// The mode — `light`, `dark`, or `system`.
    pub theme: String,
}

/// One pending composer image on the wire, from a paste, a picked file,
/// or a scanned URL.
///
/// Deliberately one shape for all three: the browser holds every pending
/// image as base64 bytes and a name, and the server ingests them the same
/// way whatever their source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUploadBody {
    /// A display name for the chip (the picked file's name, or
    /// "pasted image" for a paste).
    pub name: String,
    /// The base64-encoded image bytes, with no data-URL prefix.
    pub data_base64: String,
}

/// The new-session form's choices, read from the live configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSessionOptionsBody {
    /// The configured `[[project.entry]]` paths, as the form shows them.
    /// An empty list renders the "no projects configured" refusal.
    pub projects: Vec<String>,
    /// The configured lifecycle names, each paired with the argument
    /// count its setup declares (0 for a lifecycle that takes none).
    /// The blank lifecycle is the form's own first row and is not listed.
    pub lifecycles: Vec<LifecycleOptionBody>,
}

/// One project or lifecycle option the new-session form lists.
///
/// A named struct: a tuple has no field names on the wire, and the
/// browser reads `name`, `param_count`, and `params` by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleOptionBody {
    /// The lifecycle's name — what a `CreateSession` sends back.
    pub name: String,
    /// How many arguments its setup requires. 0 hides the arguments field.
    pub param_count: usize,
    /// Each argument as the setup's template spells it, in order — `<branch>`
    /// for a named placeholder, `$1` for a positional one. What the page
    /// shows so the user knows which argument is which; a count alone
    /// cannot say.
    pub params: Vec<String>,
}
