//! The scope-focus slice — the sync interaction substrate.
//!
//! Owns one cell ([`scope_focus_slot`]) bundling the focus-scope
//! stack, the TUI signals, and the quit latch. The kernel's
//! IntentHandler (and the intent fns it dispatches to) writes through
//! a facade on `FrontendState`; the TUI render pass, keymap
//! generation, and the run loop read it. There is no actor and no
//! route row: the only writer is the exempt sync IntentHandler, and
//! the state is not a rendered element.

pub mod state;

pub use jinn_slices::scope_focus_slot;
pub use state::ScopeFocusState;

use jinn_slices::SliceHost;

/// Activates the slice: mints the scope-focus cell. No routes, no
/// actors, no view.
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a
/// wiring bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let _cell = host
        .register_cell(scope_focus_slot(), ScopeFocusState::default())
        .expect("scope-focus slot is registered exactly once at wiring");
}
