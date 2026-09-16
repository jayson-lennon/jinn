//! Events the coordinator actor publishes.

use serde::{Deserialize, Serialize};

/// A chat session's terminal screen changed.
///
/// Published by the realtime screen task on every visible change (and by the
/// coordinator on resize), keyed by the owning chat session — the same
/// identity as the frontend mirror. This is the bus-side mirror of screen
/// changes; the tool-call keepalive is separate (`ToolExecutionOutput`,
/// published by the tool layer's `with_keepalive` pacer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermScreenUpdated {
    /// The chat session whose screen changed.
    pub chat_session_id: crate::protocol::SessionId,
    /// The rendered screen (plain text).
    pub screen: String,
    /// The styled cell grid matching `screen`.
    pub cells: crate::feat::interactive_term::emulator::ScreenCells,
    /// Cursor position as (row, col).
    pub cursor: (u16, u16),
    /// Whether the program hid the cursor.
    pub cursor_hidden: bool,
}

impl crate::common::bus::BusMessage for TermScreenUpdated {}
