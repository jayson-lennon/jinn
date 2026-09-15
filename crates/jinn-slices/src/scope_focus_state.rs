//! The scope-focus slice's shared cell vocabulary.
//!
//! [`ScopeFocusState`] bundles the sync interaction substrate — the
//! focus-scope stack, the TUI signals, and the quit latch — into one
//! cell. The kernel's IntentHandler (and the intent fns it dispatches
//! to) writes through a facade on `FrontendState`; the TUI render pass,
//! keymap generation, and the run loop read it. The concrete types live
//! beside this one in `jinn-slices` ([`crate::focus`],
//! [`crate::tui_signals`]) so the kernel never depends on the slice
//! crate.

use crate::focus::{FocusScope, ScopeStack};
use crate::tui_signals::TuiSignals;

/// The scope-focus slice's cell payload.
#[derive(Debug)]
pub struct ScopeFocusState {
    /// The focus/overlay stack. The top scope drives mode, keymap
    /// resolution, and which overlays render.
    pub stack: ScopeStack,

    /// Signals raised by the intent handler for the platform layer
    /// (which-key toggle, editor launch, yank, cwd change request).
    pub signals: TuiSignals,

    /// The quit latch: set by the Quit intent, polled by the run loop.
    pub quit: bool,
}

impl Default for ScopeFocusState {
    fn default() -> Self {
        // The historical `FrontendState::default()` booted into Input
        // (the chat input focused); keep that exact default.
        let mut stack = ScopeStack::default();
        stack.push(FocusScope::Input);
        Self {
            stack,
            signals: TuiSignals::new(),
            quit: false,
        }
    }
}

/// The slot key the scope-focus cell lives under.
#[must_use]
pub fn scope_focus_slot() -> crate::SlotKey {
    crate::SlotKey::builtin("scope-focus", "state")
}
