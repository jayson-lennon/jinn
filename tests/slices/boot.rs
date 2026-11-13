//! End-to-end crossing test for the boot slice: `install_actors` spawns
//! the startup trio over the real fabric, the readiness channel fires on
//! `AllActorsSpawned`, the `GetEnvironmentConfig` ask round-trips the
//! config through the env-init actor, and `EnvironmentLoaded` fans out
//! to a probe subscriber — the production startup shape from
//! `actor_wiring.rs`, exercised against a fresh trouper system.
#![allow(
    clippy::indexing_slicing,
    reason = "integration test: fixture vectors with known lengths"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::collections::BTreeMap;
use std::time::Duration;

use jinn_boot::install_actors;
use jinn_boot_msg::{
    AllActorsSpawned, EnvironmentConfigReply, EnvironmentLoaded, GetEnvironmentConfig,
};
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::services::Services;
use jinn_kernel::common::services::bus_service::BusService;
use jinn_provider_config::ProviderEntry;
use jinn_provider_config::{ConfigStorageService, InMemoryConfigStorage};
use jinn_testutil::bus_harness::{TestHarness, await_recorded};

/// A sample single-provider config (the provider-init tests' fixture shape).
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

/// A harness whose `Services` carry the given provider config.
async fn harness_with_config(
    config: &jinn_provider_config::ProvidersConfig,
) -> (TestHarness, Services) {
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.config_storage =
        ConfigStorageService::new(Arc::new(InMemoryConfigStorage::with_config(config)));
    (harness, services)
}

use std::sync::Arc;

/// Mints a lone provider cell for boot tests (boot's init actor writes
/// the disk-loaded cache through it).
fn test_provider_cell(
    services: &jinn_kernel::Services,
) -> jinn_slices::TypedCell<jinn_provider_selection_msg::ProviderCell> {
    let _ = services.slices.register(
        jinn_provider_selection_msg::provider_state_slot(),
        jinn_provider_selection_msg::ProviderCell::default(),
    );
    services
        .slices
        .reader(&jinn_provider_selection_msg::provider_state_slot())
        .expect("provider cell registered")
}

#[rstest::rstest]
#[tokio::test]
async fn install_spawns_trio_and_readiness_fires_on_all_actors_spawned() {
    // Given a fresh harness with the boot trio installed and a config seeded.
    let config = sample_config();
    let (harness, services) = harness_with_config(&config).await;
    let boot = install_actors(
        harness.system(),
        jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus()),
        &services,
        test_provider_cell(&services),
    );

    // When composition publishes AllActorsSpawned (the startup tail's order).
    harness.publish(AllActorsSpawned).await;

    // Then the readiness receiver fires within the timeout.
    let result =
        tokio::time::timeout(Duration::from_secs(2), boot.ready_rx.to_async().recv()).await;
    assert!(result.is_ok(), "readiness should fire on AllActorsSpawned");
}

#[rstest::rstest]
#[tokio::test]
async fn env_config_ask_round_trips_seeded_config() {
    // Given a harness whose in-memory config storage carries the sample config.
    let config = sample_config();
    let (harness, services) = harness_with_config(&config).await;
    let boot = install_actors(
        harness.system(),
        jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus()),
        &services,
        test_provider_cell(&services),
    );

    // When composition asks the env-init actor for the config (the startup tail).
    let reply = harness
        .system()
        .ask(
            boot.env_init_path.clone(),
            GetEnvironmentConfig,
            Duration::from_secs(5),
        )
        .await
        .expect("ask succeeds");
    let decoded: EnvironmentConfigReply = reply.decode().expect("decode reply");

    // Then the seeded config round-trips.
    let loaded = decoded.config.expect("config present");
    assert!(
        loaded.providers.contains_key("sample"),
        "sample provider present"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn environment_loaded_fans_out_to_subscribers() {
    // Given the trio installed and a probe recorder subscribed to EnvironmentLoaded.
    let config = sample_config();
    let (harness, services) = harness_with_config(&config).await;
    let boot = install_actors(
        harness.system(),
        jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus()),
        &services,
        test_provider_cell(&services),
    );
    let probe = harness.spawn_recorder::<EnvironmentLoaded>().await;

    // When the startup tail asks for config and publishes EnvironmentLoaded.
    let reply = harness
        .system()
        .ask(
            boot.env_init_path.clone(),
            GetEnvironmentConfig,
            Duration::from_secs(5),
        )
        .await
        .expect("ask succeeds");
    let decoded: EnvironmentConfigReply = reply.decode().expect("decode reply");
    let config = decoded.config.expect("config present");
    harness.publish(EnvironmentLoaded { config }).await;

    // Then the probe subscriber receives the event through the real fabric.
    let events = await_recorded(&probe, 1, Duration::from_secs(2)).await;
    assert_eq!(events.len(), 1, "exactly one EnvironmentLoaded delivered");
    assert!(events[0].config.providers.contains_key("sample"));
}

#[rstest::rstest]
#[tokio::test]
async fn env_config_ask_returns_none_when_storage_errors() {
    // Given a harness whose config storage errors on load. The production
    // None path is a config that fails to load/parse; in-memory storage
    // cannot error, so exercise the same `Err` arm through a failing stub.
    struct FailingStorage;
    impl jinn_provider_config::ConfigStorage for FailingStorage {
        fn name(&self) -> &'static str {
            "failing"
        }
        fn load(
            &self,
        ) -> Result<
            jinn_provider_config::ProvidersConfig,
            error_stack::Report<jinn_provider_config::ConfigError>,
        > {
            Err(
                error_stack::Report::new(jinn_provider_config::ConfigError::Parse)
                    .attach("test: always fails"),
            )
        }
        fn save(
            &self,
            _config: &jinn_provider_config::ProvidersConfig,
        ) -> Result<(), error_stack::Report<jinn_provider_config::ConfigError>> {
            Ok(())
        }
    }
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.config_storage = ConfigStorageService::new(Arc::new(FailingStorage));
    let boot = install_actors(
        harness.system(),
        jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus()),
        &services,
        test_provider_cell(&services),
    );

    // When the startup tail asks for the config.
    let reply = harness
        .system()
        .ask(
            boot.env_init_path.clone(),
            GetEnvironmentConfig,
            Duration::from_secs(5),
        )
        .await
        .expect("ask succeeds");
    let decoded: EnvironmentConfigReply = reply.decode().expect("decode reply");

    // Then the reply carries no config (the tail skips EnvironmentLoaded).
    assert!(decoded.config.is_none(), "storage failure -> None");
}

/// The wiring shape's bus is the harness's — this helper exists to pin
/// that a `BusService` built from the harness composes with
/// `install_actors` (production passes `&services`, whose bus is the
/// same fabric).
#[rstest::rstest]
#[tokio::test]
async fn install_composes_over_a_recording_bus() {
    // Given a recording bus + system wired into Services (from_parts shape).
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let bus = BusService::new_trouper(system.clone());
    let harness = TestHarness::from_parts(bus.clone(), system.clone());
    let mut services = harness.services().await;
    services.config_storage = ConfigStorageService::new(Arc::new(
        InMemoryConfigStorage::with_config(&sample_config()),
    ));

    // When installing the trio.
    let boot = install_actors(
        &system,
        jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus()),
        &services,
        test_provider_cell(&services),
    );

    // Then the readiness handoff completes on AllActorsSpawned.
    bus.publish(AllActorsSpawned).await;
    let result =
        tokio::time::timeout(Duration::from_secs(2), boot.ready_rx.to_async().recv()).await;
    assert!(result.is_ok(), "readiness fires over the from_parts bus");
}

#[rstest::rstest]
#[tokio::test]
async fn provider_init_writes_the_disk_cache_through_the_provider_cell() {
    // Given a trio installed with a provider cell and a seeded config.
    let config = sample_config();
    let (harness, services) = harness_with_config(&config).await;
    let state = jinn_kernel::State::new(jinn_kernel::AppState::default_with_scope_focus());
    let cell = test_provider_cell(&services);
    let _boot = install_actors(harness.system(), state, &services, cell.clone());

    // When the startup tail publishes EnvironmentLoaded.
    harness
        .publish(EnvironmentLoaded {
            config: config.clone(),
        })
        .await;

    // Then the boot actor's cache write went through the provider cell.
    // With no disk cache seeded, the write lands as `None` — the
    // observable proof the actor reached the cell at all is that the
    // provider registry is now built from the loaded config.
    //
    // The wait is on the registry itself — a fixed sleep would only guess
    // at when the boot actor built it, and the read could land before it,
    // leaving the assertion to fail on an empty registry.
    jinn_testutil::wait::wait_for(Duration::from_secs(5), || {
        services
            .provider_registry
            .read()
            .get(&jinn_provider_config::ProviderId::new(
                "sample/sample".to_owned(),
            ))
            .is_some()
    })
    .await;
    let registry = services.provider_registry.read();
    assert!(
        registry
            .get(&jinn_provider_config::ProviderId::new(
                "sample/sample".to_owned()
            ))
            .is_some(),
        "provider-init built the registry from the loaded config"
    );
    drop(registry);
    assert!(
        cell.read().model_cache.is_none(),
        "with no disk cache on disk, the cell write is a cleared cache"
    );
}
