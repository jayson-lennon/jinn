use crate::BusService;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::session::protocol::history_appended::HistoryAppended;
use crate::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use crate::protocol::SessionId;

/// Emit a `SessionPhaseChanged` event if the phase actually changed.
///
/// Call this outside the write lock with the before/after phases captured inside.
pub(in crate::feat::session::session_actor) async fn emit_phase_changed(
    bus: &BusService,
    session_id: &SessionId,
    old_phase: impl Into<PhaseKind>,
    new_phase: impl Into<PhaseKind>,
) {
    let old_phase = old_phase.into();
    let new_phase = new_phase.into();
    if old_phase != new_phase {
        bus.publish(SessionPhaseChanged {
            session_id: session_id.clone(),
            old_phase,
            new_phase,
        })
        .await;
    }
}

/// Emit a `HistoryAppended` event.
///
/// Call this outside the write lock.
pub(in crate::feat::session::session_actor) async fn emit_history_appended(
    bus: &BusService,
    session_id: &SessionId,
) {
    bus.publish(HistoryAppended {
        session_id: session_id.clone(),
    })
    .await;
}

#[cfg(test)]
pub(crate) async fn test_actor() -> super::SessionPersistenceActor {
    use crate::common::app_state::AppState;
    use crate::common::state::State;
    use crate::feat::auto_prune_worker::entry_token_cache::HistoryWorkerChatEntryTokenCache;
    use crate::feat::context::strategy::token_estimator::TiktokenCounter;

    super::SessionPersistenceActor {
        state: State::new(AppState::default_with_scope_focus()),
        cap: crate::common::tcaps::mint::mint_session_cap(),
        frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
        services: crate::common::services::Services::new_fake().await,
        counter: TiktokenCounter::o200k_base(),
        token_cache: HistoryWorkerChatEntryTokenCache::default(),
        builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry::new(),
        shell: "/bin/sh".to_owned(),
        lifecycle_child: None,
        image_converter: test_image_converter(),
    }
}

#[cfg(test)]
#[cfg(test)]
pub(crate) async fn test_actor_recording() -> (
    super::SessionPersistenceActor,
    crate::common::services::BusAudit,
) {
    use crate::common::app_state::AppState;
    use crate::common::state::State;
    use crate::feat::auto_prune_worker::entry_token_cache::HistoryWorkerChatEntryTokenCache;
    use crate::feat::context::strategy::token_estimator::TiktokenCounter;

    let (bus, audit) = crate::common::services::BusService::new_recording();
    let mut services = crate::common::services::Services::new_fake_with_bus(bus).await;
    // Dispatch paths assemble through the trouper context-assembly
    // service; spawn it so session-actor tests exercise the real ask.
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);

    (
        super::SessionPersistenceActor {
            state: State::new(AppState::default()),
            cap: crate::common::tcaps::mint::mint_session_cap(),
            frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
            services,
            counter: TiktokenCounter::o200k_base(),
            token_cache: HistoryWorkerChatEntryTokenCache::default(),
            builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry::new(),
            shell: "/bin/sh".to_owned(),
            lifecycle_child: None,
            image_converter: test_image_converter(),
        },
        audit,
    )
}

#[cfg(test)]
use parking_lot::Mutex;

#[cfg(test)]
/// A fake session store that returns pre-loaded sessions for testing.
pub(crate) struct PopulatedFakeStore {
    summaries: parking_lot::Mutex<Vec<crate::feat::session::session_summary::SessionSummary>>,
    sessions: Vec<crate::feat::session::chat_session::ChatSessionState>,
    archived: parking_lot::Mutex<Vec<crate::protocol::SessionId>>,
    saved: parking_lot::Mutex<Vec<crate::feat::session::chat_session::ChatSessionState>>,
    fail_load_summaries: parking_lot::Mutex<bool>,
}

#[cfg(test)]
impl PopulatedFakeStore {
    pub(super) fn new(sessions: Vec<crate::feat::session::chat_session::ChatSessionState>) -> Self {
        let summaries = sessions
            .iter()
            .map(|s| crate::feat::session::session_summary::SessionSummary {
                session_id: s.session_id().clone(),
                title: s.title().unwrap_or("Untitled Session").to_owned(),
                updated_at: *s.updated_at(),
                created_at: *s.created_at(),
                session_state: crate::feat::session::chat_session::SessionState::Loaded,
                parent_session: s.parent_session().clone(),
                project: s.project().map(std::path::Path::to_path_buf),
            })
            .collect();
        Self {
            summaries: Mutex::new(summaries),
            sessions,
            archived: Mutex::new(Vec::new()),
            saved: Mutex::new(Vec::new()),
            fail_load_summaries: Mutex::new(false),
        }
    }

    pub(super) fn last_saved_session(
        &self,
        id: &crate::protocol::SessionId,
    ) -> Option<crate::feat::session::chat_session::ChatSessionState> {
        self.saved
            .lock()
            .iter()
            .rev()
            .find(|s| s.session_id() == id)
            .cloned()
    }

    /// Snapshot of all IDs passed to `set_archived`/`set_archived_many`
    /// (append order, duplicates preserved).
    pub(super) fn archived_ids(&self) -> Vec<crate::protocol::SessionId> {
        self.archived.lock().clone()
    }

    /// Replaces the summaries the store reports (simulates store-only rows).
    pub(super) fn set_summaries(
        &self,
        summaries: Vec<crate::feat::session::session_summary::SessionSummary>,
    ) {
        *self.summaries.lock() = summaries;
    }

    /// Makes `load_summaries` fail (simulates a store read error).
    pub(super) fn set_fail_load_summaries(&self, fail: bool) {
        *self.fail_load_summaries.lock() = fail;
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl crate::feat::session::session_store::SessionStore for PopulatedFakeStore {
    fn name(&self) -> &'static str {
        "populated-fake"
    }

    async fn save(
        &self,
        session: &crate::feat::session::chat_session::ChatSessionState,
    ) -> Result<(), error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        self.saved.lock().push(session.clone());
        Ok(())
    }

    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<crate::feat::session::session_summary::SessionSummary>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        if *self.fail_load_summaries.lock() {
            return Err(error_stack::Report::new(
                crate::feat::session::session_store::SessionStoreError,
            ));
        }
        Ok(self.summaries.lock().clone())
    }

    async fn load_session(
        &self,
        session_id: &crate::protocol::SessionId,
    ) -> Result<
        Option<crate::feat::session::chat_session::ChatSessionState>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(self
            .sessions
            .iter()
            .find(|s| s.session_id() == session_id)
            .cloned())
    }

    async fn delete(
        &self,
        _session_id: &crate::protocol::SessionId,
    ) -> Result<(), error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &crate::protocol::SessionId,
        _at_ordinal: usize,
    ) -> Result<
        crate::protocol::SessionId,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(crate::protocol::SessionId::new())
    }

    async fn set_archived(
        &self,
        session_id: &crate::protocol::SessionId,
        archived: bool,
    ) -> Result<(), error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        if archived {
            self.archived.lock().push(session_id.clone());
        }
        Ok(())
    }

    async fn set_archived_many(
        &self,
        session_ids: &[crate::protocol::SessionId],
        archived: bool,
    ) -> Result<(), error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        if archived {
            let mut archived = self.archived.lock();
            archived.extend(session_ids.iter().cloned());
        }
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<
        Vec<crate::feat::session::session_summary::SessionSummary>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(self.summaries.lock().clone())
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<
        Vec<crate::protocol::SessionId>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &crate::protocol::SessionId,
        _max_entries: usize,
    ) -> Result<bool, error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        Ok(true)
    }

    async fn pending_dirty_count(
        &self,
    ) -> Result<usize, error_stack::Report<crate::feat::session::session_store::SessionStoreError>>
    {
        Ok(0)
    }

    async fn search(
        &self,
        _params: crate::feat::session_search::SearchParams,
    ) -> Result<
        crate::feat::session_search::SearchOutcome,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(crate::feat::session_search::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &crate::protocol::SessionId,
        _anchor: &crate::protocol::ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<crate::feat::session_search::TranscriptWindow>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &crate::protocol::SessionId,
        _limit: usize,
    ) -> Result<
        Option<crate::feat::session_search::TranscriptWindow>,
        error_stack::Report<crate::feat::session::session_store::SessionStoreError>,
    > {
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) async fn test_actor_with_store_recording(
    sessions: Vec<crate::feat::session::chat_session::ChatSessionState>,
) -> (
    super::SessionPersistenceActor,
    std::sync::Arc<PopulatedFakeStore>,
    crate::common::services::BusAudit,
) {
    let store = std::sync::Arc::new(PopulatedFakeStore::new(sessions));
    let (bus, audit) = crate::common::services::BusService::new_recording();
    let services = crate::TestServices::builder()
        .session_store(crate::feat::session::SessionStoreService::new(
            store.clone(),
        ))
        .with_bus(bus)
        .build();
    (
        super::SessionPersistenceActor {
            state: crate::common::state::State::new(crate::common::app_state::AppState::default()),
            cap: crate::common::tcaps::mint::mint_session_cap(),
            frontend_cap: crate::common::tcaps::mint::mint_frontend_cap(),
            services,
            counter: crate::feat::context::strategy::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: crate::feat::auto_prune_worker::entry_token_cache::HistoryWorkerChatEntryTokenCache::default(),
            builtin_registry: crate::feat::session_lifecycle::builtin::BuiltinRegistry::new(),
            shell: "/bin/sh".to_owned(),
            lifecycle_child: None,
            image_converter: test_image_converter(),
        },
        store,
        audit,
    )
}

/// Constructs an [`ImageConverterService`] for tests. Uses a no-op
/// converter so tests don't spawn ImageMagick. Actors that test the
/// conversion path inject their own converter.
#[cfg(test)]
pub(in crate::feat::session::session_actor) fn test_image_converter()
-> crate::feat::image_convert::ImageConverterService {
    crate::feat::image_convert::ImageConverterService::unavailable()
}
