//! The payload types carried by the two message enums.
//!
//! Every field is plain data the browser can render without another round
//! trip: entries ride as the core `ChatEntry` (its `kind` tag intact), the
//! phase as the core `PhaseView`, and ids as the core newtypes.

use jinn_core_types::{ChatEntry, ChatEntryId, SessionId};
use serde::{Deserialize, Serialize};

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
    /// The active session's transcript, in order.
    pub entries: Vec<EntryEnvelope>,
    /// The loaded sessions the sidebar shows.
    pub sessions: Vec<SessionRowBody>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryEnvelope {
    /// The kind's stable snake_case name, from the core `kind_str`.
    pub kind: String,
    /// The entry, in its persisted serde form.
    pub entry: ChatEntry,
}

impl EntryEnvelope {
    /// Wraps an entry, deriving its kind name from the core vocabulary.
    #[must_use]
    pub fn new(entry: ChatEntry) -> Self {
        Self {
            kind: entry.kind_str().to_owned(),
            entry,
        }
    }
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

/// The stored-session picker's rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRowsBody {
    /// The filter the rows answer (empty for the unfiltered list).
    pub filter: String,
    /// At most fifty rows.
    pub rows: Vec<SessionRowBody>,
}

/// The rows the model picker shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRowsBody {
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
    /// The rows, in the sidebar's order.
    pub rows: Vec<SessionRowBody>,
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
}
