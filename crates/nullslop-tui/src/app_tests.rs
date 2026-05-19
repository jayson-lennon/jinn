#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test file, panics are acceptable"
)]

use std::sync::Arc;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use nullslop_domain::feat::ui::sidebar::Sidebar;
use nullslop_domain::{
    ActorHostService, AppCore, AppState, AppUiRegistry, FakeActorHost, Intent, PickerKind,
    Services, State,
};
use ratatui::layout::Rect;

use crate::app::{WhichKeyInstance, scope_for_focus};
use crate::config::TuiConfig;
use crate::keymap;
use crate::msg::Msg;
use crate::scope::Scope;
use crate::selection::{SelectableRects, SelectionState};
use crate::{AppStatus, MsgHandler, TuiApp};

/// Creates a minimal `TuiApp` for testing.
fn test_app() -> TuiApp {
    let services = Services::new();
    let (sender, _receiver) = kanal::unbounded();
    let core = AppCore {
        state: State::new(AppState::default()),
        sender,
    };
    let fake_host = ActorHostService::new(Arc::new(FakeActorHost::new()));
    let mut ui_registry = AppUiRegistry::new();
    nullslop_domain::register_all_ui_elements(&mut ui_registry);
    nullslop_domain::feat::ui::status_bar::register(&mut ui_registry);
    TuiApp {
        core,
        services,
        actor_host: fake_host,
        ui_registry,
        events: MsgHandler::new(),
        which_key: WhichKeyInstance::new(keymap::init(), Scope::Normal),
        suspend: crate::suspend::Suspend::new(),
        event_thread: None,
        status: AppStatus::Starting,
        selection: SelectionState::Idle,
        selectable_rects: SelectableRects::default(),
        pending_clipboard: false,
        config: TuiConfig::default(),
        sidebar: {
            let mut s = Sidebar::new();
            nullslop_domain::feat::ui::sidebar::register_sections(&mut s);
            s
        },
    }
}

#[rstest::rstest]
#[case::normal_chat(nullslop_domain::FocusScope::Normal, Scope::Normal)]
#[case::sidebar(nullslop_domain::FocusScope::SidebarPersona, Scope::SidebarPersona)]
#[case::input(nullslop_domain::FocusScope::Input, Scope::Input)]
#[case::picker(nullslop_domain::FocusScope::Picker { kind: nullslop_domain::PickerKind::Provider }, Scope::Picker)]
#[case::sidebar_resize(nullslop_domain::FocusScope::SidebarResize, Scope::SidebarResize)]
fn scope_for_focus_maps_correctly(
    #[case] focus: nullslop_domain::FocusScope,
    #[case] expected: Scope,
) {
    // Given a focus scope.
    // When mapping to a keymap scope.
    // Then the expected scope is returned.
    assert_eq!(scope_for_focus(&focus), expected);
}

#[rstest::rstest]
fn mouse_down_left_in_selectable_rect_starts_dragging() {
    // Given an app with a registered selectable rect.
    let mut app = test_app();
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
fn mouse_down_left_outside_selectable_rect_does_not_start_dragging() {
    // Given an app with a registered selectable rect.
    let mut app = test_app();
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
fn mouse_drag_updates_focus_while_dragging() {
    // Given an app with an active drag.
    let mut app = test_app();
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
fn mouse_up_left_finalizes_selection() {
    // Given an app with an active drag.
    let mut app = test_app();
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
fn mouse_down_right_cancels_selection() {
    // Given an app with an active selection.
    let mut app = test_app();
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
fn scroll_events_still_route_to_keymap() {
    // Given an app in Normal scope.
    let mut app = test_app();
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
fn mouse_events_not_handled_when_mouse_selection_disabled() {
    // Given an app with mouse selection disabled and a registered selectable rect.
    let services = Services::new();
    let (sender, _receiver) = kanal::unbounded();
    let core = AppCore {
        state: State::new(AppState::default()),
        sender,
    };
    let fake_host = ActorHostService::new(Arc::new(FakeActorHost::new()));
    let mut ui_registry = AppUiRegistry::new();
    nullslop_domain::register_all_ui_elements(&mut ui_registry);
    nullslop_domain::feat::ui::status_bar::register(&mut ui_registry);
    let mut app = TuiApp {
        core,
        services,
        actor_host: fake_host,
        ui_registry,
        events: MsgHandler::new(),
        which_key: WhichKeyInstance::new(keymap::init(), Scope::Normal),
        suspend: crate::suspend::Suspend::new(),
        event_thread: None,
        status: AppStatus::Starting,
        selection: SelectionState::Idle,
        selectable_rects: SelectableRects::default(),
        pending_clipboard: false,
        config: TuiConfig::new(false),
        sidebar: {
            let mut s = Sidebar::new();
            nullslop_domain::feat::ui::sidebar::register_sections(&mut s);
            s
        },
    };
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

// --- Keymap scope toggle tests ---

#[rstest::rstest]
fn toggle_scope_filter_to_show_all_includes_multiple_scopes() {
    // Given an app in Normal scope with keymap picker entries.
    let mut app = test_app();
    app.which_key.set_scope(Scope::Normal);

    app.route_intent(Intent::OpenPicker {
        kind: PickerKind::Keymap,
    });

    // When toggling the scope filter (false -> true).
    app.route_intent(Intent::ToggleKeymapScopeFilter);

    // Then show_all is true and entries include multiple scopes.
    {
        let state = app.core.state.read();
        assert!(
            state.frontend.keymap_picker_show_all,
            "should be true after toggle"
        );
        let all_entries = state.frontend.keymap_picker.items();
        assert!(!all_entries.is_empty(), "should have entries");
        let scopes: std::collections::HashSet<&str> =
            all_entries.iter().map(|e| e.scope.as_str()).collect();
        assert!(
            scopes.len() > 1,
            "all scopes should include multiple scopes, got: {scopes:?}"
        );
    }
}

#[rstest::rstest]
fn toggle_scope_filter_back_to_false_limits_to_normal_scope() {
    // Given an app in Normal scope with keymap picker entries.
    let mut app = test_app();
    app.which_key.set_scope(Scope::Normal);

    app.route_intent(Intent::OpenPicker {
        kind: PickerKind::Keymap,
    });

    // When toggling twice (false -> true -> false).
    app.route_intent(Intent::ToggleKeymapScopeFilter);
    app.route_intent(Intent::ToggleKeymapScopeFilter);

    // Then show_all is false and entries are Normal-scope only (the origin scope).
    {
        let state = app.core.state.read();
        assert!(
            !state.frontend.keymap_picker_show_all,
            "should be false after second toggle"
        );
        let scope_entries = state.frontend.keymap_picker.items();
        assert!(!scope_entries.is_empty(), "should have Normal entries");
        for entry in scope_entries {
            assert_eq!(
                entry.scope, "Normal",
                "all entries should be Normal scope (origin), got: {}",
                entry.scope
            );
        }
    }
}

#[rstest::rstest]
fn toggle_keymap_scope_filter_preserves_filter_text() {
    // Given an app with keymap picker open and filter text entered.
    let mut app = test_app();
    app.which_key.set_scope(Scope::Normal);

    // Populate initial entries (stores origin scope).
    app.route_intent(Intent::OpenPicker {
        kind: PickerKind::Keymap,
    });

    // Insert filter text.
    {
        let mut state = app.core.state.write();
        state.frontend.keymap_picker.insert_char('q');
    }

    // When toggling the scope filter.
    app.route_intent(Intent::ToggleKeymapScopeFilter);

    // Then the filter text is preserved.
    let state = app.core.state.read();
    assert_eq!(
        state.frontend.keymap_picker.filter(),
        "q",
        "filter text should be preserved after toggle"
    );
}
