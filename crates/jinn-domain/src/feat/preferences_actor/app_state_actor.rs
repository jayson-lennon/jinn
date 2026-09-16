//! App-state actor - persists runtime state to `state.toml`.
//!
//! Subscribes to [`UpdateAppState`] commands carrying batches of
//! [`AppStateUpdate`] diffs. On each command, loads current state,
//! applies all diffs, saves to disk, and emits an [`AppStateUpdated`]
//! event with the full result.

use kameo::prelude::{Actor, ActorRef, Context, Message};

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::state::State;
use crate::feat::preferences_actor::app_state_file::AppStateFile;
use crate::feat::preferences_actor::protocol::app_state_command::UpdateAppState;
use crate::feat::preferences_actor::protocol::app_state_event::AppStateUpdated;
use crate::feat::theme;

/// Dependencies for spawning an [`AppStateActor`].
#[derive(Clone)]
pub struct AppStateActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    pub frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

/// The app-state actor.
///
/// Subscribes to `UpdateAppState` commands and persists state
/// diffs to `state.toml`, then emits `AppStateUpdated` so
/// downstream actors can sync their caches.
pub struct AppStateActor {
    deps: ActorDeps,
    /// Shared application state — writes frontend.app_state, sidebar_width,
    /// theme, and context.active_persona inline after persist.
    state: State,
    frontend_cap: crate::common::tcaps::frontend::FrontendCap,
}

impl Actor for AppStateActor {
    type Args = AppStateActorDeps;
    type Error = kameo::error::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<UpdateAppState>())
            .await;

        Ok(Self {
            deps: args.deps.clone(),
            state: args.state,
            frontend_cap: args.frontend_cap,
        })
    }
}

impl AppStateActor {
    /// Apply state updates, persist, and emit event.
    pub(crate) async fn handle_update(&mut self, msg: UpdateAppState) {
        let mut state = self.deps.services.app_state_storage.read();
        for update in &msg.updates {
            update.apply(&mut state);
        }
        if let Err(e) = self.deps.services.app_state_storage.save(&state) {
            tracing::warn!(err = ?e, "app-state-actor failed to save app state");
            return;
        }
        // Write frontend/context fields inline after persist.
        self.sync_state(&state);
        self.publish(AppStateUpdated { state }).await;
    }

    /// Syncs persisted state into the shared `AppState` frontend/context fields.
    fn sync_state(&self, updated: &AppStateFile) {
        // Resolve the persisted theme name against the theme slice's
        // entries cell (populated by the activation-time directory scan).
        // Unknown names fall back to the embedded default.
        let new_theme = resolve_cached_theme(
            self.deps
                .services
                .slices
                .reader::<jinn_theme_msg::ThemeEntries>(&jinn_theme_msg::theme_entries_slot())
                .map(|cell| {
                    let entries = cell.read();
                    entries
                        .theme(updated.theme_name.as_deref().unwrap_or("default"))
                        .cloned()
                }),
        );

        // Cache the entire state and update sidebar/theme/caches.
        self.state.with_preferences(&self.frontend_cap, |ops| {
            let frontend = ops.frontend();
            frontend.app_state = updated.clone();
            frontend.sidebar_width = updated.sidebar_width.unwrap_or(30);
            frontend.theme = new_theme.clone();
        });

        // Invalidate theme caches at the frontend level.
        self.state.with_preferences(&self.frontend_cap, |ops| {
            ops.frontend().caches.invalidate_all();
        });

        // Sync active_persona when persona_name changes (persona
        // selection lives in the persona slice's cell).
        if let Some(ref persona_name) = updated.persona_name {
            if let Some(cell) = self
                .deps
                .services
                .slices
                .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            {
                let present = cell.read().entries.iter().any(|p| p.name == *persona_name);
                if present {
                    cell.update(|selection| {
                        selection.active = Some(persona_name.clone());
                    });
                }
            }
        }
    }
}

impl Message<UpdateAppState> for AppStateActor {
    type Reply = ();

    async fn handle(&mut self, msg: UpdateAppState, _ctx: &mut Context<Self, Self::Reply>) {
        self.handle_update(msg).await;
    }
}

impl BusPublish for AppStateActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

/// Resolves a theme name against the theme slice's entries cell, falling
/// back to the embedded default when the cell is absent (slice not
/// activated) or the name is not found.
fn resolve_cached_theme(
    resolved: Option<Option<crate::feat::theme::Theme>>,
) -> crate::feat::theme::Theme {
    resolved.flatten().unwrap_or_else(theme::default_theme)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use std::sync::Arc;

    use super::AppStateActor;
    use crate::common::actor_deps::ActorDeps;
    use crate::common::services::Services;
    use crate::common::services::bus_service::BusAudit;
    use crate::feat::preferences_actor::app_state_file::AppStateFile;
    use crate::feat::preferences_actor::app_state_storage::InMemoryAppStateStorage;
    use crate::feat::preferences_actor::protocol::app_state_command::{
        AppStateUpdate, UpdateAppState,
    };
    use crate::feat::preferences_actor::protocol::app_state_event::AppStateUpdated;
    use crate::feat::session::model_selection::ModelSelection;
    async fn create_actor() -> (AppStateActor, BusAudit, Services) {
        let (bus, audit) = crate::common::services::BusService::new_recording();
        let mut services = Services::new_fake_with_bus(bus).await;

        let storage = InMemoryAppStateStorage::new();
        let svc = crate::feat::preferences_actor::app_state_storage::AppStateStorageService::new(
            Arc::new(storage),
        );
        svc.reload().expect("test app state storage initial reload");
        services.app_state_storage = svc;

        let actor = AppStateActor {
            deps: ActorDeps {
                services: services.clone(),
            },
            state: crate::common::state::State::new(crate::common::app_state::AppState::default()),
            frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
        };
        (actor, audit, services)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_last_model_persists_and_emits() {
        // Given an app-state actor.
        let (mut actor, audit, services) = create_actor().await;

        // When handling UpdateAppState with SetLastModel.
        actor
            .handle_update(UpdateAppState {
                updates: vec![AppStateUpdate::SetLastModel(Some(
                    ModelSelection::from_single("anthropic/claude-sonnet-4".to_owned()),
                ))],
            })
            .await;

        // Then the storage has the last model.
        let loaded = services.app_state_storage.read();
        let expected = ModelSelection::from_single("anthropic/claude-sonnet-4".to_owned());
        assert_eq!(loaded.last_model, Some(expected));

        // And an AppStateUpdated event was emitted.
        let events = audit.of_type::<AppStateUpdated>();
        assert_eq!(events.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_theme_persists_and_emits() {
        // Given an app-state actor.
        let (mut actor, audit, services) = create_actor().await;

        // When handling UpdateAppState with SetTheme.
        actor
            .handle_update(UpdateAppState {
                updates: vec![AppStateUpdate::SetTheme(Some("dracula".to_owned()))],
            })
            .await;

        // Then the storage has the theme.
        let loaded = services.app_state_storage.read();
        assert_eq!(loaded.theme_name.as_deref(), Some("dracula"));

        // And an AppStateUpdated event was emitted.
        let events = audit.of_type::<AppStateUpdated>();
        assert_eq!(events.len(), 1);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn multiple_updates_in_one_command() {
        // Given an app-state actor.
        let (mut actor, _audit, services) = create_actor().await;

        // When handling a batch with multiple updates.
        actor
            .handle_update(UpdateAppState {
                updates: vec![
                    AppStateUpdate::SetLastModel(Some(ModelSelection::from_single(
                        "openrouter/gpt-4".to_owned(),
                    ))),
                    AppStateUpdate::SetSidebarWidth(Some(40)),
                    AppStateUpdate::SetTheme(Some("nord".to_owned())),
                ],
            })
            .await;

        // Then all three fields are persisted.
        let loaded = services.app_state_storage.read();
        let expected = ModelSelection::from_single("openrouter/gpt-4".to_owned());
        assert_eq!(loaded.last_model, Some(expected));
        assert_eq!(loaded.sidebar_width, Some(40));
        assert_eq!(loaded.theme_name.as_deref(), Some("nord"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_sets_sidebar_width_default_30() {
        // Given an app-state actor.
        let (actor, _audit, _services) = create_actor().await;

        // When syncing AppStateFile with sidebar_width = None.
        let app_state = AppStateFile {
            sidebar_width: None,
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then sidebar_width is the default 30.
        let guard = actor.state.read();
        assert_eq!(guard.frontend.sidebar_width, 30);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_updates_sidebar_width() {
        // Given an app-state actor.
        let (actor, _audit, _services) = create_actor().await;

        // When syncing AppStateFile with sidebar_width = 50.
        let app_state = AppStateFile {
            sidebar_width: Some(50),
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then sidebar_width is 50.
        let guard = actor.state.read();
        assert_eq!(guard.frontend.sidebar_width, 50);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_sets_correct_persona() {
        // If the condition were flipped, the wrong persona would be set.
        // Given an app-state actor with two personas in the persona cell.
        let (actor, _audit, _services) = create_actor().await;
        let cell = actor
            .deps
            .services
            .slices
            .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
            .expect("test: persona cell seeded");
        cell.update(|selection| {
            selection.entries = vec![
                jinn_persona_msg::Persona {
                    name: "coder".to_owned(),
                    description: String::new(),
                    body: String::new(),
                },
                jinn_persona_msg::Persona {
                    name: "writer".to_owned(),
                    description: String::new(),
                    body: String::new(),
                },
            ];
        });

        // When syncing AppStateFile with persona_name = "writer".
        let app_state = AppStateFile {
            persona_name: Some("writer".to_owned()),
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then the active persona is "writer", not "coder".
        let active = cell
            .read()
            .active
            .clone()
            .expect("should have active persona");
        assert_eq!(active, "writer");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_resolves_default_theme_when_none() {
        // Given an app-state actor.
        let (actor, _audit, _services) = create_actor().await;

        // When syncing AppStateFile with theme_name = None.
        let app_state = AppStateFile {
            theme_name: None,
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then the theme was resolved and caches invalidated without panic.
        // resolve_theme(None, ...) returns the embedded default theme.
        let _guard = actor.state.read();
        // If we reach here, the handler completed successfully.
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_applies_contributed_theme_from_cache() {
        // Given an app-state actor whose theme cell holds a scanned theme.
        let (actor, _audit, _services) = create_actor().await;
        let mut contributed = crate::feat::theme::default_theme();
        contributed.focus_accent = ratatui::style::Color::Red;
        actor
            .deps
            .services
            .slices
            .register(
                jinn_theme_msg::theme_entries_slot(),
                jinn_theme_msg::ThemeEntries {
                    entries: vec![jinn_theme_msg::NamedTheme {
                        name: "dracula".to_owned(),
                        theme: contributed.clone(),
                    }],
                },
            )
            .expect("theme cell minted once");

        // When syncing AppStateFile with theme_name = Some("dracula").
        let app_state = AppStateFile {
            theme_name: Some("dracula".to_owned()),
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then the frontend theme is the contributed one.
        assert_eq!(
            actor.state.read().frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sync_state_unknown_theme_falls_back_to_default() {
        // Given an app-state actor whose theme cell lacks the name.
        let (actor, _audit, _services) = create_actor().await;

        // When syncing AppStateFile with a name the cache lacks.
        let app_state = AppStateFile {
            theme_name: Some("no-such-theme".to_owned()),
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then the frontend keeps the embedded default theme.
        let applied = actor.state.read().frontend.theme.focus_accent;
        assert_eq!(applied, crate::feat::theme::default_theme().focus_accent);
    }
}
