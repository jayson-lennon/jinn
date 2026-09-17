//! `interactive_term` preferences — `[interactive_term]` in `jinn.toml`.
//!
//! Configures the takeover UI's control-toggle key and the tool-side settle
//! wait. Every field has a default, so the whole block is optional; unknown or
//! unusable control-toggle bindings fall back to the default with a warning
//! (degrade gracefully, never brick the terminal overlay).

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

/// `[interactive_term]` preferences.
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

impl Default for InteractiveTermPrefs {
    fn default() -> Self {
        Self {
            control_toggle_key: DEFAULT_CONTROL_TOGGLE_KEY.to_owned(),
            settle_quiet_ms: DEFAULT_SETTLE_QUIET_MS,
            settle_max_wait_ms: DEFAULT_SETTLE_MAX_WAIT_MS,
        }
    }
}
