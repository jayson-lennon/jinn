//! The status hint write path — the kernel's seam into the status-bar
//! slice's cell.
//!
//! The hint's *behavior* lives with the intents that raise it (terminal
//! takeover/yank/push, the inert overlay toggle) and with the
//! IntentHandler's every-intent prologue that clears it. The hint's
//! *storage* lives in the status-bar slice's cell
//! ([`jinn_slices::StatusBarState`], shared vocabulary so the kernel
//! never depends on slice crates). This module is the one write seam
//! between the two: every writer goes through [`set_hint`], which is a
//! silent no-op when the slice is not activated (the removability proof
//! — without the slice, hints are never stored and the bar always
//! renders the model).

use crate::AppState;
use jinn_slices::Slices;

/// Sets (or clears) the transient status hint in the status-bar slice's
/// cell.
///
/// A no-op when the cell is absent — the slice was never activated — so
/// kernel hint writers stay correct (and silent) in a slice-free
/// configuration.
pub fn set_hint(state: &mut AppState, slices: &Slices, hint: Option<String>) {
    if let Some(cell) =
        slices.reader::<jinn_slices::StatusBarState>(&jinn_slices::status_bar_slot())
    {
        cell.update(|s| s.hint = hint);
    }
    // `state` is taken for call-site symmetry: every hint write already
    // holds `&mut AppState` (the IntentHandler's lock), and keeping it in
    // the signature keeps `set_hint` a drop-in replacement for the old
    // `state.frontend.status_hint = ...` assignments.
    let _ = &state;
}

/// Reads the current hint, if the slice is activated.
#[must_use]
pub fn hint(slices: &Slices) -> Option<String> {
    let cell = slices.reader::<jinn_slices::StatusBarState>(&jinn_slices::status_bar_slot())?;
    cell.read().hint.clone()
}
