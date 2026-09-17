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

/// Normalizes the configured control-toggle binding (trimmed), or `None`
/// when it is unusable (caller should fall back to the default).
///
/// Any binding the keybind system accepts is allowed — single keys
/// (`<c-g>`, `<m-g>`, `<f4>`, `'x'`) and sequences (`gg`) alike: validation
/// delegates to [`ratatui_which_key::parse_key_sequence`] with the same
/// [`KeyEvent`](jinn_slices::KeyEvent) the keymap binds through, so a
/// config value accepted here is guaranteed to bind. Modifier-name case is
/// irrelevant to parsing, so the raw spelling is returned unchanged.
#[must_use]
pub fn normalize_control_toggle_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let keys = ratatui_which_key::parse_key_sequence::<jinn_slices::KeyEvent>(
        trimmed,
        &<jinn_slices::KeyEvent as ratatui_which_key::Key>::space(),
    );
    if keys.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::normalize_control_toggle_key;

    #[rstest::rstest]
    #[case("<c-g>")]
    #[case(" <m-g> ")]
    #[case("<f4>")]
    fn accepts_bindings_the_keymap_binds(#[case] raw: &str) {
        // Given a well-formed control-toggle binding.
        // When normalizing it.
        let normalized = normalize_control_toggle_key(raw);
        // Then it survives (trimmed), so the keymap will accept it too.
        assert_eq!(normalized.as_deref(), Some(raw.trim()));
    }

    #[rstest::rstest]
    #[case("")]
    #[case("   ")]
    fn rejects_empty_bindings(#[case] raw: &str) {
        // Given an empty binding.
        // When normalizing it.
        // Then it is rejected (caller falls back to the default).
        assert!(normalize_control_toggle_key(raw).is_none());
    }
}
