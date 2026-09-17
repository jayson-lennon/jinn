//! The fact names the terminal overlay's renderer consumes from the
//! per-frame facts context.
//!
//! The kernel's `RenderCtx::facts` seeds these each frame; the term
//! slice's overlay view reads them. One shared vocabulary so producer
//! and consumer can't drift.

/// The active chat session's id (the mirror key) — `"session.id"`.
pub const SESSION_ID: &str = "session.id";

/// `"1"` when the overlay is capturing input (the `term:control` scope
/// is on top), `"0"` otherwise — `"term.capturing"`.
pub const CAPTURING: &str = "term.capturing";

/// The configured control-toggle key (the border hint's capture glyph)
/// — `"term.toggle-key"`.
pub const TOGGLE_KEY: &str = "term.toggle-key";
