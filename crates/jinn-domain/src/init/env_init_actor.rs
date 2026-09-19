//! Environment initialization actor - reads env vars and populates API keys.
//!
//! During startup (`on_start`), loads `providers.toml`, resolves API keys from
//! environment variables, populates the shared `ApiKeysService`, and stores
//! the config for downstream actors to request via `ask(GetEnvironmentConfig)`.
//!
//! The `EnvironmentLoaded` event is retained for runtime reloads only.

use crate::common::bus::BusMessage;
use crate::feat::provider_infra::ProvidersConfig;
use error_stack::Report;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use crate::common::actor_deps::{ActorDeps, BusPublish};
use wherror::Error;

/// Error type for environment initialization failures.
#[derive(Debug, Error)]
#[error(debug)]
pub struct EnvInitError;

/// The environment has been loaded and API keys are available.
///
/// Emitted after the env init actor has populated `ApiKeysService`.
/// Published at runtime for environment reloads (not during startup).
/// Downstream actors should use `ask(GetEnvironmentConfig)` for initial config.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentLoaded {
    /// The parsed provider configuration from `providers.toml`.
    pub config: ProvidersConfig,
}

impl BusMessage for EnvironmentConfigReply {}

jinn_slices::crossing_schema!(EnvironmentConfigReply, "EnvironmentConfigReply",
trouper::schema::SchemaKind::Event,
description: "Reply payload for the GetEnvironmentConfig ask.",
fields: ["config" => trouper::schema::FieldTy::Json]);

impl BusMessage for EnvironmentLoaded {}

jinn_slices::crossing_schema!(EnvironmentLoaded, "EnvironmentLoaded",
trouper::schema::SchemaKind::Event,
description: "The environment has been loaded and API keys are available.",
fields: ["config" => trouper::schema::FieldTy::Json]);

/// Ask message to retrieve the loaded environment config.
///
/// Downstream actors use this during their `on_start` to pull config
/// directly from the EnvInitActor via the actor registry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GetEnvironmentConfig;

jinn_slices::crossing_schema!(GetEnvironmentConfig, "GetEnvironmentConfig",
trouper::schema::SchemaKind::Command,
description: "Ask the env-init actor for the parsed provider configuration.",
fields: []);

/// The environment initialization actor.
///
/// Loads `providers.toml` lazily on the first `GetEnvironmentConfig` ask,
/// resolves API keys, and populates `ApiKeysService`. The ask is the one
/// real startup ask path: composition asks it (with a mandatory timeout)
/// before spawning downstream actors.
pub struct EnvInitActor {
    deps: ActorDeps,
    config: Option<ProvidersConfig>,
}

/// Dependencies for spawning an [`EnvInitActor`].
#[derive(Clone)]
pub struct EnvInitActorDeps {
    /// Universal actor dependencies (bus, services, etc.).
    pub deps: ActorDeps,
}

/// The reply payload of the `GetEnvironmentConfig` ask (JSON-friendly twin
/// of `Option<ProvidersConfig>`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnvironmentConfigReply {
    /// The loaded config, or `None` when the file is missing/unreadable.
    pub config: Option<ProvidersConfig>,
}

impl ServiceActor for EnvInitActor {
    async fn start(_args: &serde_json::Value) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s start_with (typed deps can't
        // ride the JSON args).
        let _ = _args;
        Err(Report::new(RegistryError::InvalidSpec)
            .attach("EnvInitActor spawns via start_with"))
    }
}

/// Static path the env-init actor spawns at (one instance per process).
pub const ENV_INIT_PATH: &str = "jinn.init.env";

impl EnvInitActor {
    /// Spawns the env-init actor onto the trouper system.
    pub fn spawn(system: &trouper::system::ActorSystem, deps: EnvInitActorDeps) -> ActorPath {
        let path = ActorPath::new(ENV_INIT_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            deps: deps.deps,
                            config: None,
                        })
                    })
                }
            })
            .handles::<GetEnvironmentConfig>()
            .handles::<EnvironmentLoaded>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<GetEnvironmentConfig> for EnvInitActor {
    async fn handle(&mut self, _msg: GetEnvironmentConfig, ctx: &mut MsgCtx<'_>) {
        if self.config.is_none() {
            self.config = self.load_config_and_resolve_keys();
        }
        ctx.reply(EnvironmentConfigReply {
            config: self.config.clone(),
        });
    }
}

impl MsgHandler<EnvironmentLoaded> for EnvInitActor {
    async fn handle(&mut self, _msg: EnvironmentLoaded, _ctx: &mut MsgCtx<'_>) {
        // No-op: EnvInitActor doesn't react to EnvironmentLoaded.
    }
}

impl BusPublish for EnvInitActor {
    fn bus(&self) -> &crate::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

impl EnvInitActor {
    /// Loads config, resolves API keys and MCP header variables.
    ///
    /// Returns config on success. Providers contribute their configured env
    /// vars; configured MCP servers contribute every `${VAR}` referenced by
    /// their `headers` values. Missing or empty variables are skipped
    /// silently here — a server whose headers cannot expand fails loudly at
    /// connect time instead, where the user can see which server is dead.
    fn load_config_and_resolve_keys(&self) -> Option<ProvidersConfig> {
        let config = match self.deps.services.config_storage.load() {
            Ok(config) => config,
            Err(e) => {
                tracing::error!(err = ?e, "env-init failed to load provider config");
                return None;
            }
        };

        // Resolve API keys from environment variables.
        for provider in config.providers.values() {
            if let Some(ref env_var) = provider.api_key_env
                && let Ok(value) = std::env::var(env_var)
                && !value.is_empty()
            {
                self.deps.services.api_keys.insert(env_var.clone(), value);
            }
        }

        // Resolve MCP header variables from environment variables.
        self.resolve_mcp_header_variables();

        tracing::info!("environment loaded, API keys resolved");
        Some(config)
    }

    /// Scans configured MCP server header values for `${VAR}` references and
    /// seeds each one found into `ApiKeysService` from the process
    /// environment (present non-empty values only).
    fn resolve_mcp_header_variables(&self) {
        let prefs = self.deps.services.user_preferences_storage.read();
        let values: Vec<&str> = prefs
            .mcp_server
            .values()
            .flat_map(|server| server.headers.values().map(String::as_str))
            .collect();
        for name in jinn_mcp_msg::referenced_header_variables(&values) {
            if let Ok(value) = std::env::var(&name)
                && !value.is_empty()
            {
                self.deps.services.api_keys.insert(name, value);
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
    use std::time::Duration;

    use crate::common::bus::test_harness::{TestHarness, await_recorded};
    use crate::feat::provider_infra::ProvidersConfig;
    use jinn_mcp_msg::McpServerConfig;
    use jinn_preferences_config::user_preferences::UserPreferences;

    use super::{EnvInitActor, EnvInitActorDeps, EnvironmentConfigReply, EnvironmentLoaded, GetEnvironmentConfig};

    /// Unique env-var names so parallel test runs never collide.
    const SET_VAR: &str = "JINN_TEST_MCP_HEADER_RESOLVED";
    const MISSING_VAR: &str = "JINN_TEST_MCP_HEADER_NEVER_SET";

    /// Builds default preferences declaring one MCP server whose headers
    /// reference the given env-var names.
    fn prefs_referencing(vars: &[&str]) -> UserPreferences {
        let mut prefs = UserPreferences::default();
        let headers = vars
            .iter()
            .map(|v| (format!("X-{v}"), format!("Bearer ${{{v}}}")))
            .collect();
        prefs.mcp_server.insert(
            "header-probe".to_owned(),
            McpServerConfig {
                transport: jinn_mcp_msg::TransportKind::RemoteHttp,
                url: Some("http://localhost:3001/mcp".to_owned()),
                headers,
                ..McpServerConfig::default()
            },
        );
        prefs
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn referenced_header_variables_seed_api_keys_store() {
        // Given preferences declaring an MCP server header referencing a
        // variable that IS set in the process environment.
        // SAFETY: single-threaded test setup; unique var name avoids races.
        unsafe { std::env::set_var(SET_VAR, "live-value") };
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        let service = deps.services.user_preferences_storage.clone();
        service.save(&prefs_referencing(&[SET_VAR])).expect("save");
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = harness.services().await;
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = serde_json::from_value(reply).expect("decode reply");

        // Then startup succeeded and the referenced key landed in the store.
        assert!(loaded.config.is_some(), "config should load");
        assert_eq!(keys.get(SET_VAR), Some("live-value".to_owned()));
        // SAFETY: removing the test-only var set above; no concurrent readers.
        unsafe {
            std::env::remove_var(SET_VAR);
        };
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unset_header_variable_skips_store_and_startup_still_succeeds() {
        // Given preferences declaring a header referencing a variable that is
        // NOT present in the environment.
        // SAFETY: ensures the name is truly absent despite prior test runs.
        unsafe { std::env::remove_var(MISSING_VAR) };
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        let service = deps.services.user_preferences_storage.clone();
        service
            .save(&prefs_referencing(&[MISSING_VAR]))
            .expect("save");
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = harness.services().await;
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = serde_json::from_value(reply).expect("decode reply");

        // Then startup still succeeds (silent skip).
        assert!(loaded.config.is_some(), "config should load");
        // And nothing was seeded for the missing variable.
        assert!(keys.get(MISSING_VAR).is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn get_environment_config_returns_none_without_config_file() {
        // Given an env init actor with no config file.
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        let path = EnvInitActor::spawn(
            &services.trouper_system,
            EnvInitActorDeps {
                deps: harness.actor_deps().await,
            },
        );

        // When asking for config.
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await;

        // Then ask succeeds (but config may be None without a config file).
        assert!(reply.is_ok(), "ask should succeed");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn environment_loaded_can_be_published_for_reload() {
        // Given an env init actor and a recorder.
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        let _path = EnvInitActor::spawn(
            &services.trouper_system,
            EnvInitActorDeps {
                deps: harness.actor_deps().await,
            },
        );
        let recorder = harness.spawn_recorder::<EnvironmentLoaded>().await;

        // When publishing EnvironmentLoaded manually (runtime reload).
        let bus = harness.bus();
        bus.publish(EnvironmentLoaded {
            config: crate::feat::provider_infra::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
            },
        })
        .await;

        // Then the event is received by subscribers.
        let events = await_recorded(&recorder, 1, Duration::from_secs(2)).await;
        assert_eq!(events.len(), 1, "expected EnvironmentLoaded event");
    }
}
