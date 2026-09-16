#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_lifetimes,
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
#[case::normal_chat(jinn_domain::FocusScope::Normal, Scope::Normal)]
#[case::sidebar(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope(), Scope::Dynamic(jinn_slices::SliceScopeId::navigation("sidebar", "persona")))]
#[case::input(jinn_domain::FocusScope::Input, Scope::Input)]
#[case::picker_provider(jinn_domain::FocusScope::Picker { kind: jinn_domain::PickerKind::Provider }, Scope::PickerProvider)]
#[case::picker_task_list(jinn_domain::FocusScope::Picker { kind: jinn_domain::PickerKind::TaskList }, Scope::PickerTaskList)]
fn scope_for_focus_maps_correctly(#[case] focus: jinn_domain::FocusScope, #[case] expected: Scope) {
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

/// A keymap with the sidebar's route rows bound (as launch.rs does).
fn keymap_with_routes_at(scope: Scope) -> WhichKeyInstance {
    let mut km = keymap::init();
    let routes = jinn_slices::route::KeyRoutes::new();
    jinn_sidebar::key_routes::attach_sidebar_rows(&routes);
    crate::keymap_gen::bind_route_rows(&routes, &mut km);
    WhichKeyInstance::new(km, scope)
}

fn key<'a>(notation: &'a str) -> jinn_domain::KeyEvent {
    jinn_domain::KeyEvent::parse_notation(notation).expect("notation should parse")
}

#[rstest::rstest]
#[case::normal(Scope::Normal)]
#[case::input(Scope::Input)]
#[case::sidebar_sessions(Scope::Dynamic(jinn_slices::SliceScopeId::navigation(
    "sidebar", "sessions"
)))]
#[case::picker_session(Scope::PickerSession)]
fn s_outside_sidebar_task_list_does_not_open_task_list_picker(#[case] scope: Scope) {
    // Given the keymap rooted at a non-sidebar-task-list scope.
    let mut wk = keymap_at(scope);

    // When pressing `s`.
    let intent = wk.handle_key(key("s"));

    // Then it does NOT resolve to the TaskList open intent. It may resolve to
    // some other intent (e.g. Input's catch-all `InsertChar('s')`) or None,
    // but never to "search task list".
    assert_ne!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("search task list")
    );
}

#[rstest::rstest]
fn esc_in_picker_task_list_returns_to_normal_mode() {
    // Given the keymap rooted at PickerTaskList.
    let mut wk = keymap_at(Scope::PickerTaskList);

    // When pressing `<esc>`.
    let intent = wk.handle_key(key("escape"));

    // Then it resolves to EnterNormalMode (the existing handler closes the picker
    // and restores the prior sidebar task-list scope).
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("enter normal mode")
    );
}

#[rstest::rstest]
#[test]
fn alt_q_in_input_scope_toggles_input_mode() {
    // Given the keymap rooted at Input scope.
    let mut wk = keymap_at(Scope::Input);

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
    // Given the keymap rooted at Normal scope.
    let mut wk = keymap_at(Scope::Normal);

    // When pressing `r`.
    let intent = wk.handle_key(key("r"));

    // Then it resolves to ChatEntryResetSelected.
    assert_eq!(
        intent.map(|i| i.to_string()).as_deref(),
        Some("reset entry to default context")
    );
}
