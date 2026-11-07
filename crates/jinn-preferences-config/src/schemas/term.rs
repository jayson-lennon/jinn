//! The `[term]` section — the interactive terminal overlay's preferences.
//!
//! Configures the takeover UI's control-toggle key and the tool-side
//! settle wait. Every field has a default, so the whole block is
//! optional; unknown or unusable control-toggle bindings fall back to
//! the default with a warning (degrade gracefully, never brick the
//! terminal overlay).
//!
//! Normalizing a configured binding is behavior and stays in
//! `jinn-term-msg`, which owns the keybind parser this crate does not
//! depend on.

use serde::{Deserialize, Serialize};

/// The default control-toggle key binding (`<c-g>`, matching the pi agent's
/// convention and near-unused by TUI programs). Toggles control mode in both
/// directions: view → control, and control → view.
pub const DEFAULT_CONTROL_TOGGLE_KEY: &str = "<c-g>";

/// The default quiet window (ms of silence before a send call returns).
pub const DEFAULT_SETTLE_QUIET_MS: u64 = 400;

/// The default hard cap (ms bounding any single settle wait).
pub const DEFAULT_SETTLE_MAX_WAIT_MS: u64 = 3000;

fn default_control_toggle_key() -> String {
    DEFAULT_CONTROL_TOGGLE_KEY.to_owned()
}

fn default_settle_quiet_ms() -> u64 {
    DEFAULT_SETTLE_QUIET_MS
}

fn default_settle_max_wait_ms() -> u64 {
    DEFAULT_SETTLE_MAX_WAIT_MS
}

/// `[term]` preferences.
///
/// # Behavior: Snapshot Read (the settle keys) + read-once (the toggle
/// key).
///
/// `settle_quiet_ms` and `settle_max_wait_ms` are re-read at each
/// blocking settle, so an edit applies at the next call. The
/// `control_toggle_key` is bound once when the term slice activates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveTermPrefs {
    /// Key that toggles terminal control mode in both directions: enters
    /// control mode from view mode, and exits control mode back to view mode.
    /// Notation follows the keymap (`<c-g>`, `<c-'>`, ...).
    #[serde(default = "default_control_toggle_key")]
    pub control_toggle_key: String,
    /// Milliseconds of output silence before a blocking call returns.
    #[serde(default = "default_settle_quiet_ms")]
    pub settle_quiet_ms: u64,
    /// Hard cap (ms) on any single settle wait, for programs that never
    /// stop repainting (htop, btop).
    #[serde(default = "default_settle_max_wait_ms")]
    pub settle_max_wait_ms: u64,
}

impl jinn_config::Configurable for InteractiveTermPrefs {
    // The section absorbs its own name: `[term]`, not
    // `[term.interactive_term]`, so the umbrella does not stutter.
    const KEY: &'static str = "term";
}

impl Default for InteractiveTermPrefs {
    fn default() -> Self {
        Self {
            control_toggle_key: DEFAULT_CONTROL_TOGGLE_KEY.to_owned(),
            settle_quiet_ms: DEFAULT_SETTLE_QUIET_MS,
            settle_max_wait_ms: DEFAULT_SETTLE_MAX_WAIT_MS,
        }
    }
}
