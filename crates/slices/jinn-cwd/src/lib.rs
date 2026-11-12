//! The cwd slice — the in-app "change working directory" popup.
//!
//! Owns one cell ([`jinn_cwd_msg::cwds_slot`]) holding the popup's single
//! [`jinn_cwd_msg::CwdInputState`], the dynamic-scope overlay that renders it,
//! and the route rows that open, confirm, ghost-accept, and leave the popup.
//! Typing goes through a route-table input hook, which also mints a
//! [`jinn_cwd_msg::SuggestCwdCompletion`] per edit; [`cwd_suggest_actor::CwdSuggestActor`]
//! answers those off the key thread (one `read_dir` per request, never
//! recursing) and writes the ranked ghost back behind the request-id guard.
//! Tab accepts the ghost (append, type-over, or walk); confirm resolves the
//! typed path with the shared pure resolver and returns a publish closure
//! for the kernel's `SetSessionCwd` through
//! [`jinn_slices::SliceActionState`], so this crate's popup logic never
//! depends on the kernel — only the actor (which needs the bus and the
//! cell) does. The external `<M-c>`/`<M-d>` selector flow is
//! composition-side (TUI suspend) and unaffected by this slice.

pub use jinn_cwd_msg::cwds_slot;

pub mod cwd_suggest_actor;

mod intent;
mod render;
mod suggest;

pub use intent::cwd_scope;
pub use render::cwd_input_popup_rect;

use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_slices::SliceHost;

/// Installs the slice's overlay and key bindings, and spawns the
/// suggestion actor behind the ghost.
///
/// The cwd cell is not minted here: the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`) registers every slice cell in
/// one place, so this resolves the handle the same way every other
/// consumer does.
///
/// # Panics
///
/// Panics if the catalog has not run — the overlay would otherwise render
/// against an absent cell and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>, deps: ActorDeps) {
    let cell = host
        .slices()
        .reader::<jinn_cwd_msg::CwdInputState>(&cwds_slot())
        .expect("the cell catalog registers the cwd slot before any slice activates");
    cwd_suggest_actor::CwdSuggestActor::spawn(
        host.system(),
        cwd_suggest_actor::CwdSuggestActorDeps {
            deps,
            cell: cell.clone(),
            slot: cwds_slot(),
        },
    );
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
