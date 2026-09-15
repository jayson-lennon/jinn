//! Preferences actor - persists user preferences to `jinn.toml`.
//!
//! Subscribes to [`UpdatePreferences`] commands carrying batches of
//! [`PreferenceUpdate`] diffs. On each command, loads current preferences,
//! applies all diffs, saves to disk, and emits a [`PreferencesUpdated`]
//! event with the full result. Also writes `frontend.preferences` inline
//! after a successful save and reloads the open project picker.
//!
//! # State ownership
//!
//! This actor owns `AppState.frontend.preferences` (authoritative writer).
//! It writes the field inline after persisting to `jinn.toml` — see the
//! "sync sibling" anti-pattern in AGENTS.md §3.

use std::convert::Infallible;

use kameo::prelude::{Actor, ActorRef, Context, Message};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::feat::preferences_actor::protocol::command::UpdatePreferences;
use crate::feat::preferences_actor::protocol::event::PreferencesUpdated;

/// The preferences actor.
///
/// Subscribes to `UpdatePreferences` commands and persists preference
/// diffs to `jinn.toml`, then emits `PreferencesUpdated` so
/// downstream actors can sync their caches.
pub struct PreferencesActor {
    /// Runtime deps (services + bus).
    deps: ActorDeps,
    /// Shared application state — writes `frontend.preferences` inline after persist.
    state: State,
    /// Write authority for `frontend.preferences`.
    cap: crate::common::tcaps::FrontendCap,
}

/// Dependencies for [`PreferencesActor`].
#[derive(Clone)]
pub struct PreferencesActorDeps {
    /// Runtime deps (services + bus).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Write authority for `frontend.preferences`.
    pub cap: crate::common::tcaps::FrontendCap,
}

impl Actor for PreferencesActor {
    type Args = PreferencesActorDeps;
    type Error = Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<UpdatePreferences>())
            .await;
        Ok(Self {
            deps: args.deps,
            state: args.state,
            cap: args.cap,
        })
    }
}

impl BusPublish for PreferencesActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl Message<UpdatePreferences> for PreferencesActor {
    type Reply = ();

    async fn handle(&mut self, msg: UpdatePreferences, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_update_preferences(msg).await;
    }
}

impl PreferencesActor {
    /// Processes a batch of preference diffs: load, apply, save, emit.
    async fn handle_update_preferences(&self, payload: UpdatePreferences) {
        let mut prefs = self.deps.services.user_preferences_storage.read();
        for update in &payload.updates {
            update.apply(&mut prefs);
        }
        if let Err(e) = self.deps.services.user_preferences_storage.save(&prefs) {
            tracing::warn!(err = ?e, "preferences-actor failed to save user preferences");
            return;
        }

        // Write the persisted preferences into `frontend.preferences` inline, and
        // reload the open project picker so adds/removes round-tripping through
        // this actor are reflected immediately. The author of `frontend.preferences`
        // is this actor — keep the writes in one state guard.
        {
            self.state.with_preferences(&self.cap, |view| {
                let frontend = view.frontend();
                frontend.preferences = prefs.clone();
                if frontend.is_picker()
                    && frontend.picker_kind() == Some(crate::feat::picker::PickerKind::Project)
                {
                    crate::feat::picker::project_spec::load_project_entries(frontend);
                }
            });
        }

        self.publish(PreferencesUpdated { preferences: prefs })
            .await;
    }
}

#[cfg(test)]
mod preferences_actor_tests;
