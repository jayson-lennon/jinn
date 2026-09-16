//! Application-wide runtime services.
//!
//! This crate defines the [`Services`] container, which holds long-lived
//! runtime infrastructure that subsystems need access to. It is created
//! once during startup and shared throughout the application.
#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::missing_panics_doc,
        reason = "test utilities"
    )
)]

use std::sync::Arc;

use derive_more::Debug;
use kameo::actor::Spawn;

use crate::feat::preferences_actor::{
    AppStateStorageService, InMemoryAppStateStorage, InMemoryUserPreferencesStorage,
    UserPreferencesStorageService,
};

pub use crate::feat::provider_infra;
use crate::feat::provider_infra::{
    ApiKeys, ApiKeysService, ConfigStorageService, InMemoryConfigStorage, LlmServiceFactoryService,
    ProviderRegistry, ProviderRegistryService, ProvidersConfig,
};
use crate::feat::session::SessionStoreService;
use tokio::runtime::Handle;

use crate::common::request_dump::RequestDumpService;

pub mod test_services;

pub mod bus_service;
pub use bus_service::BusService;

#[cfg(test)]
pub use bus_service::{BusAudit, RecordedMessage};

/// Runtime services shared across the application.
///
/// Holds references to all services, enabling dependency injection
/// and making it easy to swap implementations for testing.
///
/// Production code should construct this via struct initialization syntax
/// to get compiler-verified completeness.
///
/// Tests can use [`Services::new()`] which provides all-fake defaults,
#[derive(Clone, Debug)]
pub struct Services {
    /// Application filesystem paths (configured once at init).
    pub paths: crate::common::app_paths::AppPaths,
    /// Async runtime handle for spawning background tasks.
    pub handle: Handle,
    /// LLM service factory for creating streaming chat instances.
    pub llm_service: LlmServiceFactoryService,
    /// Provider registry for looking up and validating provider configs.
    pub provider_registry: ProviderRegistryService,
    /// Resolved API keys for provider availability checks and factory creation.
    pub api_keys: ApiKeysService,
    /// Config storage for persisting provider configuration.
    pub config_storage: ConfigStorageService,
    /// Session store for persisting chat session data.
    pub session_store: SessionStoreService,
    /// User preferences storage for persisting `jinn.toml`.
    pub user_preferences_storage: UserPreferencesStorageService,
    /// App state storage for persisting `state.toml`.
    pub app_state_storage: AppStateStorageService,
    /// Test-only owned temp directory. `None` in production.
    ///
    /// Held here so the dir outlives the [`AppPaths`] that points at it
    /// and is cleaned up when the last `Services` clone is dropped.
    /// Production code passes `None` because [`AppPaths::default`] resolves
    /// real user dirs.
    #[debug(skip)]
    pub tempdir: Option<Arc<tempfile::TempDir>>,

    /// Kameo message bus for type-based pub/sub routing.
    #[debug(skip)]
    pub bus: bus_service::BusService,

    /// Kanal closure bridge from sync TUI to async bus.
    pub bridge: crate::common::bridge::Bridge,

    /// Root supervision-tree actor.
    ///
    /// `Some` in production (spawned in `actor_wiring::build`) so the TUI
    /// can gracefully shut down the actor system on exit. `None` in tests
    /// that don't exercise the full shutdown path.
    #[debug(skip)]
    pub root_supervisor: crate::common::root_supervisor::RootSupervisorRef,

    #[debug(skip)]
    pub mcp_coordinator:
        Arc<std::sync::OnceLock<std::sync::Arc<dyn jinn_mcp_msg::McpCoordinatorHandle>>>,

    /// Interactive-term coordinator actor ref, exposed to the tool layer
    /// (the `interactive_term*` tools) after actor wiring spawns it.
    pub interactive_term: Arc<
        std::sync::OnceLock<
            kameo::actor::ActorRef<
                crate::feat::interactive_term::interactive_term_actor::InteractiveTermActor,
            >,
        >,
    >,

    /// Request dump directory. `None` disables dumping (default).
    pub request_dump: RequestDumpService,

    /// In-flight subagent registry: parent → child sessions spawned by the
    /// `task` tool. Read by the stall watchdog to skip waiting parents.
    #[debug(skip)]
    pub task_spawns: crate::feat::tools_actor::task_registry::TaskSpawnRegistry,

    /// Dynamic registry of per-slice render cells.
    ///
    /// `register` mints the one write handle for a slice; the renderer
    /// and intent router hold read handles only. Shared by all clones.
    #[debug(skip)]
    pub slices: crate::common::slices::Slices,

    /// Feature-registered keybind routes (intent → message).
    ///
    /// The intent handler consults this table before its own arms; rows
    /// attach after startup wiring as features and plugins register.
    #[debug(skip)]
    pub key_routes: crate::common::slices::key_routes::KeyRoutes,

    /// Erased slice views, one per rendered slot. Views pair with their
    /// slice at registration (type-checked at startup); the renderer asks
    /// the viewport for the active slot's view instead of hand-written
    /// tab code.
    pub viewport: crate::common::slices::view::Viewport,

    /// Slice-registered overlay renderers for dynamic scopes. Written at
    /// activation; the generic overlay pass resolves the active scope's
    /// renderer.
    #[debug(skip)]
    pub overlay_views: crate::common::overlay_views::OverlayViews<jinn_slices::RenderFacts>,

    /// Actor-canvas runtime system hosting the ported slice actors
    /// (dashboard, quake-bar). Built once here; slice `activate` functions
    /// spawn their canvas actors onto it and subscribe them to topics fed
    /// by the kameo→trouper bridge. See `.plans/actor-canvas/plan.md`.
    #[debug(skip)]
    pub trouper_system: trouper::system::ActorSystem,

    /// Generic picker spec registry. Populated by domain composition
    /// (feat/picker/registry) after construction; specs register as they
    /// migrate off the legacy per-kind handlers.
    #[debug(skip)]
    pub picker_registry: jinn_picker::PickerRegistry,
}

impl Services {
    /// Creates a new `Services` with all fake/noop implementations.
    ///
    /// Suitable for unit tests that need a `Services` but don't test
    /// specific behavior. Shares a single process-wide tokio runtime
    /// across all tests to avoid FD exhaustion under parallel execution.
    ///
    /// # Panics
    ///
    /// Panics if the tokio runtime fails to create (extremely unlikely).
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "test-only defaults, panics are acceptable"
    )]
    pub async fn new_fake() -> Self {
        let handle = test_services::shared_test_handle();

        let tempdir = Arc::new(tempfile::TempDir::new().expect("test temp dir"));

        let bus = {
            let bus_actor = kameo_actors::message_bus::MessageBus::new(
                kameo_actors::DeliveryStrategy::BestEffort,
            );
            let bus_ref = kameo_actors::message_bus::MessageBus::spawn(bus_actor);
            bus_service::BusService::new(bus_ref)
        };
        let bridge = crate::common::bridge::Bridge::new(bus.actor_ref().clone());
        let root_supervisor = crate::common::root_supervisor::RootSupervisor::spawn_root().await;

        Self {
            paths: crate::common::app_paths::AppPaths::new_in(tempdir.path()),
            handle,
            llm_service: LlmServiceFactoryService::new(Arc::new(
                crate::feat::provider_infra::FakeLlmServiceFactory::new(vec![]),
            )),
            provider_registry: ProviderRegistryService::new(
                ProviderRegistry::from_config(ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                })
                .expect("empty config is valid"),
            ),
            api_keys: ApiKeysService::new(ApiKeys::new()),
            config_storage: ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new())),
            session_store: SessionStoreService::new(Arc::new(test_services::FakeSessionStore)),
            user_preferences_storage: {
                let svc = UserPreferencesStorageService::new(Arc::new(
                    InMemoryUserPreferencesStorage::new(),
                ));
                svc.reload().expect("test prefs storage initial reload");
                svc
            },
            app_state_storage: {
                let svc = AppStateStorageService::new(Arc::new(InMemoryAppStateStorage::new()));
                svc.reload().expect("test app state storage initial reload");
                svc
            },
            tempdir: Some(tempdir),
            bus,
            bridge,
            root_supervisor,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: RequestDumpService::default(),
            task_spawns: crate::feat::tools_actor::task_registry::TaskSpawnRegistry::default(),
            slices: crate::common::slices::Slices::new(),
            key_routes: crate::common::slices::key_routes::KeyRoutes::new(),
            viewport: crate::common::slices::view::Viewport::new(),
            overlay_views:
                crate::common::overlay_views::OverlayViews::<jinn_slices::RenderFacts>::new(),
            trouper_system: trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ),
            picker_registry: jinn_picker::PickerRegistry::new(),
        }
    }

    /// Construct a fake Services with a pre-built bus (e.g. BusService::new_recording()).
    #[cfg(test)]
    pub async fn new_fake_with_bus(bus: bus_service::BusService) -> Self {
        let handle = test_services::shared_test_handle();
        let tempdir = Arc::new(tempfile::TempDir::new().expect("test temp dir"));

        let bridge = crate::common::bridge::Bridge::new_for_test();
        let root_supervisor = crate::common::root_supervisor::RootSupervisor::spawn_root().await;

        Self {
            paths: crate::common::app_paths::AppPaths::new_in(tempdir.path()),
            handle,
            llm_service: LlmServiceFactoryService::new(Arc::new(
                crate::feat::provider_infra::FakeLlmServiceFactory::new(vec![]),
            )),
            provider_registry: ProviderRegistryService::new(
                ProviderRegistry::from_config(ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                })
                .expect("empty config is valid"),
            ),
            api_keys: ApiKeysService::new(ApiKeys::new()),
            config_storage: ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new())),
            session_store: SessionStoreService::new(Arc::new(test_services::FakeSessionStore)),
            user_preferences_storage: {
                let svc = UserPreferencesStorageService::new(Arc::new(
                    InMemoryUserPreferencesStorage::new(),
                ));
                svc.reload().expect("test prefs storage initial reload");
                svc
            },
            app_state_storage: {
                let svc = AppStateStorageService::new(Arc::new(InMemoryAppStateStorage::new()));
                svc.reload().expect("test app state storage initial reload");
                svc
            },
            tempdir: Some(tempdir),
            bus,
            bridge,
            root_supervisor,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: RequestDumpService::default(),
            task_spawns: crate::feat::tools_actor::task_registry::TaskSpawnRegistry::default(),
            slices: crate::common::slices::Slices::new(),
            key_routes: crate::common::slices::key_routes::KeyRoutes::new(),
            viewport: crate::common::slices::view::Viewport::new(),
            overlay_views:
                crate::common::overlay_views::OverlayViews::<jinn_slices::RenderFacts>::new(),
            trouper_system: trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ),
            picker_registry: jinn_picker::PickerRegistry::new(),
        }
    }
}
