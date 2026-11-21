//! The web UI contract — the vocabulary crossing the webui slice's
//! boundary.
//!
//! Everything here is pure data: the form payloads the browser posts,
//! the transcript query's cursor grammar, the SSE envelope the
//! fan-out actor produces, and the unread-mark cell payload beside
//! its slot key. No bus schema lives here — the web UI
//! publishes *existing* schemas (`EnqueueUserMessage`, `CancelTurn`,
//! `ProviderSwitch`, `SessionLoadRequested`) and subscribes existing
//! events, so this crate never depends on an implementation crate and
//! never derives a message schema.
//!
//! The transcript response itself is deliberately absent: it is
//! server-rendered HTML, not JSON — the browser swaps it into the page
//! as-is, so its "type" is a template, not a struct.

pub mod unread;
pub mod wire;

pub use unread::UnreadSessionMarks;
pub use unread::webui_unread_slot;
pub use wire::{
    CancelForm, CreateSessionForm, McpToggleForm, ModelForm, ModelsRefreshForm, PersonaForm,
    RenameForm, SendForm, SessionListItem, SessionNewQuery, SessionsQuery, SkillLoadForm,
    SkillToggleForm, SseEnvelope, SseKind, SwitchForm, TranscriptQuery,
};

#[cfg(test)]
mod wire_tests;
