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
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once per harness");
    let mcp_runtime_cell = slices
        .register(
            jinn_mcp_msg::mcp_runtime_slot(),
            jinn_mcp_msg::McpRuntimeState::default(),
        )
        .expect("the MCP runtime cell registers once");
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
        personas_cell,
        mcp_runtime_cell,
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
        jinn_config::empty_config_layer().clone(),
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
    working: bool,
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
                working: class.split(' ').any(|part| part == contract::WORKING_CLASS),
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("please set"), None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("ardvark"), None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("please set"), None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, Some("qqqqzzzz"), None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, Some(""), None)
        .await
        .expect("sessions pull renders");

    // Then every row renders — absent and empty are the same
    // unfiltered case; the matcher's empty-pattern behavior never
    // defines it.
    let ids = ids_in_order(&html);
    assert_eq!(ids.len(), 3, "every session renders: {html}");
    assert!(ids.contains(&loaded.to_string()), "{html}");
}

// ── The per-row active flag: the VIEWED session, not the pointer ────

#[rstest::rstest]
#[tokio::test]
async fn a_viewed_archived_row_renders_active() {
    // Given an archived row and a viewer naming it — the deep-link
    // shape: the page opened the archived session without activating
    // it, and the pointer points elsewhere (or nowhere).
    let archived_id = SessionId::new();
    let (_state, shared) = harness_with(
        |_state| archived_id.clone(),
        |seeded| {
            vec![SessionSummary {
                session_id: seeded.clone(),
                title: "Aardvark".to_owned(),
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

    // When pulling with the archived session as the viewer.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&archived_id))
        .await
        .expect("sessions pull renders");

    // Then the archived row carries the active class — the viewed
    // fill follows the page, live or archived.
    let rows = rows_in_order(&html);
    assert!(
        row_for(&rows, &archived_id).active,
        "the viewed archived row is active: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_instance_pointer_styles_no_row_when_nothing_is_viewed() {
    // Given two live sessions with the instance's active pointer on
    // the first, and a pull with NO viewer (the landing page).
    let (state, shared) = harness(Vec::new());
    let first = seed_titled(&state, "one");
    seed_titled(&state, "two");
    assert!(
        state.write().session.set_active(first.clone()),
        "the seeded session activates"
    );

    // When pulling the sessions fragment without a viewer.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");

    // Then no row carries the active class — the pointer drives no
    // web styling; the landing page paints no blue row.
    let rows = rows_in_order(&html);
    let actives: Vec<&Row> = rows.iter().filter(|row| row.active).collect();
    assert!(
        actives.is_empty(),
        "no row is active without a viewer: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn exactly_the_viewed_row_renders_active() {
    // Given three loaded sessions, the instance's active pointer on
    // the second, and a pull viewing the THIRD — the two-pages
    // disagreeing shape: this page opened a session the instance
    // never activated.
    let (state, shared) = harness(Vec::new());
    seed_titled(&state, "one");
    let pointer_target = seed_titled(&state, "two");
    let viewed = seed_titled(&state, "three");
    assert!(
        state.write().session.set_active(pointer_target.clone()),
        "the pointer session activates"
    );

    // When pulling with the third session as the viewer.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&viewed))
        .await
        .expect("sessions pull renders");

    // Then exactly one row — the VIEWED one — carries the active
    // class, and it is not the row the pointer names.
    let rows = rows_in_order(&html);
    let actives: Vec<&Row> = rows.iter().filter(|row| row.active).collect();
    assert_eq!(actives.len(), 1, "exactly one row is active: {html}");
    let active_id = actives.first().map_or("", |row| row.session_id.as_str());
    assert_eq!(active_id, viewed.to_string(), "{html}");
    assert_ne!(
        active_id,
        pointer_target.to_string(),
        "the pointer's row stays dark: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn two_pulls_with_different_viewers_each_mark_their_own_row() {
    // Given two live sessions and two pulls — one viewing each.
    let (state, shared) = harness(Vec::new());
    let alpha = seed_titled(&state, "alpha");
    let beta = seed_titled(&state, "beta");
    assert!(
        state.write().session.set_active(alpha.clone()),
        "the pointer activates one of them"
    );

    // When pulling once as alpha's page and once as beta's page.
    let alpha_html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&alpha))
        .await
        .expect("alpha's pull renders");
    let beta_html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&beta))
        .await
        .expect("beta's pull renders");

    // Then each pull marks exactly its own row — the fill is the
    // request's data, not shared server state.
    let alpha_rows = rows_in_order(&alpha_html);
    let beta_rows = rows_in_order(&beta_html);
    assert!(
        row_for(&alpha_rows, &alpha).active,
        "alpha's pull marks alpha: {alpha_html}"
    );
    assert!(
        !row_for(&alpha_rows, &beta).active,
        "alpha's pull leaves beta dark: {alpha_html}"
    );
    assert!(
        row_for(&beta_rows, &beta).active,
        "beta's pull marks beta: {beta_html}"
    );
    assert!(
        !row_for(&beta_rows, &alpha).active,
        "beta's pull leaves alpha dark: {beta_html}"
    );
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
    // Given two live sessions, one marked unread, and a pull viewing
    // Bar.
    let (state, shared) = harness(Vec::new());
    let foo = seed_titled(&state, "Foo");
    let bar = seed_titled(&state, "Bar");
    mark_unread(&shared, std::slice::from_ref(&foo));

    // When pulling the merged sessions fragment as Bar's page.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&bar))
        .await
        .expect("sessions pull renders");

    // Then Foo's row wears the unread class, Bar's does not, and
    // Bar's row is the viewed one — the merged path reads the same
    // cell the fan-out writes.
    let rows = rows_in_order(&html);
    assert!(row_for(&rows, &foo).unread, "Foo renders marked: {html}");
    assert!(!row_for(&rows, &bar).unread, "Bar renders clean: {html}");
    assert!(row_for(&rows, &bar).active, "Bar is the viewed row");
}

#[rstest::rstest]
#[tokio::test]
async fn the_pushed_fragment_marks_the_unread_row() {
    // Given a marked session in live state.
    let (state, shared) = harness(Vec::new());
    let foo = seed_titled(&state, "Foo");
    let bar = seed_titled(&state, "Bar");
    mark_unread(&shared, std::slice::from_ref(&foo));
    let guard = state.read();

    // When rendering the fan-out's live (pushed) fragment.
    let html = crate::regions::sessions::sessions_live_fragment(&guard);

    // Then the same row is marked on the push path — both paths
    // produce the same shape through `picker_rows` (and the pushed
    // envelope names no viewer: its HTML is discarded on the wire).
    let rows = rows_in_order(&html);
    assert!(row_for(&rows, &foo).unread, "Foo renders marked: {html}");
    assert!(!row_for(&rows, &bar).unread, "Bar renders clean: {html}");
    assert!(
        !row_for(&rows, &bar).active,
        "the envelope render names no viewer"
    );
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");

    // Then the archived row still wears the mark — archived-but-
    // unviewed is still unviewed; viewing (activation or page
    // arrival) clears it, and neither has happened.
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
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
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once per harness");
    let mcp_runtime_cell = slices
        .register(
            jinn_mcp_msg::mcp_runtime_slot(),
            jinn_mcp_msg::McpRuntimeState::default(),
        )
        .expect("the MCP runtime cell registers once");
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
        personas_cell,
        mcp_runtime_cell,
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
        jinn_config::empty_config_layer().clone(),
        fanout,
        0,
        "no-registry-test".to_owned(),
    );
    seed_titled(&state, "Foo");

    // When pulling the fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
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

// ── The working spinner ─────────────────────────────────────────────
//
// The busy condition is `PhaseView::is_working`, seeded through the
// session's public projection methods (`begin_streaming` → Streaming,
// `begin_sending` → Working — exactly what the turn ledger's mint and
// tool-loop edges write; `settle_to_idle` for the idle negative). The
// display projection has no `Cancelling` writer (the drain lives only
// in the ledger's derived view), so the third busy phase is proven at
// the template level: the spinner markup is one unconditional
// `item.working` branch, so a `Cancelling` row renders the same span
// `Streaming` does — no phase-specific branch exists to get it wrong.

/// The spinner span inside `id`'s row — the span whose class carries
/// the working hook. `None` when the row rendered without one (the
/// idle signal).
fn spinner_of(html: &str, id: &SessionId) -> Option<String> {
    let row_id = id.to_string();
    let spinner_attr = format!("class=\"{}", contract::WORKING_CLASS);
    html.split("<a ")
        .skip(1)
        .filter(|anchor| {
            anchor.split('>').next().is_some_and(|tag| {
                tag.contains(&format!("{}=\"{row_id}\"", contract::SESSION_ID_DATA_ATTR))
            })
        })
        .find_map(|anchor| {
            anchor.split("</a>").next().and_then(|body| {
                body.split("<span")
                    .skip(1)
                    .map(|span| span.split('>').next().unwrap_or_default().to_owned())
                    .find(|attrs| attrs.contains(&spinner_attr))
            })
        })
}

#[rstest::rstest]
#[case::streaming(jinn_slices::PhaseView::Streaming)]
#[case::working(jinn_slices::PhaseView::Working)]
#[tokio::test]
async fn a_busy_session_renders_the_working_spinner(
    #[case] expected_phase: jinn_slices::PhaseView,
) {
    // Given two live sessions, one driven into a busy phase through
    // its public projection method.
    let (state, shared) = harness(Vec::new());
    let busy_id = seed_titled(&state, "Busy");
    let idle_id = seed_titled(&state, "Idle");
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&busy_id).expect("seeded session");
        if expected_phase == jinn_slices::PhaseView::Streaming {
            session.begin_streaming();
        } else {
            session.begin_sending();
        }
    }
    assert_eq!(
        state
            .read()
            .session
            .get(&busy_id)
            .map(jinn_session_state::ChatSessionState::phase),
        Some(expected_phase),
        "the seed landed the phase the projection writer names"
    );

    // When pulling the merged sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");

    // Then the busy row wears the working class, and the spinner span
    // sits ahead of the title inside that row.
    let rows = rows_in_order(&html);
    assert!(
        row_for(&rows, &busy_id).working,
        "the busy row renders working: {html}"
    );
    assert!(
        !row_for(&rows, &idle_id).working,
        "the idle row renders clean: {html}"
    );
    let spinner = spinner_of(&html, &busy_id).expect("the busy row carries a spinner span");
    assert!(
        spinner.contains(&format!("class=\"{}", contract::WORKING_CLASS)),
        "the spinner span carries the working class: {spinner}"
    );
    // And the spinner precedes the title.
    let row_html = html
        .split(&format!(
            "{}=\"{}\"",
            contract::SESSION_ID_DATA_ATTR,
            busy_id
        ))
        .nth(1)
        .expect("the busy row renders");
    let spinner_pos = row_html
        .find(&format!("class=\"{}", contract::WORKING_CLASS))
        .expect("the spinner renders in the row");
    let title_pos = row_html
        .find("stitle")
        .expect("the title renders in the row");
    assert!(
        spinner_pos < title_pos,
        "the spinner precedes the title: {row_html}"
    );
    // And the fragment carries the fill-scoped ink rule alongside the
    // spinner rule (the CSS travels with the fragment's region sheet).
    let css = crate::regions::sessions::SIDEBAR_CSS;
    assert!(
        css.contains(&format!(
            ".srow.{} .{}",
            contract::WORKING_CLASS,
            contract::WORKING_CLASS
        )),
        "the spinner rule exists in the region stylesheet"
    );
    assert!(
        css.contains(&format!(".srow.active .{}", contract::WORKING_CLASS))
            && css.contains(&format!(".srow.unread .{}", contract::WORKING_CLASS)),
        "the fill-scoped ink rules exist: {css}"
    );
}

/// The stylesheet's fill-precedence order, asserted positionally: the
/// `.active` fill rule must resolve OVER `.unread` when both classes
/// ride one row (the transient window between a settle's mark and the
/// viewed page's clear). CSS resolves equal specificity by source
/// order, so this is an ORDER claim, not a presence claim — the two
/// rules exist at all times; only their sequence changed.
#[rstest::rstest]
#[test]
fn the_active_fill_rule_follows_the_unread_rule_in_the_stylesheet() {
    // Given the sessions region stylesheet.
    let css = crate::regions::sessions::SIDEBAR_CSS;

    // When locating the two fill rules.
    let active_pos = css
        .find(".srow.active { background: var(--accent); }")
        .expect("the active fill rule renders");
    let unread_pos = css
        .find(".srow.unread { background: var(--mark); }")
        .expect("the unread fill rule renders");

    // Then `.active` comes after `.unread` — the viewed fill wins the
    // transient both-classes window.
    assert!(
        active_pos > unread_pos,
        ".active must follow .unread in the stylesheet: {css}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_row_wearing_both_fills_resolves_to_the_viewed_fill() {
    // Given a session that is simultaneously the viewer's session and
    // still marked unread — the transient race window between a
    // settle's mark and the page's clear event.
    let (state, shared) = harness(Vec::new());
    let foo = seed_titled(&state, "Foo");
    mark_unread(&shared, std::slice::from_ref(&foo));

    // When pulling the fragment as Foo's page (its `active` row also
    // carries the `unread` class in the same markup).
    let html = crate::regions::sessions::sessions_fragment(&shared, None, Some(&foo))
        .await
        .expect("sessions pull renders");

    // Then the row carries both classes, and the stylesheet's order
    // hands the fill to `.active`: its accent background declaration
    // is the last fill rule standing between the two. The markup
    // carries both; the rendered background is the accent's, not the
    // unread yellow's, because `.active` follows `.unread` (the
    // positional pin above) — the row the user looks at reads as the
    // viewed one.
    let rows = rows_in_order(&html);
    let row = row_for(&rows, &foo);
    assert!(
        row.active && row.unread,
        "precondition: the row carries both classes: {html}"
    );
    let css = crate::regions::sessions::SIDEBAR_CSS;
    let active_pos = css
        .find(".srow.active { background: var(--accent); }")
        .expect("the active fill rule renders");
    let unread_pos = css
        .find(".srow.unread { background: var(--mark); }")
        .expect("the unread fill rule renders");
    assert!(
        active_pos > unread_pos,
        "the viewed fill wins the co-occurrence window: {css}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_idle_session_renders_no_spinner() {
    // Given a live session that streamed and settled back to idle —
    // the settle path's own projection writer.
    let (state, shared) = harness(Vec::new());
    let id = seed_titled(&state, "Settled");
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("seeded session");
        session.begin_streaming();
        session.settle_to_idle();
    }

    // When pulling the fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");

    // Then the row carries no working class and no spinner span — its
    // absence is the idle signal.
    let rows = rows_in_order(&html);
    assert!(!row_for(&rows, &id).working, "{html}");
    assert!(spinner_of(&html, &id).is_none(), "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_archived_row_never_carries_the_working_class() {
    // Given an archived summary — the store alone serves its row — and
    // one live session for contrast.
    let (state, shared) = harness(vec![archived_summary(SessionId::new(), "Aardvark")]);
    let live = seed_titled(&state, "Live");

    // When pulling the merged fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");

    // Then no archived row wears the working class — an archived
    // session has no turn in flight by definition.
    let rows = rows_in_order(&html);
    assert!(
        rows.iter()
            .filter(|row| row.session_id != live.to_string())
            .all(|row| !row.working),
        "archived rows never spin: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn both_render_paths_produce_the_same_working_markup() {
    // Given one busy live session.
    let (state, shared) = harness(Vec::new());
    let id = seed_titled(&state, "Busy");
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .begin_streaming();

    // When rendering through the pull path and the pushed path.
    let pulled = crate::regions::sessions::sessions_fragment(&shared, None, None)
        .await
        .expect("sessions pull renders");
    let pushed = {
        let guard = state.read();
        crate::regions::sessions::sessions_live_fragment(&guard)
    };

    // Then both carry the working class on the same row — one row
    // shape, two render targets.
    assert!(
        row_for(&rows_in_order(&pulled), &id).working,
        "the pull path renders the spinner: {pulled}"
    );
    assert!(
        row_for(&rows_in_order(&pushed), &id).working,
        "the pushed path renders the spinner: {pushed}"
    );
}
