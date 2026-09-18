//! The project-add input popup — register a new curated project directory.
//!
//! Opened from the project picker (`<c-n>`), the popup edits a directory
//! path in its own cell; confirm resolves it with the shared cwd resolver,
//! appends it to `projects` optimistically, and emits
//! `UpdatePreferences { AddProject }` so the preferences actor persists and
//! broadcasts. The popup's scope, cell, rows, input hook, and overlay view
//! are all registered by the slice's `activate`.

pub mod intent;
pub mod render;
pub mod state;

/// The render fact carrying the active session's cwd (the seeding base and
/// the relative-path anchor for the validation footer). The kernel's
/// per-frame facts composition seeds this key.
pub const SESSION_CWD_FACT: &str = "session.cwd";

#[cfg(test)]
mod intent_tests;
