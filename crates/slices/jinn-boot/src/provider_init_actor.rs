//! Provider initialization actor - loads static config, merges cache, resolves `last_model`.
//!
//! Subscribes to [`EnvironmentLoaded`](super::EnvironmentLoaded) emitted by the
//! env init actor. On receipt: builds the `ProviderRegistry` from the config,
//! replaces the empty startup registry, loads the model cache from disk, merges
//! cache entries into the registry, loads app state, and if `last_model`
//! is set, sends a `ProviderSwitch` command to apply it.

use error_stack::Report;
use jinn_boot_msg::{EnvironmentLoaded, ProvidersTomlReloaded};
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::services::bus_service::BusService;
use jinn_kernel::common::state::State;
use jinn_provider_config::{ModelCache, ProviderRegistry};
use jinn_provider_selection_msg::ModelCacheLoaded;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_session_history_msg::PushChatEntry;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

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
    /// Provider-owned cell.
    provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
}

/// Dependencies for [`ProviderInitActor`].
#[derive(Clone)]
pub struct ProviderInitActorDeps {
    /// Shared dependencies.
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
    /// Provider-owned cell.
    pub provider_cell: jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell>,
}

impl ServiceActor for ProviderInitActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec)
            .attach("ProviderInitActor spawns via init_with"))
    }
}

/// Static path the provider-init actor spawns at (one instance per process).
pub const PROVIDER_INIT_PATH: &str = "jinn.init.provider";

impl ProviderInitActor {
    /// Spawns the provider-init actor onto the trouper system; its
    /// `EnvironmentLoaded` subscription is live when this returns.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    /// # Panics
    ///
    /// Panics if the actor's path is already taken or its topic
    /// subscription fails — both mean a wiring bug at composition.
    pub fn spawn(system: &trouper::system::ActorSystem, deps: ProviderInitActorDeps) -> ActorPath {
        let path = ActorPath::new(PROVIDER_INIT_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            deps: deps.deps,
                            state: deps.state,
                            provider_cell: deps.provider_cell,
                        })
                    })
                }
            })
            .handles::<EnvironmentLoaded>()
            // Live config: a providers.toml change rebuilds the registry.
            .handles::<ProvidersTomlReloaded>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<EnvironmentLoaded> for ProviderInitActor {
    async fn handle(&mut self, msg: &EnvironmentLoaded, _ctx: &mut MsgCtx<'_>) {
        self.on_environment_loaded(&msg.config).await;
    }
}

// Live config: a providers.toml change rebuilds the registry, re-merges
// the model cache, and republishes `ModelCacheLoaded` (whose handler
// refreshes the picker entries). Deliberately NOT re-run from startup:
// the `last_model` → `ProviderSwitch` tail would clobber a model the
// user switched to at runtime, and the no-api-keys guidance is a
// once-per-launch message, not a condition to re-report on every save.
// In-flight streams keep their already-created factories; dispatch
// resolves registry entries and keys per request, so the next request
// observes the new registry.
impl MsgHandler<ProvidersTomlReloaded> for ProviderInitActor {
    async fn handle(&mut self, _msg: &ProvidersTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_reload().await;
    }
}

impl ProviderInitActor {
    /// Re-runs the registry build against the current storage.
    ///
    /// The reload path stops after the cache and picker refresh: no
    /// `last_model` re-application (a runtime model choice must survive
    /// an edit) and no no-api-keys guidance re-push (a reload is not a
    /// startup).
    pub async fn handle_reload(&self) {
        // Given a providers.toml whose contents just changed (already
        // parse-gated by the watcher).
        let Ok(config) = self.deps.services.config_storage.load() else {
            tracing::error!("provider-init reload could not load config; keeping the old registry");
            return;
        };
        let Ok(registry) = ProviderRegistry::from_config(config) else {
            tracing::error!("provider-init reload could not build a registry; keeping the old one");
            return;
        };
        self.deps.services.provider_registry.replace(registry);

        // Re-merge the cache and announce it — the provider actor's
        // handler refreshes the picker entries from this event.
        let cache_path = self.deps.services.paths.cache_path();
        let cache = ModelCache::load(&cache_path).unwrap_or_else(|e| {
            tracing::warn!("provider-init reload failed to load model cache: {e:?}");
            None
        });
        if let Some(ref c) = cache {
            self.deps.services.provider_registry.merge_cache(c);
            self.publish(ModelCacheLoaded { cache: c.clone() }).await;
        }
        self.provider_cell.update(|cell| {
            cell.model_cache = cache;
        });
    }
}

impl BusPublish for ProviderInitActor {
    fn bus(&self) -> &BusService {
        &self.deps.services.bus
    }
}

impl ProviderInitActor {
    /// Builds registry, merges cache, resolves `last_model`.
    async fn on_environment_loaded(&self, config: &jinn_provider_config::ProvidersConfig) {
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
                entry: crate::no_api_keys::no_api_keys_msg(),
                pin: None,
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
        self.provider_cell.update(|cell| {
            cell.model_cache = cache;
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
            let id = jinn_provider_config::ProviderId::new(model_str.to_owned());
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
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_kernel::common::services::Services;
    use jinn_kernel::common::services::bus_service::BusAudit;
    use jinn_kernel::common::state::State;
    use jinn_provider_config::ConfigStorage as _;
    use jinn_provider_config::ProviderEntry;
    use jinn_provider_selection_msg::ModelCacheLoaded;
    use jinn_provider_selection_msg::ProviderSwitch;
    use jinn_session_history_msg::PushChatEntry;

    /// A lone provider cell for direct actor-construction tests.
    fn test_provider_cell() -> jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell> {
        let slices = jinn_slices::Slices::new();
        let _ = slices.register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        );
        slices
            .reader(&jinn_provider_selection_msg::provider_state_slot())
            .expect("just registered")
    }

    async fn create_actor() -> (ProviderInitActor, BusAudit, Services, State) {
        let (bus, audit) = jinn_kernel::common::services::BusService::new_recording();
        let services = Services::new_fake_with_bus(bus).await;
        let state = State::new(jinn_kernel::common::app_state::AppState::default());
        let actor = ProviderInitActor {
            deps: ActorDeps {
                services: services.clone(),
            },
            state: state.clone(),
            provider_cell: test_provider_cell(),
        };
        (actor, audit, services, state)
    }

    fn sample_config() -> jinn_provider_config::ProvidersConfig {
        jinn_provider_config::ProvidersConfig {
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
            endpoint_defaults: vec![],
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
    async fn a_providers_reload_replaces_the_registry_without_a_provider_switch() {
        // Given a provider-init actor whose registry holds one provider.
        let (actor, audit, services, _state) = create_actor().await;
        let config = sample_config();
        actor.on_environment_loaded(&config).await;
        let switches_before = audit.of_type::<ProviderSwitch>().len();

        // When a reload lands whose config no longer lists any provider.
        let empty = jinn_provider_config::ProvidersConfig {
            providers: std::collections::BTreeMap::new(),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        };
        let storage = jinn_provider_config::InMemoryConfigStorage::new();
        storage.save(&empty).expect("empty config saves");
        services
            .config_storage
            .swap(std::sync::Arc::new(storage)
                as std::sync::Arc<dyn jinn_provider_config::ConfigStorage>);
        actor.handle_reload().await;

        // Then the registry no longer resolves the removed provider, and
        // no ProviderSwitch was re-sent (a runtime model choice survives).
        let id = jinn_provider_config::ProviderId::new("sample".to_owned());
        assert!(
            services.provider_registry.get(&id).is_none(),
            "the removed provider is gone from the registry"
        );
        assert_eq!(
            audit.of_type::<ProviderSwitch>().len(),
            switches_before,
            "the reload must not re-apply last_model"
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

        let mut cache = jinn_provider_config::ModelCache::new();
        cache.entries.insert(
            "ollama".to_owned(),
            vec![jinn_provider_config::ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: jinn_provider_config::InputModalities::text(),
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
            .write()
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

    // P8-add: a provider that appears only via a reload lands in the
    // registry — availability and lookup flip without a restart.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_provider_added_by_a_reload_resolves_in_the_registry() {
        // Given a provider-init actor whose registry holds `sample` only.
        let (actor, _audit, services, _state) = create_actor().await;
        actor.on_environment_loaded(&sample_config()).await;

        // When a reload lands whose config adds `extra` (and keeps
        // `sample`).
        let mut config = sample_config();
        config.providers.insert(
            "extra".to_owned(),
            ProviderEntry {
                model_info: Vec::new(),
                backend: "sample".to_owned(),
                models: vec!["extra-model".to_owned()],
                base_url: None,
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
            },
        );
        let storage = jinn_provider_config::InMemoryConfigStorage::new();
        storage.save(&config).expect("two-provider config saves");
        services
            .config_storage
            .swap(std::sync::Arc::new(storage)
                as std::sync::Arc<dyn jinn_provider_config::ConfigStorage>);
        actor.handle_reload().await;

        // Then the registry resolves both providers (expanded ids are
        // `provider/model`).
        for id in ["sample/sample", "extra/extra-model"] {
            let id = jinn_provider_config::ProviderId::new(id.to_owned());
            assert!(
                services.provider_registry.get(&id).is_some(),
                "{id} resolves after the reload added it"
            );
        }
    }

    // P11: an in-flight stream is untouched by a providers reload. The
    // factory a stream was built from is held by the stream, not re-read
    // from the registry, so a reload that removes its provider cannot
    // break a dispatch already in flight.
    #[rstest::rstest]
    #[tokio::test]
    async fn a_factory_created_before_a_reload_keeps_working_after_it() {
        // Given a registry whose provider resolves through the sample
        // factory (the `--fake-llm` one), with the factory created BEFORE
        // any reload.
        let (actor, _audit, services, _state) = create_actor().await;
        let registry = jinn_provider_config::ProviderRegistry::from_config(sample_config())
            .expect("config expands")
            .with_factory_override(std::sync::Arc::new(jinn_provider::SampleLlmServiceFactory));
        services.provider_registry.replace(registry);
        let keys = jinn_provider_config::ApiKeys::default();
        let id = jinn_provider_config::ProviderId::new("sample/sample".to_owned());
        let pre_reload_factory = services
            .provider_registry
            .create_factory(&id, &keys, None, None)
            .expect("the pre-reload factory resolves");

        // When a reload replaces the registry with one that no longer
        // knows the provider.
        let storage = jinn_provider_config::InMemoryConfigStorage::new();
        storage
            .save(&jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            })
            .expect("empty config saves");
        services
            .config_storage
            .swap(std::sync::Arc::new(storage)
                as std::sync::Arc<dyn jinn_provider_config::ConfigStorage>);
        actor.handle_reload().await;

        // Then the registry no longer resolves the provider, but the
        // held factory still answers `create` — the in-flight stream
        // never re-reads the registry, so it keeps its own factory.
        assert!(
            services.provider_registry.get(&id).is_none(),
            "the removed provider is gone from the registry"
        );
        // `create` succeeding is the observable: the FactoryOverride path
        // is still the sample factory, not a registry miss.
        pre_reload_factory
            .create()
            .expect("the pre-reload factory still builds a service");
    }
}
