//! Terminal control registry — who may send input per session.
//!
//! Minted once at wiring (the term slice's `activate()`); the kernel
//! intent rows and the tools read it to decide input ownership.

use std::collections::HashMap;
use std::sync::Arc;

use jinn_core_types::SessionId;
use parking_lot::Mutex;

use crate::command::ControlHolder;

/// Per-session input ownership: the agent (tool calls) or the user
/// (terminal takeover). Flipped synchronously on takeover/handback so an
/// in-flight tool call's settle sees the takeover on its next poll —
/// mailbox-sequential message handling cannot deliver that. Polled from
/// async settle loops: plain mutex, never held across an await. Sessions
/// with no entry default to [`ControlHolder::Agent`].
#[derive(Debug, Clone, Default)]
pub struct TermControls(Arc<Mutex<HashMap<SessionId, ControlHolder>>>);

impl TermControls {
    /// The holder for `chat`, defaulting to [`ControlHolder::Agent`] when the
    /// session has no entry (never spawned, spawn failed, or torn down).
    #[must_use]
    pub fn holder_for(&self, chat: &SessionId) -> ControlHolder {
        self.0.lock().get(chat).copied().unwrap_or_default()
    }

    /// Sets who holds control of `chat` (mints the entry when absent).
    pub fn set(&self, chat: &SessionId, holder: ControlHolder) {
        self.0.lock().insert(chat.clone(), holder);
    }

    /// Removes `chat`'s entry (session teardown).
    pub fn remove(&self, chat: &SessionId) {
        self.0.lock().remove(chat);
    }
}

/// The process-wide control registry (minted once at wiring).
pub static TERM_CONTROLS: std::sync::OnceLock<TermControls> = std::sync::OnceLock::new();

/// The control registry, if minted.
#[must_use]
pub fn term_controls() -> Option<&'static TermControls> {
    TERM_CONTROLS.get()
}
