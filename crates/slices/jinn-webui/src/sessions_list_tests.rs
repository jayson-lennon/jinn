//! The sessions-list tests: the merged list's ordering law, the
//! `?q=` server-side filter, and the per-row active flag.
//!
//! The list IS the TUI session picker rendered as HTML: the order is
//! the store's tree-aware sort (loaded trees first, tree recency,
//! children under parents) and a `?q` runs the picker's
//! `TreePickerState` — so the tests pin the picker's observable
//! behavior through the web surface: a matching child surfaces its
//! parent, every term must match, trees rank strongest-first. The
//! ordering is observable through both render paths — the pull
//! fragment (`sessions_fragment`, live + archived) and the fan-out's
//! pushed fragment (`sessions_live_fragment`, live only). The store
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

/// An archived summary with a parent link — a subagent child session.
fn archived_child(id: SessionId, parent: SessionId, title: &str) -> SessionSummary {
    SessionSummary {
        parent_session: Some(parent),
        ..archived_summary(id, title)
    }
}

/// Seeds a live session with the given title and parent, returning
/// its id.
fn seed_child(state: &State, parent: &SessionId, title: &str) -> SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    {
        let mut guard = state.write();
        let seeded = guard.session.get_mut(&id).expect("seeded session");
        seeded.set_title(title.to_owned());
        seeded.set_parent_session(parent.clone());
    }
    id
}

/// The harness: one state, a seeded store, and the shared handler
/// state over both. `seed` runs before the store is built, so its
/// summaries can reference live session ids (parent links). Live
/// sessions seed through the returned [`State`].
fn harness_with<T>(
    seed: impl FnOnce(&State) -> T,
    summaries: impl FnOnce(&T) -> Vec<SessionSummary>,
) -> (State, ServerShared) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let unread_marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let seeded = seed(&state);
    let summaries = summaries(&seeded);
    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(Arc::new(SeededSummaryStore::with(summaries))),
        provider_cell,
        jinn_provider_config::ProviderRegistryService::new(
            jinn_provider_config::ProviderRegistry::from_config(
                jinn_provider_config::ProvidersConfig {
                    providers: std::collections::BTreeMap::default(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                },
            )
            .expect("an empty providers config expands"),
        ),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        fanout,
        0,
        "sessions-list-test".to_owned(),
    );
    (state, shared)
}

/// The plain harness: summaries known up front.
fn harness(summaries: Vec<SessionSummary>) -> (State, ServerShared) {
    harness_with(|_state| (), |_seeded| summaries)
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
    unread: bool,
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
                unread: class.split(' ').any(|part| part == "unread"),
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

// ── The ordering law (both paths) ───────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_pull_lists_loaded_rows_above_archived_rows() {
    // Given one loaded session titled "Zebra" (newest) and one
    // archived titled "Aardvark" (also fresh) — the title/timestamp
    // combination that floated the archived row above loaded ones
    // under the old page-level sort.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let loaded = seed_titled(&state, "Zebra");
    assert!(
        state.write().session.set_active(loaded.clone()),
        "the seeded session activates"
    );

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the loaded row precedes the archived row regardless of
    // title order — loaded trees form the top group (the picker's
    // tree-aware sort), archived trees the bottom one.
    let ids = ids_in_order(&html);
    assert_eq!(
        ids.first().map(String::as_str),
        Some(loaded.to_string().as_str()),
        "the loaded tree leads: {html}"
    );
    assert_eq!(ids.len(), 3, "every row renders: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_pushed_fragment_orders_by_the_same_tree_law() {
    // Given two live root sessions whose insertion (map) order runs
    // opposite to their recency — the push path rendered raw HashMap
    // order once; the tree-aware sort must hold on the push too.
    let (state, _shared) = harness(Vec::new());
    let older = seed_titled(&state, "older");
    let newer = seed_titled(&state, "newer");
    state
        .write()
        .session
        .get_mut(&older)
        .expect("seeded session")
        .restore_updated_at(jiff::Timestamp::now() - std::time::Duration::from_secs(60));
    let guard = state.read();

    // When rendering the fan-out's live fragment.
    let html = crate::regions::sessions::sessions_live_fragment(&guard);

    // Then the newest tree leads — recency, not map iteration order.
    let ids = ids_in_order(&html);
    let newer_pos = ids
        .iter()
        .position(|id| *id == newer.to_string())
        .expect("the newer session renders");
    let older_pos = ids
        .iter()
        .position(|id| *id == older.to_string())
        .expect("the older session renders");
    assert!(
        newer_pos < older_pos,
        "recency order holds on the push path: {ids:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn children_nest_under_their_parent() {
    // Given a root session and its subagent child — the shape the
    // picker draws nested and the flat list flattened.
    let (state, shared) = harness(Vec::new());
    let root = seed_titled(&state, "root session");
    let child = seed_child(&state, &root, "subagent child");

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the child renders directly under its parent — the picker's
    // DFS order.
    let ids = ids_in_order(&html);
    let root_pos = ids
        .iter()
        .position(|id| *id == root.to_string())
        .expect("the root renders");
    let child_pos = ids
        .iter()
        .position(|id| *id == child.to_string())
        .expect("the child renders");
    assert_eq!(
        child_pos,
        root_pos + 1,
        "the child directly follows its parent: {ids:?}"
    );
}

// ── The `?q` filter (the pull path) — the picker's matching ─────────

#[rstest::rstest]
#[tokio::test]
async fn a_matching_child_surfaces_its_parent() {
    // Given a live root "tts" whose archived subagent children carry
    // titles the root's own title does not contain — the exact shape
    // behind the user's report ("tts" + "please set up kokoro…"):
    // matching a child must surface the parent, the way the TUI
    // picker does.
    let kokoro = "please set up kokoro for me so i can render tts locally";
    let (_state, shared) = harness_with(
        |state| seed_titled(state, "tts"),
        |root| {
            vec![
                archived_child(SessionId::new(), root.clone(), kokoro),
                archived_child(SessionId::new(), root.clone(), kokoro),
            ]
        },
    );

    // When pulling with a `q` only the children's titles match.
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("please set"))
        .await
        .expect("sessions pull renders");

    // Then the root renders alongside its matched children — the
    // picker's ancestor-chain expansion — even though "tts" matches
    // neither term.
    assert!(
        html.contains(">tts<"),
        "the parent surfaces behind its matched children: {html}"
    );
    assert_eq!(
        html.matches("kokoro").count(),
        2,
        "both matched children render: {html}"
    );
}

// ── The `?q` filter (the pull path) — the picker's term rule ────────

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
    // server-side, on the merged tree.
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
async fn every_whitespace_term_must_match() {
    // Given a session whose title contains "please" but cannot admit
    // "set" as a subsequence (no s→e→t chain in "…please resolve"),
    // and one containing both terms.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "got a merge conflict. please resolve");
    let both = seed_titled(&state, "please set up the environment");

    // When pulling with a two-term `q`.
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("please set"))
        .await
        .expect("sessions pull renders");

    // Then only the title admitting both terms renders — the picker's
    // term-split rule, not one long subsequence probe.
    let ids = ids_in_order(&html);
    assert_eq!(ids.len(), 1, "only the both-terms match renders: {html}");
    assert!(
        html.contains(&both.to_string()),
        "the both-terms session renders: {html}"
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
    let marked_id = marked.first().map_or("", |row| row.session_id.as_str());
    assert_eq!(
        marked_id,
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
    let active_id = actives.first().map_or("", |row| row.session_id.as_str());
    assert_eq!(active_id, second.to_string(), "{html}");
}

// ── The unread mark: both render paths ──────────────────────────────

/// Marks `ids` unread through the harness's registry — the same cell
/// the fan-out actor writes, resolved the way the render paths read it.
fn mark_unread(shared: &ServerShared, ids: &[SessionId]) {
    let guard = shared.read_state();
    let slices = guard.session.slices().expect("registry attached");
    let cell = slices
        .reader::<jinn_webui_msg::UnreadSessionMarks>(&jinn_webui_msg::webui_unread_slot())
        .expect("the unread cell resolves");
    cell.update(|marks| {
        for id in ids {
            marks.mark(id.clone());
        }
    });
}

/// The row for `id`, if it rendered.
fn row_for<'a>(rows: &'a [Row], id: &SessionId) -> &'a Row {
    rows.iter()
        .find(|row| row.session_id == id.to_string())
        .expect("the session's row rendered")
}

#[rstest::rstest]
#[tokio::test]
async fn the_pull_fragment_marks_the_unread_row() {
    // Given two live sessions, one marked unread, Bar active.
    let (state, shared) = harness(Vec::new());
    let foo = seed_titled(&state, "Foo");
    let bar = seed_titled(&state, "Bar");
    assert!(state.write().session.set_active(bar.clone()), "activates");
    mark_unread(&shared, std::slice::from_ref(&foo));

    // When pulling the merged sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then Foo's row wears the unread class and Bar's does not — the
    // merged path reads the same cell the fan-out writes.
    let rows = rows_in_order(&html);
    assert!(row_for(&rows, &foo).unread, "Foo renders marked: {html}");
    assert!(!row_for(&rows, &bar).unread, "Bar renders clean: {html}");
    assert!(row_for(&rows, &bar).active, "Bar is still active");
}

#[rstest::rstest]
#[tokio::test]
async fn the_pushed_fragment_marks_the_unread_row() {
    // Given a marked session in live state.
    let (state, shared) = harness(Vec::new());
    let foo = seed_titled(&state, "Foo");
    let bar = seed_titled(&state, "Bar");
    assert!(state.write().session.set_active(bar.clone()), "activates");
    mark_unread(&shared, std::slice::from_ref(&foo));
    let guard = state.read();

    // When rendering the fan-out's live (pushed) fragment.
    let html = crate::regions::sessions::sessions_live_fragment(&guard);

    // Then the same row is marked on the push path — both paths
    // produce the same shape through `picker_rows`.
    let rows = rows_in_order(&html);
    assert!(row_for(&rows, &foo).unread, "Foo renders marked: {html}");
    assert!(!row_for(&rows, &bar).unread, "Bar renders clean: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_archived_row_keeps_its_mark() {
    // Given an archived summary — the store alone serves its row; and
    // its id marked unread in the cell.
    let archived_id = SessionId::new();
    let (_state, shared) = harness_with(
        |_state| archived_id.clone(),
        |seeded| {
            vec![SessionSummary {
                session_id: seeded.clone(),
                title: "Settled then archived".to_owned(),
                updated_at: jiff::Timestamp::now(),
                created_at: jiff::Timestamp::now(),
                session_state: SessionState::Archived,
                in_library: false,
                is_bot: false,
                parent_session: None,
                project: None,
            }]
        },
    );
    mark_unread(&shared, std::slice::from_ref(&archived_id));

    // When pulling the merged fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the archived row still wears the mark — archived-but-
    // unviewed is still unviewed; only activation clears.
    let rows = rows_in_order(&html);
    assert!(
        row_for(&rows, &archived_id).unread,
        "the archived row stays marked: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_fresh_harness_renders_no_marks() {
    // Given a fresh harness (the cell default: an empty set) with
    // live and archived sessions.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    seed_titled(&state, "Zebra");

    // When pulling the fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then no row carries the unread class — a fresh boot shows no
    // markers.
    let rows = rows_in_order(&html);
    assert!(
        rows.iter().all(|row| !row.unread),
        "no row renders marked on a fresh harness: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_harness_without_a_registry_renders_no_marks() {
    // Given a state with NO slice registry attached and a shared state
    // over it (the cell cannot resolve).
    let state = {
        let app = AppState::default();
        State::new(app)
    };
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        let marks = slices
            .register(
                jinn_webui_msg::webui_unread_slot(),
                jinn_webui_msg::UnreadSessionMarks::default(),
            )
            .expect("the unread cell registers once per harness");
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), marks)
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(Arc::new(SeededSummaryStore::default())),
        provider_cell,
        jinn_provider_config::ProviderRegistryService::new(
            jinn_provider_config::ProviderRegistry::from_config(
                jinn_provider_config::ProvidersConfig {
                    providers: std::collections::BTreeMap::default(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                },
            )
            .expect("an empty providers config expands"),
        ),
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        fanout,
        0,
        "no-registry-test".to_owned(),
    );
    seed_titled(&state, "Foo");

    // When pulling the fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the render degrades to no marks rather than failing — the
    // mark is signal, not correctness.
    let rows = rows_in_order(&html);
    assert!(
        rows.iter().all(|row| !row.unread),
        "no row renders marked without a registry: {html}"
    );
}
