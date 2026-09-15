#![expect(clippy::expect_used, reason = "test infrastructure initialization")]
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use error_stack::Report;
use kameo::actor::Spawn;
use tokio::runtime::{Handle, Runtime};

use crate::feat::preferences_actor::{
    AppStateStorageService, InMemoryAppStateStorage, InMemoryUserPreferencesStorage,
    UserPreferencesStorageService,
};
use crate::feat::provider_infra::{
    ApiKeys, ApiKeysService, ConfigStorageService, FakeLlmServiceFactory, InMemoryConfigStorage,
    LlmServiceFactoryService, ProviderRegistry, ProviderRegistryService, ProvidersConfig,
};
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::{SessionStore, SessionStoreError, SessionStoreService, SessionSummary};
use crate::protocol::SessionId;

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

    async fn save(&self, _session: &ChatSessionState) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<ChatSessionState>, Report<SessionStoreError>> {
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
        _params: crate::feat::session_search::SearchParams,
    ) -> Result<crate::feat::session_search::SearchOutcome, Report<SessionStoreError>> {
        Ok(crate::feat::session_search::SearchOutcome {
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
    ) -> Result<Option<crate::feat::session_search::TranscriptWindow>, Report<SessionStoreError>>
    {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        _limit: usize,
    ) -> Result<Option<crate::feat::session_search::TranscriptWindow>, Report<SessionStoreError>>
    {
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

    /// Alias for [`providers`](Self::providers) for backward compat.
    #[must_use]
    pub fn with_providers(self, providers: ProvidersConfig) -> Self {
        self.providers(providers)
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
            let td = Arc::new(tempfile::TempDir::new().expect("test temp dir"));
            (
                crate::common::app_paths::AppPaths::new_in(td.path()),
                Some(td),
            )
        };

        let bus = if let Some(override_bus) = self.bus_override {
            override_bus
        } else {
            let bus_actor = kameo_actors::message_bus::MessageBus::new(
                kameo_actors::DeliveryStrategy::BestEffort,
            );
            // MessageBus::spawn calls tokio::spawn internally.
            // If we're already inside a tokio runtime, use it directly.
            // Otherwise, enter the shared test runtime via block_on.
            let bus_ref = if tokio::runtime::Handle::try_current().is_ok() {
                kameo_actors::message_bus::MessageBus::spawn(bus_actor)
            } else {
                TEST_RUNTIME
                    .block_on(async { kameo_actors::message_bus::MessageBus::spawn(bus_actor) })
            };
            super::bus_service::BusService::new(bus_ref)
        };
        let bridge = if bus.is_recording() {
            // Recording mode — no real bus, no bridge needed.
            crate::common::bridge::Bridge::new_dummy(&handle)
        } else {
            crate::common::bridge::Bridge::with_handle(bus.actor_ref().clone(), &handle)
        };

        // RootSupervisor::spawn calls tokio::spawn internally — same runtime
        // detection as the bus spawn above.
        let root_supervisor = if tokio::runtime::Handle::try_current().is_ok() {
            crate::common::root_supervisor::RootSupervisor::spawn(
                crate::common::root_supervisor::RootSupervisor,
            )
        } else {
            TEST_RUNTIME.block_on(async {
                crate::common::root_supervisor::RootSupervisor::spawn(
                    crate::common::root_supervisor::RootSupervisor,
                )
            })
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
            user_preferences_storage: {
                let svc = UserPreferencesStorageService::new(Arc::new(
                    InMemoryUserPreferencesStorage::new(),
                ));
                // Populate the cache so test code that calls .read() works.
                // InMemoryUserPreferencesStorage returns Ok(default) when empty.
                svc.reload().expect("test prefs storage initial reload");
                svc
            },
            app_state_storage: {
                let svc = AppStateStorageService::new(Arc::new(InMemoryAppStateStorage::new()));
                svc.reload().expect("test app state storage initial reload");
                svc
            },
            tempdir,
            bus,
            bridge,
            root_supervisor,
            mcp_coordinator: Arc::new(std::sync::OnceLock::new()),
            interactive_term: Arc::new(std::sync::OnceLock::new()),
            request_dump: crate::common::request_dump::RequestDumpService::default(),
            task_spawns: crate::feat::tools_actor::task_registry::TaskSpawnRegistry::default(),
            slices: crate::common::slices::Slices::new(),
            key_routes: crate::common::slices::key_routes::KeyRoutes::new(),
            viewport: crate::common::slices::view::Viewport::new(),
            overlay_views: crate::common::overlay_views::OverlayViews::new(),
            trouper_system: trouper::system::ActorSystem::new(
                trouper::system::SystemConfig::production(),
            ),
            picker_registry: jinn_picker::PickerRegistry::new(),
        }
    }
}
