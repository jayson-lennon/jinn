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

    // A live turn for the active session: production mints before any token
    // or completion can arrive, so the harness mints the same way.
    let state = State::new(AppState::default_with_scope_focus());
    let services = jinn_kernel::common::services::Services::new_fake().await;
    {
        let session_id = state.read().session.active_session_id().clone();
        let _ = services.turn_ledger.mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        );
    }

    super::SessionPersistenceActor {
        state,
        services,
        counter: TiktokenCounter::o200k_base(),
        token_cache: HistoryWorkerChatEntryTokenCache::default(),
        image_converter: test_image_converter(),
        handoff_prompt: String::new(),
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

    let state = State::new(AppState::default_with_scope_focus());

    // A live turn for the active session: production mints before any
    // completion or token can arrive, so the harness mints the same way.
    {
        let session_id = state.read().session.active_session_id().clone();
        let _ = services.turn_ledger.mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        );
    }

    (
        super::SessionPersistenceActor {
            state,
            services,
            counter: TiktokenCounter::o200k_base(),
            token_cache: HistoryWorkerChatEntryTokenCache::default(),
            image_converter: test_image_converter(),
            handoff_prompt: String::new(),
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
    /// The lock-release probe: when armed with the actor's state handle,
    /// every `save` attempts one `try_write` and records whether it
    /// succeeded. A save that ran while the actor still held the write lock
    /// (the regression this detects) records `false`; the released-lock flow
    /// records `true` every time — one bit per save, no timing to flake.
    write_probe: parking_lot::Mutex<Option<WriteProbe>>,
}

#[cfg(test)]
struct WriteProbe {
    state: jinn_kernel::common::state::State,
    can_write: std::sync::Arc<parking_lot::Mutex<bool>>,
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
                in_library: s.in_library(),
                is_bot: s.is_bot(),
                parent_session: s.parent_session().clone(),
                project: s.project().cloned(),
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
            write_probe: Mutex::new(None),
        }
    }

    /// Arms the lock-release probe against the actor's state: every save
    /// attempts one `try_write` and ANDs the result into `can_write`.
    pub(super) fn arm_write_probe(
        &self,
        state: jinn_kernel::common::state::State,
        can_write: std::sync::Arc<parking_lot::Mutex<bool>>,
    ) {
        *self.write_probe.lock() = Some(WriteProbe { state, can_write });
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
        // The lock-release probe runs inside the store's save — the point
        // the save flow reaches only after the capture clone has fully
        // resolved. A try_write here fails exactly when the actor's write
        // lock is still held across the clone, which is the regression the
        // old sampling probe approximated with ratios.
        if let Some(probe) = self.write_probe.lock().as_ref() {
            let acquired = probe.state.try_write().is_some();
            *probe.can_write.lock() &= acquired;
        }
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
                in_library: false,
                is_bot: false,
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

    async fn set_in_library(
        &self,
        session_id: &jinn_core_types::SessionId,
        in_library: bool,
    ) -> Result<(), error_stack::Report<jinn_session_state::SessionStoreError>> {
        for summary in self.summaries.lock().iter_mut() {
            if &summary.session_id == session_id {
                summary.in_library = in_library;
            }
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

    // A live turn for the active session: production mints before any
    // completion can arrive, so the harness mints the same way. One `State`
    // is built here and handed to the actor — the minted session id and the
    // actor's active session must be the same one.
    let state = jinn_kernel::common::state::State::new(
        jinn_kernel::common::app_state::AppState::default_with_scope_focus(),
    );
    {
        let session_id = state.read().session.active_session_id().clone();
        let _ = services.turn_ledger.mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        );
    }

    (
        super::SessionPersistenceActor {
            state,
            services,
            counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
            image_converter: test_image_converter(),
            handoff_prompt: String::new(),
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
