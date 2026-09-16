//! The status bar slice — fixed bottom chrome rendering session info.
//!
//! Two always-visible lines: the session's working directory with the
//! tree aggregate (line 1), and token/cost/turn stats plus the model —
//! or a transient status hint (line 2). The slice owns the rendering
//! code and the hint cell; everything else it draws is read-only state
//! from the kernel (session ledger, provider cache, theme).
//!
//! There is no actor: the only writable state (the hint) is written by
//! the kernel's synchronous IntentHandler, which is an exempt writer by
//! route-table design. The element reads the cell at render time and
//! falls back to the model display when the slice is not activated.

pub mod element;
pub mod state;
pub mod turn_counter;

#[cfg(test)]
mod element_tests;

use jinn_domain::common::ui_registry::UiRegistry;
use jinn_slices::SliceHost;

pub use element::StatusBarElement;
pub use state::StatusBarState;
pub use state::status_bar_slot;

/// Activates the status bar slice: registers the hint cell.
///
/// Kernel-free of feature state, the slice needs exactly one
/// registration — everything else it renders is read-only kernel state
/// resolved through [`RenderCtx`](jinn_domain::common::render_ctx::RenderCtx).
///
/// # Panics
///
/// Panics if the cell registration races another registration for the
/// same slot — impossible at today's call pattern (one activation per
/// launch).
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    #[expect(
        clippy::expect_used,
        reason = "bootstrap assertion: a duplicate cell registration is broken wiring"
    )]
    host.register_cell(status_bar_slot(), StatusBarState::default())
        .expect("status-bar slot is registered exactly once at wiring");
}

/// Registers the status bar element into the UI registry.
///
/// Composition calls this on both launch paths (production and test) —
/// the kernel's `register_all_ui_elements` cannot reference slice
/// crates.
pub fn register(registry: &mut UiRegistry) {
    registry.register(Box::new(StatusBarElement));
}

#[cfg(test)]
mod activation_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use jinn_slices::SliceHost;

    #[rstest::rstest]
    #[tokio::test]
    async fn activate_registers_the_status_bar_cell() {
        // Given a host over an empty slice registry.
        let slices = jinn_slices::Slices::new();
        let mut viewport = jinn_domain::common::slices::view::Viewport::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_domain::Services::new_fake().await;
        let mut host = SliceHost::new(
            &slices,
            &mut viewport,
            &overlay_views,
            &key_routes,
            &services.trouper_system,
        );

        // When activating the slice.
        crate::activate(&mut host);

        // Then the status-bar cell resolves and round-trips a hint.
        let cell = slices
            .reader::<jinn_status_bar_msg::StatusBarState>(&jinn_status_bar_msg::status_bar_slot())
            .expect("activation must register the status-bar cell");
        cell.update(|s| s.hint = Some("hello".to_owned()));
        assert_eq!(cell.read().hint.as_deref(), Some("hello"));
    }
}
