//! `[interactive_term]` config validation — the prefs struct and the
//! `KeyEvent`-dependent normalization live in `jinn-term-msg` (the term
//! slice's vocabulary crate); this module is a re-export shim.

pub use jinn_term_msg::prefs::{
    DEFAULT_CONTROL_TOGGLE_KEY, DEFAULT_SETTLE_MAX_WAIT_MS, DEFAULT_SETTLE_QUIET_MS,
    InteractiveTermPrefs, normalize_control_toggle_key,
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
