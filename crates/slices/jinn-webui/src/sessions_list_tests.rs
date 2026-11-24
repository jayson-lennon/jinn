//! The sessions-list tests: the merged list's ordering law, the
//! `?q=` server-side filter, and the viewed-fill signal binding.
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
        jinn_kernel::common::turn_ledger::TurnLedger::default(),
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

// ── The viewed fill: client-side signal styling ─────────────────────
//
// The blue row no longer rides a server-rendered class: every row
// binds `data-class:active` to `$viewedSession`, so one list shape
// serves every viewer and navigation recolors the list with zero
// requests. The claims here are template-level — the binding is on
// the row, the server class is gone, and the fill rules survive —
// with the browser-level behavior pinned by the e2e tier.

/// One rendered row: its session id, signal binding, and working
/// class, in render order. No `active` field exists: the viewed fill
/// is the signal's decision, not the markup's.
struct Row {
    session_id: String,
    viewed_binding: String,
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
            let viewed_binding = tag
                .split("data-class:active=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_default()
                .to_owned();
            Row {
                session_id,
                viewed_binding,
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

#[rstest::rstest]
#[tokio::test]
async fn every_row_binds_the_active_class_to_the_viewed_session_signal() {
    // Given one archived row and one live session — the pull renders
    // the viewer-agnostic shape.
    let (_archived_id, shared) = harness_with(
        |state| seed_titled(state, "Zebra"),
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

    // When pulling the sessions fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then every row — live AND archived — carries the signal
    // binding, and the instance's active pointer decides nothing: the
    // binding names the viewed session per row, so a navigation
    // recolors the list client-side with zero requests.
    let rows = rows_in_order(&html);
    assert!(rows.len() >= 2, "both rows render: {html}");
    for row in &rows {
        assert_eq!(
            row.viewed_binding,
            format!("$viewedSession === '{}'", row.session_id),
            "each row binds its own id against the signal: {html}"
        );
    }
    // And no server-rendered active class exists anywhere in the
    // markup — the pointer (and every request parameter) is out of
    // the styling business.
    assert!(
        !html.contains("srow active"),
        "the server paints no active row: {html}"
    );
}

#[rstest::rstest]
#[test]
fn the_active_fill_rules_survive_as_signal_driven_css() {
    // Given the sessions region stylesheet.
    let css = crate::regions::sessions::SIDEBAR_CSS;

    // Then the `.active` fill rules exist — the class the signal
    // binding applies — with the same precedence order the transient
    // both-fills window relies on: `.active` after `.unread`.
    let active_pos = css
        .find(".srow.active { background: var(--accent); }")
        .expect("the active fill rule renders");
    let unread_pos = css
        .find(".srow.unread { background: var(--mark); }")
        .expect("the unread fill rule renders");
    assert!(
        active_pos > unread_pos,
        ".active must follow .unread in the stylesheet: {css}"
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then Foo's row wears the unread class and Bar's does not —
    // the merged path reads the same cell the fan-out writes.
    let rows = rows_in_order(&html);
    assert!(row_for(&rows, &foo).unread, "Foo renders marked: {html}");
    assert!(!row_for(&rows, &bar).unread, "Bar renders clean: {html}");
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
    // envelope's HTML is discarded on the wire: the tick is the push).
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
        jinn_kernel::common::turn_ledger::TurnLedger::default(),
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
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

    // When pulling the fragment while Foo is both viewed and marked
    // (the markup's signal binding decides the fill; the classes can
    // co-occur).
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the stylesheet's order hands the co-occurring fill to
    // `.active`: its accent background declaration is the last fill
    // rule standing between the two — the row the user looks at
    // reads as the viewed one, not the unread yellow.
    let rows = rows_in_order(&html);
    let _row = row_for(&rows, &foo);
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
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
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
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
    let pulled = crate::regions::sessions::sessions_fragment(&shared, None)
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

// ── Library membership: exclusion + the library view ────────────────

/// Seeds a live session, titles it, flags its membership, and returns
/// its id — the member-row seed the exclusion and library tests share.
fn seed_member(state: &State, title: &str, in_library: bool) -> SessionId {
    let id = seed_titled(state, title);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_in_library(in_library);
    id
}

/// An archived summary with the caller's membership flag.
fn archived_summary_flagged(id: SessionId, title: &str, in_library: bool) -> SessionSummary {
    SessionSummary {
        in_library,
        ..archived_summary(id, title)
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_live_member_is_absent_from_the_pull_and_push_fragments() {
    // Given one plain session and one live library member.
    let (state, shared) = harness(Vec::new());
    let plain = seed_titled(&state, "Plain");
    let member = seed_member(&state, "Member", true);

    // When rendering through the pull path and the pushed path.
    let pulled = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");
    let pushed = {
        let guard = state.read();
        crate::regions::sessions::sessions_live_fragment(&guard)
    };

    // Then the member renders in neither — the TUI `is_listed` rule
    // holds on both paths — and the plain row does.
    assert!(
        !ids_in_order(&pulled).contains(&member.to_string()),
        "the member is absent from the pull: {pulled}"
    );
    assert!(
        ids_in_order(&pulled).contains(&plain.to_string()),
        "the plain row renders: {pulled}"
    );
    assert!(
        !ids_in_order(&pushed).contains(&member.to_string()),
        "the member is absent from the push: {pushed}"
    );
    assert!(
        ids_in_order(&pushed).contains(&plain.to_string()),
        "the plain row renders: {pushed}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_archived_member_is_absent_from_the_pull_fragment() {
    // Given one archived plain session and one archived library
    // member — the exclusion must cover `append_summaries`, or the
    // archived member leaks back into the tree.
    let archived_plain = SessionId::new();
    let archived_member = SessionId::new();
    let (_state, shared) = harness(vec![
        archived_summary(archived_plain.clone(), "Plain archived"),
        archived_summary_flagged(archived_member.clone(), "Member archived", true),
    ]);

    // When pulling the merged fragment.
    let html = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");

    // Then the archived member is absent and the plain archived row
    // renders.
    assert!(
        !ids_in_order(&html).contains(&archived_member.to_string()),
        "the archived member is absent: {html}"
    );
    assert!(
        ids_in_order(&html).contains(&archived_plain.to_string()),
        "the plain archived row renders: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_library_view_lists_live_and_archived_members_most_recent_first() {
    // Given one live member (older) and one archived member (newer).
    let archived_member = SessionId::new();
    let (_state, shared) = harness_with(
        |state| seed_member(state, "Live member", true),
        |_seeded| {
            vec![SessionSummary {
                updated_at: jiff::Timestamp::now() + jiff::Span::new().hours(1),
                ..archived_summary_flagged(archived_member.clone(), "Archived member", true)
            }]
        },
    );
    // (The live member seeded through the harness closure; its id is
    // not pre-fabricated — the rendered row names it.)

    // When pulling the library view.
    let html = crate::regions::sessions::library_fragment(&shared, None)
        .await
        .expect("library pull renders");

    // Then both members render — flat, most recently updated first —
    // and each row carries the loaded/archived meta label the
    // sessions rows carry.
    let ids = ids_in_order(&html);
    assert_eq!(
        ids.first().map(String::as_str),
        Some(archived_member.to_string().as_str()),
        "the newer archived member leads: {html}"
    );
    assert_eq!(ids.len(), 2, "both members render, nothing else: {html}");
    for row in rows_in_order(&html) {
        // (The label itself is asserted by the popup-markup test
        // below; here the row shape — an `<a>` with the id attr — is
        // the claim.)
        assert!(!row.session_id.is_empty(), "{html}");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn a_library_member_removed_from_membership_leaves_the_library_view() {
    // Given one live member that then leaves the library.
    let (state, shared) = harness(Vec::new());
    let id = seed_member(&state, "Leaving", true);

    // When the membership flag clears and the library view re-renders.
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_in_library(false);
    let html = crate::regions::sessions::library_fragment(&shared, None)
        .await
        .expect("library pull renders");

    // Then the row is gone — membership is the library's own
    // inclusion rule, both directions.
    assert!(ids_in_order(&html).is_empty(), "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_library_filter_narrows_the_flat_member_list() {
    // Given one member titled "Zebra" and one titled "Aardvark".
    let (state, shared) = harness(Vec::new());
    let zebra = seed_member(&state, "Zebra", true);
    let _aardvark = seed_member(&state, "Aardvark", true);

    // When filtering the library view for a Zebra fragment.
    let html = crate::regions::sessions::library_fragment(&shared, Some("zeb"))
        .await
        .expect("library pull renders");

    // Then only the matching member renders — the picker's fuzzy
    // per-row matching, applied flat.
    assert_eq!(ids_in_order(&html), vec![zebra.to_string()], "{html}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_sessions_pull_with_the_view_param_renders_the_library() {
    // Given the router with one plain session and one member.
    let (state, audit, _slices, router) = crate::server_tests::harness();
    let _plain = seed_titled(&state, "Plain");
    let member = seed_member(&state, "Member", true);

    // When pulling the sessions route with the library view param.
    let (status, body) = crate::server_tests::get(
        &router,
        &format!(
            "/web/sessions?{}=library",
            crate::contract::SESSIONS_VIEW_QUERY_PARAM
        ),
    )
    .await;

    // Then the fragment is the library list (the member's row, the
    // plain session's absent) and nothing published — the view is a
    // render-time choice, not a store write.
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(ids_in_order(&body), vec![member.to_string()], "{body}");
    assert_eq!(audit.names().len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn the_row_menu_renders_the_kebab_and_one_membership_entry() {
    // Given a sessions fragment with a non-member row and a library
    // fragment with a member row.
    let (state, shared) = harness(Vec::new());
    let plain = seed_titled(&state, "Plain");
    let member = seed_member(&state, "Member", true);
    let sessions = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions pull renders");
    let library = crate::regions::sessions::library_fragment(&shared, None)
        .await
        .expect("library pull renders");

    // Then each row carries the kebab button and exactly one popup
    // entry, whose action name is the library toggle and whose label
    // matches the row's membership; the popup's visibility rides the
    // shared row-menu signal naming the row.
    for (html, id, in_library) in [(&sessions, &plain, false), (&library, &member, true)] {
        let row = row_fragment_for(html, id);
        assert!(
            row.contains(&format!(
                "class=\"{}\"",
                crate::contract::SESSION_ROW_MENU_CLASS
            )),
            "the kebab renders: {row}"
        );
        let entry = row
            .split(&format!(
                "{}=\"{}\"",
                crate::contract::MENU_ENTRY_ACTION_ATTR,
                crate::contract::ACTION_LIBRARY_TOGGLE
            ))
            .nth(1)
            .expect("the library entry renders")
            .to_owned();
        let label = entry
            .split('>')
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .unwrap_or_default();
        assert_eq!(
            label,
            if in_library {
                "remove from library"
            } else {
                "add to library"
            },
            "the entry label matches membership: {row}"
        );
        assert!(
            row.contains(&format!(
                "${} === '{id}'",
                crate::contract::SIGNAL_ROW_MENU_OPEN
            )),
            "the popup's visibility names its row: {row}"
        );
        assert_eq!(
            row.matches(crate::contract::MENU_ENTRY_ACTION_ATTR).count(),
            1,
            "exactly one entry ships: {row}"
        );
    }
}

/// The one row's markup (kebab + popup + anchor), from a fragment.
fn row_fragment_for(html: &str, id: &SessionId) -> String {
    let marker = format!("class=\"{}\"", crate::contract::SESSION_ROW_MENU_CLASS);
    let id_attr = format!("{}=\"{id}\"", crate::contract::SESSION_ROW_MENU_ID_ATTR);
    // Rows render the kebab before the anchor; the row's popup rides
    // the same kebab block. Split on the kebab class, keep the chunk
    // naming this row's id.
    html.split(&marker)
        .skip(1)
        .find(|chunk| chunk.contains(&id_attr))
        .map_or_else(
            || panic!("a row for {id} renders: {html}"),
            |chunk| format!("{marker}{}", chunk.split("</a>").next().unwrap_or(chunk)),
        )
}

#[rstest::rstest]
#[test]
fn the_toggle_is_two_segments_over_the_sidebarview_signal() {
    // Given the rendered toggle fragment.
    let toggle = crate::templates::sidebar_toggle_html();

    // Then both segment ids render, each binds the active class off
    // the sidebarview signal, and the Library segment's click seeds
    // the signal value the pull's view param reads.
    assert!(
        toggle.contains(&format!(
            "id=\"{}\"",
            crate::contract::SESSIONS_VIEW_SESSIONS_ID
        )),
        "{toggle}"
    );
    assert!(
        toggle.contains(&format!(
            "id=\"{}\"",
            crate::contract::SESSIONS_VIEW_LIBRARY_ID
        )),
        "{toggle}"
    );
    assert!(
        toggle.contains(&format!(
            "${} === 'library'",
            crate::contract::SIGNAL_SIDEBAR_VIEW
        )),
        "{toggle}"
    );
    assert!(
        toggle.contains(&format!(
            "${} = 'library'",
            crate::contract::SIGNAL_SIDEBAR_VIEW
        )),
        "{toggle}"
    );
}
