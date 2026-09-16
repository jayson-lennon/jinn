//! Render for the project-add input popup.
//!
//! A centered overlay that lets the user type an absolute or relative path to
//! register as a new project directory. A live-validation footer shows the
//! resolved path (green check) or the reason it is invalid (red x) on every
//! keystroke. Mirrors the layout of the cwd slice's popup renderer.

use crate::common::render_ctx::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

use jinn_cwd_msg::{CwdResolution, resolve_cwd_input};

/// Horizontal padding fraction for the popup (20% each side).
const POPUP_H_PAD_FRAC: f32 = 0.20;
/// Minimum popup width in cells.
const POPUP_MIN_WIDTH: u16 = 30;

/// Computes the popup rectangle for the project-add input overlay.
///
/// The popup is centered horizontally and placed one-third down the screen.
/// It is tall enough for the title border, the input line, and the
/// live-validation footer: `border(2) + input(1) + footer(1) = 4` rows.
#[must_use]
pub fn project_add_input_popup_rect(area: Rect) -> Rect {
    let popup_width = ((f32::from(area.width) * (1.0 - 2.0 * POPUP_H_PAD_FRAC)).ceil() as u16)
        .max(POPUP_MIN_WIDTH)
        .min(area.width);

    let popup_height = 4u16.min(area.height); // border(2) + input(1) + footer(1)

    // Integer division is intentional - we're computing cell positions for centering.
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    #[expect(clippy::integer_division, reason = "cell positions are integers")]
    let popup_y = area.height.saturating_sub(popup_height) / 3;

    Rect::new(popup_x, popup_y, popup_width, popup_height)
}

/// Builds the footer line for live validation: a green check and the resolved
/// path on success, a red x and the offending path on failure, or a muted hint
/// when the input is empty.
fn validation_footer<'a>(
    resolution: &'a CwdResolution,
    theme: &crate::feat::theme::Theme,
) -> Line<'a> {
    match resolution {
        CwdResolution::Ok(path) => Line::from(vec![
            Span::styled("✓ ", Style::default().fg(theme.success)),
            Span::styled(
                path.to_string_lossy().into_owned(),
                Style::default().fg(theme.success),
            ),
        ]),
        CwdResolution::NotADir(path) => Line::from(vec![
            Span::styled("✗ ", Style::default().fg(theme.error_text)),
            Span::styled(
                format!("not a directory: {path}"),
                Style::default().fg(theme.error_text),
            ),
        ]),
        CwdResolution::Empty => Line::from(Span::styled(
            "type a path (use ~ or a relative path)",
            Style::default().fg(theme.muted_text),
        )),
    }
}

/// Renders the project-add input popup.
///
/// Shows a centered popup with:
/// - Title: "Add project directory"
/// - Input line (`> {input}`) with a live cursor
/// - Footer: live validation (green check / red x / muted hint)
///
/// The footer is recomputed every draw by resolving the current input against
/// the active session's cwd, giving immediate feedback as the user types.
pub fn render_project_add_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let input_state = &state.frontend.project_add_input;
    let theme = &state.frontend.theme;
    let current_cwd = state.active_session().cwd();
    let popup_area = project_add_input_popup_rect(area);

    let title = Line::from(Span::styled(
        " Add project directory ",
        Style::default().fg(theme.popup_title),
    ));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));

    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    // Inner area (1 padding on each side from border).
    let inner = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: popup_area.width.saturating_sub(2),
        height: popup_area.height.saturating_sub(2),
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Input line: "> {input}" - the ">" uses focus_accent for consistency.
    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let input_span = Span::raw(&input_state.text.input);
    let input_line = Line::from(vec![prefix, input_span]);
    let input_para = Paragraph::new(input_line);
    frame.render_widget(input_para, Rect::new(inner.x, inner.y, inner.width, 1));

    // Compute cursor x position: "> " (2) + grapheme count up to cursor_pos.
    let prefix_len = 2u16;
    let grapheme_count = input_state
        .text
        .input
        .get(..input_state.text.cursor_pos)
        .map_or(0, |s| s.graphemes(true).count());
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));

    // Footer: live validation on the line below the input.
    if inner.height >= 2 {
        let resolution = resolve_cwd_input(&input_state.text.input, current_cwd);
        let footer = validation_footer(&resolution, theme);
        frame.render_widget(
            Paragraph::new(footer),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::{AppState, FocusScope};
    use crate::common::line_input::LineInput;
    use crate::feat::project_add_input::state::ProjectAddInputState;
    use jinn_testutil::setup_term;

    fn state_with_input(input: &str) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.scope_push(FocusScope::ProjectAddInput);
        state.frontend.project_add_input = ProjectAddInputState {
            text: LineInput {
                input: input.to_owned(),
                cursor_pos: input.len(),
            },
        };
        state
    }

    #[rstest::rstest]
    fn project_add_popup_rect_has_room_for_footer() {
        // Given an 80x24 area.
        let area = Rect::new(0, 0, 80, 24);

        // When computing the popup rect.
        let popup = project_add_input_popup_rect(area);

        // Then the popup is exactly 4 rows tall: border top/bottom (2)
        // + input line (1) + validation footer (1). No blank row.
        assert_eq!(popup.height, 4);
    }

    #[rstest::rstest]
    fn project_add_popup_shows_title() {
        // Given a state with any input.
        let state = state_with_input("x");
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_project_add_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the title appears in the top border.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = project_add_input_popup_rect(area);
        let mut found = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, popup_area.y))
                && (cell.symbol() == "A" || cell.symbol() == "d" || cell.symbol() == "p")
            {
                found = true;
                break;
            }
        }
        assert!(found, "title text should appear in the top border");
    }

    #[rstest::rstest]
    fn project_add_popup_shows_live_resolution_for_existing_dir() {
        // Given a state whose input resolves to the temp dir (an existing dir).
        let tmp = std::env::temp_dir();
        let state = state_with_input(&tmp.to_string_lossy());
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_project_add_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the footer shows a green check for the resolved path.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = project_add_input_popup_rect(area);
        let footer_y = popup_area.y + 2;
        let mut found_check = false;
        let success_color = state.frontend.theme.success;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, footer_y))
                && cell.symbol() == "✓"
            {
                assert_eq!(
                    cell.style().fg,
                    Some(success_color),
                    "check should be green (theme.success)"
                );
                found_check = true;
                break;
            }
        }
        assert!(
            found_check,
            "green check should appear in the footer for a valid dir"
        );
    }

    #[rstest::rstest]
    fn project_add_popup_shows_error_for_nonexistent_dir() {
        // Given a state whose input is a clearly nonexistent path.
        let state = state_with_input("/this/path/does/not/exist/xyz");
        let (mut terminal, area) = setup_term(30, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_project_add_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the footer shows a red x.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = project_add_input_popup_rect(area);
        let footer_y = popup_area.y + 2;
        let error_color = state.frontend.theme.error_text;
        let mut found_x = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, footer_y))
                && cell.symbol() == "✗"
            {
                assert_eq!(
                    cell.style().fg,
                    Some(error_color),
                    "x should be red (theme.error_text)"
                );
                found_x = true;
                break;
            }
        }
        assert!(found_x, "red x should appear in the footer for a bad path");
    }

    #[rstest::rstest]
    fn project_add_popup_shows_muted_hint_for_empty_input() {
        // Given a state with empty input.
        let state = state_with_input("");
        let (mut terminal, area) = setup_term(80, 24);

        // When rendering the popup.
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_project_add_input(frame, area, &ctx);
            })
            .unwrap();

        // Then the footer shows the muted hint text in muted_text color.
        let buffer = terminal.backend().buffer().clone();
        let popup_area = project_add_input_popup_rect(area);
        let footer_y = popup_area.y + 2;
        let muted_color = state.frontend.theme.muted_text;
        let mut found_hint = false;
        for x in popup_area.x..(popup_area.x + popup_area.width).min(buffer.area().width) {
            if let Some(cell) = buffer.cell((x, footer_y))
                && cell.style().fg == Some(muted_color)
                && cell.symbol() != " "
            {
                found_hint = true;
                break;
            }
        }
        assert!(found_hint, "muted hint should appear for empty input");
    }
}
