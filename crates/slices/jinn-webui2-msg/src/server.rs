//! The server → browser messages.
//!
//! One enum, one noun or past-tense fact per variant. The browser
//! dispatches every frame through a single exhaustive switch; a new
//! variant is a compile error until it has a handler.

use crate::payload::{
    EntryDeltaBody, EntryUpsertBody, ErrorBody, ModelBody, ModelRowsBody, PhaseBody,
    PickerRowsBody, SessionsBody, SkillRowsBody, SnapshotBody, TitleBody,
};
use serde::{Deserialize, Serialize};

/// A message from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "Snapshot carries a whole transcript by design; boxing would add a heap hop to every frame, and the enum is matched by value on both dispatch paths"
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The whole state, on a fresh connect or whenever the server cannot
    /// determine what to send incrementally.
    Snapshot(SnapshotBody),
    /// A whole entry, added or rewritten.
    EntryUpsert(EntryUpsertBody),
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
    /// A request was refused.
    Error(ErrorBody),
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
        ServerMsg::EntryDelta(_) => "entry_delta",
        ServerMsg::Phase(_) => "phase",
        ServerMsg::Sessions(_) => "sessions",
        ServerMsg::Model(_) => "model",
        ServerMsg::Title(_) => "title",
        ServerMsg::PickerRows(_) => "picker_rows",
        ServerMsg::ModelRows(_) => "model_rows",
        ServerMsg::SkillRows(_) => "skill_rows",
        ServerMsg::Error(_) => "error",
        ServerMsg::Ping => "ping",
    }
}
