//! The preferences slice — persistence for `jinn.toml` and `state.toml`.
//!
//! Two actors, moved out of the kernel per the migration doc's preferences
//! row: [`PreferencesActor`] owns `AppState.frontend.preferences`
//! (authoritative writer — it applies `UpdatePreferences` diffs, saves, and
//! writes the field inline) and [`AppStateActor`] owns `state.toml`
//! persistence (applies `UpdateAppState` diffs, saves, and syncs the
//! frontend theme/sidebar/persona fields inline). The file schemas,
//! storage traits, and bus protocol types they operate on live in the
//! kernel-free `jinn-preferences-config` crate.

mod app_state_actor;
mod preferences_actor;
mod project_add;

pub use app_state_actor::AppStateActor;
pub use app_state_actor::AppStateActorDeps;
pub use preferences_actor::PreferencesActor;
pub use preferences_actor::PreferencesActorDeps;
pub use project_add::intent::project_add_scope;
pub use project_add::intent::project_add_slot;

/// Activates the preferences slice: mints the project-add popup cell,
/// registers its overlay geometry/view, attaches the confirm/leave rows
/// and the editing hook, and binds the `<c-n>` opener in the project
/// picker's scope. The two persistence actors spawn from `actor_wiring`
/// (they need the bus root + caps, not the slice host).
///
/// # Panics
///
/// Panics if the popup slot is already registered — double activation is a
/// wiring bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut jinn_slices::SliceHost<'_, jinn_slices::RenderFacts>) {
    let cell = host
        .register_cell(
            project_add_slot(),
            project_add::state::ProjectAddInputState::default(),
        )
        .expect("project-add slot is registered exactly once at wiring");
    host.register_overlay(
        project_add::intent::project_add_scope(),
        std::sync::Arc::new(project_add::render::project_add_overlay_rect),
    );
    host.register_overlay_selectable(&project_add::intent::project_add_scope());
    host.register_overlay_slot(project_add::intent::project_add_scope(), project_add_slot());
    host.register_overlay_view(
        project_add::intent::project_add_scope(),
        std::sync::Arc::new(project_add::render::render_project_add_input),
    );
    project_add::intent::attach_project_add_rows(host.key_routes(), &cell);
    project_add::intent::register_project_add_input_hook(host.key_routes(), &cell);
}
