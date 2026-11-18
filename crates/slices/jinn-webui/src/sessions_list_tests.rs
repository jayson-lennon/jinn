//! The sessions-list tests: the merged list's ordering law, the
//! `?q=` server-side filter, and the per-row active flag.
//!
//! The ordering is observable through both render paths — the pull
//! fragment (`sessions_fragment`, live + archived) and the fan-out's
//! pushed fragment (`sessions_live_fragment`, live only) — so each
//! behavior is asserted on the path that can express it: grouping on
//! both, the filter and the archived-row flag on the pull. The store
//! is a seeded fake; the kernel's `FakeSessionStore` returns empty
//! summaries and cannot produce an archived group.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use std::sync::Arc;
use std::sync::Mutex;

use jinn_app_state::AppState;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_session_state::SessionStore;
use jinn_session_state::SessionStoreError;
use jinn_session_store_msg::SessionState;
use jinn_session_store_msg::SessionSummary;
use jinn_slices::Slices;
use jinn_slices::bus::BusService;

use crate::contract;
use crate::server::ServerShared;

/// A store fake returning a fixed summary set — the archived side of
/// the merged list.
#[derive(Debug, Default)]
struct SeededSummaryStore {
    summaries: Mutex<Vec<SessionSummary>>,
}

impl SeededSummaryStore {
    fn with(summaries: Vec<SessionSummary>) -> Self {
        Self {
            summaries: Mutex::new(summaries),
        }
    }
}

#[async_trait::async_trait]
impl SessionStore for SeededSummaryStore {
    fn name(&self) -> &'static str {
        "seeded-summaries"
    }

    async fn save(
        &self,
        _snapshot: &jinn_session_state::SessionSnapshot,
    ) -> Result<(), error_stack::Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, error_stack::Report<SessionStoreError>> {
        Ok(self.summaries.lock().expect("summary lock").clone())
    }

    async fn load_session(
        &self,
        _session_id: &SessionId,
    ) -> Result<Option<jinn_session_state::SessionSnapshot>, error_stack::Report<SessionStoreError>>
    {
        Ok(None)
    }

    async fn delete(
        &self,
        _session_id: &SessionId,
    ) -> Result<(), error_stack::Report<SessionStoreError>> {
        Ok(())
    }

    async fn fork(
        &self,
        _source_session_id: &SessionId,
        _at_ordinal: usize,
    ) -> Result<SessionId, error_stack::Report<SessionStoreError>> {
        Ok(SessionId::new())
    }

    async fn set_archived(
        &self,
        _session_id: &SessionId,
        _archived: bool,
    ) -> Result<(), error_stack::Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_archived_many(
        &self,
        _session_ids: &[SessionId],
        _archived: bool,
    ) -> Result<(), error_stack::Report<SessionStoreError>> {
        Ok(())
    }

    async fn set_in_library(
        &self,
        _session_id: &SessionId,
        _in_library: bool,
    ) -> Result<(), error_stack::Report<SessionStoreError>> {
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, error_stack::Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn dirty_session_ids(
        &self,
    ) -> Result<Vec<SessionId>, error_stack::Report<SessionStoreError>> {
        Ok(Vec::new())
    }

    async fn reindex_session_chunk(
        &self,
        _session_id: &SessionId,
        _max_entries: usize,
    ) -> Result<bool, error_stack::Report<SessionStoreError>> {
        Ok(true)
    }

    async fn pending_dirty_count(&self) -> Result<usize, error_stack::Report<SessionStoreError>> {
        Ok(0)
    }

    async fn search(
        &self,
        _params: jinn_session_store_msg::SearchParams,
    ) -> Result<jinn_session_store_msg::SearchOutcome, error_stack::Report<SessionStoreError>> {
        Ok(jinn_session_store_msg::SearchOutcome {
            total_matches: 0,
            per_session: Vec::new(),
            hits: Vec::new(),
        })
    }

    async fn fetch_window(
        &self,
        _session_id: &SessionId,
        _anchor: &jinn_core_types::ChatEntryId,
        _context: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<SessionStoreError>,
    > {
        Ok(None)
    }

    async fn fetch_tail(
        &self,
        _session_id: &SessionId,
        _limit: usize,
    ) -> Result<
        Option<jinn_session_store_msg::TranscriptWindow>,
        error_stack::Report<SessionStoreError>,
    > {
        Ok(None)
    }
}

/// One archived session's summary.
fn archived_summary(id: SessionId, title: &str) -> SessionSummary {
    SessionSummary {
        session_id: id,
        title: title.to_owned(),
        updated_at: jiff::Timestamp::now(),
        created_at: jiff::Timestamp::now(),
        session_state: SessionState::Archived,
        in_library: false,
        is_bot: false,
        parent_session: None,
        project: None,
    }
}

/// The harness: one state, a seeded store, and the shared handler
/// state over both. Live sessions seed through the returned [`State`].
fn harness(summaries: Vec<SessionSummary>) -> (State, ServerShared) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone())
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(Arc::new(SeededSummaryStore::with(summaries))),
        provider_cell,
        fanout,
        0,
        "sessions-list-test".to_owned(),
    );
    (state, shared)
}

/// Seeds a live session with the given title, returning its id.
fn seed_titled(state: &State, title: &str) -> SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_title(title.to_owned());
    id
}

/// One rendered row: its session id and active flag, in render order.
struct Row {
    session_id: String,
    active: bool,
}

/// Parses the fragment into its rows, in render order. Rows are the
/// template's `<a class="srow …" data-session-id="…">` anchors; the
/// parse reads each anchor tag's attributes, in order.
fn rows_in_order(html: &str) -> Vec<Row> {
    html.split("<a ")
        .skip(1)
        .map(|anchor| {
            let tag = anchor.split('>').next().unwrap_or_default();
            let class = tag
                .split("class=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_default();
            let session_id = tag
                .split(&format!("{}=\"", contract::SESSION_ID_DATA_ATTR))
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_default()
                .to_owned();
            Row {
                session_id,
                active: class.split(' ').any(|part| part == "active"),
            }
        })
        .filter(|row| !row.session_id.is_empty())
        .collect()
}

/// The ids in the rendered fragment, in row order.
fn ids_in_order(html: &str) -> Vec<String> {
    rows_in_order(html)
        .into_iter()
        .map(|row| row.session_id)
        .collect()
}

/// The instance's active session id before any test-made switch.
fn active_id(state: &State) -> SessionId {
    state.read().session.active_session_id().clone()
}

// ── The ordering law (both paths) ───────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_pull_lists_loaded_rows_above_archived_rows() {
    // Given one loaded session titled "Zebra" (activated) plus the
    // map's default session, and one archived titled "Aardvark" — the
    // title order that floated the archived row above the loaded ones
    // under the old page-level sort.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let untitled = active_id(&state);
    let loaded = seed_titled(&state, "Zebra");
    assert!(
        state.write().session.set_active(loaded.clone()),
        "the seeded session activates"
    );

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then every loaded row precedes every archived row regardless of
    // title order — loaded is the top group, archived the bottom one.
    let ids = ids_in_order(&html);
    let archived_pos = ids
        .iter()
        .position(|id| *id != loaded.to_string() && *id != untitled.to_string())
        .expect("the archived row renders");
    assert_eq!(ids.len(), 3, "every row renders: {html}");
    assert!(
        ids[..archived_pos].contains(&loaded.to_string()),
        "the loaded row precedes the archived row: {ids:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_pushed_fragment_orders_by_the_same_grouping_law() {
    // Given two live sessions: the active one titled later in the
    // alphabet than its sibling (the push path had no sort at all —
    // raw HashMap order), plus the map's default session.
    let (state, _shared) = harness(Vec::new());
    let untitled = active_id(&state);
    let active = seed_titled(&state, "zulu");
    let sibling = seed_titled(&state, "alpha");
    assert!(
        state.write().session.set_active(active.clone()),
        "the seeded session activates"
    );
    let guard = state.read();

    // When rendering the fan-out's live fragment.
    let html = crate::regions::sessions::sessions_live_fragment(&guard);

    // Then the same law the pull applies holds: the active session
    // leads, then title order — not map iteration order.
    let ids = ids_in_order(&html);
    assert_eq!(
        ids,
        vec![
            active.to_string(),
            sibling.to_string(),
            untitled.to_string(),
        ],
        "the pushed fragment applies the pull's ordering: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn loaded_rows_sort_active_first_then_by_title() {
    // Given three loaded sessions: the active one titled "middle",
    // with title-neighbors "alpha" and "zed", plus the map's default
    // session (untitled).
    let (state, shared) = harness(Vec::new());
    let untitled = active_id(&state);
    let before = seed_titled(&state, "alpha");
    let active = seed_titled(&state, "middle");
    let after = seed_titled(&state, "zed");
    assert!(
        state.write().session.set_active(active.clone()),
        "the seeded session activates"
    );

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the active session leads and the rest follow by title
    // (case-insensitive: alpha < Untitled Session < zed).
    let ids = ids_in_order(&html);
    assert_eq!(
        ids,
        vec![
            active.to_string(),
            before.to_string(),
            untitled.to_string(),
            after.to_string(),
        ],
        "active first, then title A→Z: {html}"
    );
}

// ── The `?q` filter (the pull path) ─────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn a_matching_q_returns_only_the_matching_session() {
    // Given a loaded "Fix the leak" and an archived "Aardvark notes".
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark notes")]);
    let loaded = seed_titled(&state, "Fix the leak");

    // When pulling with a `q` only the archived title fuzzy-matches.
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("ardvark"))
        .await
        .expect("sessions pull renders");

    // Then only the archived session's row renders — matched
    // server-side, on the merged list.
    let ids = ids_in_order(&html);
    assert_eq!(ids.len(), 1, "only the match renders: {html}");
    assert!(
        html.contains("Aardvark notes"),
        "the archived match renders: {html}"
    );
    assert!(
        !html.contains(&loaded.to_string()),
        "the loaded non-match is filtered out: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_non_matching_q_returns_an_empty_list() {
    // Given one titled session.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "Fix the leak");

    // When pulling with a `q` nothing matches.
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("qqqqzzzz"))
        .await
        .expect("sessions pull renders");

    // Then no row renders — an empty fragment, not an error.
    assert!(ids_in_order(&html).is_empty(), "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_absent_q_returns_every_session() {
    // Given a loaded session and an archived one.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let loaded = seed_titled(&state, "Fix the leak");

    // When pulling with no `q`.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then every row renders — the unfiltered case (the map's default
    // session rides along).
    let ids = ids_in_order(&html);
    assert_eq!(ids.len(), 3, "every session renders: {html}");
    assert!(ids.contains(&loaded.to_string()), "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_empty_q_returns_every_session() {
    // Given a loaded session and an archived one.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let loaded = seed_titled(&state, "Fix the leak");

    // When pulling with an empty `q` (the input cleared but sent).
    let html = crate::regions::sessions::sessions_fragment(&shared, Some(""))
        .await
        .expect("sessions pull renders");

    // Then every row renders — absent and empty are the same
    // unfiltered case; the matcher's empty-pattern behavior never
    // defines it.
    let ids = ids_in_order(&html);
    assert_eq!(ids.len(), 3, "every session renders: {html}");
    assert!(ids.contains(&loaded.to_string()), "{html}");
}

// ── The per-row active flag ─────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn archived_rows_never_render_active() {
    // Given the active session and an archived row — the shape where
    // the old page-level comparison painted every archived row
    // active.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let active = seed_titled(&state, "Fix the leak");
    assert!(
        state.write().session.set_active(active.clone()),
        "the seeded session activates"
    );

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then only rows whose own id is the instance's active id carry
    // the active class; the archived row does not.
    let rows = rows_in_order(&html);
    assert_eq!(rows.len(), 3, "every row renders: {html}");
    let marked: Vec<&Row> = rows.iter().filter(|row| row.active).collect();
    assert_eq!(marked.len(), 1, "exactly one row is active: {html}");
    assert_eq!(
        marked[0].session_id,
        active.to_string(),
        "the active mark is on the active session's row: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn exactly_one_row_renders_active_after_a_switch() {
    // Given three loaded sessions.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "one");
    let second = seed_titled(&state, "two");
    seed_titled(&state, "three");

    // When the second session activates and the list re-pulls.
    assert!(
        state.write().session.set_active(second.clone()),
        "the second session activates"
    );
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then exactly one row — the second — carries the active class:
    // per-row truth, not a page-level comparison.
    let rows = rows_in_order(&html);
    let actives: Vec<&Row> = rows.iter().filter(|row| row.active).collect();
    assert_eq!(actives.len(), 1, "exactly one row is active: {html}");
    assert_eq!(actives[0].session_id, second.to_string(), "{html}");
}
