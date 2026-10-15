#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_lifetimes,
    clippy::panic,
    reason = "test file, panics are acceptable"
)]
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::TuiApp;
use crate::app::{WhichKeyInstance, scope_for_focus};
use crate::config::TuiConfig;
use crate::keymap;
use crate::msg::Msg;
use crate::scope::Scope;
use crate::selection::SelectionState;

/// Creates a minimal `TuiApp` for testing.
async fn test_app() -> TuiApp {
    TuiApp::test_builder().build().await
}

#[rstest::rstest]
#[case::normal_chat(jinn_slices::FocusScope::Normal, Scope::Normal)]
#[case::sidebar(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope(), Scope::Dynamic(jinn_slices::SliceScopeId::navigation("sidebar", "persona")))]
#[case::input(jinn_slices::FocusScope::Input, Scope::Input)]
fn scope_for_focus_maps_correctly(#[case] focus: jinn_slices::FocusScope, #[case] expected: Scope) {
    // Given a focus scope.
    // When mapping to a keymap scope.
    // Then the expected scope is returned.
    assert_eq!(scope_for_focus(&focus), expected);
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_down_left_in_selectable_rect_starts_dragging() {
    // Given an app with a registered selectable rect.
    let mut app = test_app().await;
    let rect = Rect::new(5, 5, 20, 10);
    app.selectable_rects.rebuild(vec![rect]);

    // When sending a left-click inside the rect.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 8,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection is Dragging with anchor at (10, 8).
    assert_eq!(
        app.selection,
        SelectionState::Dragging {
            anchor: (10, 8),
            focus: (10, 8),
            bounds: rect,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_down_left_outside_selectable_rect_does_not_start_dragging() {
    // Given an app with a registered selectable rect.
    let mut app = test_app().await;
    app.selectable_rects.rebuild(vec![Rect::new(5, 5, 10, 10)]);

    // When sending a left-click outside the rect.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 30,
        row: 30,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection remains Idle.
    assert_eq!(app.selection, SelectionState::Idle);
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_drag_updates_focus_while_dragging() {
    // Given an app with an active drag.
    let mut app = test_app().await;
    let rect = Rect::new(0, 0, 40, 24);
    app.selectable_rects.rebuild(vec![rect]);
    app.selection = SelectionState::start_drag(5, 5, rect);

    // When sending a drag event.
    let mouse = MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 15,
        row: 10,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the focus is updated to (15, 10).
    assert_eq!(
        app.selection,
        SelectionState::Dragging {
            anchor: (5, 5),
            focus: (15, 10),
            bounds: rect,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_up_left_finalizes_selection() {
    // Given an app with an active drag.
    let mut app = test_app().await;
    let rect = Rect::new(0, 0, 40, 24);
    app.selection = SelectionState::start_drag(2, 3, rect).update_focus(10, 12);

    // When sending a mouse-up event.
    let mouse = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: 10,
        row: 12,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection is Active with the same anchor and focus.
    assert_eq!(
        app.selection,
        SelectionState::Active {
            anchor: (2, 3),
            focus: (10, 12),
            bounds: rect,
        }
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_down_right_cancels_selection() {
    // Given an app with an active selection.
    let mut app = test_app().await;
    let rect = Rect::new(0, 0, 40, 24);
    app.selection = SelectionState::start_drag(5, 5, rect);

    // When sending a right-click.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: 5,
        row: 5,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection is cancelled to Idle.
    assert_eq!(app.selection, SelectionState::Idle);
}

#[rstest::rstest]
#[tokio::test]
async fn scroll_events_still_route_to_keymap() {
    // Given an app in Normal scope.
    let mut app = test_app().await;
    let initial_selection = app.selection.clone();

    // When sending a scroll-up mouse event.
    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 10,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection is unchanged (event fell through to keymap).
    assert_eq!(app.selection, initial_selection);
}

#[rstest::rstest]
#[tokio::test]
async fn mouse_events_not_handled_when_mouse_selection_disabled() {
    // Given an app with mouse selection disabled and a registered selectable rect.
    let mut app = test_app().await;
    app.config = TuiConfig::new(false);
    let rect = Rect::new(5, 5, 20, 10);
    app.selectable_rects.rebuild(vec![rect]);

    // When sending a left-click inside the rect.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 8,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    app.handle_msg(Msg::Input(crossterm::event::Event::Mouse(mouse)));

    // Then the selection remains Idle (event was not handled).
    assert_eq!(app.selection, SelectionState::Idle);
}

// -----------------------------------------------------------------------------
// Keymap tests for the task-list zoom picker (sidebar `s` binding)
// -----------------------------------------------------------------------------
//
// These tests use a bare `WhichKeyInstance` rather than a full `TuiApp` because
// they only verify that the keymap resolves the right `Intent`. They don't need
// the actor host, sidebar, or selection state.

fn keymap_at(scope: Scope) -> WhichKeyInstance {
    WhichKeyInstance::new(keymap::init(), scope)
}

/// A keymap with the chat input box's route rows bound (as launch.rs does).
fn keymap_with_chat_input_rows_at(scope: Scope) -> WhichKeyInstance {
    let mut km = keymap::init();
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_chat_input::routes::attach_chat_input_rows(&routes);
    crate::keymap_gen::bind_route_rows(&routes, &mut km);
    WhichKeyInstance::new(km, scope)
}

/// A keymap with the chat input box's rows AND key hook bound, as the real
/// composition does — rows for the explicit keys, the hook for characters
/// and cursor motion.
fn keymap_with_chat_input_at(scope: Scope) -> WhichKeyInstance {
    let mut km = keymap::init();
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_chat_input::routes::attach_all(&routes);
    crate::keymap_gen::bind_route_rows(&routes, &mut km);
    WhichKeyInstance::new(km, scope)
}

/// A keymap with the chat log's route rows bound (as launch.rs does).
fn keymap_with_chat_log_at(scope: Scope) -> WhichKeyInstance {
    let mut km = keymap::init();
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_chat_log_view::routes::attach_all(&routes);
    crate::keymap_gen::bind_route_rows(&routes, &mut km);
    WhichKeyInstance::new(km, scope)
}

/// A keymap with the sidebar's route rows bound (as launch.rs does).
fn keymap_with_routes_at(scope: Scope) -> WhichKeyInstance {
    let mut km = keymap::init();
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_sidebar::key_routes::attach_sidebar_rows(&routes);
    crate::keymap_gen::bind_route_rows(&routes, &mut km);
    WhichKeyInstance::new(km, scope)
}

fn key<'a>(notation: &'a str) -> jinn_kernel::KeyEvent {
    jinn_kernel::KeyEvent::parse_notation(notation).expect("notation should parse")
}

#[rstest::rstest]
fn raw_shift_enter_binding_parses_and_resolves() {
    // Given a bare keymap holding one shifted binding in the Input scope.
    let mut km: ratatui_which_key::Keymap<
        jinn_kernel::KeyEvent,
        Scope,
        jinn_kernel::protocol::KernelIntent,
        crate::keymap::KeyCategory,
    > = ratatui_which_key::Keymap::new();
    km.bind(
        "<s-enter>",
        jinn_kernel::protocol::KernelIntent::Quit,
        crate::keymap::KeyCategory::General,
        Scope::Input,
    );
    let mut wk = WhichKeyInstance::new(km, Scope::Input);

    // When pressing Shift+Enter exactly as the notation parses it
    // (bare form — parse_notation takes no angle brackets).
    let parsed = key("s-enter");

    // And the parse is Enter with shift and nothing else.
    assert!(parsed.modifiers.shift);
    assert!(!parsed.modifiers.ctrl);
    assert!(!parsed.modifiers.alt);

    // Then the raw binding resolves without any spread in the way.
    let intent = wk.handle_key(parsed);
    assert!(intent.is_some(), "raw <s-enter> binding must resolve");
}

#[rstest::rstest]
fn shift_enter_in_the_input_scope_resolves_to_insert_newline_shift() {
    // Given the keymap with the box's rows and hook bound, in the Input
    // scope the box resolves keys in.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing Shift+Enter — the box's explicit newline chord.
    let intent = wk.handle_key(jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Enter,
        modifiers: jinn_kernel::Modifiers::shift(),
    });

    // Then it resolves to the box's insert-newline-shift action.
    let jinn_kernel::protocol::KernelIntent::Dynamic(intent) =
        intent.expect("Shift+Enter must resolve")
    else {
        panic!("expected a dynamic intent");
    };
    assert_eq!(intent.action, "insert-newline-shift");
}

#[rstest::rstest]
fn ctrl_enter_in_the_input_scope_resolves_to_insert_newline_ctrl() {
    // Given the keymap with the box's rows and hook bound, in the Input
    // scope the box resolves keys in.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing Ctrl+Enter — the box's other newline chord.
    let intent = wk.handle_key(jinn_kernel::KeyEvent {
        key: jinn_kernel::Key::Enter,
        modifiers: jinn_kernel::Modifiers::ctrl(),
    });

    // Then it resolves to the box's insert-newline-ctrl action.
    let jinn_kernel::protocol::KernelIntent::Dynamic(intent) =
        intent.expect("Ctrl+Enter must resolve")
    else {
        panic!("expected a dynamic intent");
    };
    assert_eq!(intent.action, "insert-newline-ctrl");
}

#[rstest::rstest]
fn shift_arrow_in_the_input_scope_resolves_to_cursor_motion() {
    // Given the keymap with the box's rows and hook bound, in the Input
    // scope the box resolves keys in.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing Shift+Left through the production dispatch helper.
    let intent = crate::keymap_gen::handle_key_with_shift_fallback(
        &mut wk,
        jinn_kernel::KeyEvent {
            key: jinn_kernel::Key::Left,
            modifiers: jinn_kernel::Modifiers::shift(),
        },
    );

    // Then it resolves to the same cursor motion as plain Left — a
    // shifted variant of a bound key must not be swallowed.
    assert!(intent.is_some(), "Shift+Left must not be swallowed");
}

#[rstest::rstest]
fn shift_char_in_the_input_scope_resolves_to_insert_char() {
    // Given the keymap with the box's rows and hook bound, in the Input
    // scope the box resolves keys in.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When a printable char arrives with shift set — the form terminals
    // that do not pre-shift the glyph report — through the production
    // dispatch helper.
    let intent = crate::keymap_gen::handle_key_with_shift_fallback(
        &mut wk,
        jinn_kernel::KeyEvent {
            key: jinn_kernel::Key::Char('G'),
            modifiers: jinn_kernel::Modifiers::shift(),
        },
    );

    // Then it resolves to the box's insert-char intent carrying the
    // glyph — shift alone is not a chord the box declines.
    let jinn_kernel::protocol::KernelIntent::Dynamic(intent) =
        intent.expect("shifted char must insert")
    else {
        panic!("expected a dynamic insert-char intent");
    };
    assert_eq!(intent.action, "insert-char");
}

#[rstest::rstest]
#[case::normal(Scope::Normal)]
#[case::input(Scope::Input)]
#[case::sidebar_sessions(Scope::Dynamic(jinn_slices::SliceScopeId::navigation(
    "sidebar", "sessions"
)))]
fn s_outside_sidebar_task_list_does_not_open_task_list_picker(#[case] scope: Scope) {
    // Given the keymap rooted at a non-sidebar-task-list scope.
    let mut wk = keymap_at(scope);

    // When pressing `s`.
    let intent = wk.handle_key(key("s"));

    // Then it does NOT resolve to the TaskList open intent. It may resolve to
    // some other intent (e.g. Input's catch-all `InsertChar('s')`) or None,
    // but never to "search task list".
    // The browser is opened only from the sidebar's task-list section. Since
    // the picker is slice-owned it has no kernel intent name, so the check is
    // that `s` did not become a dynamic route into the task-list scope.
    let opened_the_task_list = matches!(
        intent,
        Some(jinn_kernel::KernelIntent::Dynamic(dynamic))
            if dynamic.slice == jinn_tools_msg::task_list_picker_scope()
    );
    assert!(
        !opened_the_task_list,
        "`s` outside the sidebar's task-list section must not open the browser"
    );
}

#[rstest::rstest]
#[test]
fn alt_q_in_input_scope_toggles_input_mode() {
    // Given the keymap (with the box's route rows) rooted at Input scope.
    let mut wk = keymap_with_chat_input_rows_at(Scope::Input);

    // When pressing Alt+q (notation: `m-q`).
    let intent = wk.handle_key(key("m-q"));

    // Then it resolves to ToggleInputMode (Queue ↔ Steer).
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("toggle input mode")
    );
}

#[rstest::rstest]
#[test]
fn alt_s_in_input_scope_focuses_sidebar_sessions() {
    // Given the keymap (with sidebar route rows) rooted at Input scope.
    let mut wk = keymap_with_routes_at(Scope::Input);

    // When pressing Alt+s (notation: `m-s`).
    let intent = wk.handle_key(key("m-s"));

    // Then it resolves to SidebarFocusSessions (now bound in Input scope too).
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("focus session list")
    );
}

#[rstest::rstest]
#[test]
fn alt_s_in_normal_scope_focuses_sidebar_sessions() {
    // Given the keymap (with sidebar route rows) rooted at Normal scope.
    let mut wk = keymap_with_routes_at(Scope::Normal);

    // When pressing Alt+s (notation: `m-s`).
    let intent = wk.handle_key(key("m-s"));

    // Then it resolves to SidebarFocusSessions (scope-aware binding).
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("focus session list")
    );
}

#[rstest::rstest]
#[test]
fn r_in_normal_scope_resets_entry_to_default_context() {
    // Given a keymap with the chat log's route rows bound, at Normal scope.
    let mut wk = keymap_with_chat_log_at(Scope::Normal);

    // When pressing `r`.
    let intent = wk.handle_key(key("r"));

    // Then it resolves to the log's reset action.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("reset entry to default context")
    );
}

#[rstest::rstest]
#[test]
fn enter_in_input_scope_submits_through_the_slice_row() {
    // Given the keymap with the box's rows and hook bound, at Input scope.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing Enter.
    let intent = wk.handle_key(key("enter"));

    // Then it resolves to the box's submit action, not a kernel intent.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("send this message")
    );
}

#[rstest::rstest]
#[test]
fn printable_char_in_input_scope_reaches_the_slice_insert_action() {
    // Given the keymap with the box's rows and hook bound, at Input scope.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing `a`.
    let intent = wk.handle_key(key("a"));

    // Then it resolves to the box's character insertion, not a kernel
    // `insert-char` intent.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("type a character")
    );
}

#[rstest::rstest]
#[test]
fn ctrl_c_in_input_scope_still_reaches_its_kernel_bind() {
    // Given the keymap with the box's hook bound, at Input scope.
    let mut wk = keymap_with_chat_input_at(Scope::Input);

    // When pressing Ctrl+c.
    let intent = wk.handle_key(key("c-c"));

    // Then it resolves to a kernel intent, not one of the box's actions —
    // the box's key hook declines it rather than swallowing it.
    let resolved = intent.map(|i| i.to_string()).expect("ctrl-c is bound");
    assert!(
        !resolved.contains("type a character"),
        "ctrl-c must not be captured by the box, got {resolved}"
    );
}

/// `i` in Normal scope enters the chat input box.
///
/// This is the box's only door from chat history into insert mode, so its
/// absence is invisible until someone tries to type and nothing happens.
/// Asserting the key directly — not "whatever rows got attached resolve" —
/// is deliberate: a test that walks the attached rows cannot see a row that
/// was never attached.
#[rstest::rstest]
#[test]
fn i_in_normal_scope_enters_insert_mode() {
    // Given the keymap with the box's rows and hook bound, at Normal scope.
    let mut wk = keymap_with_chat_input_at(Scope::Normal);

    // When pressing `i`.
    let intent = wk.handle_key(key("i"));

    // Then it resolves to the box's insert-mode action.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("type a message")
    );
}

/// `<c-j>` in Normal scope enters the box, the alternate door.
#[rstest::rstest]
#[test]
fn ctrl_j_in_normal_scope_enters_insert_mode() {
    // Given the keymap with the box's rows and hook bound, at Normal scope.
    let mut wk = keymap_with_chat_input_at(Scope::Normal);

    // When pressing Ctrl+j.
    let intent = wk.handle_key(key("c-j"));

    // Then it resolves to the box's insert-mode action.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("type a message")
    );
}
