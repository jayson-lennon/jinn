//! The applets picker's overlay rendering.
//!
//! A slice-owned picker renders from the slice's own cell. The render
//! context deliberately never sees `AppState` (see `RenderFacts`), so
//! this reads `AppletsPickerState` straight off the registered slot.

use jinn_applets_msg::AppletsPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the applets picker: the shared selection-widget popup.
#[must_use]
pub fn applets_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the applets picker draws with — the same chrome/accent split
/// the sibling pickers use.
#[must_use]
pub fn applets_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The applets picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the
/// footer can never advertise a key the picker does not bind.
#[must_use]
pub fn applets_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::picker_routes::APPLETS_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// Draws the applets picker popup for one frame.
pub fn render_applets_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_applets_msg::applets_picker_slot())
    else {
        return;
    };

    // The guard must end before the write-back below: `TypedCell` wraps a
    // non-reentrant lock, so writing while the read guard is held would
    // deadlock the render thread.
    let layout = {
        let guard = cell.read();
        let state: &AppletsPickerState = &guard;
        applets_picker_widget(state, &facts.theme).render(frame, area)
    };

    // Publish what the widget actually solved, for the navigation keys,
    // which need a real row count to keep the highlight on screen.
    cell.update(|state: &mut AppletsPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}

/// Assembles the picker's widget: title, keybind footer, and palette,
/// from the cell's state and the live theme.
///
/// Both the render pass and the render tests build through this helper,
/// so a test's second construction is identical to what the frame drew.
#[must_use]
pub fn applets_picker_widget<'a>(
    state: &'a AppletsPickerState,
    theme: &jinn_theme::Theme,
) -> jinn_selection_widget::SelectionWidget<
    'a,
    jinn_picker::PickerEntry<jinn_applets_msg::AppletPickerEntry>,
> {
    let palette = applets_picker_palette(theme);
    let binds = applets_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);
    jinn_selection_widget::SelectionWidget::new(&state.selection)
        .title(Line::from(" Applets "))
        .title_style(Style::default().fg(theme.popup_title))
        .footers(vec![Line::from(keybind.0)])
        .colors(palette.selection_colors())
}
