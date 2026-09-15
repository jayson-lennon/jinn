//! Snapshot of TUI signal flags, extracted from AppState before releasing the write lock.

/// Snapshot of [`jinn_domain::TuiSignals`] fields, copied
/// out of the scope-focus cell before releasing the write lock.
#[derive(Debug)]
pub(super) struct TuiSignalsSnapshot {
    /// Whether to toggle the which-key overlay.
    pub toggle_whichkey: bool,
    /// Whether an external editor was requested.
    pub edit_requested: bool,
    /// Text to copy to the system clipboard (from yank-selected-entry intent).
    pub yank_text: Option<String>,
    /// Request to change CWD via external command. Carries the search root.
    pub change_cwd_requested: Option<jinn_domain::protocol::CwdRoot>,
}

impl TuiSignalsSnapshot {
    /// Extracts TUI signal flags from the given app state.
    pub(super) fn from_state(state: &jinn_domain::AppState) -> Self {
        let signals = state.frontend.signals_snapshot();
        Self {
            toggle_whichkey: signals.toggle_whichkey,
            edit_requested: signals.edit_requested,
            yank_text: signals.yank_text,
            change_cwd_requested: signals.change_cwd_requested,
        }
    }
}
