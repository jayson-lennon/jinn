//! Environment initialization actor - reads env vars and populates API keys.
//!
//! During startup (`on_start`), loads `providers.toml`, resolves API keys from
//! environment variables, populates the shared `ApiKeysService`, and stores
//! the config for downstream actors to request via `ask(GetEnvironmentConfig)`.
//!
//! The `EnvironmentLoaded` event is retained for runtime reloads only.

use error_stack::Report;
use jinn_provider_config::ProvidersConfig;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use wherror::Error;

use jinn_boot_msg::{
    EnvironmentConfigReply, EnvironmentLoaded, GetEnvironmentConfig, ProvidersTomlReloaded,
};
use jinn_preferences_config::schemas::mcp::McpServersConfig;

/// Error type for environment initialization failures.
#[derive(Debug, Error)]
#[error(debug)]
pub struct EnvInitError;

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

impl ServiceActor for EnvInitActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec).attach("EnvInitActor spawns via init_with"))
    }
}

/// Static path the env-init actor spawns at (one instance per process).
pub const ENV_INIT_PATH: &str = "jinn.init.env";

impl EnvInitActor {
    /// Spawns the env-init actor onto the trouper system.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &trouper::system::ActorSystem, deps: EnvInitActorDeps) -> ActorPath {
        let path = ActorPath::new(ENV_INIT_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
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
            // Live config: a providers.toml change re-resolves the keys.
            .handles::<ProvidersTomlReloaded>()
            // Ask replies leave the handler through ctx.reply; the flush
            // gate drops any outbound type not declared here.
            .emits::<EnvironmentConfigReply>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<GetEnvironmentConfig> for EnvInitActor {
    async fn handle(&mut self, _msg: &GetEnvironmentConfig, ctx: &mut MsgCtx<'_>) {
        if self.config.is_none() {
            self.config = self.load_config_and_resolve_keys();
        }
        ctx.reply(EnvironmentConfigReply {
            config: self.config.clone(),
        });
    }
}

impl MsgHandler<EnvironmentLoaded> for EnvInitActor {
    async fn handle(&mut self, _msg: &EnvironmentLoaded, _ctx: &mut MsgCtx<'_>) {
        // No-op: EnvInitActor doesn't react to EnvironmentLoaded.
    }
}

// Live config: a providers.toml reload re-resolves everything this actor
// resolved at startup. The key store is rebuilt fresh and swapped whole
// (`replace`, not per-key inserts): a provider removed from the file must
// drop its key too, or availability checks keep reporting a provider
// that no longer exists. The fresh load is also the parse gate — the
// watcher refused a malformed file before publishing, so a handler-time
// failure is unexpected and keeps the previous cached config.
impl MsgHandler<ProvidersTomlReloaded> for EnvInitActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "async signature symmetry; the reload body is synchronous"
    )]
    async fn handle(&mut self, _msg: &ProvidersTomlReloaded, _ctx: &mut MsgCtx<'_>) {
        // Given a providers.toml whose contents just changed.
        if let Some(config) = self.load_config_and_resolve_keys() {
            // Then the cached config is replaced; readers observe the
            // new file on their next read.
            self.config = Some(config);
        }
    }
}

impl BusPublish for EnvInitActor {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        self.deps.bus()
    }
}

impl EnvInitActor {
    /// Loads config, resolves API keys and MCP header/env variables.
    ///
    /// Returns config on success. Providers contribute their configured env
    /// vars; configured MCP servers contribute every `${VAR}` referenced by
    /// their `headers` or `env` values. Missing or empty variables are
    /// skipped silently here — a server whose headers cannot expand fails
    /// loudly at connect time instead, where the user can see which server
    /// is dead.
    fn load_config_and_resolve_keys(&self) -> Option<ProvidersConfig> {
        let config = match self.deps.services.config_storage.load() {
            Ok(config) => config,
            Err(e) => {
                tracing::error!(err = ?e, "env-init failed to load provider config");
                return None;
            }
        };

        // Resolve API keys into a fresh store, then swap it whole: a
        // provider removed from the file drops its key with it (per-key
        // inserts would leave the removed provider resolvable).
        let mut keys = jinn_provider_config::ApiKeys::new();
        for provider in config.providers.values() {
            if let Some(ref env_var) = provider.api_key_env
                && let Ok(value) = std::env::var(env_var)
                && !value.is_empty()
            {
                keys.insert(env_var.clone(), value);
            }
        }
        self.deps.services.api_keys.replace(keys);

        // Resolve MCP header variables from environment variables.
        self.resolve_mcp_header_variables();

        tracing::info!("environment loaded, API keys resolved");
        Some(config)
    }

    /// Scans configured MCP server header and env values for `${VAR}`
    /// references and seeds each one found into `ApiKeysService` from the
    /// process environment (present non-empty values only).
    fn resolve_mcp_header_variables(&self) {
        let servers = self
            .deps
            .services
            .config
            .get::<McpServersConfig>()
            .unwrap_or_default();
        let values: Vec<&str> = servers
            .values()
            .flat_map(|server| {
                server
                    .headers
                    .values()
                    .chain(server.env.values())
                    .map(String::as_str)
            })
            .collect();
        for name in jinn_preferences_config::schemas::mcp::referenced_header_variables(&values) {
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

    use super::{
        EnvInitActor, EnvInitActorDeps, EnvironmentConfigReply, EnvironmentLoaded,
        GetEnvironmentConfig,
    };
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_testutil::bus_harness::{TestHarness, await_recorded};
    use std::sync::Arc;

    use jinn_boot_msg::ProvidersTomlReloaded;
    use jinn_preferences_config::schemas::mcp::McpServerConfig;
    use jinn_provider_config::ConfigStorage as _;

    /// Unique env-var names so parallel test runs never collide.
    const SET_VAR: &str = "JINN_TEST_MCP_HEADER_RESOLVED";
    const RELOAD_VAR: &str = "JINN_TEST_PROVIDERS_RELOAD_KEY";
    const RELOAD_VAR_SECOND: &str = "JINN_TEST_PROVIDERS_RELOAD_KEY_2";
    const MISSING_VAR: &str = "JINN_TEST_MCP_HEADER_NEVER_SET";
    const ENV_SET_VAR: &str = "JINN_TEST_MCP_ENV_RESOLVED";
    const ENV_MISSING_VAR: &str = "JINN_TEST_MCP_ENV_NEVER_SET";

    /// Serializes one server entry as the body of a `[mcp.<name>]` table.
    fn server_body(server: McpServerConfig) -> String {
        let table = toml::Value::try_from(server)
            .expect("server serializes")
            .as_table()
            .expect("server is a table")
            .clone();
        table.iter().fold(String::new(), |mut acc, (key, value)| {
            use std::fmt::Write as _;
            let _ = writeln!(acc, "{key} = {value}");
            acc
        })
    }

    /// A document declaring one MCP server whose headers reference the
    /// given env-var names, under the section's umbrella.
    fn config_referencing(vars: &[&str]) -> String {
        let headers = vars
            .iter()
            .map(|v| (format!("X-{v}"), format!("Bearer ${{{v}}}")))
            .collect();
        let server = McpServerConfig {
            transport: jinn_preferences_config::schemas::mcp::TransportKind::RemoteHttp,
            url: Some("http://localhost:3001/mcp".to_owned()),
            headers,
            ..McpServerConfig::default()
        };
        // The section is the map: one sub-table per server, directly
        // under the `mcp` umbrella.
        format!("[mcp.header-probe]\n{}", server_body(server))
    }

    /// A document declaring one MCP server whose `env` values reference the
    /// given env-var names, under the section's umbrella.
    fn config_referencing_via_env(vars: &[&str]) -> String {
        let env = vars
            .iter()
            .map(|v| (format!("{v}_KEY"), format!("${{{v}}}")))
            .collect();
        let server = McpServerConfig {
            transport: jinn_preferences_config::schemas::mcp::TransportKind::RemoteHttp,
            url: Some("http://localhost:3001/mcp".to_owned()),
            env,
            ..McpServerConfig::default()
        };
        format!("[mcp.env-probe]\n{}", server_body(server))
    }

    /// Points the services' config layer at a document seeded with
    /// `document`, then re-reads so the swap is visible immediately.
    /// Seeds the services' providers storage with a `[providers.sample]`
    /// entry whose `api_key_env` names `VAR=value` (empty string = no
    /// providers at all), the way an edited providers.toml would land.
    fn seed_providers_storage(services: &jinn_kernel::Services, spec: &str) {
        let body = if spec.is_empty() {
            String::new()
        } else {
            let (var, _value) = spec.split_once('=').unwrap_or((spec, ""));
            format!(
                "[providers.sample]\nbackend = \"sample\"\nmodels = [\"m\"]\napi_key_env = \"{var}\"\n"
            )
        };
        let storage = jinn_provider_config::InMemoryConfigStorage::new();
        if !body.is_empty() {
            let config: jinn_provider_config::ProvidersConfig =
                toml::from_str(&body).expect("seed config parses");
            storage.save(&config).expect("seed saves");
        }
        services
            .config_storage
            .swap(Arc::new(storage) as Arc<dyn jinn_provider_config::ConfigStorage>);
    }

    fn layer_with(services: &jinn_kernel::Services, document: &str) {
        let parsed = document.parse().expect("test TOML parses");
        services
            .config
            .use_storage(Arc::new(jinn_config::InMemoryConfigStorage::new(parsed)))
            .expect("layer re-reads from the seeded backend");
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
        layer_with(&deps.services, &config_referencing(&[SET_VAR]));
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = deps.services.clone();
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = reply.decode().expect("decode reply");

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
        layer_with(&deps.services, &config_referencing(&[MISSING_VAR]));
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = deps.services.clone();
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = reply.decode().expect("decode reply");

        // Then startup still succeeds (silent skip).
        assert!(loaded.config.is_some(), "config should load");
        // And nothing was seeded for the missing variable.
        assert!(keys.get(MISSING_VAR).is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn referenced_env_variables_seed_api_keys_store() {
        // Given preferences declaring an MCP server env value referencing a
        // variable that IS set in the process environment.
        // SAFETY: single-threaded test setup; unique var name avoids races.
        unsafe { std::env::set_var(ENV_SET_VAR, "env-live-value") };
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        layer_with(&deps.services, &config_referencing_via_env(&[ENV_SET_VAR]));
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = deps.services.clone();
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = reply.decode().expect("decode reply");

        // Then startup succeeded and the env-referenced key landed in the
        // store under the referenced variable's name — the same key the
        // connect-time resolver looks up.
        assert!(loaded.config.is_some(), "config should load");
        assert_eq!(keys.get(ENV_SET_VAR), Some("env-live-value".to_owned()));
        // SAFETY: removing the test-only var set above; no concurrent readers.
        unsafe {
            std::env::remove_var(ENV_SET_VAR);
        };
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unset_env_variable_skips_store_and_startup_still_succeeds() {
        // Given preferences declaring an env value referencing a variable
        // that is NOT present in the environment.
        // SAFETY: ensures the name is truly absent despite prior test runs.
        unsafe { std::env::remove_var(ENV_MISSING_VAR) };
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        layer_with(
            &deps.services,
            &config_referencing_via_env(&[ENV_MISSING_VAR]),
        );
        let keys = deps.services.api_keys.clone();

        // When the env init actor resolves keys for a config request.
        let services = deps.services.clone();
        let path = EnvInitActor::spawn(&services.trouper_system, EnvInitActorDeps { deps });
        let reply = services
            .trouper_system
            .ask(path, GetEnvironmentConfig, Duration::from_secs(5))
            .await
            .expect("ask succeeds");
        let loaded: EnvironmentConfigReply = reply.decode().expect("decode reply");

        // Then startup still succeeds (silent skip).
        assert!(loaded.config.is_some(), "config should load");
        // And nothing was seeded for the missing variable.
        assert!(keys.get(ENV_MISSING_VAR).is_none());
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
    async fn a_providers_reload_drops_a_removed_provider_s_key() {
        // Given an env-init actor whose store holds a key for a provider
        // the fresh file no longer lists.
        // SAFETY: single-threaded test setup; unique var name avoids races.
        unsafe { std::env::set_var(RELOAD_VAR, "stale-value") };
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        seed_providers_storage(&services, &format!("{RELOAD_VAR}=kept"));
        let bus = harness.bus();
        let path = EnvInitActor::spawn(
            &services.trouper_system,
            EnvInitActorDeps {
                deps: jinn_kernel::common::actor_deps::ActorDeps {
                    services: services.clone(),
                },
            },
        );
        bus.publish(ProvidersTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &path,
            Duration::from_secs(2),
        )
        .await;

        // When the reload re-resolves after the provider was removed.
        seed_providers_storage(&services, "");
        bus.publish(ProvidersTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &path,
            Duration::from_secs(2),
        )
        .await;

        // Then the removed provider's key is gone from the store.
        assert!(
            services.api_keys.get(RELOAD_VAR).is_none(),
            "a removed provider must drop its key"
        );
        // SAFETY: removing the test-only var set above; no concurrent readers.
        unsafe { std::env::remove_var(RELOAD_VAR) };
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_providers_reload_resolves_a_changed_env_var_name() {
        // Given an env-init actor with no key in the store.
        // SAFETY: single-threaded test setup; unique var name avoids races.
        unsafe { std::env::set_var(RELOAD_VAR_SECOND, "fresh-value") };
        let harness = TestHarness::new().await;
        let services = harness.services().await;
        let bus = harness.bus();

        // When a reload lands whose provider now names the env var.
        seed_providers_storage(&services, &format!("{RELOAD_VAR_SECOND}=kept"));
        let path = EnvInitActor::spawn(
            &services.trouper_system,
            EnvInitActorDeps {
                deps: jinn_kernel::common::actor_deps::ActorDeps {
                    services: services.clone(),
                },
            },
        );
        bus.publish(ProvidersTomlReloaded).await;
        jinn_testutil::wait::await_inbox_drained(
            &services.trouper_system,
            &path,
            Duration::from_secs(2),
        )
        .await;

        // Then the new env var is resolved and cached.
        assert_eq!(
            services.api_keys.get(RELOAD_VAR_SECOND),
            Some("fresh-value".to_owned()),
            "the reload resolved the new key"
        );
        // SAFETY: removing the test-only var set above; no concurrent readers.
        unsafe { std::env::remove_var(RELOAD_VAR_SECOND) };
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
            config: jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            },
        })
        .await;

        // Then the event is received by subscribers.
        let events = await_recorded(&recorder, 1, Duration::from_secs(2)).await;
        assert_eq!(events.len(), 1, "expected EnvironmentLoaded event");
    }
}
