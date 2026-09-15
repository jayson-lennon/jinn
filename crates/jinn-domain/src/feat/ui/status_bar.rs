//! Status bar - displays the session CWD, active prompt strategy, and current model.
//!
//! A 2-line display-only component at the bottom of the screen showing the
//! session's working directory (line 1) and status info (line 2).

pub mod element;
pub mod turn_counter;

#[cfg(test)]
mod element_tests;

pub use element::StatusBarElement;

use crate::common::AppUiRegistry;

/// Register the status bar UI element.
pub fn register(registry: &mut AppUiRegistry) {
    registry.register(Box::new(StatusBarElement));
}
