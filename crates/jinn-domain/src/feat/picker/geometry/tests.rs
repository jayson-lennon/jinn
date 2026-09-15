//! Tests for [`super`].

use ratatui::layout::Rect;

use crate::PickerKind;
use crate::common::app_state::AppState;
use crate::feat::picker::geometry::{
    PICKER_VIEWPORT_FALLBACK, measure_active_picker_results_height,
};
use crate::feat::ui::picker_states::PickerExt;

/// Frame area used by the standard popup-fit scenarios below. Large enough
/// that the popup hits its max-height cap and is wide enough to exercise the
/// skill picker's vertical split.
const LARGE_FRAME: Rect = Rect::new(0, 0, 120, 50);

#[rstest::rstest]
#[test]
fn measure_returns_fallback_when_no_picker_active() {
    // Given default app state with no picker open.
    let state = AppState::default_with_scope_focus();

    // When measuring the active picker viewport.
    let height = measure_active_picker_results_height(&state, LARGE_FRAME);

    // Then the fallback height is returned.
    assert_eq!(height, PICKER_VIEWPORT_FALLBACK);
}

#[rstest::rstest]
#[test]
fn measure_writes_into_state_field() {
    // Given a default app state.
    let mut state = AppState::default_with_scope_focus();

    // When writing a measured viewport directly.
    state.frontend.set_picker_results_viewport(7);

    // Then the field reflects the written value.
    assert_eq!(state.frontend.picker_results_viewport(), 7);
}

fn state_with_picker(kind: PickerKind) -> AppState {
    use crate::common::app_state::FocusScope;
    let state = AppState::default_with_scope_focus();
    state.frontend.scope_push(FocusScope::Picker { kind });
    state
}

#[rstest::rstest]
#[test]
fn measure_provider_picker_reserves_two_footer_rows() {
    // Given a Provider picker active (renders refresh + mode footers).
    let state = state_with_picker(PickerKind::Provider);

    // When measuring at LARGE_FRAME.
    let height = measure_active_picker_results_height(&state, LARGE_FRAME);

    // Then the height is inner minus chrome (2) minus two footers.
    // At LARGE_FRAME the popup inner is 39 rows; 39 - 2 - 2 = 35.
    assert_eq!(height, 35);
}

#[rstest::rstest]
#[test]
fn measure_persona_picker_reserves_one_footer_row() {
    // Given a Persona picker active (one footer).
    let state = state_with_picker(PickerKind::Persona);

    // When measuring at LARGE_FRAME.
    let height = measure_active_picker_results_height(&state, LARGE_FRAME);

    // Then the height is inner minus chrome (2) minus one footer.
    // At LARGE_FRAME the popup inner is 39 rows; 39 - 2 - 1 = 36.
    assert_eq!(height, 36);
}

#[rstest::rstest]
#[test]
fn measure_skill_picker_uses_vertical_split_when_wide() {
    // Given a Skill picker active on a wide frame.
    let state = state_with_picker(PickerKind::Skill);

    // When measuring at a frame wide enough for the vertical split
    // (popup width = frame*0.8 must be >= VERTICAL_SPLIT_MIN_WIDTH=101,
    // so frame width >= 127).
    let wide = Rect::new(0, 0, 140, 50);
    let height = measure_active_picker_results_height(&state, wide);

    // Then height is content minus skill list chrome (inner - 1 footer - 2 chrome).
    // Popup inner height is 39; 39 - 1 - 2 = 36.
    assert_eq!(height, 36);
}

#[rstest::rstest]
#[test]
fn measure_skill_picker_uses_horizontal_split_when_narrow() {
    // Given a Skill picker active on a narrow frame.
    let state = state_with_picker(PickerKind::Skill);

    // When measuring at a narrow frame (width < VERTICAL_SPLIT_MIN_WIDTH).
    let narrow = Rect::new(0, 0, 40, 50);
    let height = measure_active_picker_results_height(&state, narrow);

    // Then height is fixed at HORIZONTAL_LIST_ROWS.
    assert_eq!(height, jinn_selection_widget::HORIZONTAL_LIST_ROWS);
}

#[rstest::rstest]
#[test]
fn measure_never_returns_zero_on_tiny_terminal() {
    // Given a Persona picker active on a tiny frame.
    let state = state_with_picker(PickerKind::Persona);

    // When measuring at a 1x1 frame.
    let tiny = Rect::new(0, 0, 1, 1);
    let height = measure_active_picker_results_height(&state, tiny);

    // Then height is at least 1.
    assert!(height >= 1);
}
