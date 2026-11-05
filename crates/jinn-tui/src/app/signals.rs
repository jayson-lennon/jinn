//! Snapshot of TUI signal flags, extracted from AppState before releasing the write lock.

use jinn_slices::ClipboardRequest;

/// Snapshot of [`jinn_slices::TuiSignals`] fields, copied
/// out of the scope-focus cell before releasing the write lock.
#[derive(Debug)]
pub(super) struct TuiSignalsSnapshot {
    /// Whether to toggle the which-key overlay.
    pub toggle_whichkey: bool,
    /// Whether an external editor was requested.
    pub edit_requested: bool,
    /// A clipboard write to perform: the payload text and the label the
    /// clipboard writer reports it under.
    pub yank_text: Option<ClipboardRequest>,
    /// Request to change CWD via external command. Carries the search root.
    pub change_cwd_requested: Option<jinn_kernel::protocol::CwdRoot>,
}

impl TuiSignalsSnapshot {
    /// Extracts TUI signal flags from the given app state.
    pub(super) fn from_state(state: &jinn_kernel::AppState) -> Self {
        let signals = state.frontend.signals_snapshot();
        Self {
            toggle_whichkey: signals.toggle_whichkey,
            edit_requested: signals.edit_requested,
            yank_text: signals.yank_text,
            change_cwd_requested: signals.change_cwd_requested,
        }
    }
}
