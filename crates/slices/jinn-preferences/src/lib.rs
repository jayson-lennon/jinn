//! The preferences slice — persistence for `jinn.toml` and `state.toml`.
//!
//! Two trouper actors, moved out of the kernel per the migration doc's
//! preferences row: [`PreferencesActor`] owns
//! `AppState.frontend.preferences` (authoritative writer — it applies
//! `UpdatePreferences` diffs, saves, and writes the field inline) and
//! [`AppStateActor`] owns `state.toml` persistence (applies
//! `UpdateAppState` diffs and syncs the frontend theme/sidebar/persona
//! fields inline). Both spawn at slice activation and subscribe to the
//! slice's crossing topic (see [`bridge`]); the commands arrive via
//! kernel bridge routes, the file schemas, storage traits, and protocol
//! types they operate on live in the kernel-free
//! `jinn-preferences-config` crate.

pub mod app_state_actor;
pub mod bridge;
mod preferences_actor;
mod project_add;

pub use app_state_actor::AppStateActor;
pub use preferences_actor::PreferencesActor;
pub use project_add::intent::project_add_scope;
pub use project_add::intent::project_add_slot;

use jinn_domain::common::state::State;
use jinn_slices::SliceHost;

/// Activates the preferences slice: mints the project-add popup cell,
/// registers its overlay geometry/view, attaches the confirm/leave rows
/// and the editing hook, binds the `<c-n>` opener in the project
/// picker's scope, and spawns the two persistence actors on the
/// system's trouper runtime, subscribing them to the preferences topic.
///
/// The subscribes are the readiness point: this function must complete
/// before anything publishes `EnvironmentLoaded` (whose handlers emit
/// `UpdateAppState`/`UpdatePreferences` on first boot).
///
/// # Panics
///
/// Panics if the popup slot is already registered — double activation is a
/// wiring bug. Panics if an actor's topic subscription fails — a broken
/// actor system, not a caller bug.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(
    host: &mut SliceHost<'_, jinn_slices::RenderFacts>,
    system: &trouper::system::ActorSystem,
    services: jinn_domain::Services,
    state: State,
) {
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

    // Spawn the persistence actors (caps minted here — activation is
    // the single writer grant for each) and subscribe them
    // synchronously to the slice topic.
    let prefs_path = PreferencesActor::spawn(
        system,
        services.clone(),
        state.clone(),
        jinn_domain::common::tcaps::mint::mint_frontend_cap(),
    );
    system
        .subscribe(&prefs_path, &bridge::preferences_topic(), None)
        .expect("preferences actor subscribes to the preferences topic");
    let app_state_path = AppStateActor::spawn(
        system,
        services,
        state,
        jinn_domain::common::tcaps::mint::mint_frontend_cap(),
    );
    system
        .subscribe(&app_state_path, &bridge::preferences_topic(), None)
        .expect("app-state actor subscribes to the preferences topic");
}
