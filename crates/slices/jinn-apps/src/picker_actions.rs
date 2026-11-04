//! The apps picker's actions — the rename handoff and the empty-picker
//! guards, factored out of the route rows so the tests can drive them
//! without a full route table.

use jinn_apps_msg::{AppsPickerState, RenameApp, apps_rename_scope};
use jinn_slices::RouteResult as IntentResult;
use jinn_slices::ScopeSignal;
use jinn_slices::cell::TypedCell;
use jinn_slices::line_input::LineInput;

/// The picker's cell.
pub type AppsPickerCell = TypedCell<AppsPickerState>;

/// `<c-r>`: seed the rename popup with the highlighted title and push
/// the popup's scope.
///
/// A no-op when nothing is highlighted — the empty picker accepts every
/// key as a no-op by contract.
#[must_use]
pub fn begin_rename(picker: &AppsPickerCell) -> IntentResult {
    let Some(title) = picker.read().highlighted_title() else {
        return IntentResult::empty();
    };
    picker.update(|state| {
        state.rename_input = rename_input_seeded(&title);
    });
    IntentResult::empty().with_scope_signal(ScopeSignal::Push(apps_rename_scope()))
}

/// The rename popup's seeded input: the current title, cursor at the
/// end — the session-rename seeding shape.
#[must_use]
pub fn rename_input_seeded(title: &str) -> LineInput {
    let mut input = LineInput::new();
    input.set(title.to_owned());
    input
}

/// Confirms the rename: validate, publish [`RenameApp`], pop the popup.
///
/// An empty (or whitespace-only) title is refused silently — no partial
/// mutation, no error surfaced; the popup stays open for another try,
/// the same policy the session rename's confirm applies.
#[must_use]
pub fn confirm_rename(picker: &AppsPickerCell) -> IntentResult {
    let (id, text) =
        picker.update(|state| (state.highlighted_id(), state.rename_input.input.clone()));
    let Some(id) = id else {
        return IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(apps_rename_scope()));
    };
    let title = text.trim().to_owned();
    if title.is_empty() {
        return IntentResult::empty();
    }

    picker.update(|state| {
        state.rename_input = LineInput::new();
    });
    IntentResult::new_message(RenameApp { id, title })
        .with_scope_signal(ScopeSignal::PopIf(apps_rename_scope()))
}

/// Cancels the rename: discard the input, pop the popup.
#[must_use]
pub fn cancel_rename(picker: &AppsPickerCell) -> IntentResult {
    picker.update(|state| {
        state.rename_input = LineInput::new();
    });
    IntentResult::empty().with_scope_signal(ScopeSignal::PopIf(apps_rename_scope()))
}

/// Clears a non-empty rename input, or leaves the popup when already
/// empty — the session-rename clear-or-leave shape.
#[must_use]
pub fn clear_or_leave_rename(picker: &AppsPickerCell) -> IntentResult {
    let empty = picker.update(|state| state.rename_input.input.is_empty());
    if empty {
        return cancel_rename(picker);
    }
    picker.update(|state| {
        state.rename_input = LineInput::new();
    });
    IntentResult::empty()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::{
        begin_rename, cancel_rename, clear_or_leave_rename, confirm_rename, rename_input_seeded,
    };
    use jinn_apps_msg::{AppEntry, AppsPickerState, apps_picker_slot, build_app_entries};
    use jinn_slices::RouteResult;
    use jinn_slices::ScopeSignal;
    use jinn_slices::cell::TypedCell;

    fn picker_cell_with_apps() -> TypedCell<AppsPickerState> {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register::<AppsPickerState>(apps_picker_slot(), AppsPickerState::default())
            .expect("register picker cell");
        let theme = jinn_theme::default_theme();
        let entries = vec![
            AppEntry {
                id: "alpha".to_owned(),
                title: "Alpha".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            },
            AppEntry {
                id: "beta".to_owned(),
                title: "Beta".to_owned(),
                created_at: "2026-01-02T00:00:00Z".to_owned(),
            },
        ];
        let items = build_app_entries(&entries, &theme);
        cell.update(|state| {
            state.selection.set_items(items);
        });
        cell
    }

    #[rstest::rstest]
    #[test]
    fn begin_rename_pushes_the_rename_scope_seeded_with_the_title() {
        // Given a picker with the highlight on Alpha.
        let cell = picker_cell_with_apps();

        // When beginning a rename.
        let result = begin_rename(&cell);

        // Then the rename scope is pushed.
        let SignalCheck::Push(scope) = signal_of(&result) else {
            panic!("expected a push signal");
        };
        assert_eq!(scope, jinn_apps_msg::apps_rename_scope());
        // And the input is seeded with the highlighted title, cursor at
        // the end.
        let state = cell.read();
        assert_eq!(state.rename_input.input, "Alpha");
        assert_eq!(state.rename_input.cursor_pos, "Alpha".len());
    }

    #[rstest::rstest]
    #[test]
    fn begin_rename_no_ops_on_an_empty_picker() {
        // Given a picker with no rows.
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register::<AppsPickerState>(apps_picker_slot(), AppsPickerState::default())
            .expect("register");

        // When beginning a rename.
        let result = begin_rename(&cell);

        // Then nothing happens — no signal, no message.
        assert!(signal_of(&result).is_none());
        assert_eq!(result.messages.len(), 0);
    }

    #[rstest::rstest]
    #[test]
    fn confirm_rename_publishes_and_pops() {
        // Given a rename popup seeded from Alpha.
        let cell = picker_cell_with_apps();
        let _ = begin_rename(&cell);
        // And a new title typed in.
        cell.update(|state| {
            state.rename_input.set("Alphabet".to_owned());
        });

        // When confirming.
        let result = confirm_rename(&cell);

        // Then a RenameApp command is published naming the id and the
        // new title.
        assert_eq!(result.message_names, ["RenameApp"]);
        // And the rename scope pops.
        let SignalCheck::PopIf(scope) = signal_of(&result) else {
            panic!("expected a pop signal");
        };
        assert_eq!(scope, jinn_apps_msg::apps_rename_scope());
        // And the popup input is cleared.
        assert_eq!(cell.read().rename_input.input, "");
    }

    #[rstest::rstest]
    #[test]
    fn confirm_rename_refuses_an_empty_title() {
        // Given a seeded popup whose input was cleared.
        let cell = picker_cell_with_apps();
        let _ = begin_rename(&cell);
        cell.update(|state| {
            state.rename_input.set(String::new());
        });

        // When confirming.
        let result = confirm_rename(&cell);

        // Then nothing is published and the popup stays open.
        assert_eq!(result.messages.len(), 0);
        assert!(signal_of(&result).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn cancel_rename_discards_and_pops() {
        // Given a seeded popup.
        let cell = picker_cell_with_apps();
        let _ = begin_rename(&cell);

        // When cancelling.
        let result = cancel_rename(&cell);

        // Then the popup pops with no message.
        assert_eq!(result.messages.len(), 0);
        let SignalCheck::PopIf(scope) = signal_of(&result) else {
            panic!("expected a pop signal");
        };
        assert_eq!(scope, jinn_apps_msg::apps_rename_scope());
        // And the input is cleared.
        assert_eq!(cell.read().rename_input.input, "");
    }

    #[rstest::rstest]
    #[test]
    fn clear_or_leave_clears_then_leaves_on_the_second_press() {
        // Given a popup with text.
        let cell = picker_cell_with_apps();
        let _ = begin_rename(&cell);

        // When clearing with text present.
        let first = clear_or_leave_rename(&cell);

        // Then the popup stays and the text is gone.
        assert!(signal_of(&first).is_none());
        assert_eq!(cell.read().rename_input.input, "");

        // When clearing again with no text.
        let second = clear_or_leave_rename(&cell);

        // Then the popup pops.
        assert!(signal_of(&second).is_some());
    }

    #[rstest::rstest]
    #[test]
    fn rename_input_seeding_puts_the_cursor_at_the_end() {
        // Given a title.
        // When seeding the input.
        let input = rename_input_seeded("Q4 Dashboard");
        // Then the cursor sits at the end, ready to append.
        assert_eq!(input.input, "Q4 Dashboard");
        assert_eq!(input.cursor_pos, "Q4 Dashboard".len());
    }

    /// What scope signal a result carries, as a checkable value.
    #[derive(Debug, PartialEq, Eq)]
    enum SignalCheck {
        Push(jinn_slices::SliceScopeId),
        PopIf(jinn_slices::SliceScopeId),
        None,
    }

    impl SignalCheck {
        fn is_none(&self) -> bool {
            matches!(self, SignalCheck::None)
        }

        fn is_some(&self) -> bool {
            !self.is_none()
        }
    }

    fn signal_of(result: &RouteResult) -> SignalCheck {
        match &result.scope_signal {
            Some(ScopeSignal::Push(scope)) => SignalCheck::Push(scope.clone()),
            Some(ScopeSignal::PopIf(scope)) => SignalCheck::PopIf(scope.clone()),
            None => SignalCheck::None,
        }
    }
}
