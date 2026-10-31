#![expect(clippy::expect_used, reason = "test infrastructure initialization")]
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use error_stack::Report;
use tokio::runtime::{Handle, Runtime};

use jinn_core_types::SessionId;
use jinn_preferences_config::{AppStateStorageService, InMemoryAppStateStorage};
use jinn_provider_config::{
    ApiKeys, ApiKeysService, ConfigStorageService, FakeLlmServiceFactory, InMemoryConfigStorage,
    LlmServiceFactoryService, ProviderRegistry, ProviderRegistryService, ProvidersConfig,
};
use jinn_session_state::SessionSnapshot;
use jinn_session_state::{SessionStore, SessionStoreError, SessionStoreService};
use jinn_session_store_msg::SessionSummary;

use super::Services;
/// Single shared tokio runtime for the entire test binary.
///
/// Initializes exactly once via `LazyLock`. Without this, every
/// `Services::new()` / `TestServices::build()` call leaked a fresh
/// `Runtime` via `Box::leak`, which (across 3000+ parallel tests)
/// exhausted the FD limit (`EMFILE`).
///
/// The `Runtime` itself is intentionally leaked via `Box::leak` at
/// static-init time; it lives for the lifetime of the test binary.
/// The `Handle` is cheaply cloneable and shared by all tests.
static TEST_RUNTIME: LazyLock<&'static Runtime> =
    LazyLock::new(|| Box::leak(Box::new(Runtime::new().expect("shared test runtime"))));

/// Returns a clone of the shared test runtime handle.
///
/// # Panics
///
/// Panics if the underlying tokio runtime fails to create (extremely
/// unlikely in tests).
pub(crate) fn shared_test_handle() -> Handle {
    TEST_RUNTIME.handle().clone()
}

/// A no-op session store for tests.
///
/// All operations succeed with empty results. Suitable for tests that
/// need a [`Services`] but don't test session persistence.
#[derive(Debug)]
pub struct FakeSessionStore;

#[async_trait]
impl SessionStore for FakeSessionStore {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn save(&self, _snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn delete(&self, _session_id: &SessionId) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &SessionId,
        _at_ordinal: usize,
    ) -> Result<SessionId, Report<SessionStoreError>> {
        Ok(SessionId::new())
    }

    async fn set_archived(
        &self,
        _session_id: &SessionId,
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[SessionId],
        _archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_in_library(
        &self,
        _session_id: &SessionId,
        _in_library: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &SessionId,
        _max_entries: usize,
    ) -> Result<bool, Report<SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>> {
        Ok(0)
    }

    async fn search(
        &self,
        _params: jinn_session_store_msg::SearchParams,
    ) -> Result<jinn_session_store_msg::SearchOutcome, Report<SessionStoreError>> {
        Ok(jinn_session_store_msg::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &SessionId,
        _anchor: &crate::protocol::ChatEntryId,
        _context: usize,
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        _limit: usize,
    ) -> Result<Option<jinn_session_store_msg::TranscriptWindow>, Report<SessionStoreError>> {
        Ok(None)
    }
}

/// A builder for constructing [Services] with fake implementations for tests.
///
/// All services default to empty/noop implementations. Use the builder methods
/// to customize specific services when needed.
///
/// Uses a leaked tokio runtime - acceptable for unit tests.
///
/// # Example
///
/// See the tests in this crate for full usage patterns.
pub struct TestServices {
    /// Provider configuration for the registry.
    providers: ProvidersConfig,
    /// Custom tokio runtime handle (if provided).
    handle: Option<Handle>,
    /// Custom LLM service factory (if provided).
    llm_service: Option<LlmServiceFactoryService>,
    /// Custom session store (if provided).
    session_store: Option<SessionStoreService>,
    /// Custom app paths (if provided).
    paths: Option<crate::common::app_paths::AppPaths>,
    bus_override: Option<super::bus_service::BusService>,
}

impl Default for TestServices {
    fn default() -> Self {
        Self {
            providers: ProvidersConfig {
                providers: std::collections::BTreeMap::new(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            },
            handle: None,
            llm_service: None,
            session_store: None,
            paths: None,
            bus_override: None,
        }
    }
}

impl TestServices {
    /// Create a new builder with defaults.
    #[must_use]
    pub fn builder() -> Self {
        Self::default()
    }

    /// Set the providers config.
    #[must_use]
    pub fn providers(mut self, providers: ProvidersConfig) -> Self {
        self.providers = providers;
        self
    }

    /// Set a custom runtime handle.
    #[must_use]
    pub fn handle(mut self, handle: Handle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Set a custom LLM service factory.
    #[must_use]
    pub fn llm_service(mut self, service: LlmServiceFactoryService) -> Self {
        self.llm_service = Some(service);
        self
    }

    /// Set a custom session store.
    #[must_use]
    pub fn session_store(mut self, store: SessionStoreService) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Set custom app paths.
    #[must_use]
    pub fn paths(mut self, paths: crate::common::app_paths::AppPaths) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Use the provided bus service instead of spawning a new one.
    #[must_use]
    pub fn with_bus(mut self, bus: super::bus_service::BusService) -> Self {
        self.bus_override = Some(bus);
        self
    }

    /// Build the [`Services`] instance.
    ///
    /// Uses the shared process-wide test runtime if no custom handle is provided.
    ///
    /// # Panics
    ///
    /// Panics if the tokio runtime fails to create (extremely unlikely in tests).
    #[must_use]
    #[expect(clippy::expect_used, reason = "test-only code, panics are acceptable")]
    pub fn build(self) -> Services {
        let handle = self.handle.unwrap_or_else(shared_test_handle);

        let (paths, tempdir) = if let Some(p) = self.paths {
            (p, None)
        } else {
            let td = Arc::new(jinn_testutil::retained_tempdir());
            (
                crate::common::app_paths::AppPaths::new_in(&td.path.as_marked_path()),
                Some(td),
            )
        };

        let bus = if let Some(override_bus) = self.bus_override {
            override_bus
        } else {
            super::bus_service::BusService::new_trouper(trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ))
        };
        let bridge = if bus.is_recording() {
            // Recording mode — no real bus, no bridge needed.
            crate::common::bridge::Bridge::new_dummy(&handle)
        } else {
            crate::common::bridge::Bridge::with_handle(bus.clone(), &handle)
        };

        Services {
            paths,
            handle,
            llm_service: self.llm_service.unwrap_or_else(|| {
                LlmServiceFactoryService::new(Arc::new(FakeLlmServiceFactory::new(vec![])))
            }),
            provider_registry: ProviderRegistryService::new(
                ProviderRegistry::from_config(self.providers).expect("test registry"),
            ),
            api_keys: ApiKeysService::new(ApiKeys::new()),
            config_storage: ConfigStorageService::new(Arc::new(InMemoryConfigStorage::new())),
            session_store: self
                .session_store
                .unwrap_or_else(|| SessionStoreService::new(Arc::new(FakeSessionStore))),
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
            tempdir,
            bus,
            bridge,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            canvas_server: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: crate::common::request_dump::RequestDumpService::default(),
            task_spawns: jinn_tools_msg::TaskSpawnRegistry::default(),
            slices: jinn_slices::Slices::new(),
            key_routes: jinn_slices::route::KeyRoutes::new(),
            viewport: jinn_slices::view::Viewport::new(),
            overlay_views: jinn_slices::OverlayViews::new(),
            trouper_system: trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ),
            project_picker: None,
            turn_ledger: crate::common::turn_ledger::TurnLedger::default(),
        }
    }
}
