//! Dashboard-slice integration tests: the slice's scope, cell, and actor
//! inside the composed system.
//!
//! These exercise the dashboard slice **as composed** — its dynamic scope
//! receiving composition chrome, its scope staying free of kernel keybind
//! groups, the `j`-key E2E through the routed message to the actor, and
//! rendering against the real dashboard cell (tab highlighting, actor
//! rows, status messages, placeholder, selection marker).
#![expect(
    clippy::unused_async_trait_impl,
    clippy::unused_async,
    reason = "trouper trait impls and harness fns are async by contract even when nothing awaits"
)]
#![allow(
    clippy::items_after_statements,
    reason = "test-local probe structs read best beside their use"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use crate::common::{composed_keymap, test_app, wait_for, wait_for_bounded};
use jinn_dashboard::dashboard_scope;
use jinn_kernel::{Bridge, KernelIntent, Key, KeyEvent, Modifiers};
use jinn_slices::TypedCell;
use jinn_tui::Scope;

/// A bare trouper service actor that handles and emits nothing.
///
/// Spawning one of these is how a test proves the dashboard's census
/// covers actors with no jinn-side footprint whatsoever: the runtime
/// announces its spawn, and nothing in jinn ever names it.
struct CensusProbe;

impl trouper::actor::ServiceActor for CensusProbe {
    fn manifest() -> trouper::schema::ActorManifest {
        trouper::schema::ActorManifest::new()
    }
    async fn init(
        _args: &trouper::json::Json,
    ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
        Ok(Self)
    }
}

/// Spawns a [`CensusProbe`] at `name` and returns its path.
async fn spawn_probe(system: &trouper::system::ActorSystem, name: &str) -> String {
    let path = trouper::builder::spawn_service_builder::<CensusProbe>(system)
        .at(trouper::actor::ActorPath::new(name))
        .start();
    path.to_string()
}

/// The composed keymap carries the terminal-overlay toggle in the
/// dashboard's dynamic scope: registered slice scopes get the per-scope
/// chrome too.
#[rstest::rstest]
#[test]
fn alt_t_resolves_in_the_dashboard_dynamic_scope() {
    // Given the composed keymap in the dashboard's dynamic scope.
    let keymap = composed_keymap();
    let mut wk = jinn_tui::app::WhichKeyInstance::new(keymap, Scope::Dynamic(dashboard_scope()));

    // When pressing <M-t>.
    let alt_t = KeyEvent {
        key: Key::Char('t'),
        modifiers: Modifiers {
            ctrl: false,
            alt: true,
            shift: false,
        },
    };
    let intent = wk.handle_key(alt_t);

    // Then the terminal overlay toggle fires (the term slice's
    // GlobalToggle row, materialized by composition).
    assert!(
        matches!(
            &intent,
            Some(KernelIntent::Dynamic(d))
                if d.slice == jinn_term_msg::view_scope() && d.action == "toggle-overlay"
        ),
        "dashboard scope: expected the term toggle-overlay dynamic intent, got {intent:?}"
    );
}

/// Kernel chat-history bindings never leak into the dashboard scope:
/// the slice's scope carries only its own rows.
#[rstest::rstest]
#[test]
fn dashboard_scope_has_no_chathistory_or_sidebar_bindings() {
    // Given the composed keymap.
    let keymap = composed_keymap();

    // When listing the dashboard scope's bindings.
    let groups = keymap.bindings_for_scope(Scope::Dynamic(dashboard_scope()));
    let all_desc: Vec<String> = groups
        .iter()
        .flat_map(|g| g.bindings.iter().map(|b| b.description.clone()))
        .collect();

    // Then no ChatHistory group descriptions appear.
    assert!(
        !all_desc
            .iter()
            .any(|d| d.contains("next") || d.contains("previous")),
        "ChatHistory groups leaked into Dashboard: {all_desc:?}"
    );
}

/// E2E: the j keypress routes through the composed keymap to the
/// dashboard actor, which applies the selection move to the slice cell.
#[rstest::rstest]
#[tokio::test]
async fn j_keypress_routes_to_dashboard_actor_and_moves_selection() {
    // Given a wired app: the harness runs `dashboard::activate`, which
    // spawns THE dashboard actor subscribed to the bus.
    let mut app = test_app().await;
    app.core
        .state
        .write()
        .frontend
        .scope_swap_base(jinn_slices::FocusScope::Dynamic(
            jinn_dashboard::dashboard_scope(),
        ));
    let slot = jinn_dashboard::dashboard_slot();
    let cell: TypedCell<jinn_dashboard::DashboardState> =
        app.services.slices.reader(&slot).expect("cell");
    cell.update(|d| {
        for i in 0..3 {
            d.mark_running(format!("actor-{i}"), None);
        }
    });
    app.which_key
        .set_scope(Scope::Dynamic(jinn_dashboard::dashboard_scope()));

    // When the j key resolves through the keymap and routes like the run loop.
    let protocol_key = {
        use crossterm::event::{KeyCode, KeyEvent as XKeyEvent, KeyModifiers};
        jinn_tui::convert::from_crossterm(XKeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .expect("j converts")
    };
    let before = cell.read().selected_index();
    let intent = app
        .which_key
        .handle_key(protocol_key)
        .expect("j resolves in Dashboard scope");
    app.route_intent(intent);
    wait_for("the dashboard actor to move the selection", || {
        cell.read().selected_index() == before + 1
    })
    .await;

    // Then the actor applied the move to the slice.
    assert_eq!(
        cell.read().selected_index(),
        before + 1,
        "j moves selection via the routed message"
    );
}

/// Writes into the dashboard slice cell through the app registry.
fn write_dashboard(app: &jinn_tui::TuiApp, f: impl FnOnce(&mut jinn_dashboard::DashboardState)) {
    let cell: TypedCell<jinn_dashboard::DashboardState> = app
        .services
        .slices
        .reader(&jinn_dashboard::dashboard_slot())
        .expect("harness registers the dashboard slot");
    cell.update(f);
}

/// Collects the entire terminal buffer into a single string.
fn buffer_string(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

fn render_app(app: &mut jinn_tui::TuiApp) -> ratatui::Terminal<ratatui::backend::TestBackend> {
    let (mut terminal, _area) = jinn_testutil::setup_term(80, 24);
    terminal
        .draw(|frame| app.render(frame))
        .expect("render succeeds");
    terminal
}

async fn dashboard_app() -> jinn_tui::TuiApp {
    let app = test_app().await;
    app.core
        .state
        .write()
        .frontend
        .scope_swap_base(jinn_slices::FocusScope::Dynamic(
            jinn_dashboard::dashboard_scope(),
        ));
    app
}

/// The registered dashboard tab renders highlighted in its own scope.
#[rstest::rstest]
#[tokio::test]
async fn registered_tab_is_highlighted_in_its_scope() {
    // Given a composed app whose base scope is the registered dashboard tab.
    let mut app = dashboard_app().await;

    // When rendering it at 80x24.
    let (mut terminal, _area) = jinn_testutil::setup_term(80, 24);
    terminal.draw(|frame| app.render(frame)).expect("render");

    // Then the dashboard tab cell has an active background (non-Reset).
    let layout = jinn_tui::render::app_layout::AppLayout::new(
        ratatui::layout::Rect::new(0, 0, 80, 24),
        1,
        12,
        30,
    );
    let buffer = terminal.backend().buffer();
    // " Chat " (6 cols) + separator space (1) = 7 cols offset.
    let dash_x = layout.tab_bar.x + 1 + " Chat ".len() as u16 + 1;
    let cell = buffer
        .cell((dash_x, layout.tab_bar.y))
        .expect("dashboard tab cell");
    assert_ne!(
        cell.bg,
        ratatui::style::Color::Reset,
        "dashboard tab should be highlighted in Dashboard scope"
    );
}

/// The dashboard tab stays highlighted when another overlay opens (the
/// tab bar follows the base scope, not the top of the stack).
#[rstest::rstest]
#[tokio::test]
async fn registered_tab_stays_highlighted_when_another_overlay_opens() {
    // Given a composed app in the dashboard scope with a quake overlay pushed.
    let mut app = dashboard_app().await;
    app.core
        .state
        .write()
        .frontend
        .scope_push(jinn_slices::FocusScope::Dynamic(
            jinn_slices::SliceScopeId::new("quake-bar", "bar"),
        ));
    let (mut terminal, _area) = jinn_testutil::setup_term(80, 24);

    // When rendering it with the quake overlay on the scope stack.
    terminal.draw(|frame| app.render(frame)).expect("render");

    // Then the dashboard tab is still highlighted (uses base scope, not top).
    let layout = jinn_tui::render::app_layout::AppLayout::new(
        ratatui::layout::Rect::new(0, 0, 80, 24),
        1,
        12,
        30,
    );
    let buffer = terminal.backend().buffer();
    let dash_x = layout.tab_bar.x + 1 + " Chat ".len() as u16 + 1;
    let chat_cell = buffer
        .cell((layout.tab_bar.x + 1, layout.tab_bar.y))
        .expect("chat tab cell");
    let dash_cell = buffer
        .cell((dash_x, layout.tab_bar.y))
        .expect("dashboard tab cell");
    assert_eq!(
        chat_cell.bg,
        ratatui::style::Color::Reset,
        "chat tab should NOT be highlighted when base is Dashboard"
    );
    assert_ne!(
        dash_cell.bg,
        ratatui::style::Color::Reset,
        "dashboard tab should stay highlighted when an overlay is open"
    );
}

/// The dashboard tab shows the actor name and lifecycle column.
#[rstest::rstest]
#[tokio::test]
async fn dashboard_tab_shows_actor_name_and_lifecycle() {
    // Given a composed app with a dashboard cell holding one running actor.
    let mut app = dashboard_app().await;
    write_dashboard(&app, |d| {
        d.mark_running("discord", Some("Discord bot".to_owned()));
    });

    // When rendering.
    let terminal = render_app(&mut app);

    // Then the buffer contains "discord".
    let buf_str = buffer_string(&terminal);
    assert!(buf_str.contains("discord"), "dashboard should show name");
    // And the lifecycle column reads "Running".
    assert!(
        buf_str.contains("Running"),
        "dashboard should show lifecycle"
    );
}

/// The dashboard tab shows a per-actor status message.
#[rstest::rstest]
#[tokio::test]
async fn dashboard_tab_shows_status_message_for_discord() {
    // Given a composed app with discord in a connected state.
    let mut app = dashboard_app().await;
    write_dashboard(&app, |d| {
        d.mark_running("discord", None);
        d.set_status_message("discord", Some("Connected".to_owned()));
    });

    // When rendering.
    let terminal = render_app(&mut app);

    // Then the buffer contains "Connected".
    let buf_str = buffer_string(&terminal);
    assert!(
        buf_str.contains("Connected"),
        "dashboard should show status message"
    );
}

/// An empty dashboard cell renders the placeholder.
#[rstest::rstest]
#[tokio::test]
async fn dashboard_tab_shows_empty_placeholder_when_no_actors() {
    // Given a composed app with an empty dashboard cell.
    let mut app = dashboard_app().await;
    write_dashboard(&app, jinn_dashboard::DashboardState::clear);

    // When rendering.
    let terminal = render_app(&mut app);

    // Then the buffer contains the placeholder.
    let buf_str = buffer_string(&terminal);
    assert!(
        buf_str.contains("No services"),
        "empty dashboard should show placeholder"
    );
}

/// The selected dashboard row carries the selection marker.
#[rstest::rstest]
#[tokio::test]
async fn dashboard_tab_shows_selection_marker_on_selected_entry() {
    // Given a composed app with two actors, the second selected.
    let mut app = dashboard_app().await;
    write_dashboard(&app, |d| {
        d.mark_running("alpha", None);
        d.mark_running("beta", None);
        d.select_next(); // select beta (index 1)
    });

    // When rendering.
    let terminal = render_app(&mut app);

    // Then the buffer contains the selection marker ▸.
    let buf_str = buffer_string(&terminal);
    assert!(buf_str.contains('▸'), "selected entry should have marker");
}

/// The dashboard tab draws no em-dash separator between name and
/// description.
#[rstest::rstest]
#[tokio::test]
async fn dashboard_tab_has_no_em_dash_separator() {
    // Given a composed app with an actor that has a description.
    let mut app = dashboard_app().await;
    write_dashboard(&app, |d| {
        d.mark_running("discord", Some("Discord bot".to_owned()));
    });

    // When rendering.
    let terminal = render_app(&mut app);

    // Then the buffer contains no em-dash characters.
    let buf_str = buffer_string(&terminal);
    assert!(
        !buf_str.contains('\u{2014}'),
        "dashboard should not contain em-dashes"
    );
}

/// THE decisive test for the census migration. The dashboard used to
/// learn about actors only from hand-written `ActorStarting` /
/// `ActorStarted` publishes at a single spawn site, so it listed two of
/// roughly forty-five live actors. Rows now come from the runtime's own
/// spawn announcement, so a real actor spawned in the composed app must
/// appear without any jinn-side publish naming it.
#[rstest::rstest]
#[tokio::test]
async fn a_real_actor_appears_in_the_dashboard_without_any_feature_publish() {
    // Given a composed app whose dashboard slice is active.
    let app = test_app().await;
    let slot = jinn_dashboard::dashboard_slot();
    let cell: TypedCell<jinn_dashboard::DashboardState> =
        app.services.slices.reader(&slot).expect("cell");

    // When the runtime spawns an actor that publishes no jinn event.
    let path = spawn_probe(&app.services.trouper_system, "census-probe").await;

    // Then a row appears for it, keyed by the runtime's actor path.
    wait_for("the spawned actor's row to appear", || {
        cell.read().actors().iter().any(|e| e.name == path)
    })
    .await;
    let row = {
        let reader = cell.read();
        reader
            .actors()
            .into_iter()
            .find(|e| e.name == path)
            .cloned()
            .expect("row exists")
    };
    assert_eq!(row.lifecycle, jinn_dashboard::ActorLifecycle::Running);
}

/// The census is fed by the runtime announcement alone, so a row must
/// exist with no description: the description column is populated only
/// where a feature publishes a `ServiceStatusUpdate`, and most actors
/// have none.
#[rstest::rstest]
#[tokio::test]
async fn a_census_row_carries_no_description_without_a_feature_status_publish() {
    // Given a composed app whose census cell is readable.
    let app = test_app().await;
    let slot = jinn_dashboard::dashboard_slot();
    let cell: TypedCell<jinn_dashboard::DashboardState> =
        app.services.slices.reader(&slot).expect("cell");
    let path = spawn_probe(&app.services.trouper_system, "undescribed-probe").await;

    // When waiting for the spawned actor's row to appear.
    wait_for("the row to appear", || {
        cell.read().actors().iter().any(|e| e.name == path)
    })
    .await;

    // Then its description is empty — the runtime reports identity and
    // liveness, never prose.
    let row = {
        let reader = cell.read();
        reader
            .actors()
            .into_iter()
            .find(|e| e.name == path)
            .cloned()
            .expect("row exists")
    };
    assert_eq!(row.description, None);
}

/// A feature's status message still populates the row it names. The
/// census owns existence and liveness; the feature owns prose.
#[rstest::rstest]
#[tokio::test]
async fn a_feature_status_message_populates_a_census_row() {
    // Given a composed app and a row born from the runtime's census.
    let app = test_app().await;
    let slot = jinn_dashboard::dashboard_slot();
    let cell: TypedCell<jinn_dashboard::DashboardState> =
        app.services.slices.reader(&slot).expect("cell");
    let update = jinn_slices::ServiceStatusUpdate {
        name: "status-probe".to_owned(),
        description: Some("probe service".to_owned()),
        status_message: Some("working".to_owned()),
        note_tone: None,
    };
    let _ = app.core.bridge.send(Bridge::publish_closure(update));

    // When waiting for the status row to appear.
    wait_for("the status row to appear", || {
        cell.read()
            .actors()
            .iter()
            .any(|e| e.name == "status-probe" && e.description.as_deref() == Some("probe service"))
    })
    .await;

    // Then the row keeps its status message.
    let row = {
        let reader = cell.read();
        reader
            .actors()
            .into_iter()
            .find(|e| e.name == "status-probe")
            .cloned()
            .expect("row exists")
    };
    assert_eq!(row.status_message.as_deref(), Some("working"));
}

/// A startup-scale burst of spawn announcements must arrive complete: a
/// row lost to a full inbox is an actor silently missing from the census,
/// and the missing set would differ on every launch.
#[rstest::rstest]
#[tokio::test]
#[timeout(std::time::Duration::from_secs(30))]
async fn a_spawn_announcement_flood_loses_no_rows() {
    // Given a composed app and its cell reader.
    let app = test_app().await;
    let slot = jinn_dashboard::dashboard_slot();
    let cell: TypedCell<jinn_dashboard::DashboardState> =
        app.services.slices.reader(&slot).expect("cell");

    // When 200 actors spawn back to back.
    const SPAWNS: usize = 200;
    let system = app.services.trouper_system.clone();
    let mut handles = Vec::with_capacity(SPAWNS);
    for i in 0..SPAWNS {
        let system = system.clone();
        handles.push(tokio::spawn(async move {
            spawn_probe(&system, &format!("flood-{i}")).await;
        }));
    }
    for handle in handles {
        handle.await.expect("spawn task");
    }

    // Then every flooded actor has a row.
    wait_for_bounded("all flooded rows to appear", 20, || {
        let reader = cell.read();
        let missing: Vec<String> = (0..SPAWNS)
            .filter(|i| {
                !reader
                    .actors()
                    .iter()
                    .any(|e| e.name == format!("flood-{i}"))
            })
            .map(|i| format!("flood-{i}"))
            .collect();
        missing.is_empty()
    })
    .await;
}
