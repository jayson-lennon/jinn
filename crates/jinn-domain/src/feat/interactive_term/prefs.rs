//! `[interactive_term]` config validation — the kernel keeps only the
//! `KeyEvent`-dependent normalization; the prefs struct itself lives in
//! `jinn-term-msg`.

pub use jinn_term_msg::prefs::{
    DEFAULT_CONTROL_TOGGLE_KEY, DEFAULT_SETTLE_MAX_WAIT_MS, DEFAULT_SETTLE_QUIET_MS,
    InteractiveTermPrefs,
};

/// Returns the corrected config to persist/use and whether any correction
/// happened (so the caller can warn).
#[must_use]
pub fn validated(prefs: &InteractiveTermPrefs) -> (InteractiveTermPrefs, bool) {
    let mut corrected = prefs.clone();
    let mut changed = false;
    if normalize_control_toggle_key(&prefs.control_toggle_key)
        .is_none_or(|norm| norm != prefs.control_toggle_key)
    {
        corrected.control_toggle_key = normalize_control_toggle_key(&prefs.control_toggle_key)
            .unwrap_or_else(|| DEFAULT_CONTROL_TOGGLE_KEY.to_owned());
        changed = true;
    }
    if prefs.settle_quiet_ms == 0 {
        corrected.settle_quiet_ms = DEFAULT_SETTLE_QUIET_MS;
        changed = true;
    }
    if prefs.settle_max_wait_ms < prefs.settle_quiet_ms {
        corrected.settle_max_wait_ms = prefs.settle_quiet_ms;
        changed = true;
    }
    (corrected, changed)
}

/// Normalizes the configured control-toggle binding (trimmed), or `None`
/// when it is unusable (caller should fall back to the default).
///
/// Any binding the keybind system accepts is allowed — single keys
/// (`<c-g>`, `<m-g>`, `<f4>`, `'x'`) and sequences (`gg`) alike: validation
/// delegates to [`ratatui_which_key::parse_key_sequence`] with the same
/// [`KeyEvent`](crate::protocol::KeyEvent) the keymap binds through, so a
/// config value accepted here is guaranteed to bind. Modifier-name case is
/// irrelevant to parsing, so the raw spelling is returned unchanged.
#[must_use]
pub fn normalize_control_toggle_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let keys = ratatui_which_key::parse_key_sequence::<crate::protocol::KeyEvent>(
        trimmed,
        &<crate::protocol::KeyEvent as ratatui_which_key::Key>::space(),
    );
    if keys.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}
