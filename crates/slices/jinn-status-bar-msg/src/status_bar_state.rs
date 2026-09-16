//! The status bar slice's shared cell vocabulary.
//!
//! [`StatusBarState`] lives in the slice-surface layer (not in the
//! slice crate) for the same reason [`crate::service_status`]
//! does: the *writers* include kernel-resident code (the
//! IntentHandler's terminal-intent arms and its every-intent
//! clear prologue), while the reader — the status bar element —
//! lives in the slice crate. Both import this one type; neither
//! depends on the other. The kernel never depends on slice
//! crates, and the slice crate re-exports this type under its
//! own name.

use jinn_slices::SlotKey;

/// The status bar slice's cell payload.
///
/// A transient hint (e.g. "yanked 42 terminal lines to the clipboard")
/// replaces the model display on the info line until the next intent.
/// The kernel's IntentHandler is the writer — set by the terminal-intent
/// arms, cleared by its every-intent prologue — and the element reads it
/// at render time.
#[derive(Debug, Default, Clone)]
pub struct StatusBarState {
    /// The transient hint text, or `None` when the model should show.
    pub hint: Option<String>,
}

/// The slot key the status bar's cell lives under.
#[must_use]
pub fn status_bar_slot() -> SlotKey {
    SlotKey::builtin("status-bar", "state")
}
