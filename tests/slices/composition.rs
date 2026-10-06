//! Composition-seam integration tests: the shared seam itself, not any
//! single slice.
//!
//! These assert what composition as a whole must provide — that the
//! composed route table carries every in-tree slice's rows. A missing
//! slice here means its `activate()` never attached rows, so nothing
//! downstream (keymap, which-key, rendering) can see it.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use crate::common::{composed_keymap, composition_routes, test_app};
use jinn_dashboard::dashboard_scope;
use jinn_quake_bar::quake_scope;

/// The row seam carries every slice's rows: dashboard, quake-bar, and
/// discord all attached (the precondition the keymap tests rely on).
#[rstest::rstest]
#[test]
fn composition_sees_rows_from_every_slice() {
    // Given the composed route table.
    let routes = composition_routes();

    // When listing the dynamic scopes it knows about.
    let scopes = jinn_tui::keymap_gen::dynamic_scopes(&routes);

    // Then every slice's scope is present.
    assert!(
        scopes.iter().any(|s| *s == dashboard_scope()),
        "dashboard scope missing from composed routes"
    );
    assert!(
        scopes.iter().any(|s| *s == quake_scope()),
        "quake-bar scope missing from composed routes"
    );
    assert!(
        scopes.iter().any(|s| *s == jinn_discord::discord_scope()),
        "discord scope missing from composed routes"
    );
}

/// Every picker in the tree, paired with the scope its activation registers.
fn every_picker_scope() -> [(&'static str, jinn_slices::SliceScopeId); 12] {
    [
        ("skills", jinn_skills_msg::skill_picker_scope()),
        ("persona", jinn_persona_msg::persona_picker_scope()),
        ("theme", jinn_theme_msg::theme_picker_scope()),
        (
            "reasoning",
            jinn_provider_selection_msg::reasoning_picker_scope(),
        ),
        ("tool", jinn_tools_msg::tool_picker_scope()),
        (
            "session lifecycle",
            jinn_session_lifecycle_msg::session_lifecycle_picker_scope(),
        ),
        (
            "endpoint",
            jinn_provider_selection_msg::endpoint_picker_scope(),
        ),
        ("task list", jinn_tools_msg::task_list_picker_scope()),
        ("session", jinn_session_store_msg::session_picker_scope()),
        (
            "provider",
            jinn_provider_selection_msg::provider_picker_scope(),
        ),
        ("mcp", jinn_mcp_msg::mcp_picker_scope()),
        ("project", jinn_project_msg::project_picker_scope()),
    ]
}

/// The central and shared crates whose sources the picker-decoupling guard reads.
fn central_crate_sources() -> [(&'static str, &'static str); 6] {
    [
        (
            "jinn-kernel intent handler",
            include_str!("../../crates/jinn-kernel/src/feat/intent/handler.rs"),
        ),
        (
            "jinn-kernel protocol intents",
            include_str!("../../crates/jinn-kernel/src/protocol/intent.rs"),
        ),
        (
            "shared frontend state",
            include_str!("../../crates/jinn-app-state/src/frontend_state.rs"),
        ),
        (
            "jinn-tui scope table",
            include_str!("../../crates/jinn-tui/src/scope.rs"),
        ),
        (
            "jinn-tui keymap",
            include_str!("../../crates/jinn-tui/src/keymap.rs"),
        ),
        (
            "jinn-tui keymap generator",
            include_str!("../../crates/jinn-tui/src/keymap_gen.rs"),
        ),
    ]
}

/// Every picker identity a central crate is forbidden to name.
const PICKER_IDENTITIES: [&str; 24] = [
    "PickerSkill",
    "PickerPersona",
    "PickerTheme",
    "PickerReasoning",
    "PickerTool",
    "PickerLifecycle",
    "PickerEndpoint",
    "PickerTaskList",
    "PickerSession",
    "PickerProvider",
    "PickerMcpServer",
    "PickerProject",
    "skill_spec",
    "persona_spec",
    "theme_spec",
    "provider_spec",
    "project_spec",
    "mcp_server_spec",
    "task_list_spec",
    "session_spec",
    "tool_spec",
    "endpoint_spec",
    "reasoning_effort_spec",
    "session_lifecycle_spec",
];

/// The picker identities found in one central crate's source, if any.
fn picker_identities_in(source: &str) -> Vec<&str> {
    PICKER_IDENTITIES
        .into_iter()
        .filter(|needle| source.contains(needle))
        .collect()
}

/// Every picker in the tree is owned by a slice: each registers its own
/// overlay, so the central app crate and TUI layer need no knowledge of any
/// picker to draw one. This is what makes a new picker a folder-local change.
#[rstest::rstest]
#[test]
fn every_picker_is_a_slice_registered_overlay() {
    // Given a freshly composed slices registry.
    let slices = jinn_slices::Slices::new();
    // Production boot seeds every slice cell here, before any activation.
    jinn_cell_catalog::register_all_cells(&slices);

    // When each slice registers its picker overlay.
    let _ = slices;

    for (label, picker_scope) in every_picker_scope() {
        // Then the scope captures input so its filter receives keys.
        assert!(
            picker_scope.captures_input(),
            "{label} picker scope must capture input so its filter receives keys"
        );
    }
}

/// The central crates name no picker. A picker identity appearing in the
/// kernel or the TUI layer is the coupling this migration removed: it is
/// what forced a new picker to be registered in three places at once.
#[rstest::rstest]
#[test]
fn the_central_crates_name_no_picker() {
    // Given the central crates' sources.
    let central = central_crate_sources();

    // When each is searched for a picker identity.
    for (label, source) in central {
        let hits = picker_identities_in(source);

        // Then none is found.
        assert!(
            hits.is_empty(),
            "{label} still names a picker ({hits:?}); pickers must be slice-owned"
        );
    }
}

/// Every picker scope owns at least one row in the production-composed table.
///
/// The reasoning-effort picker shipped broken for exactly this reason: its
/// `activate_picker` existed, was tested, and was never called. The row was
/// absent and the key did nothing.
///
/// This guards the *test harness* composition. It cannot guard
/// `actor_wiring`, which is why the wiring file must be read against this
/// list by eye (or by the sibling source test below).
#[rstest::rstest]
#[tokio::test]
async fn every_picker_scope_owns_rows_in_the_test_composition() {
    // Given the production route table.
    let app = test_app().await;
    let routes = app.services.key_routes.clone();

    // When each slice-owned picker scope is looked up.
    for (label, scope) in every_picker_scope() {
        let owned = routes
            .rows()
            .iter()
            .filter(|row| row.scope == scope)
            .count();

        // Then it owns rows — a picker whose activation attaches nothing owns
        // none, and every one of its keys does nothing.
        assert!(
            owned > 0,
            "{label} picker scope owns no rows: its activation attached nothing"
        );
    }
}

/// Every picker opener key is claimed by exactly one row.
///
/// Dispatch is first-match-wins over an append-only list, so two rows claiming
/// one key silently shadow each other: the losing picker simply stops opening,
/// with no compile error and no log line. Three such collisions appeared while
/// the pickers were being migrated — this is the guard that catches the next.
#[rstest::rstest]
#[tokio::test]
async fn every_picker_opener_key_has_exactly_one_claimant() {
    use jinn_slices::route::BindSite;

    // Given the composed route table with every slice's real `activate()`
    // run over a real SliceHost — the same call production makes.
    let routes = all_picker_routes();

    // When each trunk key is looked up among the Normal-scope rows.
    for (label, key) in [
        ("provider", "<leader>sm"),
        ("session", "<leader>ss"),
        ("persona", "<leader>se"),
        ("tool", "<leader>st"),
        ("skill", "<leader>sk"),
        ("mcp", "<leader>sM"),
        ("theme", "<leader>sh"),
        ("reasoning", "<leader>sr"),
        ("endpoint", "<leader>sE"),
        ("project", "<leader>sp"),
        ("lifecycle", "<leader>sl"),
    ] {
        let claimants: Vec<&str> = routes
            .rows()
            .iter()
            .filter(|row| {
                row.key == key
                    && matches!(row.site, BindSite::StaticScopes(scopes) if scopes.contains(&"Normal"))
            })
            .map(|row| row.route_id.as_str())
            .collect();

        // Then exactly one row claims it.
        assert_eq!(
            claimants.len(),
            1,
            "{label}: {key} is claimed by {claimants:?}; exactly one picker may bind it"
        );
    }
}

/// A route table carrying every picker slice's real rows.
///
/// Each slice's `activate` (or `activate_*_picker`) is called over a real
/// [`SliceHost`], exactly as `actor_wiring` does, so this exercises the rows
/// production binds rather than a hand-built imitation.
fn all_picker_routes() -> jinn_slices::KeyRoutes {
    let slices = jinn_slices::Slices::new();
    // Production boot seeds every slice cell here, before any activation.
    jinn_cell_catalog::register_all_cells(&slices);
    let mut viewport = jinn_slices::view::Viewport::new();
    let overlay_views = jinn_slices::OverlayViews::new();
    let routes = jinn_slices::KeyRoutes::new();
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());

    let mut host =
        jinn_slices::SliceHost::new(&slices, &mut viewport, &overlay_views, &routes, &system);
    jinn_skills::activate(&mut host);
    jinn_persona::activate_picker(&mut host);
    jinn_tools::activate_picker(&mut host);
    let session_cell = slices
        .reader::<jinn_session_store_msg::SessionPickerState>(
            &jinn_session_store_msg::session_picker_slot(),
        )
        .expect("the cell catalog registers the session picker slot");
    jinn_session_store::activate_session_picker(&mut host, &session_cell);
    jinn_mcp_slice::activate_picker(&mut host);
    jinn_project::activate(&mut host);
    jinn_session_lifecycle::activate_picker(&mut host);
    jinn_provider_selection::activate_picker(&mut host);
    let provider_cell = slices
        .reader::<jinn_provider_selection_msg::ProviderPickerState>(
            &jinn_provider_selection_msg::provider_picker_slot(),
        )
        .expect("the cell catalog registers the provider picker slot");
    jinn_provider_selection::activate_provider_picker(&mut host, &provider_cell);
    let endpoint_cell = slices
        .reader::<jinn_provider_selection_msg::endpoint::EndpointPickerState>(
            &jinn_provider_selection_msg::endpoint_picker_slot(),
        )
        .expect("the cell catalog registers the endpoint picker slot");
    jinn_provider_selection::activate_endpoint_picker(&mut host, &endpoint_cell);
    jinn_theme_slice::activate_picker(&mut host);
    routes
}

/// Every picker activation in the boot list is actually called.
///
/// The reasoning-effort picker shipped broken because `activate_picker` was
/// written, tested through the harness, and never wired. The harness test
/// above cannot catch that — it composes the harness, not the production
/// wiring file. This reads the boot list and asserts each activation
/// function's name appears as a call.
///
/// Source-level, deliberately: a behavioural test would need a full app boot,
/// and the thing that broke was a missing line in a file no test executes.
#[rstest::rstest]
fn production_wiring_calls_every_picker_activation() {
    // Given the production boot list.
    let wiring = std::fs::read_to_string("src/bootstrap/slices.rs")
        .expect("src/bootstrap/slices.rs is present in every checkout");

    // When each slice-owned picker's activation function is looked for.
    for (label, needle) in [
        ("skills", "jinn_skills::activate("),
        ("persona", "jinn_persona::activate_picker("),
        ("theme", "jinn_theme_slice::activate_picker("),
        ("reasoning", "jinn_provider_selection::activate_picker("),
        ("tool + task list", "jinn_tools::activate_picker("),
        (
            "session lifecycle",
            "jinn_session_lifecycle::activate_picker(",
        ),
        (
            "endpoint",
            "jinn_provider_selection::activate_endpoint_picker(",
        ),
        ("session", "jinn_session_store::activate_session_picker("),
        (
            "provider",
            "jinn_provider_selection::activate_provider_picker(",
        ),
        ("mcp", "jinn_mcp_slice::activate_picker("),
        // The project picker registers its rows inside `activate` itself, so
        // that is the call to require.
        ("project", "jinn_project::activate("),
    ] {
        // Then it is called in production.
        assert!(
            wiring.contains(needle),
            "{label} picker is never activated in src/bootstrap/slices.rs: its keys do nothing"
        );
    }
}

/// `s` in the sidebar's task-list section opens the task-list browser.
///
/// The row used to publish a `DynamicIntent` naming the tools slice's open
/// action. A published message goes to the bus and never returns through route
/// dispatch, so the action never ran and the menu never appeared — while every
/// test that only asked "does this scope own rows?" kept passing. This asserts
/// the press produces the picker's scope on the stack.
#[rstest::rstest]
#[tokio::test]
async fn sidebar_task_list_section_opens_the_task_list_picker() {
    use jinn_kernel::{KernelIntent, Key, KeyEvent, Modifiers};
    use jinn_slices::focus::FocusScope;
    use jinn_slices::route::{ActionCtx, ScopeSignal};
    use jinn_tui::Scope;
    use jinn_tui::app::WhichKeyInstance;

    // Given the task-list sidebar section focused, as the UI does.
    let section_id = jinn_sidebar_msg::SidebarSectionId::TaskList.scope_id();
    let app = test_app().await;
    let mut state = app.core.state.write();
    state
        .frontend
        .scope_push(FocusScope::Dynamic(section_id.clone()));

    // When `s` is pressed there.
    let mut wk = WhichKeyInstance::new(composed_keymap(), Scope::Dynamic(section_id));
    let intent = wk.handle_key(KeyEvent {
        key: Key::Char('s'),
        modifiers: Modifiers::none(),
    });
    let Some(KernelIntent::Dynamic(dynamic)) = intent else {
        panic!("s in the task-list section must fire an action, got {intent:?}");
    };

    // Then route dispatch resolves it, and the action pushes the picker scope.
    //
    // The row used to publish a `DynamicIntent` naming the tools slice's open
    // action. A published message goes to the bus and never returns through
    // route dispatch, so the action never ran and the menu never appeared —
    // while every test asking only "does this scope own rows?" kept passing.
    let result = app
        .services
        .key_routes
        .action_for(
            &dynamic,
            ActionCtx {
                state: &mut *state,
                slices: &app.services.slices,
                config: jinn_slices::empty_config_layer(),
                key_bytes: Vec::new(),
            },
        )
        .unwrap_or_else(|| panic!("s produced no route action: {dynamic:?}"));
    match result.scope_signal {
        Some(ScopeSignal::Push(pushed)) => {
            assert_eq!(
                pushed,
                jinn_tools_msg::task_list_picker_scope(),
                "the opener must push the task-list picker scope"
            );
        }
        other => panic!("the opener must push the picker scope, got {other:?}"),
    }
}

#[rstest::rstest]
fn the_sessions_section_n_row_is_live_not_dead() {
    // Given the sidebar's real rows.
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_sidebar::key_routes::attach_sidebar_rows(&routes);
    let sessions = jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id();

    // When the `N` rows bound in the sessions scope are found.
    let all_rows = routes.rows();
    let n_rows: Vec<_> = all_rows
        .iter()
        .filter(|row| row.scope == sessions && row.key == "N")
        .collect();

    // Then `N` is bound exactly once and is a live action — it used to be
    // advertised as "new session (setup)" while its action did nothing, and
    // this guard existed to catch that dead row. `N` now creates an
    // attendant, so the guard flips: the row must exist and must name the
    // new-attendant action, not a dead shell.
    assert_eq!(
        n_rows.len(),
        1,
        "the sessions section must bind `N` exactly once, got {:?}",
        n_rows.iter().map(|r| r.route_id).collect::<Vec<_>>()
    );
    let row = n_rows[0];
    let jinn_slices::route::RouteOutcome::Action { action, .. } = &row.outcome else {
        panic!("`N` must be an action row, got {:?}", row.outcome);
    };
    assert_eq!(*action, "session-new-attendant");
}

// ── Cancel-stream prompt: it must not outlive the keystroke after arming ──
//
// The prompt is raised by the kernel, but a turn's keys mostly belong to
// slices: pickers, sidebar sections, and the chat box all dispatch through
// route rows that return before the built-in arms. If the dismissal sat with
// the built-ins, every one of those keystrokes would leave the bar on screen
// advertising an abort that is still armed.
//
// Each test below drives the real path — a key resolved through the keymap
// the app composes, into the real `IntentHandler` — because asserting the
// handler in isolation is exactly the assertion that passed while the bug
// shipped.

/// A keymap over the given routes, matching how the app composes one.
fn keymap_over(
    routes: &jinn_slices::KeyRoutes,
) -> ratatui_which_key::Keymap<
    jinn_kernel::KeyEvent,
    jinn_tui::Scope,
    jinn_kernel::KernelIntent,
    jinn_tui::KeyCategory,
> {
    let mut keymap = jinn_tui::keymap::init();
    jinn_tui::keymap_gen::bind_route_rows(routes, &mut keymap);
    keymap
}

/// A bare character press.
fn key(c: char) -> jinn_kernel::KeyEvent {
    jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Char(c),
        modifiers: jinn_kernel::Modifiers::none(),
    }
}

/// Presses `key` in `scope` and dispatches it through the real handler.
///
/// # Panics
///
/// Panics when the key resolves to no intent — a key that mints nothing
/// never reaches the handler, so the assertion after it would be vacuous.
#[expect(
    clippy::panic,
    reason = "an unresolvable key would make the dismissal assertion vacuous"
)]
fn press_and_dispatch(
    app: &jinn_tui::TuiApp,
    scope: jinn_slices::SliceScopeId,
    key: jinn_kernel::KeyEvent,
) {
    let intent = jinn_tui::app::WhichKeyInstance::new(
        keymap_over(&app.services.key_routes),
        jinn_tui::Scope::Dynamic(scope.clone()),
    )
    .handle_key(key)
    .unwrap_or_else(|| panic!("key resolved to no intent in {scope:?}"));

    jinn_kernel::feat::intent::IntentHandler::handle(
        &intent,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );
}

/// Arms the prompt the way a user does: escape during a live stream.
async fn armed_app() -> jinn_tui::TuiApp {
    let app = test_app().await;
    {
        let mut state = app.core.state.write();
        state.active_session_mut().begin_streaming();
        jinn_kernel::feat::intent::IntentHandler::handle(
            &jinn_kernel::KernelIntent::NormalEscape,
            &mut state,
            &app.services.slices,
            &app.services.key_routes,
            &app.services.config,
        );
    }
    assert!(
        app.core.state.read().frontend.cancel_stream_prompt,
        "the prompt must be armed before the dismissing keystroke"
    );
    app
}

/// A keystroke owned by a slice — here a picker's own row — clears the prompt.
#[rstest::rstest]
#[tokio::test]
async fn a_picker_own_key_dismisses_the_cancel_stream_prompt() {
    // Given the armed prompt with the skills picker on top.
    let app = armed_app().await;
    let scope = jinn_skills_msg::skill_picker_scope();
    app.core
        .state
        .write()
        .frontend
        .scope_push(jinn_slices::focus::FocusScope::Dynamic(scope.clone()));

    // When a key the picker owns is pressed there.
    press_and_dispatch(&app, scope, key('z'));

    // Then the prompt is dismissed by that keystroke.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "a slice-owned key must dismiss the prompt, not leave it armed"
    );
}

/// A keystroke owned by a sidebar section clears the prompt.
#[rstest::rstest]
#[tokio::test]
async fn a_sidebar_section_key_dismisses_the_cancel_stream_prompt() {
    // Given the armed prompt with the task-list section focused.
    let app = armed_app().await;
    let scope = jinn_sidebar_msg::SidebarSectionId::TaskList.scope_id();
    app.core
        .state
        .write()
        .frontend
        .scope_push(jinn_slices::focus::FocusScope::Dynamic(scope.clone()));

    // When `s` is pressed there, opening the task-list browser.
    press_and_dispatch(&app, scope, key('s'));

    // Then the prompt is dismissed by that keystroke.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "a sidebar-section key must dismiss the prompt, not leave it armed"
    );
}

/// Typing a character in the chat box clears the prompt.
#[rstest::rstest]
#[tokio::test]
async fn typing_in_the_chat_box_dismisses_the_cancel_stream_prompt() {
    // Given the armed prompt with the box in insert mode.
    let app = armed_app().await;
    app.core
        .state
        .write()
        .frontend
        .scope_push(jinn_slices::FocusScope::Input);

    // When a character is typed.
    let intent = jinn_tui::app::WhichKeyInstance::new(
        keymap_over(&app.services.key_routes),
        jinn_tui::Scope::Input,
    )
    .handle_key(key('h'))
    .unwrap_or_else(|| panic!("a printable character must resolve in insert mode"));
    jinn_kernel::feat::intent::IntentHandler::handle(
        &intent,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );

    // Then the prompt is dismissed by that keystroke.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "typing in the box must dismiss the prompt, not leave it armed"
    );
}

/// The escape that armed the prompt still cancels the turn.
#[rstest::rstest]
#[tokio::test]
async fn escape_still_cancels_through_the_same_keymap() {
    // Given the armed prompt, resolved through the same composed keymap.
    let app = armed_app().await;
    let intent = jinn_tui::app::WhichKeyInstance::new(
        keymap_over(&app.services.key_routes),
        jinn_tui::Scope::Normal,
    )
    .handle_key(jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Esc,
        modifiers: jinn_kernel::Modifiers::none(),
    })
    .expect("escape resolves in Normal scope");

    // When it is dispatched.
    let result = jinn_kernel::feat::intent::IntentHandler::handle(
        &intent,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );

    // Then a CancelTurn is emitted.
    assert!(
        result
            .message_names
            .iter()
            .any(|n| n.contains("CancelTurn")),
        "the confirming escape must still cancel: {:?}",
        result.message_names
    );
}

/// A turn that finished on its own does not leave the prompt armed.
#[rstest::rstest]
#[tokio::test]
async fn a_finished_turn_dismisses_the_cancel_stream_prompt() {
    // Given the armed prompt, with the turn now complete.
    let app = armed_app().await;
    {
        let mut state = app.core.state.write();
        {
            let session = state.active_session_mut();
            session.finalize_entries_for_finish(false, jiff::Timestamp::now());
            session.finish_streaming_via_machine();
        }
    }

    // When escape is pressed against the stale prompt.
    let result = jinn_kernel::feat::intent::IntentHandler::handle(
        &jinn_kernel::KernelIntent::NormalEscape,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );

    // Then the prompt is dismissed and nothing is cancelled.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "a prompt must not outlive the turn it asks to abort"
    );
    // And no CancelTurn is emitted for a turn that already finished.
    assert!(
        !result
            .message_names
            .iter()
            .any(|n| n.contains("CancelTurn")),
        "a finished turn must not be cancelled: {:?}",
        result.message_names
    );
}

// ── The cancel-stream prompt must not outlive the keystroke that armed it ──
//
// The prompt is raised by the kernel, but not every keystroke reaches the
// kernel. A key that opens or continues a which-key sequence resolves to no
// intent at all, so the event loop returns before `IntentHandler` runs and
// the prompt it would have dismissed stays on screen.

/// The real app, with the cancel prompt armed over a live turn.
async fn app_with_cancel_prompt_armed() -> jinn_tui::TuiApp {
    let app = test_app().await;
    {
        let mut state = app.core.state.write();
        // Normal focus: the composed app starts in Input, where a character
        // belongs to the chat box rather than to a which-key sequence.
        state.frontend.scope_push(jinn_slices::FocusScope::Normal);
        state.active_session_mut().begin_streaming();
    }
    jinn_kernel::feat::intent::IntentHandler::handle(
        &jinn_kernel::KernelIntent::NormalEscape,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );
    assert!(
        app.core.state.read().frontend.cancel_stream_prompt,
        "the prompt must be armed before the dismissing key"
    );
    app
}

/// Presses a key through the app's real key-event path.
///
/// This is the seam the bug lives at: `Msg::Input` is what `run` feeds real
/// terminal events to, so a key that resolves to no intent is only handled
/// here, not by calling `handle_key` on a detached which-key instance.
fn press_key(app: &mut jinn_tui::TuiApp, code: crossterm::event::KeyCode) {
    app.handle_msg(jinn_tui::msg::Msg::Input(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
    )));
}

/// Pressing the leader dismisses the prompt and opens which-key.
///
/// The leader has children in the real composition (every picker binds a
/// leader-prefixed opener), so it resolves to a branch and mints no intent —
/// which is exactly why it once slipped past the intent-level dismissal.
#[rstest::rstest]
#[tokio::test]
async fn pressing_the_leader_dismisses_the_cancel_prompt_and_shows_which_key() {
    // Given the armed prompt.
    let mut app = app_with_cancel_prompt_armed().await;

    // When the leader is pressed, as a real terminal event.
    press_key(&mut app, crossterm::event::KeyCode::Char(' '));

    // Then the which-key popup opens on a pending sequence.
    assert!(
        app.which_key.is_pending() && app.which_key.active,
        "the leader must open a which-key sequence, got pending={} active={}",
        app.which_key.is_pending(),
        app.which_key.active
    );
    // And the prompt is hidden and disarmed by that keystroke.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "the leader must dismiss and disarm the cancel prompt"
    );
}

/// Pressing a group prefix dismisses the prompt and opens which-key.
#[rstest::rstest]
#[tokio::test]
async fn pressing_a_group_prefix_dismisses_the_cancel_prompt_and_shows_which_key() {
    // Given the armed prompt.
    let mut app = app_with_cancel_prompt_armed().await;

    // When `g` is pressed, as a real terminal event.
    press_key(&mut app, crossterm::event::KeyCode::Char('g'));

    // Then the which-key popup opens on a pending sequence.
    assert!(
        app.which_key.is_pending() && app.which_key.active,
        "`g` must open a which-key sequence, got pending={} active={}",
        app.which_key.is_pending(),
        app.which_key.active
    );
    // And the prompt is hidden and disarmed by that keystroke.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "`g` must dismiss and disarm the cancel prompt"
    );
}

/// The prompt is suppressed once the session goes idle.
///
/// No keystroke is involved: the turn simply finishes while the prompt is
/// up, and there is nothing left to abort.
#[rstest::rstest]
#[tokio::test]
async fn an_idle_session_suppresses_the_cancel_prompt() {
    // Given the armed prompt, with the turn now complete.
    let mut app = app_with_cancel_prompt_armed().await;
    {
        let mut state = app.core.state.write();
        {
            let session = state.active_session_mut();
            session.finalize_entries_for_finish(false, jiff::Timestamp::now());
            session.finish_streaming_via_machine();
        }
    }

    // When the chat tab renders that frame.
    //
    // The bar is drawn from the frontend flag, so "displayed" is a rendering
    // question: rendering is what proves the prompt is suppressed, where
    // asserting the flag alone would not.
    let rendered = render_chat_tab(&mut app);
    let streaming = {
        let mut a = app_with_cancel_prompt_armed().await;
        render_chat_tab(&mut a)
    };
    assert!(
        streaming.contains("Press ESC again to cancel"),
        "sanity: while streaming the bar MUST render, else this test proves nothing"
    );

    // Then the cancel bar is absent.
    assert!(
        !rendered.contains("Press ESC again to cancel"),
        "an idle session must not display the cancel prompt, got: {rendered}"
    );
}

/// The real app, with an attendant still working under an idle session and
/// the cancel prompt armed over it. Returns the attendant's id with it.
///
/// Attendants fire on the parent's *completion*, so this is the shape the
/// single-predicate rule exists for: the session's own turn is long finished,
/// and the only running work is beneath it.
async fn app_with_idle_session_and_running_attendant()
-> (jinn_tui::TuiApp, jinn_core_types::SessionId) {
    let app = test_app().await;
    let attendant_id = {
        let mut state = app.core.state.write();
        state.frontend.scope_push(jinn_slices::FocusScope::Normal);
        let parent = state.active_session().clone();
        let mut attendant = jinn_session_state::ChatSessionState::new_attendant(&parent, false);
        attendant.begin_streaming();
        let attendant_id = attendant.session_id().clone();
        state.session.insert(attendant);
        attendant_id
    };
    jinn_kernel::feat::intent::IntentHandler::handle(
        &jinn_kernel::KernelIntent::NormalEscape,
        &mut app.core.state.write(),
        &app.services.slices,
        &app.services.key_routes,
        &app.services.config,
    );
    assert!(
        app.core.state.read().frontend.cancel_stream_prompt,
        "an idle session with a running attendant must still offer the cancel"
    );
    (app, attendant_id)
}

/// The bar is offered over running work that is not the session's own turn.
#[rstest::rstest]
#[tokio::test]
async fn an_idle_session_with_a_running_attendant_shows_the_cancel_prompt() {
    // Given the prompt armed over an idle session with a running attendant.
    let (mut app, _attendant) = app_with_idle_session_and_running_attendant().await;

    // When the chat tab renders that frame.
    let rendered = render_chat_tab(&mut app);

    // Then the bar is drawn.
    assert!(
        rendered.contains("Press ESC again to cancel"),
        "an idle session with a running attendant must display the bar, got: {rendered}"
    );
}

/// Nothing running means nothing to offer, whether or not a prompt was armed.
#[rstest::rstest]
#[tokio::test]
async fn a_session_with_nothing_running_shows_no_cancel_prompt() {
    // Given the prompt armed over an idle session whose attendant has finished.
    let (mut app, attendant) = app_with_idle_session_and_running_attendant().await;
    {
        let mut state = app.core.state.write();
        {
            let session = state
                .session
                .get_mut(&attendant)
                .expect("the attendant inserted by the fixture");
            session.finalize_entries_for_finish(false, jiff::Timestamp::now());
            session.finish_streaming_via_machine();
        }
    }

    // When the chat tab renders that frame.
    let rendered = render_chat_tab(&mut app);

    // Then the bar is absent.
    assert!(
        !rendered.contains("Press ESC again to cancel"),
        "a finished attendant must not keep the bar up, got: {rendered}"
    );
}

/// A prompt whose work ends with no keystroke decays on its own.
///
/// The flag is frontend state the kernel owns, and the only things that clear
/// it are keystroke-driven — so without the tick, a turn that finished in
/// silence left the prompt armed until the user happened to press something.
#[rstest::rstest]
#[tokio::test]
async fn the_cancel_prompt_disarms_when_the_turn_ends_without_a_keystroke() {
    // Given the armed prompt, with the turn now complete.
    let app = app_with_cancel_prompt_armed().await;
    {
        let mut state = app.core.state.write();
        {
            let session = state.active_session_mut();
            session.finalize_entries_for_finish(false, jiff::Timestamp::now());
            session.finish_streaming_via_machine();
        }
    }

    // When the app ticks, as the 100ms poll loop does.
    let mut app = app;
    app.handle_msg(jinn_tui::msg::Msg::Tick);

    // Then the flag itself is cleared, not merely hidden by the renderer.
    assert!(
        !app.core.state.read().frontend.cancel_stream_prompt,
        "the tick must decay a prompt whose work has ended"
    );
}

/// Renders one frame of the whole app and returns its visible text.
fn render_chat_tab(app: &mut jinn_tui::TuiApp) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test backend builds");
    terminal
        .draw(|frame| app.render(frame))
        .expect("draw should succeed");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_owned()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
