//! Startup handler - applies config defaults on environment load.
//!
//! On startup, loads user preferences, applies model/strategy defaults to the
//! default session, loads unarchived sessions from SQLite, and emits commands
//! to initialize the context and preferences pipelines.

use super::super::SessionPersistenceActor;
use crate::common::actor_deps::BusPublish;
use jinn_preferences_config::protocol::app_state_command::{AppStateUpdate, UpdateAppState};

impl SessionPersistenceActor {
    /// Applies config defaults to the default session profile on startup.
    ///
    /// Loads app state and applies `last_model`
    /// to the default session, then sends an `UpdateAppState` command so
    /// the state pipeline handles persistence and state sync.
    ///
    /// NOTE: Using `active_session_mut()` is acceptable here because this runs
    /// at startup before any user interaction. There is only one session.
    pub(in crate::feat::session::session_actor) async fn on_environment_loaded(
        &self,
        _config: &crate::feat::provider_infra::ProvidersConfig,
    ) {
        let app_state = self.services.app_state_storage.read();
        let prefs = self.services.user_preferences_storage.read();

        // When the welcome session seeds enabled MCP servers from
        // `auto_enable`, the coordinator is notified here. This only works
        // because actor_wiring spawns and fully awaits the coordinator
        // (which subscribes to `McpEnablementChanged` on start) BEFORE any
        // publisher emits `EnvironmentLoaded` — do not reorder that wiring.
        let mut welcome_mcp_enablement: Option<(
            crate::protocol::SessionId,
            std::collections::BTreeSet<String>,
        )> = None;

        {
            // Apply config defaults to the default session.
            // Only set the model if the session still has the no-provider sentinel.
            // Bench sessions are created with an explicit model before this handler
            // fires, so we must not overwrite them with the user's saved preference.
            self.state.with_session(&self.cap, |view| {
                let session = view.session.map().active_session_mut();
                if session.profile().model.is_no_provider() {
                    if let Some(ref model) = app_state.last_model {
                        session.set_model(model.clone());
                    }
                    // Seed the welcome session's reasoning effort from the
                    // persisted global default, matching every other
                    // session-creation path. Without this, the initial
                    // program-load session shows no thinking-effort bracket
                    // in the status bar until the user creates a new session.
                    session.profile_mut().reasoning_effort = app_state.reasoning_effort;

                    // Seed disablement sets + auto-enabled MCP servers from
                    // jinn.toml, matching every other session-creation path.
                    let seed = crate::feat::session::profile::SessionSeed::from_preferences(&prefs);
                    {
                        let profile = session.profile_mut();
                        profile.disabled_tools.clone_from(&seed.disabled_tools);
                        profile.disabled_skills.clone_from(&seed.disabled_skills);
                    }
                    for server in &seed.enabled_mcp {
                        session.enable_mcp_server(server);
                    }
                    if seed.has_auto_enabled_mcp() {
                        welcome_mcp_enablement =
                            Some((session.session_id().clone(), seed.enabled_mcp.clone()));
                    }
                }
            });

            // Seed frontend.app_state from persisted state so the persona scan
            // (which fires on the same EnvironmentLoaded event) finds persona_name
            // populated when on_personas_loaded resolves the active persona.
            self.state
                .with_frontend_app_state(&self.frontend_cap, |ops| ops.set(app_state.clone()));
        }

        if let Some((session_id, enabled)) = welcome_mcp_enablement {
            self.publish(jinn_mcp_msg::McpEnablementChanged {
                session_id,
                enabled,
            })
            .await;
        }

        tracing::info!("DIAG on_environment_loaded model/strategy applied");

        // Note: no scan commands are emitted here. The three scan actors
        // (skills, prompts, context-files) subscribe to this `EnvironmentLoaded`
        // event directly and self-trigger their per-session scans.

        tracing::info!("DIAG on_environment_loaded loading unarchived sessions");

        // Load unarchived sessions from SQLite into memory.
        // These are sessions with `archived=false` - corresponding to `SessionState::Loaded`.
        // On load they get the default `SessionState::Loaded` and `LifecycleScriptState::NothingRan`.
        {
            let store = &self.services.session_store;
            let summaries = match store.load_unarchived_summaries().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(err = ?e, "session-actor failed to load unarchived summaries on startup");
                    return;
                }
            };
            tracing::info!(
                count = summaries.len(),
                "DIAG on_environment_loaded summaries loaded"
            );

            if !summaries.is_empty() {
                // Sort by updated_at descending to find the most recent.
                let mut sorted = summaries;
                sorted.sort_by_key(|s| std::cmp::Reverse(s.updated_at));

                // Load full sessions outside the lock.
                let mut loaded = Vec::new();
                for summary in &sorted {
                    if let Ok(Some(session)) = store.load_session(&summary.session_id).await {
                        loaded.push(session);
                    }
                }
                tracing::info!(
                    count = loaded.len(),
                    "DIAG on_environment_loaded sessions loaded"
                );

                tracing::info!("DIAG on_environment_loaded inserting sessions");
                if !loaded.is_empty() {
                    for mut session in loaded {
                        // Mark startup-loaded sessions as interacted - they came from disk.
                        session.mark_interacted();
                        self.load_and_insert(session).await;
                    }

                    // NOTE: We intentionally do NOT switch the active session.
                    // The user should land on the fresh welcome session.
                    // Previously loaded sessions appear in the sidebar but
                    // don't steal focus.

                    // Hydrate frozen nodes for archived tree members.
                    // Archived sessions (e.g., a parent that was archived while its
                    // children remain unarchived) need frozen node snapshots so the
                    // tree summary shows complete historical totals.
                    self.hydrate_all_tree_frozen_nodes(&self.services.session_store)
                        .await;
                    tracing::info!("DIAG on_environment_loaded frozen nodes hydrated");
                }
            }
        }

        tracing::info!("DIAG on_environment_loaded sending UpdateAppState");
        // Send UpdateAppState command so the pipeline handles persistence + state sync.
        self.publish(UpdateAppState {
            updates: vec![AppStateUpdate::SetLastModel(app_state.last_model.clone())],
        })
        .await;
        tracing::info!("DIAG on_environment_loaded DONE");
    }
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
    use super::super::super::helpers::test_actor_with_store_recording;
    use crate::feat::session::chat_session::ChatSessionState;
    use jinn_core_types::model_selection::ModelSelection;

    #[rstest::rstest]
    #[tokio::test]
    async fn loading_unarchived_sessions_does_not_switch_active_session() {
        // Given an actor with a default welcome session and one session in the store.
        let store_session = ChatSessionState::new();
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![store_session]).await;

        // Record the default session's ID before loading.
        let default_id = actor.state.read().session.active_session_id().clone();

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the active session is still the default.
        let state = actor.state.read();
        assert_eq!(*state.session.active_session_id(), default_id);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn on_environment_loaded_emits_no_scan_commands() {
        // Given an actor with a default welcome session.
        let (actor, _store, audit) = test_actor_with_store_recording(vec![]).await;

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then no scan commands are emitted. The three scan actors
        // (skills, prompts, context-files) subscribe to this `EnvironmentLoaded`
        // event directly and self-trigger their per-session scans.
        assert!(
            !audit.contains_name("ScanSkills"),
            "should not emit ScanSkills"
        );
        assert!(
            !audit.contains_name("RescanPromptTemplates"),
            "should not emit RescanPromptTemplates"
        );
        assert!(
            !audit.contains_name("ScanContextFiles"),
            "should not emit ScanContextFiles"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn loading_unarchived_sessions_does_not_remove_default_session() {
        // Given an actor with a default welcome session and one session in the store.
        let store_session = ChatSessionState::new();
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![store_session]).await;

        let default_id = actor.state.read().session.active_session_id().clone();

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the default session still exists in the map.
        let state = actor.state.read();
        assert!(
            state.session.contains(&default_id),
            "default session should not be removed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn loading_unarchived_sessions_inserts_them_into_session_map() {
        // Given an actor with a default session and two sessions in the store.
        let store_session1 = ChatSessionState::new();
        let store_id1 = store_session1.session_id().clone();
        let store_session2 = ChatSessionState::new();
        let store_id2 = store_session2.session_id().clone();
        let (actor, _audit, _store) =
            test_actor_with_store_recording(vec![store_session1, store_session2]).await;

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then both loaded sessions are in the session map.
        let state = actor.state.read();
        assert!(
            state.session.contains(&store_id1),
            "first store session should be in map"
        );
        assert!(
            state.session.contains(&store_id2),
            "second store session should be in map"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn saved_model_overwrites_no_provider_sentinel() {
        // Given an actor with a default session (NO_PROVIDER_ID model)
        // and saved preferences with a last_model.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // Save state with a last_model.
        let state_file = jinn_preferences_config::app_state_file::AppStateFile {
            last_model: Some(ModelSelection::from_single("my-model".to_owned())),
            ..Default::default()
        };
        actor
            .services
            .app_state_storage
            .save(&state_file)
            .expect("save state");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the active session's model was updated from the saved preference.
        let state = actor.state.read();
        assert_eq!(
            state.active_session().profile().model,
            ModelSelection::Single("my-model".to_owned()),
            "default session model should be updated from saved preference"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn saved_model_does_not_overwrite_explicitly_set_model() {
        // Given an actor with a session that has an explicit model (not NO_PROVIDER_ID)
        // and saved preferences with a different last_model.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // Set an explicit model on the active session (simulating bench actor behavior).
        {
            let mut state = actor.state.write_test_no_cap();
            state
                .active_session_mut()
                .set_model(ModelSelection::Single("bench-model".to_owned()));
        }

        // Save state with a different last_model.
        let state_file = jinn_preferences_config::app_state_file::AppStateFile {
            last_model: Some(ModelSelection::from_single("wrong-model".to_owned())),
            ..Default::default()
        };
        actor
            .services
            .app_state_storage
            .save(&state_file)
            .expect("save state");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        let state = actor.state.read();
        assert_eq!(
            state.active_session().profile().model,
            ModelSelection::Single("bench-model".to_owned()),
            "explicitly set model should not be overwritten by saved preference"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_seeds_persisted_persona_into_frontend_app_state() {
        // Given an actor and saved app state with a persona_name.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;
        let state_file = jinn_preferences_config::app_state_file::AppStateFile {
            persona_name: Some("general".to_owned()),
            ..Default::default()
        };
        actor
            .services
            .app_state_storage
            .save(&state_file)
            .expect("save state");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then frontend.app_state.persona_name is seeded so the persona scan
        // (which fires on the same EnvironmentLoaded event) can resolve it.
        let state = actor.state.read();
        assert_eq!(
            state.frontend.app_state.persona_name.as_deref(),
            Some("general"),
            "startup should seed frontend.app_state from persisted state"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_seeds_none_persona_when_state_absent() {
        // Given an actor with default (empty) app state storage.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then frontend.app_state.persona_name is None (falls back to
        // DEFAULT_PERSONA_NAME downstream in on_personas_loaded, not here).
        let state = actor.state.read();
        assert_eq!(
            state.frontend.app_state.persona_name, None,
            "startup should not fabricate a persona name when none is persisted"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn saved_reasoning_effort_seeds_into_welcome_session() {
        // Given an actor with a default welcome session (NO_PROVIDER_ID model)
        // and saved preferences with a reasoning_effort.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // Save state with a reasoning_effort.
        let state_file = jinn_preferences_config::app_state_file::AppStateFile {
            reasoning_effort: Some(crate::ReasoningEffort::High),
            ..Default::default()
        };
        actor
            .services
            .app_state_storage
            .save(&state_file)
            .expect("save state");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the welcome session's reasoning effort is seeded from the saved value.
        let state = actor.state.read();
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(crate::ReasoningEffort::High),
            "welcome session reasoning effort should be seeded from the saved preference"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn saved_reasoning_effort_does_not_seed_into_explicit_model_session() {
        // Given an actor with a session that has an explicit model (not
        // NO_PROVIDER_ID, simulating a bench session) and saved preferences with
        // a reasoning_effort.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // Set an explicit model on the active session (simulating bench actor behavior).
        {
            let mut state = actor.state.write_test_no_cap();
            state
                .active_session_mut()
                .set_model(ModelSelection::Single("bench-model".to_owned()));
        }

        // Save state with a reasoning_effort.
        let state_file = jinn_preferences_config::app_state_file::AppStateFile {
            reasoning_effort: Some(crate::ReasoningEffort::High),
            ..Default::default()
        };
        actor
            .services
            .app_state_storage
            .save(&state_file)
            .expect("save state");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the explicit-model session's reasoning effort is left as None,
        // not overwritten by the saved preference.
        let state = actor.state.read();
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            None,
            "explicit-model (bench) session reasoning effort should not be seeded"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_seeds_none_reasoning_effort_when_unpersisted() {
        // Given an actor with default (empty) app state storage.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the welcome session's reasoning effort is None (no fabricated value).
        let state = actor.state.read();
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            None,
            "startup should not fabricate a reasoning effort when none is persisted"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_seeds_disabled_tools_and_skills_into_welcome_session() {
        // Given an actor whose preferences storage disables a tool and a skill.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;
        let prefs_with_disablement = jinn_preferences_config::user_preferences::UserPreferences {
            disabled_tools: ["bash"].iter().map(|s| (*s).to_owned()).collect(),
            disabled_skills: ["phased-task-loop"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            ..Default::default()
        };
        actor
            .services
            .user_preferences_storage
            .save(&prefs_with_disablement)
            .expect("save prefs");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the welcome session carries both disablement sets.
        let state = actor.state.read();
        assert!(
            state
                .active_session()
                .profile()
                .disabled_tools
                .contains("bash"),
            "welcome session should seed disabled_tools from jinn.toml"
        );
        assert!(
            state
                .active_session()
                .profile()
                .disabled_skills
                .contains("phased-task-loop"),
            "welcome session should seed disabled_skills from jinn.toml"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_seeds_auto_enabled_mcp_and_notifies_coordinator() {
        // Given an actor whose preferences mark one server auto_enable.
        let (actor, _store, audit) = test_actor_with_store_recording(vec![]).await;
        actor
            .services
            .user_preferences_storage
            .save(&prefs_with_auto_enabled_server("excalimate"))
            .expect("save prefs");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the welcome session has the server enabled.
        {
            let state = actor.state.read();
            assert!(
                state.active_session().is_mcp_server_enabled("excalimate"),
                "auto_enable should enable the server on the welcome session"
            );
        }
        // And an McpEnablementChanged event was published so the coordinator
        // spawns the connection.
        assert!(
            audit.contains_name("McpEnablementChanged"),
            "coordinator must be notified of seeded enablement; got {:?}",
            audit.names()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_without_auto_enable_publishes_no_enablement_event() {
        // Given an actor with a configured server that is NOT auto-enabled.
        let (actor, _store, audit) = test_actor_with_store_recording(vec![]).await;
        actor
            .services
            .user_preferences_storage
            .save(&prefs_with_auto_enabled_server_off("manual"))
            .expect("save prefs");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then no McpEnablementChanged was published.
        assert!(
            !audit.contains_name("McpEnablementChanged"),
            "no enablement notification expected when nothing is auto-enabled"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn startup_does_not_seed_explicit_model_session() {
        // Given a session with an explicit model (bench-style) and preferences
        // that disable a tool.
        let (actor, _store, _audit) = test_actor_with_store_recording(vec![]).await;
        actor
            .services
            .user_preferences_storage
            .save(&prefs_with_auto_enabled_server("excalimate"))
            .expect("save prefs");
        {
            let mut state = actor.state.write_test_no_cap();
            state
                .active_session_mut()
                .set_model(ModelSelection::Single("bench-model".to_owned()));
        }
        actor
            .services
            .user_preferences_storage
            .save(
                &jinn_preferences_config::user_preferences::UserPreferences {
                    disabled_tools: ["bash"].iter().map(|s| (*s).to_owned()).collect(),
                    ..Default::default()
                },
            )
            .expect("save prefs");

        // When handling EnvironmentLoaded.
        actor
            .on_environment_loaded(&crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            })
            .await;

        // Then the explicit-model session is NOT seeded (its profile belongs
        // to the bench harness, not jinn.toml defaults).
        let state = actor.state.read();
        assert!(
            !state.active_session().disabled_tools().contains("bash"),
            "explicit-model sessions keep their own disablement sets"
        );
        // And nothing got auto-enabled either.
        assert!(!state.active_session().is_mcp_server_enabled("excalimate"));
    }

    /// Preferences fixture with one `auto_enable`d MCP server.
    fn prefs_with_auto_enabled_server(
        name: &str,
    ) -> jinn_preferences_config::user_preferences::UserPreferences {
        jinn_preferences_config::user_preferences::UserPreferences {
            mcp_server: [(
                name.to_owned(),
                jinn_mcp_msg::McpServerConfig {
                    command: Some("npx".to_owned()),
                    auto_enable: true,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    /// Preferences fixture with one server that has `auto_enable` off.
    fn prefs_with_auto_enabled_server_off(
        name: &str,
    ) -> jinn_preferences_config::user_preferences::UserPreferences {
        jinn_preferences_config::user_preferences::UserPreferences {
            mcp_server: [(
                name.to_owned(),
                jinn_mcp_msg::McpServerConfig {
                    command: Some("npx".to_owned()),
                    auto_enable: false,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }
}
