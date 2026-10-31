//! The stream-rule picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render context
//! deliberately never sees `AppState` (see `RenderFacts`), so this reads
//! `StreamRulePickerState` straight off the registered slot.

use jinn_selection_widget::PickerLayout;
use jinn_slices::RenderFacts;
use jinn_stream_rule_picker_msg::StreamRulePickerState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the stream-rule picker: the shared selection-widget
/// popup.
#[must_use]
pub fn stream_rule_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the stream-rule picker draws with — the same chrome/accent
/// split the sibling pickers use.
#[must_use]
pub fn stream_rule_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
    jinn_picker::Palette {
        border: ratatui::style::Color::DarkGray,
        filter_text: ratatui::style::Color::White,
        separator: ratatui::style::Color::DarkGray,
        footer: ratatui::style::Color::DarkGray,
        highlight_bg: ratatui::style::Color::DarkGray,
        muted_text: theme.muted_text,
        accent_action: theme.accent_action,
        popup_title: theme.popup_title,
        primary_text: theme.primary_text,
    }
}

/// The stream-rule picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer
/// can never advertise a key the picker does not bind.
#[must_use]
pub fn stream_rule_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::stream_rule_picker_routes::STREAM_RULE_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Draws the stream-rule picker popup for one frame.
pub fn render_stream_rule_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_stream_rule_picker_msg::stream_rule_picker_slot())
    else {
        return;
    };

    let layout = render_widget_from_state(&cell, frame, area, &facts.theme);

    // Publish what the widget actually solved for the navigation keys, which
    // need a real row count to keep the highlight on screen. The helper's
    // read guard has died by this line, so the write cannot deadlock.
    cell.update(|state: &mut StreamRulePickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}

/// Constructs the picker's widget from the cell's state and renders it into
/// `area`, returning the [`PickerLayout`] the widget solved.
///
/// The shared construction for the render fn and its tests: both must build
/// the identical widget (same two footer lines) so the layout a test reports
/// is the layout a frame actually drew. The read guard dies inside the block —
/// before the render fn writes the cell — because the `parking_lot::RwLock`
/// behind [`TypedCell`](jinn_slices::cell::TypedCell) is not reentrant; the
/// layout (`Copy`) outlives it.
pub(crate) fn render_widget_from_state(
    cell: &jinn_slices::cell::TypedCell<StreamRulePickerState>,
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &jinn_theme::Theme,
) -> PickerLayout {
    let binds = stream_rule_picker_binds();
    {
        let guard = cell.read();
        let state: &StreamRulePickerState = &guard;

        let palette = stream_rule_picker_palette(theme);
        let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

        // The status line (confirm feedback, or a failure's reason) rides above
        // the keybind line — the same two-row footer shape the preview pickers
        // use for their preview hints. Absent status, a blank line keeps the
        // geometry stable so a confirm does not resize the popup.
        let status_line = state.status.clone().unwrap_or_default();
        let footers = vec![Line::from(status_line), Line::from(keybind.0)];

        let widget = jinn_selection_widget::SelectionWidget::new(&state.selection)
            .title(Line::from(" Stream Rules "))
            .title_style(Style::default().fg(theme.popup_title))
            .footers(footers)
            .colors(palette.selection_colors());

        widget.render(frame, area)
    }
}
