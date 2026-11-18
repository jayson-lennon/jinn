//! The web UI contract — the vocabulary crossing the webui slice's
//! boundary.
//!
//! Everything here is pure data: the form payloads the browser posts,
//! the transcript query's cursor grammar, and the SSE envelope the
//! fan-out actor produces. No bus schema lives here — the web UI
//! publishes *existing* schemas (`EnqueueUserMessage`, `CancelTurn`,
//! `ProviderSwitch`, `SessionLoadRequested`) and subscribes existing
//! events, so this crate never depends on an implementation crate and
//! never derives a message schema.
//!
//! The transcript response itself is deliberately absent: it is
//! server-rendered HTML, not JSON — the browser swaps it into the page
//! as-is, so its "type" is a template, not a struct.

pub mod wire;

pub use wire::{
    CancelForm, ModelForm, ModelsRefreshForm, RenameForm, SendForm, SessionListItem, SseEnvelope,
    SseKind, SwitchForm, TranscriptQuery,
};

#[cfg(test)]
mod wire_tests;
