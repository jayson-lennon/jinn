//! Sidebar — the left panel with its five sections (pins, persona, task
//! list, sessions, MCP servers) plus the rename-session popup.
//!
//! The slice owns the per-section view state in one shared cell
//! (`sidebar:state`), the navigation and section logic as feature
//! modules, the resize mode, the rename popup, and the
//! [`SidebarStateActor`] that clamps the sessions cursor when a session
//! closes. Keybindings resolve through route rows attached at
//! activation; a de-activated sidebar is inert by construction (no
//! rows, no bindings, no cell).

pub mod key_routes;
pub mod overlay;
pub mod sections;

pub use jinn_sidebar_msg::sidebar_sections_slot;

use jinn_slices::SliceHost;

/// Activates the slice: mints the sidebar sections cell and attaches
/// the sidebar's keybind rows plus the rename input hook.
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
    let cell = host
        .register_cell(
            sidebar_sections_slot(),
            jinn_sidebar_msg::SidebarSections::default(),
        )
        .expect("sidebar slot is registered exactly once at wiring");
    key_routes::attach_sidebar_rows(host.key_routes());
    key_routes::register_rename_input_hook(host.key_routes(), &cell);
    // The rename popup renders through the overlay registry keyed on its
    // dynamic scope; its rect registers as selectable (it hosts an input).
    host.register_overlay(
        key_routes::rename_scope(),
        std::sync::Arc::new(overlay::rename_overlay_rect),
    );
    host.register_overlay_slot(key_routes::rename_scope(), sidebar_sections_slot());
    host.register_overlay_view(
        key_routes::rename_scope(),
        std::sync::Arc::new(overlay::render_rename_overlay),
    );
    host.register_overlay_selectable(&key_routes::rename_scope());
    // The sessions-cursor clamp actor spawns from composition (it needs
    // the supervised runtime), not from `activate`.
}
