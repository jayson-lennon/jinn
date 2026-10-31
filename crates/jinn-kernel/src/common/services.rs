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

use jinn_preferences_config::{AppStateStorageService, InMemoryAppStateStorage};

pub use jinn_provider_config;
use jinn_provider_config::{
    ApiKeys, ApiKeysService, ConfigStorageService, InMemoryConfigStorage, LlmServiceFactoryService,
    ProviderRegistry, ProviderRegistryService, ProvidersConfig,
};
use jinn_session_state::SessionStoreService;
use tokio::runtime::Handle;

use crate::common::request_dump::RequestDumpService;

pub mod test_services;

/// Re-export of the bus service, which now lives in `jinn-slices`.
///
/// `BusService` is shared actor-fabric vocabulary consumed by every slice, so it
/// is no longer kernel-owned. This module is a path alias only; the
/// implementation is `jinn_slices::bus`.
pub mod bus_service {
    pub use jinn_slices::bus::{BusAudit, BusService, RecordedMessage};
}

pub use jinn_slices::bus::BusService;
pub use jinn_slices::bus::{BusAudit, RecordedMessage};

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

    /// The configuration layer: every `jinn.toml` value is read and
    /// written through this handle, and none is cached anywhere else.
    pub config: jinn_config::ConfigLayer,
    /// The shells jinn runs commands through, resolved once at launch.
    ///
    /// The environment is probed here and nowhere else: a tool, the PTY,
    /// and the lifecycle runner all read the resolved value rather than
    /// re-probing `PATH` for themselves.
    pub shells: jinn_common::shell::Shells,
    /// App state storage for persisting `state.toml`.
    pub app_state_storage: AppStateStorageService,
    /// Test-only owned temp directory. `None` in production.
    ///
    /// Held here so the dir outlives the [`AppPaths`] that points at it.
    /// Cleanup cannot rely on `Drop`: spawned actors retain their `Services`
    /// (via their deps structs) in trouper's registry for the process
    /// lifetime, so the dir is created kept (`keep(true)`) by
    /// [`jinn_testutil::retained_tempdir`] and removed by that module's
    /// exit destructor when the test binary exits.
    /// Production code passes `None` because [`AppPaths::default`] resolves
    /// real user dirs.
    #[debug(skip)]
    pub tempdir: Option<Arc<jinn_testutil::JinnTempDir>>,

    /// Message fabric for schema-based pub/sub routing.
    #[debug(skip)]
    pub bus: bus_service::BusService,

    /// Kanal closure bridge from sync TUI to async bus.
    pub bridge: crate::common::bridge::Bridge,

    #[debug(skip)]
    pub mcp_coordinator:
        Arc<std::sync::OnceLock<std::sync::Arc<dyn jinn_mcp_msg::McpCoordinatorHandle>>>,

    /// Canvas serving handle, exposed to the tool layer (the `canvas`
    /// tool) after the canvas slice's activation spawns its actor and
    /// mints the implementation. Unset until then; the tool's graceful
    /// error path covers the gap.
    #[debug(skip)]
    pub canvas_server:
        Arc<std::sync::OnceLock<std::sync::Arc<dyn jinn_canvas_msg::CanvasServerHandle>>>,

    /// Interactive-term coordinator handle, exposed to the tool layer
    /// (the `interactive_term*` tools) after actor wiring spawns the
    /// term slice's coordinator and mints the implementation.
    #[debug(skip)]
    pub interactive_term: Arc<std::sync::OnceLock<std::sync::Arc<dyn jinn_term_msg::TermHandle>>>,

    /// Request dump directory. `None` disables dumping (default).
    pub request_dump: RequestDumpService,

    /// In-flight subagent registry: parent → child sessions spawned by the
    /// `spawn_subagent` tool. Read by the stall watchdog to skip waiting parents.
    #[debug(skip)]
    pub task_spawns: jinn_tools_msg::TaskSpawnRegistry,

    /// Dynamic registry of per-slice render cells.
    ///
    /// `register` mints the one write handle for a slice; the renderer
    /// and intent router hold read handles only. Shared by all clones.
    #[debug(skip)]
    pub slices: jinn_slices::Slices,

    /// Feature-registered keybind routes (intent → message).
    ///
    /// The intent handler consults this table before its own arms; rows
    /// attach after startup wiring as features register.
    #[debug(skip)]
    pub key_routes: jinn_slices::route::KeyRoutes,

    /// Erased slice views, one per rendered slot. Views pair with their
    /// slice at registration (type-checked at startup); the renderer asks
    /// the viewport for the active slot's view instead of hand-written
    /// tab code.
    pub viewport: jinn_slices::view::Viewport,

    /// Slice-registered overlay renderers for dynamic scopes. Written at
    /// activation; the generic overlay pass resolves the active scope's
    /// renderer.
    #[debug(skip)]
    pub overlay_views: jinn_slices::OverlayViews<jinn_slices::RenderFacts>,

    /// Actor-canvas runtime system hosting the ported slice actors
    /// (dashboard, quake-bar). Built once here; slice `activate` functions
    /// spawn their canvas actors onto it and subscribe them to topics fed
    /// by the fabric canvas actor.
    #[debug(skip)]
    pub trouper_system: trouper::system::ActorSystem,

    /// The project picker's cell, published by the project slice so the
    /// preferences actor can refresh an open menu after a save.
    pub project_picker: Option<jinn_slices::cell::TypedCell<jinn_project_msg::ProjectPickerState>>,

    /// The turn ledger: the sole authority for turn-scoped admission.
    ///
    /// One map of live turns keyed by session; no entry means `Idle`.
    /// Turn work asks here, never a stored phase snapshot.
    #[debug(skip)]
    pub turn_ledger: crate::common::turn_ledger::TurnLedger,
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
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "async signature symmetry; body has no await"
    )]
    #[expect(
        clippy::unused_async,
        reason = "async signature symmetry; body has no await"
    )]
    pub async fn new_fake() -> Self {
        let handle = test_services::shared_test_handle();

        let tempdir = Arc::new(jinn_testutil::retained_tempdir());

        // Create the fabric once; the bus publishes through it and the
        // `Services` container carries it for actor spawns — one system.
        let (bus, trouper_system) = {
            let system =
                trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
            (bus_service::BusService::new_trouper(system.clone()), system)
        };
        let bridge = crate::common::bridge::Bridge::new(bus.clone());

        Self {
            paths: crate::common::app_paths::AppPaths::new_in(&tempdir.path.as_marked_path()),
            handle,
            llm_service: LlmServiceFactoryService::new(Arc::new(
                jinn_provider_config::FakeLlmServiceFactory::new(vec![]),
            )),
            provider_registry: ProviderRegistryService::new(
                ProviderRegistry::from_config(ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                })
                .expect("empty config is valid"),
            ),
            api_keys: ApiKeysService::new(ApiKeys::new()),
            config_storage: ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new())),
            session_store: SessionStoreService::new(Arc::new(test_services::FakeSessionStore)),
            config: jinn_config::ConfigLayer::load(Arc::new(
                jinn_config::InMemoryConfigStorage::default(),
            ))
            .expect("test config layer initial load"),
            app_state_storage: {
                let svc = AppStateStorageService::new(Arc::new(InMemoryAppStateStorage::new()));
                svc.reload().expect("test app state storage initial reload");
                svc
            },
            shells: jinn_common::shell::Shells::default(),
            tempdir: Some(tempdir),
            bus,
            bridge,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            canvas_server: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: RequestDumpService::default(),
            task_spawns: jinn_tools_msg::TaskSpawnRegistry::default(),
            slices: seeded_slices(),
            key_routes: jinn_slices::route::KeyRoutes::new(),
            viewport: jinn_slices::view::Viewport::new(),
            overlay_views: jinn_slices::OverlayViews::<jinn_slices::RenderFacts>::new(),
            // The same fabric the bus publishes through: one `Services`,
            // one trouper system.
            trouper_system,
            project_picker: None,
            turn_ledger: crate::common::turn_ledger::TurnLedger::default(),
        }
    }

    /// Construct a fake Services with a pre-built bus (e.g. BusService::new_recording()).
    /// # Panics
    ///
    /// Panics if the embedded temp dir, provider registry, or storage
    /// reloads fail — test infrastructure initialization must abort.
    #[expect(clippy::expect_used, reason = "test infrastructure initialization")]
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "async signature symmetry; body has no await"
    )]
    #[expect(
        clippy::unused_async,
        reason = "async signature symmetry; body has no await"
    )]
    pub async fn new_fake_with_bus(bus: bus_service::BusService) -> Self {
        let handle = test_services::shared_test_handle();
        let tempdir = Arc::new(jinn_testutil::retained_tempdir());

        let bridge = crate::common::bridge::Bridge::new_for_test();
        let trouper_system =
            trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());

        Self {
            paths: crate::common::app_paths::AppPaths::new_in(&tempdir.path.as_marked_path()),
            handle,
            llm_service: LlmServiceFactoryService::new(Arc::new(
                jinn_provider_config::FakeLlmServiceFactory::new(vec![]),
            )),
            provider_registry: ProviderRegistryService::new(
                ProviderRegistry::from_config(ProvidersConfig {
                    providers: std::collections::BTreeMap::new(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                })
                .expect("empty config is valid"),
            ),
            api_keys: ApiKeysService::new(ApiKeys::new()),
            config_storage: ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new())),
            session_store: SessionStoreService::new(Arc::new(test_services::FakeSessionStore)),
            config: jinn_config::ConfigLayer::load(Arc::new(
                jinn_config::InMemoryConfigStorage::default(),
            ))
            .expect("test config layer initial load"),
            app_state_storage: {
                let svc = AppStateStorageService::new(Arc::new(InMemoryAppStateStorage::new()));
                svc.reload().expect("test app state storage initial reload");
                svc
            },
            shells: jinn_common::shell::Shells::default(),
            tempdir: Some(tempdir),
            bus,
            bridge,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            canvas_server: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: RequestDumpService::default(),
            task_spawns: jinn_tools_msg::TaskSpawnRegistry::default(),
            slices: seeded_slices(),
            key_routes: jinn_slices::route::KeyRoutes::new(),
            viewport: jinn_slices::view::Viewport::new(),
            overlay_views: jinn_slices::OverlayViews::<jinn_slices::RenderFacts>::new(),
            // The same fabric the bus publishes through: one `Services`,
            // one trouper system.
            trouper_system,
            project_picker: None,
            turn_ledger: crate::common::turn_ledger::TurnLedger::default(),
        }
    }
}

#[cfg(test)]
impl Services {
    /// Test wiring: spawn the context-assembly service on this fake
    /// services' trouper system, mirroring production composition.
    /// (The slice crate is a dev-dependency; the production spawn lives
    /// in `src/actor_wiring.rs`.)
    #[expect(
        clippy::unused_async,
        reason = "async for API symmetry with production wiring"
    )]
    pub async fn spawn_context_assembly_for_test(&mut self) {
        let _ = jinn_context_assembly::service::spawn(&self.trouper_system);
    }
}

/// A test registry with every slice cell registered.
///
/// The same catalog production boot uses: a fake that seeded a private
/// subset diverged from the real wiring, and a test relying on a cell it
/// happened not to seed failed only by rendering nothing.
fn seeded_slices() -> jinn_slices::Slices {
    let slices = jinn_slices::Slices::new();
    jinn_cell_catalog::register_all_cells(&slices);
    slices
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test-only assertions")]

    use super::*;
    use marked_path::MarkedPathAccess as _;

    #[rstest::rstest]
    #[tokio::test]
    async fn new_fake_fixture_paths_are_prefixed() {
        // Given the fake services fixture.
        let services = Services::new_fake().await;

        // When reading the fixture's config dir.
        let config_dir = services.paths.config_dir();

        // Then it sits under a root carrying the jinn test prefix, and the
        // fixture's retained tempdir is that same root.
        let root = services
            .tempdir
            .as_ref()
            .expect("new_fake always carries a tempdir")
            .path
            .clone();
        let root_name = root
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("named temp root");
        assert!(
            root_name.starts_with(jinn_testutil::TEST_PREFIX),
            "fixture root {root_name:?} lacks the jinn test prefix"
        );
        assert_eq!(
            config_dir.as_path(),
            root.join("config")
                .expect("name join keeps the root absolute")
                .as_path(),
            "config_dir must derive from the prefixed fixture root"
        );
    }
}
