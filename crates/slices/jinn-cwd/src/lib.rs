//! The cwd slice — the in-app "change working directory" popup.
//!
//! Owns one cell ([`jinn_cwd_msg::cwds_slot`]) holding the popup's single
//! [`jinn_cwd_msg::CwdInputState`], the dynamic-scope overlay that renders it,
//! and the route rows that open, confirm, and leave the popup. Typing goes
//! through a route-table input hook; confirm resolves the typed path with the
//! shared pure resolver and publishes the kernel's `SetSessionCwd` through
//! the [`jinn_slices::SliceActionState`] capability, so this crate never
//! depends on the kernel. The external `<M-c>`/`<M-d>` selector flow is
//! composition-side (TUI suspend) and unaffected by this slice.

pub use jinn_cwd_msg::cwds_slot;

mod intent;
mod render;

pub use intent::cwd_scope;
pub use render::cwd_input_popup_rect;

use jinn_slices::SliceHost;

/// Activates the slice: mints the cwd cell, registers the overlay, attaches
/// the route rows and the input hook. No actor.
///
/// # Panics
///
/// Panics if the slot is already registered — double activation is a wiring
/// bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .register_cell(cwds_slot(), jinn_cwd_msg::CwdInputState::default())
        .expect("cwd slot is registered exactly once at wiring");
    host.register_overlay(
        intent::cwd_scope(),
        std::sync::Arc::new(render::cwd_overlay_rect),
    );
    host.register_overlay_selectable(&intent::cwd_scope());
    host.register_overlay_slot(intent::cwd_scope(), cwds_slot());
    host.register_overlay_view(
        intent::cwd_scope(),
        std::sync::Arc::new(render::render_cwd_input),
    );
    intent::attach_cwd_rows(host.key_routes(), &cell);
    intent::register_cwd_input_hook(host.key_routes(), &cell);
}
