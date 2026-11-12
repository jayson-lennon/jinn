//! The status bar slice — fixed bottom chrome rendering session info.
//!
//! Two always-visible lines: the session's working directory with the
//! tree aggregate (line 1), and token/cost/turn stats plus the model —
//! or a transient status hint (line 2). The slice owns the rendering
//! code and the hint cell; everything else it draws is read-only state
//! from the kernel (session ledger, provider cache, theme).
//!
//! The per-session two-line presentation is exported
//! ([`render_session_cwd_line`], [`render_session_info_line`],
//! [`session_working_time`]) so other surfaces — the sidebar's session
//! preview — draw the identical lines for the session they describe.
//! The tree aggregate and the transient hint stay private: both are
//! active-session chrome, and the shared surface excludes them.
//!
//! There is no actor: the only writable state (the hint) is written by
//! the kernel's synchronous IntentHandler, which is an exempt writer by
//! route-table design. The element reads the cell at render time and
//! falls back to the model display when the slice is not activated.

pub mod element;

#[cfg(test)]
mod element_tests;

use jinn_kernel::common::ui_registry::UiRegistry;
use jinn_slices::SliceHost;

pub use element::StatusBarElement;
pub use element::model_cache_of;
pub use element::render_session_cwd_line;
pub use element::render_session_info_line;
pub use element::session_working_time;
pub use jinn_status_bar_msg::StatusBarState;
pub use jinn_status_bar_msg::status_bar_slot;

/// Activates the status bar slice.
///
/// Kernel-free of feature state, the slice needs no registration of its
/// own — everything it renders is read-only kernel state resolved through
/// [`RenderCtx`](jinn_kernel::common::render_ctx::RenderCtx). The hint
/// cell is registered by the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`) before any slice activates;
/// the element resolves it read-only at render time.
///
/// # Panics
///
/// Never panics: the element falls back to the model display when the
/// hint cell has not been registered.
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    // The status bar's own screen region. The element holds no animation
    // state, so the draw function needs no interior mutability.
    host.slices()
        .register_render_slot::<jinn_kernel::common::app_state::AppState>(
            jinn_slices::Region::StatusBar,
            std::sync::Arc::new(
                |frame: &mut ratatui::Frame<'_>,
                 target: jinn_slices::DrawTarget,
                 ctx: &dyn jinn_slices::DrawContext<jinn_kernel::common::app_state::AppState>,
                 _rects: &mut Vec<ratatui::layout::Rect>| {
                    element::paint(frame, target.area, ctx);
                },
            ),
        );
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

    /// Registers the hint cell the catalog owns.
    ///
    /// The catalog is the production registrar; this test exercises the
    /// cell itself, so it seeds the same entry locally rather than depend
    /// on the catalog (which would be a cycle through `jinn-quake-bar`
    /// and `jinn-dashboard`).
    fn seed_status_bar_cell(slices: &jinn_slices::Slices) {
        slices
            .register(
                jinn_status_bar_msg::status_bar_slot(),
                jinn_status_bar_msg::StatusBarState::default(),
            )
            .expect("the status-bar slot is free in a fresh registry");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn activate_registers_the_status_bar_cell() {
        // Given a host over a registry holding the catalog's status-bar cell.
        let slices = jinn_slices::Slices::new();
        seed_status_bar_cell(&slices);
        let mut viewport = jinn_slices::view::Viewport::new();
        let overlay_views = jinn_slices::OverlayViews::new();
        let key_routes = jinn_slices::KeyRoutes::new();
        let services = jinn_kernel::Services::new_fake().await;
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
