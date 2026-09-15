//! Picker viewport measurement.
//!
//! [`measure_active_picker_results_height`] replicates the layout math of the
//! three picker widgets (`SelectionWidget`, `PreviewSelectionWidget`,
//! `TreePickerWidget`) to compute the results-area row count for the
//! currently-active picker. The render pre-pass writes that value into
//! [`PickerStates::picker_results_viewport`](crate::feat::ui::PickerStates::picker_results_viewport)
//! so the navigation intents can keep the cursor inside the visible window
//! instead of using a stale hardcoded constant.

use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};

use jinn_picker::WidgetKind;

use crate::common::app_state::AppState;
use crate::feat::ui::picker_states::PickerExt;

/// Fallback results height used before the first render of a picker writes a
/// measured value. Keeps the first keypress after opening a picker sane.
pub const PICKER_VIEWPORT_FALLBACK: u16 = 20;

/// Input + separator rows carved out above the results area in the standard
/// (`SelectionWidget` / `TreePickerWidget`) layout.
const CHROME_ROWS_STANDARD: u16 = 2;

/// Input + separator rows carved out of the skill picker's list pane in the
/// vertical-split layout.
const CHROME_ROWS_SKILL_LIST: u16 = 2;

/// Computes the active picker's results-area row count from terminal geometry.
///
/// Returns [`PICKER_VIEWPORT_FALLBACK`] when no picker is active. The result
/// is at least 1, even on tiny terminals, so the navigation math never
/// divides or windows against zero.
pub fn measure_active_picker_results_height(
    state: &AppState,
    frame_area: Rect,
    registry: &jinn_picker::PickerRegistry,
) -> u16 {
    let Some(kind) = state.frontend.scope_stack.picker_kind().copied() else {
        return PICKER_VIEWPORT_FALLBACK;
    };

    let popup_area = jinn_selection_widget::compute_popup_rect(frame_area);
    let inner = Block::default().borders(Borders::ALL).inner(popup_area);

    // Spec-driven geometry: a spec's widget kind selects the layout math and
    // its `bottom_rows()` reserves the footer. Every kind is spec-driven; the
    // fallback (empty registry, test seams) reserves the legacy single row.
    let height = match crate::feat::picker::registry::spec_id_for_kind(&kind)
        .and_then(|id| registry.get(id))
    {
        Some(spec) => match spec.widget_kind() {
            WidgetKind::Preview => skill_results_height(inner, popup_area, spec.bottom_rows()),
            WidgetKind::List | WidgetKind::Tree => {
                standard_results_height(inner, spec.bottom_rows())
            }
        },
        None => standard_results_height(inner, 1),
    };

    height.max(1)
}

/// Standard `SelectionWidget` / `TreePickerWidget` layout: results height is
/// the inner popup height minus the input+separator chrome and the footer
/// rows.
fn standard_results_height(inner: Rect, footer_rows: u16) -> u16 {
    inner
        .height
        .saturating_sub(CHROME_ROWS_STANDARD)
        .saturating_sub(footer_rows)
}

/// Preview-widget (`PreviewSelectionWidget`) results height, branching on
/// the split layout the widget selects from `popup_area.width`.
fn skill_results_height(inner: Rect, popup_area: Rect, bottom_rows: u16) -> u16 {
    // PreviewSelectionWidget reserves its footer rows, then splits the rest
    // into a content area.
    let content_height = inner.height.saturating_sub(bottom_rows);

    if popup_area.width >= jinn_selection_widget::VERTICAL_SPLIT_MIN_WIDTH {
        // Side-by-side split: list pane spans the full content height minus
        // its own input + separator chrome.
        content_height.saturating_sub(CHROME_ROWS_SKILL_LIST)
    } else {
        // Stacked split: list pane is fixed at HORIZONTAL_LIST_ROWS rows of
        // results (the widget's `HORIZONTAL_LIST_ROWS + 2` already includes
        // input + separator).
        jinn_selection_widget::HORIZONTAL_LIST_ROWS
    }
}

/// Reads the measured viewport from state, falling back to
/// [`PICKER_VIEWPORT_FALLBACK`] when it has not yet been written (zero) —
/// e.g. on the first keypress after opening a picker.
pub fn active_viewport(state: &AppState) -> usize {
    let h = state.frontend.picker_results_viewport();
    (if h == 0 { PICKER_VIEWPORT_FALLBACK } else { h }) as usize
}

#[cfg(test)]
mod tests;
