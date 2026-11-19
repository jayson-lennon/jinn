//! Model discovery actor - discovers available models from configured providers.
//!
//! Subscribes to `RefreshModels` commands and iterates over all configured
//! providers, calling each provider's `list_models()` endpoint for each.
//! Results are saved to disk as a [`ModelCache`] and emitted as a
//! `ModelsRefreshed` event.

use std::collections::HashMap;
use std::time::Duration;

use error_stack::Report;
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::state::State;
use jinn_provider::{
    Backend, LlmServiceError, ModelInfo, OpenAiCompatibleService, ProviderConfig,
    anthropic::AnthropicService, google::GoogleService,
};
use jinn_provider_config::ModelCache;
use jinn_provider_selection_msg::ModelsRefreshed;
use jinn_provider_selection_msg::RefreshModels;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

/// Error type for model discovery failures.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct DiscoverError;

/// Model discovery actor.
///
/// On `RefreshModels`, iterates all provider entries from the registry,
/// builds an LLM provider for each, calls `list_models(None)`, and collects
/// results. Saves the cache to disk and emits `ModelsRefreshed`.
pub struct DiscoverActor {
    deps: ActorDeps,
    state: State,
}

/// Dependencies for spawning a [`DiscoverActor`].
#[derive(Clone)]
pub struct DiscoverActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
    /// Shared application state.
    pub state: State,
}

impl ServiceActor for DiscoverActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec).attach("DiscoverActor spawns via init_with"))
    }
}

/// Static path the discover actor spawns at (one instance per process).
pub const DISCOVER_ACTOR_PATH: &str = "jinn.provider.discover";

impl DiscoverActor {
    /// Spawns the discover actor onto the trouper system; its
    /// subscription is live when this returns.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    /// # Panics
    ///
    /// Panics if the actor's path is already taken or its topic
    /// subscription fails — both mean a wiring bug at composition.
    pub fn spawn(system: &trouper::system::ActorSystem, deps: DiscoverActorDeps) -> ActorPath {
        let path = ActorPath::new(DISCOVER_ACTOR_PATH);
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
                        })
                    })
                }
            })
            .handles::<RefreshModels>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<RefreshModels> for DiscoverActor {
    async fn handle(&mut self, _msg: &RefreshModels, _ctx: &mut MsgCtx<'_>) {
        self.refresh_models().await;
    }
}

impl BusPublish for DiscoverActor {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        self.deps.bus()
    }
}
/// How long one provider's discovery may take before the refresh gives
/// up on it and records the error. Discovery must always terminate —
/// both surfaces' refresh feedback (the TUI's summary table, the web
/// modal's spinner reset) depend on `ModelsRefreshed` publishing even
/// when an endpoint is unreachable.
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);

impl DiscoverActor {
    async fn refresh_models(&self) {
        let entries = {
            let registry = self.deps.services.provider_registry.read();
            registry.config().providers.clone()
        };

        let mut results: HashMap<String, Vec<ModelInfo>> = HashMap::new();
        let mut errors: HashMap<String, String> = HashMap::new();

        // Load models.dev reference data for context length fallback.
        let models_dev = jinn_provider_config::ModelsDevData::load(
            &self.deps.services.paths.models_dev_user_path(),
        );

        for (name, entry) in &entries {
            match self.discover_provider(name, entry, &models_dev).await {
                DiscoveryOutcome::Discovered(models) => {
                    results.insert(name.clone(), models);
                }
                DiscoveryOutcome::Failed(reason) => {
                    errors.insert(name.clone(), reason);
                }
            }
        }

        // Save cache to disk.
        let cache = ModelCache {
            entries: results.clone(),
            last_updated_at: Some(jiff::Timestamp::now()),
        };
        let path = self.deps.services.paths.cache_path();
        if let Err(e) = cache.save(&path) {
            tracing::warn!("failed to save model cache: {e:?}");
        }

        // Emit ModelsRefreshed event.
        let session_id = self.state.read().session.active_session_id().clone();
        self.publish(ModelsRefreshed {
            session_id,
            results,
            errors,
        })
        .await;
    }

    /// Discovers one provider's models: validates its config, resolves
    /// its key, calls `list_models` under [`PROVIDER_TIMEOUT`], and
    /// enriches the result. A failure (config, key, or timeout) is a
    /// [`DiscoveryOutcome::Failed`] carrying the reason — one
    /// provider's failure never stops the refresh.
    async fn discover_provider(
        &self,
        name: &str,
        entry: &jinn_provider_config::ProviderEntry,
        models_dev: &jinn_provider_config::ModelsDevData,
    ) -> DiscoveryOutcome {
        // Need a placeholder model for the builder - use the first static model.
        let Some(placeholder_model) = entry.models.first() else {
            return DiscoveryOutcome::Failed(
                "no models configured (skipping discovery)".to_owned(),
            );
        };

        let backend = match entry.backend.parse::<Backend>() {
            Ok(b) => b,
            Err(e) => return DiscoveryOutcome::Failed(format!("invalid backend: {e}")),
        };

        // Resolve API key.
        let api_key = if entry.requires_key {
            let Some(ref env_var) = entry.api_key_env else {
                return DiscoveryOutcome::Failed("requires_key but no api_key_env set".to_owned());
            };
            match self.deps.services.api_keys.get(env_var) {
                Some(key) => Some(key),
                None => return DiscoveryOutcome::Failed("API key not resolved".to_owned()),
            }
        } else {
            Some("dummy-key".to_owned())
        };

        // Build provider and call list_models, bounded per provider:
        // one unreachable endpoint delays this refresh by at most
        // PROVIDER_TIMEOUT, and never consumes another provider's
        // budget (the wrap is per-await, not around the loop).
        let api_key_str = api_key.as_deref().unwrap_or("");
        let bounded = match backend {
            Backend::Anthropic => {
                let svc = AnthropicService::new(placeholder_model.clone(), api_key_str.to_owned());
                tokio::time::timeout(PROVIDER_TIMEOUT, svc.list_models()).await
            }
            Backend::Google => {
                let svc = GoogleService::new(placeholder_model.clone(), api_key_str.to_owned());
                tokio::time::timeout(PROVIDER_TIMEOUT, svc.list_models()).await
            }
            _ => {
                let config = ProviderConfig::from(&backend);
                let svc = OpenAiCompatibleService::new(
                    config,
                    placeholder_model.clone(),
                    entry.base_url.clone(),
                    api_key_str.to_owned(),
                    entry.extra_body.clone(),
                    None,
                );
                tokio::time::timeout(PROVIDER_TIMEOUT, svc.list_models()).await
            }
        };
        let discovered: Result<Vec<ModelInfo>, Report<LlmServiceError>> = match bounded {
            Ok(discovered) => discovered,
            Err(_elapsed) => Err(Report::new(LlmServiceError::Provider).attach(format!(
                "discovery timed out after {}s",
                PROVIDER_TIMEOUT.as_secs()
            ))),
        };

        match discovered {
            Ok(mut models) => {
                enrich_with_models_dev(&mut models, models_dev);
                tracing::info!(provider = %name, count = models.len(), "discovered models");
                DiscoveryOutcome::Discovered(models)
            }
            Err(e) => {
                tracing::warn!(provider = %name, err = %e, "list_models failed");
                DiscoveryOutcome::Failed(format!("{e}"))
            }
        }
    }
}

/// One provider's discovery result — the model list, or the reason the
/// refresh gave up on it (fed into the refresh's error map either way).
enum DiscoveryOutcome {
    /// The provider answered: its discovered models.
    Discovered(Vec<ModelInfo>),
    /// The provider failed (config, key, or timeout): the reason.
    Failed(String),
}

/// Enrich freshly discovered models with models.dev data (context-length
/// fallback + image modality stamping) by delegating to the shared
/// [`ModelsDevData::enrich`] single source of truth.
fn enrich_with_models_dev(
    models: &mut [ModelInfo],
    models_dev: &jinn_provider_config::ModelsDevData,
) {
    for model in models {
        models_dev.enrich(model);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        reason = "test code"
    )]

    use std::time::Duration;

    use error_stack::Report;
    use jinn_kernel::AppState;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_kernel::common::state::State;
    use jinn_provider::LlmServiceError;
    use jinn_provider_selection_msg::ModelsRefreshed;
    use jinn_provider_selection_msg::RefreshModels;
    use jinn_testutil::bus_harness::{TestHarness, await_recorded};

    use super::{DISCOVER_ACTOR_PATH, DiscoverActor, DiscoverActorDeps};
    use trouper::actor::ActorPath;

    #[rstest::rstest]
    #[tokio::test]
    async fn refresh_models_emits_models_refreshed_event() {
        // Given a discover actor with no configured providers.
        let harness = TestHarness::new().await;
        let state = State::new(AppState::default());
        let services = harness.services().await;
        let _actor = DiscoverActor::spawn(
            &services.trouper_system,
            DiscoverActorDeps {
                deps: harness.actor_deps().await,
                state: state.clone(),
            },
        );
        let recorder = harness.spawn_recorder::<ModelsRefreshed>().await;

        // When telling the actor to refresh (RefreshModels is a COMMAND:
        // point-to-point to the actor's path, not a broadcast).
        harness
            .system()
            .tell(ActorPath::new(DISCOVER_ACTOR_PATH), RefreshModels)
            .await
            .expect("command delivers to the discover actor");

        // Then a ModelsRefreshed event is emitted (with empty results).
        let events = await_recorded(&recorder, 1, Duration::from_secs(2)).await;
        assert_eq!(events.len(), 1, "should emit one ModelsRefreshed event");
        assert!(
            events[0].results.is_empty(),
            "no providers configured, so results are empty"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn the_provider_timeout_bounds_a_never_resolving_discovery() {
        // Given the per-provider timeout wrap around a discovery call
        // that never resolves (an unreachable endpoint).
        let hang = std::future::pending::<
            Result<Vec<jinn_provider::ModelInfo>, Report<LlmServiceError>>,
        >();
        let start = std::time::Instant::now();

        // When the timeout elapses (a shortened probe of the same wrap
        // `refresh_models` applies — 30s is production, 50ms here).
        let outcome = tokio::time::timeout(Duration::from_millis(50), hang).await;

        // Then it takes the error branch within the bound — the actor's
        // refresh loop feeds this into the errors map, so the refresh
        // always terminates and always emits ModelsRefreshed.
        assert!(outcome.is_err(), "a hanging discovery must be cut off");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "the timeout must fire near the bound, took {:?}",
            start.elapsed()
        );
    }

    fn image_model(id: &str) -> jinn_provider::ModelInfo {
        jinn_provider::ModelInfo {
            id: id.to_owned(),
            context_length: None,
            input_modalities: jinn_provider::InputModalities::text(),
        }
    }

    fn dev_data(image_support: &[(&str, bool)]) -> jinn_provider_config::ModelsDevData {
        use std::collections::HashMap;
        jinn_provider_config::ModelsDevData {
            context_lengths: HashMap::new(),
            image_support: image_support
                .iter()
                .map(|(k, v)| ((*k).to_owned(), *v))
                .collect(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn enrich_stamps_image_bit_for_known_image_model() {
        // Given a discovered model that models.dev lists as image-capable.
        let mut models = vec![image_model("gpt-4o")];
        let dev = dev_data(&[("gpt-4o", true)]);

        // When enriching with models.dev data.
        super::enrich_with_models_dev(&mut models, &dev);

        // Then the model has both Text and Image modalities.
        let m = &models[0];
        assert!(m.input_modalities.contains(jinn_provider::Modality::Text));
        assert!(m.input_modalities.contains(jinn_provider::Modality::Image));
    }

    #[rstest::rstest]
    #[test]
    fn enrich_leaves_text_only_for_known_false_model() {
        // Given a discovered model that models.dev lists as NOT image-capable.
        let mut models = vec![image_model("gpt-3.5-turbo")];
        let dev = dev_data(&[("gpt-3.5-turbo", false)]);

        // When enriching with models.dev data.
        super::enrich_with_models_dev(&mut models, &dev);

        // Then the model stays text-only.
        let m = &models[0];
        assert!(m.input_modalities.contains(jinn_provider::Modality::Text));
        assert!(!m.input_modalities.contains(jinn_provider::Modality::Image));
    }

    #[rstest::rstest]
    #[test]
    fn enrich_leaves_text_only_for_unknown_model() {
        // Given a discovered model that models.dev does not know.
        let mut models = vec![image_model("my-custom-llama")];
        let dev = dev_data(&[]);

        // When enriching with models.dev data.
        super::enrich_with_models_dev(&mut models, &dev);

        // Then the model stays text-only (conservative default).
        let m = &models[0];
        assert!(m.input_modalities.contains(jinn_provider::Modality::Text));
        assert!(!m.input_modalities.contains(jinn_provider::Modality::Image));
    }
}
