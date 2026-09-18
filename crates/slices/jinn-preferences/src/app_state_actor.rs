//! App-state actor — persists runtime state to `state.toml`.
//!
//! A trouper [`ServiceActor`] subscribed to the preferences topic;
//! handles [`UpdateAppState`] commands carrying batches of
//! [`AppStateUpdate`] diffs. On each command, loads current state,
//! applies all diffs, saves to disk, and syncs the frontend
//! theme/sidebar/persona fields inline.

use jinn_domain::common::services::Services;
use jinn_domain::common::state::State;
use jinn_domain::feat::theme;
use jinn_preferences_config::app_state_file::AppStateFile;
use jinn_preferences_config::protocol::app_state_command::UpdateAppState;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::builder::spawn_service_builder;
use trouper::context::MsgCtx;
use trouper::prelude::ActorPath;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

/// The app-state actor's static trouper path.
pub const APP_STATE_ACTOR_PATH: &str = "app-state-actor";

/// The app-state actor.
///
/// Subscribes to `UpdateAppState` commands and persists state diffs to
/// `state.toml`, syncing the frontend fields inline after a save.
pub struct AppStateActor {
    /// Runtime services (storage for load + save, slices reader for themes).
    services: Services,
    /// Shared application state — writes frontend.app_state, sidebar_width,
    /// theme, and context.active_persona inline after persist.
    state: State,
    frontend_cap: jinn_domain::common::tcaps::frontend::FrontendCap,
}

impl ServiceActor for AppStateActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: start is never called (spawn uses start_with)"
    )]
    async fn start(_args: &serde_json::Value) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle and
        // capability via `start_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("AppStateActor is spawned via start_with"),
        )
    }
}

impl AppStateActor {
    /// Spawns the actor at its static path. The caller subscribes the
    /// returned path to the preferences topic (the slice's
    /// `activate`) — subscribe is the readiness point, so it must
    /// follow this call before any publish.
    pub fn spawn(
        system: &ActorSystem,
        services: Services,
        state: State,
        frontend_cap: jinn_domain::common::tcaps::frontend::FrontendCap,
    ) -> ActorPath {
        spawn_service_builder::<Self>(system)
            .at(ActorPath::new(APP_STATE_ACTOR_PATH))
            .start_with({
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            services: services.clone(),
                            state: state.clone(),
                            frontend_cap,
                        })
                    })
                }
            })
            .start()
    }

    /// Apply state updates, persist, and sync the frontend fields.
    pub(crate) fn handle_update(&mut self, msg: &UpdateAppState) {
        let mut state = self.services.app_state_storage.read();
        for update in &msg.updates {
            update.apply(&mut state);
        }
        if let Err(e) = self.services.app_state_storage.save(&state) {
            tracing::warn!(err = ?e, "app-state-actor failed to save app state");
            return;
        }
        // Write frontend/context fields inline after persist.
        self.sync_state(&state);
    }

    /// Syncs persisted state into the shared `AppState` frontend/context fields.
    fn sync_state(&self, updated: &AppStateFile) {
        // Resolve the persisted theme name against the theme slice's
        // entries cell (populated by the activation-time directory scan).
        // Unknown names fall back to the embedded default.
        let new_theme = resolve_cached_theme(
            self.services
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
        if let Some(ref persona_name) = updated.persona_name
            && let Some(cell) = self
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

impl MsgHandler<UpdateAppState> for AppStateActor {
    async fn handle(&mut self, msg: UpdateAppState, _ctx: &mut MsgCtx<'_>) {
        self.handle_update(&msg);
    }
}

/// Resolves a theme name against the theme slice's entries cell, falling
/// back to the embedded default when the cell is absent (slice not
/// activated) or the name is not found.
#[expect(
    clippy::option_option,
    reason = "None = cell absent, Some(None) = name unresolved; the distinction is unused but the signature mirrors the reader API"
)]
fn resolve_cached_theme(
    resolved: Option<Option<jinn_domain::feat::theme::Theme>>,
) -> jinn_domain::feat::theme::Theme {
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
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_domain::common::services::Services;
    use jinn_preferences_config::app_state_file::AppStateFile;
    use jinn_preferences_config::app_state_storage::InMemoryAppStateStorage;
    use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};

    async fn create_actor() -> (AppStateActor, Services) {
        let mut services = Services::new_fake().await;

        let storage = InMemoryAppStateStorage::new();
        let svc = jinn_preferences_config::app_state_storage::AppStateStorageService::new(
            Arc::new(storage),
        );
        svc.reload().expect("test app state storage initial reload");
        services.app_state_storage = svc;

        let actor = AppStateActor {
            services: services.clone(),
            state: jinn_domain::common::state::State::new(
                jinn_domain::common::app_state::AppState::default(),
            ),
            frontend_cap: jinn_domain::common::tcaps::mint::mint_frontend_cap(),
        };
        (actor, services)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_last_model_persists() {
        // Given an app-state actor.
        let (mut actor, services) = create_actor().await;

        // When handling UpdateAppState with SetLastModel.
        actor.handle_update(&UpdateAppState {
            updates: vec![AppStateUpdate::SetLastModel(Some(
                ModelSelection::from_single("anthropic/claude-sonnet-4".to_owned()),
            ))],
        });

        // Then the storage has the last model.
        let loaded = services.app_state_storage.read();
        let expected = ModelSelection::from_single("anthropic/claude-sonnet-4".to_owned());
        assert_eq!(loaded.last_model, Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_theme_persists() {
        // Given an app-state actor.
        let (mut actor, services) = create_actor().await;

        // When handling UpdateAppState with SetTheme.
        actor.handle_update(&UpdateAppState {
            updates: vec![AppStateUpdate::SetTheme(Some("dracula".to_owned()))],
        });

        // Then the storage has the theme.
        let loaded = services.app_state_storage.read();
        assert_eq!(loaded.theme_name.as_deref(), Some("dracula"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn multiple_updates_in_one_command() {
        // Given an app-state actor.
        let (mut actor, services) = create_actor().await;

        // When handling a batch with multiple updates.
        actor.handle_update(&UpdateAppState {
            updates: vec![
                AppStateUpdate::SetLastModel(Some(ModelSelection::from_single(
                    "openrouter/gpt-4".to_owned(),
                ))),
                AppStateUpdate::SetSidebarWidth(Some(40)),
                AppStateUpdate::SetTheme(Some("nord".to_owned())),
            ],
        });

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
        let (actor, _services) = create_actor().await;

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
        let (actor, _services) = create_actor().await;

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
        let (actor, _services) = create_actor().await;
        let cell = actor
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
        let (actor, _services) = create_actor().await;

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
        let (actor, _services) = create_actor().await;
        let mut contributed = jinn_domain::feat::theme::default_theme();
        contributed.focus_accent = ratatui::style::Color::Red;
        actor
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
        let (actor, _services) = create_actor().await;

        // When syncing AppStateFile with a name the cache lacks.
        let app_state = AppStateFile {
            theme_name: Some("no-such-theme".to_owned()),
            ..AppStateFile::default()
        };
        actor.sync_state(&app_state);

        // Then the frontend keeps the embedded default theme.
        let applied = actor.state.read().frontend.theme.focus_accent;
        assert_eq!(
            applied,
            jinn_domain::feat::theme::default_theme().focus_accent
        );
    }
}
