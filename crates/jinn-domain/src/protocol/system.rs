//! System domain: application-level events.

use serde::{Deserialize, Serialize};

use crate::common::bus::BusMessage;
use crate::protocol::Mode;
use crate::protocol::key::KeyEvent;

/// A key was pressed down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyDown {
    /// The key event.
    pub key: KeyEvent,
}

/// A key was released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyUp {
    /// The key event.
    pub key: KeyEvent,
}

/// The application mode changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeChanged {
    /// The previous mode.
    pub from: Mode,
    /// The new mode.
    pub to: Mode,
}
impl BusMessage for KeyDown {}
impl BusMessage for KeyUp {}

/// The active session changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSessionChanged {
    /// The new active session ID.
    pub session_id: crate::protocol::SessionId,
}

impl BusMessage for ActiveSessionChanged {}

jinn_slices::crossing_schema!(ActiveSessionChanged, "ActiveSessionChanged",
trouper::schema::SchemaKind::Event,
description: "The active session changed.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);
