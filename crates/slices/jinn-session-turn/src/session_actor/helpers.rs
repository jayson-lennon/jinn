use jinn_core_types::SessionId;
use jinn_kernel::BusService;
use jinn_session_history_msg::HistoryAppended;

/// Emit a `HistoryAppended` event.
///
/// Call this outside the write lock.
pub(in crate::session_actor) async fn emit_history_appended(
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
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_llm_support::token_estimator::TiktokenCounter;
    use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;

    super::SessionPersistenceActor {
        state: State::new(AppState::default_with_scope_focus()),
        services: jinn_kernel::common::services::Services::new_fake().await,
        counter: TiktokenCounter::o200k_base(),
        token_cache: HistoryWorkerChatEntryTokenCache::default(),
        image_converter: test_image_converter(),
    }
}

#[cfg(test)]
pub(crate) fn ensure_context_assembly(system: &trouper::system::ActorSystem) {
    let _ = jinn_context_assembly::service::ensure_spawned(system);
}

#[cfg(test)]
#[cfg(test)]
pub(crate) async fn test_actor_recording() -> (
    super::SessionPersistenceActor,
    jinn_kernel::common::services::BusAudit,
) {
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_llm_support::token_estimator::TiktokenCounter;
    use jinn_token_count_msg::HistoryWorkerChatEntryTokenCache;

    let (bus, audit) = jinn_kernel::common::services::BusService::new_recording();
    let services = jinn_kernel::common::services::Services::new_fake_with_bus(bus.clone()).await;
    ensure_context_assembly(&services.trouper_system);

    // The phase actor: the sole phase writer, and the one every turn-path
    // publisher asks (the session actor's cancel, intercept, and retry
    // handlers; the queue actor). Production composition spawns it beside
    // the session actor over the same `State` — the harness must too, or
    // every admission ask errors and the handler refuses.
    let state = State::new(AppState::default_with_scope_focus());
    let _phase = crate::phase_actor::ensure_spawned(&services.trouper_system, state.clone(), bus);

    (
        super::SessionPersistenceActor {
            state,
            services,
            counter: TiktokenCounter::o200k_base(),
            token_cache: HistoryWorkerChatEntryTokenCache::default(),
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
    summaries: parking_lot::Mutex<Vec<jinn_session_store_msg::SessionSummary>>,
    sessions: parking_lot::Mutex<Vec<jinn_session_state::SessionSnapshot>>,
    archived: parking_lot::Mutex<Vec<jinn_core_types::SessionId>>,
    saved: parking_lot::Mutex<Vec<jinn_session_state::SessionSnapshot>>,
    fail_load_summaries: parking_lot::Mutex<bool>,
}

#[cfg(test)]
impl PopulatedFakeStore {
    pub(super) fn new(sessions: &[jinn_session_state::ChatSessionState]) -> Self {
        let summaries = sessions
            .iter()
            .map(|s| jinn_session_store_msg::SessionSummary {
                session_id: s.session_id().clone(),
                title: s.title().unwrap_or("Untitled Session").to_owned(),
                updated_at: *s.updated_at(),
                created_at: *s.created_at(),
                session_state: jinn_session_store_msg::SessionState::Loaded,
                parent_session: s.parent_session().clone(),
                project: s.project().map(std::path::Path::to_path_buf),
            })
            .collect();
        Self {
            summaries: Mutex::new(summaries),
            sessions: Mutex::new(
                sessions
                    .iter()
                    .map(jinn_session_state::ChatSessionState::capture_snapshot)
                    .collect(),
            ),
            archived: Mutex::new(Vec::new()),
            saved: Mutex::new(Vec::new()),
            fail_load_summaries: Mutex::new(false),
        }
    }

    pub(super) fn last_saved_session(
        &self,
        id: &jinn_core_types::SessionId,
    ) -> Option<jinn_session_state::SessionSnapshot> {
        self.saved
            .lock()
            .iter()
            .rev()
            .find(|s| s.session_id() == id)
            .cloned()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl jinn_session_state::SessionStore for PopulatedFakeStore {
    fn name(&self) -> &'static str {
        "populated-fake"
    }

    async fn save(
        &self,
        snapshot: &jinn_session_state::SessionSnapshot,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        self.saved.lock().push(snapshot.clone());
        // Upsert into the readable sessions vec (the real store persists the
        // session so later reads — fork, load — see it).
        let mut sessions = self.sessions.lock();
        match sessions
            .iter_mut()
            .find(|stored| stored.session_id() == snapshot.session_id())
        {
            Some(existing) => *existing = snapshot.clone(),
            None => sessions.push(snapshot.clone()),
        }
        Ok(())
    }

    async fn load_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        if *self.fail_load_summaries.lock() {
            return Err(error_stack::Report::new(
                jinn_session_state::SessionStoreError,
            ));
        }
        Ok(self.summaries.lock().clone())
    }

    async fn load_session(
        &self,
        session_id: &jinn_core_types::SessionId,
    ) -> Result<
        Option<jinn_session_state::SessionSnapshot>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(self
            .sessions
            .lock()
            .iter()
            .find(|s| s.session_id() == session_id)
            .cloned())
    }

    async fn delete(
        &self,
        _session_id: &jinn_core_types::SessionId,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        source_session_id: &jinn_core_types::SessionId,
        at_ordinal: usize,
    ) -> Result<
        jinn_core_types::SessionId,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        // Mirror the SQL fork's contract: error when the source is not in the
        // store, otherwise copy entries up to and including `at_ordinal` into
        // a new child session (fresh id, parent set).
        let sessions = self.sessions.lock();
        let Some(source) = sessions
            .iter()
            .find(|s| s.session_id() == source_session_id)
            .cloned()
        else {
            return Err(error_stack::Report::new(
                jinn_session_state::SessionStoreError,
            ));
        };
        drop(sessions);
        let new_id = jinn_core_types::SessionId::new();
        let mut forked = jinn_session_state::ChatSessionState::new();
        forked.set_session_id(new_id.clone());
        forked.set_parent_session(source_session_id.clone());
        if let Some(title) = source.title() {
            forked.set_title(title.to_owned());
        }
        for entry in source.history().iter().take(at_ordinal + 1) {
            forked.push_entry(entry.clone());
        }
        let forked = forked.capture_snapshot();
        self.sessions.lock().push(forked);
        // Keep summaries in sync so follow-up loads see the fork.
        self.summaries
            .lock()
            .push(jinn_session_store_msg::SessionSummary {
                session_id: new_id.clone(),
                title: source.title().unwrap_or("Untitled Session").to_owned(),
                updated_at: source.metadata.updated_at,
                created_at: source.metadata.created_at,
                session_state: jinn_session_store_msg::SessionState::Loaded,
                parent_session: Some(source_session_id.clone()),
                project: source.metadata.project.clone(),
            });
        Ok(new_id)
    }

    async fn set_archived(
        &self,
        session_id: &jinn_core_types::SessionId,
        archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        if archived {
            self.archived.lock().push(session_id.clone());
        }
        Ok(())
    }

    async fn set_archived_many(
        &self,
        session_ids: &[jinn_core_types::SessionId],
        archived: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        if archived {
            let mut archived = self.archived.lock();
            archived.extend(session_ids.iter().cloned());
        }
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<
        Vec<jinn_session_store_msg::SessionSummary>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(self.summaries.lock().clone())
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<
        Vec<jinn_core_types::SessionId>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _max_entries: usize,
    ) -> Result<bool, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(
        &self,
    ) -> Result<usize, error_stack::Report<jinn_session_state::SessionStoreError>> {
        Ok(0)
    }

    async fn search(
        &self,
        _params: jinn_session_store_msg::SearchParams,
    ) -> Result<
        jinn_session_store_msg::SearchOutcome,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(jinn_session_store_msg::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _anchor: &jinn_core_types::ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _limit: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<jinn_session_state::SessionStoreError>,
    > {
        Ok(None)
    }
}

#[cfg(test)]
pub(crate) async fn test_actor_with_store_recording(
    sessions: Vec<jinn_session_state::ChatSessionState>,
) -> (
    super::SessionPersistenceActor,
    std::sync::Arc<PopulatedFakeStore>,
    jinn_kernel::common::services::BusAudit,
) {
    let store = std::sync::Arc::new(PopulatedFakeStore::new(&sessions));
    let (bus, audit) = jinn_kernel::common::services::BusService::new_recording();
    let services = jinn_kernel::TestServices::builder()
        .session_store(jinn_session_state::SessionStoreService::new(store.clone()))
        .with_bus(bus)
        .build();
    (
        super::SessionPersistenceActor {
            state: jinn_kernel::common::state::State::new(
                jinn_kernel::common::app_state::AppState::default(),
            ),
            services,
            counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
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
pub(in crate::session_actor) fn test_image_converter()
-> jinn_llm_support::image_convert::ImageConverterService {
    jinn_llm_support::image_convert::ImageConverterService::unavailable()
}
