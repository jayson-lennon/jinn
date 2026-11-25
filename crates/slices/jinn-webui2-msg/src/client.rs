//! The browser → server messages.
//!
//! One enum, one imperative per variant. The server dispatches every
//! frame through a single exhaustive `match`; there is no wildcard arm,
//! so a new variant is a compile error until it has a handler.

use jinn_core_types::{ChatEntryId, SessionId};
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
    },
    /// Send a message as a fresh turn (rejected while a turn runs).
    Send {
        /// The message text.
        text: String,
    },
    /// Queue a message behind the running turn.
    Queue {
        /// The message text.
        text: String,
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
}

#[must_use]
/// The variant's wire tag, for tests and logging.
pub const fn tag_of(msg: &ClientMsg) -> &'static str {
    match msg {
        ClientMsg::Hello { .. } => "hello",
        ClientMsg::Send { .. } => "send",
        ClientMsg::Queue { .. } => "queue",
        ClientMsg::Cancel => "cancel",
        ClientMsg::ActivateSession { .. } => "activate_session",
        ClientMsg::SwitchModel { .. } => "switch_model",
        ClientMsg::Rename { .. } => "rename",
        ClientMsg::PickerRows { .. } => "picker_rows",
        ClientMsg::ModelRows { .. } => "model_rows",
    }
}
