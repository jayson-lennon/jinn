//! App-state actor — persists runtime state to `state.toml`.
//!
//! A trouper [`ServiceActor`] declaring `UpdateAppState` as handled;
//! handles [`UpdateAppState`] commands carrying batches of
//! [`AppStateUpdate`] diffs. On each command, loads current state,
//! applies all diffs, saves to disk, and syncs the frontend
//! theme/sidebar/persona fields inline.

use jinn_kernel::common::services::Services;
use jinn_kernel::common::state::State;
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
}

impl ServiceActor for AppStateActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the state handle
        // via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("AppStateActor is spawned via init_with"),
        )
    }
}

impl AppStateActor {
    /// Spawns the actor at its static path, declaring `UpdateAppState`
    /// as handled — the declaration registers the command's route (its
    /// sole handler), so bridge-published commands deliver here.
    pub fn spawn(system: &ActorSystem, services: Services, state: State) -> ActorPath {
        spawn_service_builder::<Self>(system)
            .at(ActorPath::new(APP_STATE_ACTOR_PATH))
            .init_with({
                move || {
                    Box::pin(async move {
                        Ok(Self {
                            services: services.clone(),
                            state: state.clone(),
                        })
                    })
                }
            })
            .handles::<UpdateAppState>()
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
        self.state.with_frontend_state(|ops| {
            let frontend = ops.frontend();
            frontend.app_state = updated.clone();
            frontend.sidebar_width = updated.sidebar_width.unwrap_or(30);
            frontend.theme = new_theme.clone();
        });

        // The chat log's cached lines embed the old theme's colours, so a
        // theme switch must drop them. The cache is a jinn-chat-log-view
        // cell, so the clear goes through the cell rather than a kernel-level
        // sweep of every cache.
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_chat_log_view_msg::EntryLineCache>(
                &jinn_chat_log_view_msg::entry_line_cache_slot(),
            )
        {
            cell.update(jinn_chat_log_view_msg::EntryLineCache::clear);
        }

        // The skills picker caches its preview lines already rendered in the
        // old theme's colors, so a theme switch must drop them or the open
        // menu keeps showing stale text. The cache lives in the skills slice's
        // cell now, so the clear happens there rather than in the kernel's
        // a slice's own cell — a slice owns its own cached rendering.
        if let Some(cell) = self
            .services
            .slices
            .reader::<jinn_skills_msg::SkillPickerState>(&jinn_skills_msg::skill_picker_slot())
        {
            cell.update(|picker| picker.preview_cache.clear());
        }

        // The session preview holds rendered lines, not just counts, so the same
        // reasoning applies: they carry the old theme's colors. It lives in the
        // sidebar's cell, and is cleared here alongside the others.
        //
        // Reset to `Idle` rather than to `Ready`-with-nothing: the next cursor
        // move then requests a re-render in the new theme, and until one arrives
        // the popup shows its spinner rather than the old colors.
        self.state.with_frontend_state(|ops| {
            ops.frontend()
                .update_sections(|s| s.sessions.preview.reset());
        });
    }
}

impl MsgHandler<UpdateAppState> for AppStateActor {
    async fn handle(&mut self, msg: &UpdateAppState, _ctx: &mut MsgCtx<'_>) {
        self.handle_update(msg);
    }
}

/// Resolves a theme name against the theme slice's entries cell, falling
/// back to the embedded default when the cell is absent (slice not
/// activated) or the name is not found.
#[expect(
    clippy::option_option,
    reason = "None = cell absent, Some(None) = name unresolved; the distinction is unused but the signature mirrors the reader API"
)]
fn resolve_cached_theme(resolved: Option<Option<jinn_theme::Theme>>) -> jinn_theme::Theme {
    resolved.flatten().unwrap_or_else(jinn_theme::default_theme)
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
    use jinn_kernel::common::services::Services;
    use jinn_preferences_config::app_state_file::AppStateFile;
    use jinn_preferences_config::app_state_storage::InMemoryAppStateStorage;
    use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
    use jinn_selection_widget::PreviewCache as _;

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
            state: jinn_kernel::common::state::State::new(
                jinn_kernel::common::app_state::AppState::default(),
            ),
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
    async fn set_theme_clears_the_skills_picker_preview_cache() {
        // Given an app-state actor with a skills picker whose preview cache
        // holds lines rendered in the previous theme.
        let (mut actor, services) = create_actor().await;
        // `Services::new_fake` seeds its registry through the shared cell
        // catalog, which already owns the skills picker cell this test
        // inspects; it only needs a handle to it.
        let cell = services
            .slices
            .reader::<jinn_skills_msg::SkillPickerState>(&jinn_skills_msg::skill_picker_slot())
            .expect("the catalog registers the skills picker cell");
        cell.update(|picker| {
            picker.preview_cache.insert(
                "12345".to_owned(),
                80,
                vec![ratatui::text::Line::raw("old-theme")].into(),
            );
        });
        assert!(!cell.read().preview_cache.is_empty());

        // When the theme changes.
        actor.handle_update(&UpdateAppState {
            updates: vec![AppStateUpdate::SetTheme(Some("dracula".to_owned()))],
        });

        // Then the cached preview lines are dropped, so the open picker cannot
        // keep rendering the previous theme's colors.
        assert!(
            cell.read().preview_cache.is_empty(),
            "a theme change must clear the skills picker's cached preview lines"
        );
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
        let mut contributed = jinn_theme::default_theme();
        contributed.focus_accent = ratatui::style::Color::Red;
        // `Services::new_fake` seeds its registry through the shared cell
        // catalog, which already owns the theme entries cell; seed the
        // scanned entry through it rather than minting a second cell.
        actor
            .services
            .slices
            .reader::<jinn_theme_msg::ThemeEntries>(&jinn_theme_msg::theme_entries_slot())
            .expect("the catalog registers the theme entries cell")
            .update(|entries| {
                entries.entries = vec![jinn_theme_msg::NamedTheme {
                    name: "dracula".to_owned(),
                    theme: contributed.clone(),
                }];
            });

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
        assert_eq!(applied, jinn_theme::default_theme().focus_accent);
    }
}
