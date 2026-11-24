//! The applets region's tests: the pull renders the modal after the
//! bounded rescan/wake wait (and carries the published pair on the
//! recording bus), the rows are `target="_blank"` links with the
//! correct href shape on a serving cell and disabled offline spans
//! without one, the filter narrows server-side with `<mark>` runs,
//! an empty catalog renders exactly the one explanatory row, the
//! wait is deadline-bounded, and the conformance gate holds the
//! region to its pull-only declaration.
//!
//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use crate::contract;
use crate::region::WebRegion as _;
use crate::server::ServerShared;

use super::AppletsRegion;
use super::CELLS_WAIT_DEADLINE;
use jinn_applets_msg::AppletEntry;
use jinn_applets_msg::AppletsCatalog;
use jinn_slices::bus::BusAudit;

/// One catalog fixture entry.
fn entry(id: &str, title: &str, created_at: &str) -> AppletEntry {
    AppletEntry {
        id: id.to_owned(),
        title: title.to_owned(),
        created_at: created_at.to_owned(),
    }
}

/// The region-test harness: an empty app state with the two applets
/// cells registered, and the shared handler state over an empty
/// config layer (the catalog comes from the applets cells, not
/// config). Returns the slices (for seeding), the shared, and the
/// recording bus audit (the pull's publications).
fn harness() -> (jinn_slices::Slices, ServerShared, BusAudit) {
    let slices = jinn_slices::Slices::new();
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
        .expect("the MCP runtime cell registers once per harness");
    let applets_catalog_cell = slices
        .register(
            jinn_applets_msg::applets_catalog_slot(),
            jinn_applets_msg::AppletsCatalog::default(),
        )
        .expect("the applets catalog cell registers once per harness");
    let applets_picker_cell = slices
        .register(
            jinn_applets_msg::applets_picker_slot(),
            jinn_applets_msg::AppletsPickerState::default(),
        )
        .expect("the applets picker cell registers once per harness");
    let unread_marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per harness");
    let state = {
        let app = jinn_app_state::AppState::default();
        app.session.attach_slices(slices.clone());
        jinn_kernel::State::new(app)
    };
    let (bus, audit) = jinn_slices::bus::BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = crate::server::ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        jinn_kernel::common::turn_ledger::TurnLedger::default(),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        applets_catalog_cell,
        applets_picker_cell,
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
        "applets-region-test".to_owned(),
    );
    (slices, shared, audit)
}

/// Counts the recorded publishes whose schema short name matches.
fn audit_count(audit: &BusAudit, name: &str) -> usize {
    audit.names().iter().filter(|n| n.ends_with(name)).count()
}

/// Writes `catalog` into the catalog cell and folds `bind` into the
/// picker cell's serving phase (the writes the applets slice's own
/// actors perform; the web never writes these in production — the
/// test harness does, because there is no applets slice here).
fn seed_cells(slices: &jinn_slices::Slices, catalog: AppletsCatalog, serving_bind: Option<&str>) {
    let catalog_cell = slices
        .reader::<AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
        .expect("the catalog cell resolves");
    catalog_cell.update(|cell| *cell = catalog);
    let picker_cell = slices
        .reader::<jinn_applets_msg::AppletsPickerState>(&jinn_applets_msg::applets_picker_slot())
        .expect("the picker cell resolves");
    picker_cell.update(|cell| match serving_bind {
        Some(bind) => cell.apply_serving(true, bind),
        None => cell.apply_serving(false, ""),
    });
}

/// One row's full fragment, sliced from the rows fragment: from the
/// row's `<a`/`<span` open to its closing tag. Assertions probe this
/// slice — not the whole container, where another row's class would
/// satisfy a naive `contains`.
fn row_fragment(html: &str, id: &str) -> String {
    let marker = format!("{}=\"{id}\"", contract::APPLET_NAME_DATA_ATTR);
    let (before, _after) = html
        .split_once(&marker)
        .unwrap_or_else(|| panic!("a row renders for {id}: {html}"));
    let (start, close, close_len) = if let Some(pos) = before.rfind("<a ") {
        (pos, "</a>", "</a>".len())
    } else {
        let pos = before
            .rfind("<span ")
            .unwrap_or_else(|| panic!("a row element precedes the {id} marker: {html}"));
        (pos, "</span>", "</span>".len())
    };
    let end = html
        .get(start..)
        .and_then(|tail| tail.find(close))
        .unwrap_or_else(|| panic!("the {id} row's fragment closes: {html}"));
    html.get(start..start + end + close_len)
        .unwrap_or_else(|| panic!("the {id} row's fragment slices cleanly: {html}"))
        .to_owned()
}

#[rstest::rstest]
#[test]
fn the_region_conforms_with_pull_route_only_declared() {
    // Given the applets region.
    let region = AppletsRegion;

    // Then the conformance gate finds nothing — a pull route, no SSE
    // kind (the applets slice's republisher owns its cells; the web
    // never subscribes), no settle pass, and no page-shell container
    // (the modal swaps into its own host span) is the honest
    // declaration for a pull-opened modal.
    let gaps = crate::region::check_region(&region);
    assert!(gaps.is_empty(), "the applets region must conform: {gaps:?}");
    assert_eq!(region.id(), "applets");
    assert_eq!(region.container_id(), None);
    assert_eq!(region.event_kinds(), [] as [jinn_webui_msg::SseKind; 0]);
}

#[rstest::rstest]
#[test]
fn the_wait_deadline_is_the_contract_bounded_constant() {
    // Given the region's wait constant.
    // Then it is exactly the contract's 1000 ms — the open pull's
    // budget the fragment's freshness rides on.
    assert_eq!(CELLS_WAIT_DEADLINE, std::time::Duration::from_millis(1000));
}

#[rstest::rstest]
#[tokio::test]
async fn the_pull_publishes_the_rescan_and_the_wake() {
    // Given a harness with an empty catalog.
    let (_slices, shared, audit) = harness();

    // When pulling the modal through the open pull's body (the
    // snapshot → publish → bounded wait → render sequence).
    let _html = super::open_fragment(&shared)
        .await
        .expect("the modal renders");

    // Then the pull's publication pair — rescan plus wake — crossed
    // the recording bus exactly once each (the freshness gesture the
    // TUI picker's opener mirrors).
    assert_eq!(audit_count(&audit, "RescanApplets"), 1);
    assert_eq!(audit_count(&audit, "WakeAppletsServer"), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn the_bounded_wait_returns_when_the_catalog_changes() {
    // Given a harness whose catalog cell will change shortly after a
    // poll starts (the store actor's rescan landing, simulated).
    let (slices, shared, _audit) = harness();
    let writer = slices
        .reader::<AppletsCatalog>(&jinn_applets_msg::applets_catalog_slot())
        .expect("the catalog cell resolves");
    let change = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        writer.update(|cell| {
            cell.entries = vec![entry("quiz", "Quiz", "2026-01-01T00:00:00Z")];
        });
    });

    // When waiting bounded on the snapshot taken before the change.
    let before = super::catalog_snapshot(&shared);
    let serving_before = super::serving_snapshot(&shared);
    let started = std::time::Instant::now();
    let changed = super::await_cells_changed(&shared, (&before, serving_before.as_deref())).await;
    change.await.expect("the changer joins");

    // Then the wait observed the change well within the deadline —
    // the pull renders the fresh cells, not the stale snapshot.
    assert!(changed, "the wait observed the catalog change");
    assert!(
        started.elapsed() < CELLS_WAIT_DEADLINE,
        "the wait returned promptly: {:?}",
        started.elapsed()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_bounded_wait_expires_without_a_change() {
    // Given a harness whose cells never change (no applets slice is
    // running — the wedged-actor shape).
    let (_slices, shared, _audit) = harness();
    let before = super::catalog_snapshot(&shared);
    let serving_before = super::serving_snapshot(&shared);

    // When waiting bounded on that snapshot.
    let started = std::time::Instant::now();
    let changed = super::await_cells_changed(&shared, (&before, serving_before.as_deref())).await;

    // Then the wait gave up at the deadline, not before — the
    // fragment renders whatever the cells hold, never an error.
    assert!(!changed, "an unchanged cell pair expires the wait");
    assert!(
        started.elapsed() >= CELLS_WAIT_DEADLINE,
        "the wait held to its budget: {:?}",
        started.elapsed()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_pull_returns_within_the_deadline_even_when_nothing_changes() {
    // Given a harness with no applets slice running.
    let (_slices, shared, _audit) = harness();

    // When pulling the modal through the handler.
    let started = std::time::Instant::now();
    let _fragment = super::open_fragment(&shared)
        .await
        .expect("the pull renders");

    // Then the handler answered within the deadline — bounded, never
    // hung, never an error on a silent bus.
    assert!(
        started.elapsed() < CELLS_WAIT_DEADLINE + std::time::Duration::from_millis(250),
        "the pull answered within its budget: {:?}",
        started.elapsed()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_serving_cell_renders_target_blank_links_with_the_bind_href() {
    // Given a catalog with two entries and the server serving at a
    // resolved bind.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![
                entry("sales-dashboard", "Sales Dashboard", "2026-11-01T10:00:00Z"),
                entry("quiz", "Quiz App", "2026-11-02T10:00:00Z"),
            ],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering the rows.
    let html = super::rows_fragment(&shared, "").expect("the rows render");

    // Then each row is an anchor with the exact applets URL formula —
    // the resolved bind, `/applets/<id>/`, trailing slash — carrying
    // `target="_blank"` and `rel="noopener"`, and the status line's
    // sentence names the bind.
    let quiz = row_fragment(&html, "quiz");
    assert!(
        quiz.contains(r#"href="http://127.0.0.1:41234/applets/quiz/""#),
        "the href re-derives the applets URL: {quiz}"
    );
    assert!(
        quiz.contains(r#"target="_blank""#) && quiz.contains(r#"rel="noopener""#),
        "the row opens a new tab: {quiz}"
    );
    let modal = super::modal_fragment(&shared, "").expect("the modal renders");
    assert!(
        modal.contains("serving http://127.0.0.1:41234"),
        "the status line names the serving bind: {modal}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_offline_cell_renders_disabled_spans_without_href_and_the_offline_status() {
    // Given a catalog with one entry and the picker cell dormant.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![entry("quiz", "Quiz", "2026-01-01T00:00:00Z")],
        },
        None,
    );

    // When rendering the rows.
    let html = super::rows_fragment(&shared, "").expect("the rows render");

    // Then the row is a disabled span with the offline hint and no
    // href anywhere, and the status line reads the offline sentence.
    let row = row_fragment(&html, "quiz");
    assert!(row.contains("<span"), "the offline row is a span: {row}");
    assert!(
        !html.contains("href="),
        "an offline row carries no href: {html}"
    );
    assert!(
        row.contains("applet-row-disabled"),
        "the offline row wears the disabled class: {row}"
    );
    assert!(
        row.contains("The applets server is offline"),
        "the offline row carries the hint: {row}"
    );
    let modal = super::modal_fragment(&shared, "").expect("the modal renders");
    assert!(
        modal.contains("&lt;server offline&gt;"),
        "the status line reads the offline sentence: {modal}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_id_failing_the_gate_renders_disabled_without_href_even_online() {
    // Given a serving cell whose catalog carries a hand-edited
    // traversal id.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![entry("../escape", "Escape", "2026-01-01T00:00:00Z")],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering the rows.
    let html = super::rows_fragment(&shared, "").expect("the rows render");

    // Then the row renders disabled with no href — the gate refuses
    // the path a hand-edited catalog could inject.
    let row = row_fragment(&html, "../escape");
    assert!(
        !html.contains("href="),
        "a refused id renders no href: {html}"
    );
    assert!(
        row.contains("applet-row-disabled"),
        "the refused row is disabled: {row}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_empty_catalog_renders_exactly_the_one_empty_row() {
    // Given a dormant cell pair with an empty catalog.
    let (slices, shared, _audit) = harness();
    seed_cells(&slices, AppletsCatalog::default(), None);

    // When rendering the modal.
    let modal = super::modal_fragment(&shared, "").expect("the modal renders");

    // Then the rows container carries exactly one "No applets saved
    // yet" div, and the status line still speaks the serving phase.
    assert_eq!(
        modal.matches("applet-empty").count(),
        1,
        "exactly one empty row renders: {modal}"
    );
    assert!(
        modal.contains("No applets saved yet"),
        "the empty row explains: {modal}"
    );
    assert!(
        modal.contains("&lt;server offline&gt;"),
        "the status line still reflects the phase: {modal}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_renders_the_filter_input_bound_to_its_signal() {
    // Given a serving cell with a catalog.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![entry("quiz", "Quiz", "2026-01-01T00:00:00Z")],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering the modal.
    let html = super::modal_fragment(&shared, "").expect("the modal renders");

    // Then the filter input renders with the contract's id and binds
    // the filter signal, and the rows effect debounce-pulls the rows
    // route targeting the rows container only — the skeleton (and
    // the input it carries) is never re-rendered mid-typing.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::APPLET_FILTER_ID)),
        "the filter input is missing: {html}"
    );
    assert!(
        html.contains(&format!("data-bind:{}", contract::SIGNAL_APPLETS_FILTER)),
        "the input binds the filter signal: {html}"
    );
    assert!(
        html.contains(&format!(
            "data-effect__debounce.150ms=\"$appletsOpen && (${}",
            contract::SIGNAL_APPLETS_FILTER
        )),
        "the effect debounce-pulls on the filter: {html}"
    );
    let effect_at = html
        .find("data-effect__debounce.150ms")
        .expect("the effect renders");
    let effect = html.get(effect_at..effect_at + 400).unwrap_or_default();
    assert!(
        effect.contains(&format!("'{}'", contract::APPLET_ROWS_SELECTOR)),
        "the effect targets the rows container: {effect}"
    );
    assert!(
        effect.contains(contract::APPLET_ROWS_ROUTE),
        "the effect pulls the rows route: {effect}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_narrows_the_rows_server_side() {
    // Given a catalog with three entries on a serving cell.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![
                entry("alpha", "Alpha Report", "2026-01-01T00:00:00Z"),
                entry("beta", "Beta Dashboard", "2026-01-02T00:00:00Z"),
                entry("gamma", "Gamma Quiz", "2026-01-03T00:00:00Z"),
            ],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering with a filter matching one row.
    let html = super::rows_fragment(&shared, "alp").expect("the rows render");

    // Then exactly the matching row renders.
    assert!(
        html.contains(&format!("{}=\"alpha\"", contract::APPLET_NAME_DATA_ATTR)),
        "the matching row renders: {html}"
    );
    assert!(
        !html.contains(&format!("{}=\"beta\"", contract::APPLET_NAME_DATA_ATTR)),
        "non-matching rows are withheld: {html}"
    );
    assert!(
        !html.contains(&format!("{}=\"gamma\"", contract::APPLET_NAME_DATA_ATTR)),
        "non-matching rows are withheld: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_marks_the_matched_runs() {
    // Given a catalog with one entry.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![entry("alpha", "Alpha Report", "2026-01-01T00:00:00Z")],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering with a filter matching part of the title.
    let html = super::rows_fragment(&shared, "alp").expect("the rows render");

    // Then the matched run renders in a `<mark>` inside the title.
    let row = row_fragment(&html, "alpha");
    assert!(
        row.contains("<mark>Alp</mark>"),
        "the matched run is marked: {row}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_empty_filter_renders_every_row_in_catalog_order_unmarked() {
    // Given a catalog already ordered newest-first.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![
                entry("newest", "Newest", "2026-06-01T00:00:00Z"),
                entry("older", "Older", "2026-01-01T00:00:00Z"),
            ],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering with an empty filter.
    let html = super::rows_fragment(&shared, "").expect("the rows render");

    // Then every row renders in the catalog's order — the store
    // actor owns ordering — and none carries a `<mark>`.
    let newest = html
        .find(&format!("{}=\"newest\"", contract::APPLET_NAME_DATA_ATTR))
        .expect("the newest row renders");
    let older = html
        .find(&format!("{}=\"older\"", contract::APPLET_NAME_DATA_ATTR))
        .expect("the older row renders");
    assert!(
        newest < older,
        "rows render in the catalog's recency order: {html}"
    );
    assert!(!html.contains("<mark>"), "no run is marked: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_and_rows_fragments_render_no_rename_delete_or_kiosk() {
    // Given a serving cell with a catalog.
    let (slices, shared, _audit) = harness();
    seed_cells(
        &slices,
        AppletsCatalog {
            entries: vec![entry("quiz", "Quiz", "2026-01-01T00:00:00Z")],
        },
        Some("127.0.0.1:41234"),
    );

    // When rendering both fragments.
    let modal = super::modal_fragment(&shared, "").expect("the modal renders");
    let rows = super::rows_fragment(&shared, "").expect("the rows render");

    // Then neither carries a rename, delete, or kiosk affordance —
    // list + filter + open is the whole picker parity scope.
    for html in [modal, rows] {
        assert!(
            !html.contains("rename") && !html.contains("Rename"),
            "no rename affordance: {html}"
        );
        assert!(
            !html.contains("delete") && !html.contains("Delete"),
            "no delete affordance: {html}"
        );
        assert!(!html.contains("kiosk"), "no kiosk affordance: {html}");
    }
}
