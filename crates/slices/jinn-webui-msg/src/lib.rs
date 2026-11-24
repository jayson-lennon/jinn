//! The web UI contract — the vocabulary crossing the webui slice's
//! boundary.
//!
//! Everything here is pure data: the form payloads the browser posts,
//! the transcript query's cursor grammar, the SSE envelope the
//! fan-out actor produces, the unread-mark cell payload beside
//! its slot key, and the one bus Event the webui publishes
//! ([`SessionPageOpened`], the page-arrival fact the fan-out's
//! unread-clear keys on). The web UI publishes *existing* schemas
//! for its actions (`EnqueueUserMessage`, `CancelTurn`,
//! `ProviderSwitch`, `SessionLoadRequested`) and subscribes existing
//! events, so this crate owns no message beyond the arrival fact —
//! and it never depends on an implementation crate.

pub mod unread;
pub mod wire;

pub use unread::UnreadSessionMarks;
pub use unread::webui_unread_slot;
pub use wire::{
    CancelForm, CreateSessionForm, LibraryToggleForm, McpToggleForm, ModelForm, ModelsRefreshForm,
    PersonaForm, RenameForm, SendForm, SessionListItem, SessionNewQuery, SessionPageOpened,
    SessionsQuery, SessionsView, SkillLoadForm, SkillToggleForm, SseEnvelope, SseKind, SwitchForm,
    ThemeForm, TranscriptQuery,
};

#[cfg(test)]
mod wire_tests;
