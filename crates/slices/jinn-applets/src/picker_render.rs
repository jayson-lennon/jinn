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

/// Assembles the picker's widget: title, the server status line and
/// keybind footer, and palette, from the cell's state and the live
/// theme.
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
    let status = jinn_applets_msg::applets_server_status_line(state.serving.as_ref(), theme);
    jinn_selection_widget::SelectionWidget::new(&state.selection)
        .title(Line::from(" Applets "))
        .title_style(Style::default().fg(theme.popup_title))
        .footers(vec![status, Line::from(keybind.0)])
        .colors(palette.selection_colors())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use jinn_applets_msg::{AppletEntry, AppletsPickerState, applets_picker_slot};
    use jinn_slices::cell::TypedCell;

    use super::render_applets_picker;

    /// One catalog row, so the footer's place in the popup is exercised
    /// with real rows above it.
    fn alpha_entry() -> AppletEntry {
        AppletEntry {
            id: "a".to_owned(),
            title: "Alpha".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    /// A registry carrying only the applets picker cell, seeded with
    /// `serving` and one catalog row.
    fn cell_with(serving: Option<&str>) -> (jinn_slices::Slices, TypedCell<AppletsPickerState>) {
        let slices = jinn_slices::Slices::new();
        let cell = slices
            .register::<AppletsPickerState>(applets_picker_slot(), AppletsPickerState::default())
            .expect("register the picker cell");
        cell.update(|state| {
            match serving {
                Some(bind) => state.apply_serving(true, bind),
                None => state.apply_serving(false, ""),
            }
            let theme = state.theme.clone();
            let items = jinn_applets_msg::build_applet_entries(&[alpha_entry()], &theme);
            state.selection.set_items(items);
        });
        (slices, cell)
    }

    /// Draws one frame through the real render pass and returns the
    /// backend buffer — the exact cells the frame drew.
    fn draw(slices: &jinn_slices::Slices) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let facts = jinn_slices::RenderFacts::new(jinn_theme::default_theme(), slices);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("a test terminal allocates");
        terminal
            .draw(|frame| {
                let popup = super::applets_picker_overlay_rect(&area)
                    .expect("geometry fn yields a popup rect");
                render_applets_picker(frame, popup, &facts);
            })
            .expect("the applets picker renders one frame");
        terminal.backend().buffer().clone()
    }

    #[rstest::rstest]
    #[test]
    fn the_footer_shows_offline_before_any_bind() {
        // Given the picker over a dormant server.
        let (slices, _cell) = cell_with(None);

        // When rendering a frame through the real render pass.
        let buffer = draw(&slices);

        // Then the footer says the server is offline.
        let text: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("<server offline>"), "got: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn the_footer_names_the_serving_url_when_bound() {
        // Given the picker over a serving server at a resolved bind.
        let (slices, _cell) = cell_with(Some("127.0.0.1:41234"));

        // When rendering a frame through the real render pass.
        let buffer = draw(&slices);

        // Then the footer names the serving URL.
        let text: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            text.contains("serving http://127.0.0.1:41234"),
            "got: {text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_offline_status_line_is_muted() {
        // Given the picker over a dormant server.
        let theme = jinn_theme::default_theme();
        let (slices, _cell) = cell_with(None);

        // When rendering a frame.
        let buffer = draw(&slices);

        // Then the row carrying `<server offline>` renders in the
        // muted foreground.
        let row = row_containing(&buffer, "<server offline>").expect("the offline line renders");
        assert!(
            (0..usize::from(buffer.area.width))
                .map(|x| cell_at(&buffer, row, x))
                .any(|cell| cell.style().fg == Some(theme.muted_text)),
            "the offline line is muted"
        );
    }

    #[rstest::rstest]
    #[test]
    fn the_serving_status_line_is_success_colored() {
        // Given the picker over a serving server.
        let theme = jinn_theme::default_theme();
        let (slices, _cell) = cell_with(Some("127.0.0.1:41234"));

        // When rendering a frame.
        let buffer = draw(&slices);

        // Then the row carrying `serving http://…` renders in the
        // success foreground.
        let row = row_containing(&buffer, "serving http://").expect("the serving line renders");
        assert!(
            (0..usize::from(buffer.area.width))
                .map(|x| cell_at(&buffer, row, x))
                .any(|cell| cell.style().fg == Some(theme.success)),
            "the serving line is success-colored"
        );
    }

    /// The row index whose concatenated symbols contain `needle`.
    fn row_containing(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<usize> {
        (0..usize::from(buffer.area.height)).find(|&row| {
            let line: String = (0..usize::from(buffer.area.width))
                .map(|x| cell_at(buffer, row, x).symbol().to_owned())
                .collect();
            line.contains(needle)
        })
    }

    /// The buffer cell at `(row, x)`.
    ///
    /// # Panics
    ///
    /// Panics past the buffer's bounds — a test bug (a row/column
    /// outside the drawn area), not a runtime state.
    #[expect(
        clippy::indexing_slicing,
        reason = "test geometry: the row helper bounds both indexes to the drawn buffer"
    )]
    fn cell_at(buffer: &ratatui::buffer::Buffer, row: usize, x: usize) -> &ratatui::buffer::Cell {
        let index = row * usize::from(buffer.area.width) + x;
        &buffer.content[index]
    }

    #[rstest::rstest]
    #[test]
    fn the_keybind_line_still_renders_below_the_status_line() {
        // Given the picker over a serving server.
        let (slices, _cell) = cell_with(Some("127.0.0.1:41234"));

        // When rendering a frame through the real render pass.
        let buffer = draw(&slices);

        // Then the keybind line renders below the status line — the
        // status sits above it, both right-aligned rows of the footer.
        let status_row =
            row_containing(&buffer, "serving http://").expect("the status line renders");
        let keybind_row =
            row_containing(&buffer, "open in browser").expect("the keybind line renders");
        assert!(
            keybind_row > status_row,
            "the keybind line follows the status line"
        );
    }
}
