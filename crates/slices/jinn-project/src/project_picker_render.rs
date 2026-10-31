//! The project picker's overlay rendering.
//!
//! Reads `ProjectPickerState` straight off the registered slot, so the render
//! path never borrows `AppState` and the kernel holds no state for this menu.

use jinn_project_msg::ProjectPickerState;
use jinn_slices::RenderFacts;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// The popup rect for the project picker: the shared selection-widget popup.
#[must_use]
pub fn project_picker_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(jinn_selection_widget::compute_popup_rect(*area))
}

/// The colors the project picker draws with.
#[must_use]
pub fn project_picker_palette(theme: &jinn_theme::Theme) -> jinn_picker::Palette {
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

/// The project picker's declared binds, in footer order.
///
/// Sourced from the route rows the slice actually attaches, so the footer can
/// never advertise a key the picker does not bind.
#[must_use]
pub fn project_picker_binds() -> Vec<jinn_picker::BindRow> {
    crate::project_picker_routes::PROJECT_PICKER_BINDINGS
        .iter()
        .map(|(notation, label)| jinn_picker::BindRow {
            notation,
            label,
            category_hint: "input",
        })
        .collect()
}

/// The widget the project picker draws, fully configured.
///
/// Shared by the render pass and the tests, so a test's identical second
/// construction is identical by construction rather than by transcription.
#[must_use]
pub fn project_picker_widget<'a>(
    state: &'a ProjectPickerState,
    theme: &jinn_theme::Theme,
) -> jinn_selection_widget::SelectionWidget<
    'a,
    jinn_picker::PickerEntry<jinn_project_msg::ProjectEntry>,
> {
    let palette = project_picker_palette(theme);
    let binds = project_picker_binds();
    let keybind = jinn_picker::keybind_line(&binds, jinn_picker::Tail::Standard, &palette);

    jinn_selection_widget::SelectionWidget::new(&state.selection)
        .title(Line::from(" Projects "))
        .title_style(Style::default().fg(theme.popup_title))
        .footers(vec![Line::from(keybind.0)])
        .colors(palette.selection_colors())
}

/// Draws the project picker popup for one frame.
pub fn render_project_picker(frame: &mut Frame<'_>, area: Rect, facts: &RenderFacts) {
    let Some(cell) = facts
        .slices
        .reader(&jinn_project_msg::project_picker_slot())
    else {
        return;
    };

    // The widget reports the layout it actually solved; the guard must end
    // before the write below (`TypedCell`'s `parking_lot::RwLock` is not
    // reentrant on the same thread).
    let layout = {
        let guard = cell.read();
        let state: &ProjectPickerState = &guard;
        project_picker_widget(state, &facts.theme).render(frame, area)
    };

    // Publish the reported row count for the navigation keys.
    cell.update(|state: &mut ProjectPickerState| {
        state.results_viewport = usize::from(layout.inner.height);
    });
}
