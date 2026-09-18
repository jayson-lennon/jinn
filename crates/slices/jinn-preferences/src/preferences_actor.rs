//! Preferences actor — persists user preferences to `jinn.toml`.
//!
//! A trouper [`ServiceActor`] subscribed to the preferences topic;
//! handles [`UpdatePreferences`] commands carrying batches of
//! [`PreferenceUpdate`] diffs. On each command, loads current
//! preferences, applies all diffs, saves to disk, and writes
//! `frontend.preferences` inline after a successful save, reloading
//! the open project picker.

use jinn_domain::common::services::Services;
use jinn_domain::common::state::State;
use jinn_preferences_config::protocol::command::UpdatePreferences;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::builder::spawn_service_builder;
use trouper::context::MsgCtx;
use trouper::prelude::ActorPath;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The preferences actor's static trouper path.
pub const PREFERENCES_ACTOR_PATH: &str = "preferences-actor";

/// The preferences actor.
///
/// Subscribes to `UpdatePreferences` commands and persists preference
/// diffs to `jinn.toml`, writing `frontend.preferences` inline after a
/// successful save.
///
/// # State ownership
///
/// This actor owns `AppState.frontend.preferences` (authoritative writer).
/// It writes the field inline after persisting to `jinn.toml` — see the
/// "sync sibling" anti-pattern in AGENTS.md §3.
pub struct PreferencesActor {
    /// Runtime services (storage for load + save).
    services: Services,
    /// Shared application state — writes `frontend.preferences` inline after persist.
    state: State,
    /// Write authority for `frontend.preferences`.
    cap: jinn_domain::common::tcaps::FrontendCap,
}

impl ServiceActor for PreferencesActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle and
        // capability via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("PreferencesActor is spawned via start_with"),
        )
    }
}

impl PreferencesActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the preferences topic (the slice's
    /// `activate`) — subscribe is the readiness point, so it must
    /// follow this call before any publish.
    pub fn spawn(
        system: &ActorSystem,
        services: Services,
        state: State,
        cap: jinn_domain::common::tcaps::FrontendCap,
    ) -> ActorPath {
        spawn_service_builder::<Self>(system)
            .at(ActorPath::new(PREFERENCES_ACTOR_PATH))
            .start_with({
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            services: services.clone(),
                            state: state.clone(),
                            cap,
                        })
                    })
                }
            })
            .start()
    }

    /// Processes a batch of preference diffs: load, apply, save, write inline.
    pub(crate) fn handle_update_preferences(&mut self, payload: &UpdatePreferences) {
        let mut prefs = self.services.user_preferences_storage.read();
        for update in &payload.updates {
            update.apply(&mut prefs);
        }
        if let Err(e) = self.services.user_preferences_storage.save(&prefs) {
            tracing::warn!(err = ?e, "preferences-actor failed to save user preferences");
            return;
        }

        // Write the persisted preferences into `frontend.preferences` inline, and
        // reload the open project picker so adds/removes round-tripping through
        // this actor are reflected immediately. The author of `frontend.preferences`
        // is this actor — keep the writes in one state guard.
        self.state.with_preferences(&self.cap, |view| {
            let frontend = view.frontend();
            frontend.preferences = prefs.clone();
            if frontend.is_picker()
                && frontend.picker_kind() == Some(jinn_domain::feat::picker::PickerKind::Project)
            {
                jinn_domain::feat::picker::project_spec::load_project_entries(frontend);
            }
        });
    }
}

impl MsgHandler<UpdatePreferences> for PreferencesActor {
    async fn handle(&mut self, msg: UpdatePreferences, _ctx: &mut MsgCtx<'_>) {
        self.handle_update_preferences(&msg);
    }
}

#[cfg(test)]
mod preferences_actor_tests;
