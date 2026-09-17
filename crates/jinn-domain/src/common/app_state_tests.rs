#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::common::app_state::*;
use crate::protocol::{ChatEntry, Mode, PickerKind, SessionId};

#[rstest::rstest]
fn push_entry_adds_to_history() {
    // Given a new AppState.
    let mut data = AppState::default_with_scope_focus();
    let entry = ChatEntry::user("hello");

    // When pushing an entry via the active session.
    let index = data.active_session_mut().push_entry(entry);

    // Then the index is 0 and history has one entry.
    assert_eq!(index, 0);
    assert_eq!(data.active_session().history().len(), 1);
}

#[rstest::rstest]
fn default_creates_normal_base() {
    // Given a default ScopeStack.
    let stack = ScopeStack::default();

    // Then the current scope is Normal.
    assert_eq!(stack.current(), &FocusScope::Normal);
}

#[rstest::rstest]
fn push_and_pop_round_trip() {
    // Given a default ScopeStack.
    let mut stack = ScopeStack::default();

    // When pushing Input.
    stack.push(FocusScope::Input);

    // Then current is Input.
    assert_eq!(stack.current(), &FocusScope::Input);

    // When popping.
    let popped = stack.pop();

    // Then we get Input back and current is Normal.
    assert_eq!(popped, Some(FocusScope::Input));
    assert_eq!(stack.current(), &FocusScope::Normal);
}

#[rstest::rstest]
fn pop_on_base_returns_none() {
    // Given a default ScopeStack (only base).
    let mut stack = ScopeStack::default();

    // When popping the base.
    let popped = stack.pop();

    // Then nothing is returned.
    assert!(popped.is_none());
    // And the base scope remains.
    assert_eq!(stack.current(), &FocusScope::Normal);
}

#[rstest::rstest]
fn parent_returns_none_on_base() {
    // Given a default ScopeStack (only base).
    let stack = ScopeStack::default();

    // Then parent is None.
    assert!(stack.parent().is_none());
}

#[rstest::rstest]
fn parent_returns_previous_after_push() {
    // Given a ScopeStack with Input pushed.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Input);

    // Then parent is Normal.
    assert_eq!(stack.parent(), Some(&FocusScope::Normal));
}

#[rstest::rstest]
fn clear_overlays_returns_to_base() {
    // Given a ScopeStack with multiple overlays.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Input);
    stack.push(FocusScope::Picker {
        kind: PickerKind::Provider,
    });

    // When clearing overlays.
    stack.clear_overlays();

    // Then current is Normal.
    assert_eq!(stack.current(), &FocusScope::Normal);
    assert_eq!(stack.len(), 1);
}

#[rstest::rstest]
fn is_picker_returns_true_when_picker_active() {
    // Given a ScopeStack with Picker on top.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Picker {
        kind: PickerKind::Session,
    });

    // Then is_picker is true.
    assert!(stack.is_picker());
}

#[rstest::rstest]
fn is_picker_returns_false_when_input_active() {
    // Given a ScopeStack with Input on top.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Input);

    // Then is_picker is false.
    assert!(!stack.is_picker());
}

#[rstest::rstest]
fn picker_kind_returns_kind_when_picker_active() {
    // Given a ScopeStack with Picker(Provider) on top.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Picker {
        kind: PickerKind::Provider,
    });

    // Then picker_kind returns Provider.
    assert_eq!(stack.picker_kind(), Some(&PickerKind::Provider));
}

#[rstest::rstest]
fn picker_kind_returns_none_when_not_picker() {
    // Given a default ScopeStack.
    let stack = ScopeStack::default();

    // Then picker_kind is None.
    assert!(stack.picker_kind().is_none());
}

#[rstest::rstest]
fn is_sidebar_returns_true_when_sidebar_active() {
    // Given a ScopeStack with the persona section scope on top.
    let mut stack = ScopeStack::default();
    stack.push(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope());

    // Then is_sidebar is true.
    assert!(stack.is_sidebar());
}

#[rstest::rstest]
fn is_sidebar_returns_false_when_normal() {
    // Given a default ScopeStack.
    let stack = ScopeStack::default();

    // Then is_sidebar is false.
    assert!(!stack.is_sidebar());
}

#[rstest::rstest]
#[case(FocusScope::Normal, Mode::Normal)]
#[case(FocusScope::Input, Mode::Input)]
#[case(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope(), Mode::Normal)]
// Capturing slice scopes (quake bar, popups) light up input UI.
#[case(
    FocusScope::Dynamic(jinn_slices::SliceScopeId::new("quake-bar", "bar")),
    Mode::Input
)]
#[case(FocusScope::Dynamic(jinn_term_msg::view_scope()), Mode::Normal)]
// Capture mode routes keystrokes to the pty, so it must not count as
// input mode (which would light up the chat input as focused).
#[case(FocusScope::Dynamic(jinn_term_msg::control_scope()), Mode::Normal)]
#[case(FocusScope::Picker { kind: PickerKind::Provider }, Mode::Picker)]
fn focus_scope_mode_mapping(#[case] scope: FocusScope, #[case] expected: Mode) {
    // Given a FocusScope variant.
    // When calling mode().
    // Then it returns the expected Mode.
    assert_eq!(scope.mode(), expected);
}

#[rstest::rstest]
#[case(FocusScope::Normal, "Normal")]
#[case(FocusScope::Input, "Input")]
#[case(jinn_sidebar_msg::SidebarSectionId::Persona.focus_scope(), "Dynamic(sidebar:persona)")]
#[case(FocusScope::Picker { kind: PickerKind::Provider }, "Picker(models)")]
fn focus_scope_display(#[case] scope: FocusScope, #[case] expected: &str) {
    // Given a FocusScope variant.
    // When formatting as Display.
    // Then it produces the expected string.
    assert_eq!(scope.to_string(), expected);
}

#[rstest::rstest]
fn session_mut_or_create_sets_cwd_from_default_cwd() {
    // Given an AppState with a custom default CWD.
    let mut state = AppState::default_with_scope_focus();
    state
        .session
        .set_default_cwd(std::path::PathBuf::from("/custom/cwd"));

    let session_id = SessionId::new();

    // When creating a session via session_mut_or_create.
    let session = state.session_mut_or_create(&session_id);

    // Then the session's CWD is the default CWD.
    assert_eq!(session.cwd(), std::path::Path::new("/custom/cwd"));
}

#[rstest::rstest]
fn is_empty_returns_false_after_construction() {
    // Given a default ScopeStack.
    let stack = ScopeStack::default();

    // When checking emptiness.
    // Then it returns false (invariant: always has a base scope).
    assert!(!stack.is_empty());
}

#[rstest::rstest]
fn len_returns_one_after_construction() {
    // Given a default ScopeStack.
    let stack = ScopeStack::default();

    // When checking length.
    // Then it returns 1 (the base scope).
    assert_eq!(stack.len(), 1);
}

#[rstest::rstest]
fn len_increases_after_push() {
    // Given a default ScopeStack.
    let mut stack = ScopeStack::default();
    stack.push(FocusScope::Input);

    // When checking length after push.
    // Then it returns 2.
    assert_eq!(stack.len(), 2);
}

#[rstest::rstest]
fn active_picker_ops_returns_some_when_picker_active() {
    // Given an AppState with a Picker scope pushed.
    let mut state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Picker {
        kind: PickerKind::Provider,
    });

    // When getting active picker ops.
    let ops = state.active_picker_ops();

    // Then it returns Some (the provider picker).
    assert!(ops.is_some());
}

#[rstest::rstest]
fn active_picker_ops_returns_none_when_no_picker() {
    // Given an AppState in Input mode (default, no picker).
    let mut state = AppState::default_with_scope_focus();

    // When getting active picker ops.
    let ops = state.active_picker_ops();

    // Then it returns None.
    assert!(ops.is_none());
}

#[rstest::rstest]
fn session_mut_or_create_does_not_overwrite_existing_session_cwd() {
    // Given an AppState with a session that has a specific CWD.
    let mut state = AppState::default_with_scope_focus();
    state
        .session
        .set_default_cwd(std::path::PathBuf::from("/new/default"));

    let session_id = SessionId::new();
    {
        let session = state.session_mut_or_create(&session_id);
        session.set_cwd(std::path::PathBuf::from("/existing/cwd"));
    }

    // When accessing the same session via session_mut_or_create.
    let session = state.session_mut_or_create(&session_id);

    // Then the CWD is unchanged.
    assert_eq!(session.cwd(), std::path::Path::new("/existing/cwd"));
}
