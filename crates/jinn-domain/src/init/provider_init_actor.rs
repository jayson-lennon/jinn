//! Provider initialization actor - loads static config, merges cache, resolves `last_model`.
//!
//! Subscribes to [`EnvironmentLoaded`](super::EnvironmentLoaded) emitted by the
//! env init actor. On receipt: builds the `ProviderRegistry` from the config,
//! replaces the empty startup registry, loads the model cache from disk, merges
//! cache entries into the registry, loads app state, and if `last_model`
//! is set, sends a `ProviderSwitch` command to apply it.

use crate::common::actor_deps::{ActorDeps, BusPublish};
use crate::common::services::bus_service::BusService;
use crate::common::state::State;
use crate::common::tcaps::provider::ModelCacheWrite;
use crate::feat::provider::protocol::command::ProviderSwitch;
use crate::feat::provider::protocol::event::ModelCacheLoaded;
use crate::feat::provider_infra::{ModelCache, ProviderRegistry};
use crate::init::EnvironmentLoaded;
use jinn_session_history_msg::PushChatEntry;
use kameo::prelude::{Actor, ActorRef, Context, Message};

/// The provider initialization actor.
///
/// On `EnvironmentLoaded`: builds the registry from config, replaces the empty
/// registry in `ProviderRegistryService`, loads the model cache, merges into
/// registry, loads app state, and sends `ProviderSwitch` if `last_model`
/// is set.
pub struct ProviderInitActor {
    /// Shared dependencies.
    deps: ActorDeps,
    /// Shared application state (to read active session ID).
    state: State,
    /// Provider write capability.
    provider_cap: crate::common::tcaps::provider::ProviderCap,
}

/// Dependencies for [`ProviderInitActor`].
#[derive(Clone)]
pub struct ProviderInitActorDeps {
    /// Shared dependencies.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Provider write capability.
    pub provider_cap: crate::common::tcaps::provider::ProviderCap,
}

impl Actor for ProviderInitActor {
    type Args = ProviderInitActorDeps;
    type Error = std::convert::Infallible;

    async fn on_start(args: Self::Args, actor_ref: ActorRef<Self>) -> Result<Self, Self::Error> {
        args.deps
            .subscribe(actor_ref.recipient::<EnvironmentLoaded>())
            .await;
        Ok(Self {
            deps: args.deps,
            state: args.state,
            provider_cap: args.provider_cap,
        })
    }
}

impl BusPublish for ProviderInitActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl Message<EnvironmentLoaded> for ProviderInitActor {
    type Reply = ();

    async fn handle(&mut self, msg: EnvironmentLoaded, _ctx: &mut Context<Self, Self::Reply>) {
        self.on_environment_loaded(&msg.config).await;
    }
}

impl ProviderInitActor {
    /// Builds registry, merges cache, resolves `last_model`.
    async fn on_environment_loaded(&self, config: &crate::feat::provider_infra::ProvidersConfig) {
        // Build registry from config and replace the empty one.
        let registry = match ProviderRegistry::from_config(config.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(err = ?e, "provider-init failed to build registry from config");
                return;
            }
        };
        self.deps.services.provider_registry.replace(registry);

        // Check if no API keys were resolved. If so, push a guidance message.
        if self.deps.services.api_keys.is_empty() {
            tracing::warn!("no API keys found, showing guidance message");
            let session_id = self.state.read().session.active_session_id().clone();
            self.publish(PushChatEntry {
                session_id,
                entry: crate::feat::session::no_api_keys_msg(),
            })
            .await;
        }

        // Load model cache from disk and merge into registry.
        let cache_path = self.deps.services.paths.cache_path();
        let cache = ModelCache::load(&cache_path).unwrap_or_else(|e| {
            tracing::warn!("provider-init failed to load model cache: {e:?}");
            None
        });
        if let Some(ref c) = cache {
            tracing::info!(providers = c.entries.len(), "loaded model cache");
            self.deps.services.provider_registry.merge_cache(c);
            self.publish(ModelCacheLoaded { cache: c.clone() }).await;
        }
        self.state.with_provider(&self.provider_cap, |view| {
            view.provider.set_model_cache(cache);
        });

        let app_state = self.deps.services.app_state_storage.read();

        // If last_model is set, send ProviderSwitch to apply it.
        // Skip if the active session already has an explicit model (e.g., bench sessions
        // created with a CLI-specified model). Those sessions must keep their model.
        let active_session_model = {
            let state = self.state.read();
            state.active_session().profile().model.clone()
        };
        if active_session_model.is_no_provider()
            && let Some(ref selection) = app_state.last_model
        {
            let model_str = selection.display_str();
            let id = crate::feat::provider_infra::ProviderId::new(model_str.to_owned());
            let is_available = {
                let api_keys = self.deps.services.api_keys.read();
                self.deps
                    .services
                    .provider_registry
                    .is_available(&id, &api_keys)
            };
            if is_available {
                let session_id = self.state.read().session.active_session_id().clone();
                tracing::info!(last_model = %selection, "provider-init resolving last_model");
                self.publish(ProviderSwitch {
                    session_id,
                    provider_id: selection.clone(),
                })
                .await;
            } else {
                tracing::warn!(last_model = %selection, "provider-init: last_model not available, skipping");
            }
        }
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

    use std::collections::BTreeMap;

    use super::ProviderInitActor;
    use crate::common::actor_deps::ActorDeps;
    use crate::common::services::Services;
    use crate::common::services::bus_service::BusAudit;
    use crate::common::state::State;
    use crate::feat::provider::protocol::command::ProviderSwitch;
    use crate::feat::provider::protocol::event::ModelCacheLoaded;
    use crate::feat::provider_infra::ProviderEntry;
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_session_history_msg::PushChatEntry;

    async fn create_actor() -> (ProviderInitActor, BusAudit, Services, State) {
        let (bus, audit) = crate::common::services::BusService::new_recording();
        let services = Services::new_fake_with_bus(bus).await;
        let state = State::new(crate::common::app_state::AppState::default());
        let actor = ProviderInitActor {
            deps: ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            provider_cap: crate::common::tcaps::mint::mint_provider_cap(),
        };
        (actor, audit, services, state)
    }

    fn sample_config() -> crate::feat::provider_infra::ProvidersConfig {
        crate::feat::provider_infra::ProvidersConfig {
            providers: BTreeMap::from([(
                "sample".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "sample".to_owned(),
                    models: vec!["sample".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: None,
                },
            )]),
            aliases: vec![],
            default_provider: None,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sends_provider_switch_when_last_model_set() {
        // Given a provider init actor with preferences containing last_model.
        let (actor, audit, services, _state) = create_actor().await;

        services
            .app_state_storage
            .save(&jinn_preferences_config::app_state_file::AppStateFile {
                last_model: Some(ModelSelection::from_single("sample/sample".to_owned())),
                ..Default::default()
            })
            .expect("save app state");

        let config = sample_config();

        // When processing EnvironmentLoaded.
        actor.on_environment_loaded(&config).await;

        // Then a ProviderSwitch command was published.
        let switches: Vec<ProviderSwitch> = audit.of_type::<ProviderSwitch>();
        assert_eq!(switches.len(), 1);
        assert_eq!(
            switches[0].provider_id,
            ModelSelection::Single("sample/sample".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn sends_provider_switch_with_alloy_when_last_model_is_alloy() {
        // Given a provider init actor with last_model set to an alloy.
        let (actor, audit, services, _state) = create_actor().await;

        let alloy = ModelSelection::Alloy {
            models: vec!["sample/alpha".to_owned(), "sample/beta".to_owned()],
            strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 },
        };

        services
            .app_state_storage
            .save(&jinn_preferences_config::app_state_file::AppStateFile {
                last_model: Some(alloy.clone()),
                ..Default::default()
            })
            .expect("save app state");

        let mut config = sample_config();
        config.providers.get_mut("sample").expect("sample").models =
            vec!["alpha".to_owned(), "beta".to_owned()];

        // When processing EnvironmentLoaded.
        actor.on_environment_loaded(&config).await;

        // Then a ProviderSwitch command was published with the alloy.
        let switches: Vec<ProviderSwitch> = audit.of_type::<ProviderSwitch>();
        assert_eq!(switches.len(), 1);
        assert_eq!(switches[0].provider_id, alloy);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn does_not_send_provider_switch_when_no_last_model() {
        // Given a provider init actor with no last_model in preferences.
        let (actor, audit, _services, _state) = create_actor().await;

        let config = sample_config();

        // When processing EnvironmentLoaded.
        actor.on_environment_loaded(&config).await;

        // Then no ProviderSwitch command was published.
        let switches: Vec<ProviderSwitch> = audit.of_type::<ProviderSwitch>();
        assert!(switches.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn pushes_no_api_keys_msg_when_keys_empty() {
        // Given a provider init actor with no API keys.
        let (actor, audit, _services, _state) = create_actor().await;

        let mut config = sample_config();
        config.providers.insert(
            "openrouter".to_owned(),
            ProviderEntry {
                model_info: Vec::new(),
                backend: "openrouter".to_owned(),
                models: vec!["gpt-4".to_owned()],
                base_url: None,
                api_key_env: Some("OPENROUTER_API_KEY".to_owned()),
                requires_key: true,
                extra_body: None,
                context_length: None,
            },
        );

        // When processing EnvironmentLoaded with no API keys resolved.
        actor.on_environment_loaded(&config).await;

        // Then a PushChatEntry was published with no-api-keys guidance.
        let entries: Vec<PushChatEntry> = audit.of_type::<PushChatEntry>();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].entry.text().contains("No API keys found"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn emits_model_cache_loaded_when_cache_exists_on_disk() {
        // Given a provider init actor with a cache file on disk.
        let (actor, audit, services, _state) = create_actor().await;

        let mut cache = crate::feat::provider_infra::ModelCache::new();
        cache.entries.insert(
            "ollama".to_owned(),
            vec![crate::feat::provider_infra::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: crate::feat::provider_infra::InputModalities::text(),
            }],
        );
        cache.last_updated_at = Some(jiff::Timestamp::now());
        let cache_path = services.paths.cache_path();
        cache.save(&cache_path).expect("save cache");

        let mut config = sample_config();
        config.providers.insert(
            "ollama".to_owned(),
            ProviderEntry {
                model_info: Vec::new(),
                backend: "ollama".to_owned(),
                models: vec!["llama3".to_owned()],
                base_url: None,
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
            },
        );

        // When processing EnvironmentLoaded.
        actor.on_environment_loaded(&config).await;

        // Then a ModelCacheLoaded event was published.
        let loaded: Vec<ModelCacheLoaded> = audit.of_type::<ModelCacheLoaded>();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].cache.entries.contains_key("ollama"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn does_not_send_provider_switch_when_session_has_explicit_model() {
        // Given a provider init actor with app state containing last_model
        // but the active session already has an explicitly set model.
        let (actor, audit, services, state) = create_actor().await;

        // Set an explicit model on the active session.
        state
            .write_test_no_cap()
            .active_session_mut()
            .set_model(ModelSelection::Single("bench-model".to_owned()));

        services
            .app_state_storage
            .save(&jinn_preferences_config::app_state_file::AppStateFile {
                last_model: Some(ModelSelection::from_single("sample/sample".to_owned())),
                ..Default::default()
            })
            .expect("save app state");

        let config = sample_config();

        // When processing EnvironmentLoaded.
        actor.on_environment_loaded(&config).await;

        // Then no ProviderSwitch command was published.
        let switches: Vec<ProviderSwitch> = audit.of_type::<ProviderSwitch>();
        assert!(switches.is_empty());
    }
}
