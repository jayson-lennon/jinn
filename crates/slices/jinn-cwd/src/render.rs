//! Render for the cwd input popup — a kernel-free overlay view.
//!
//! A centered overlay that lets the user type an absolute or relative path. A
//! live-validation footer shows the resolved path (green check) or the reason
//! it is invalid (red x) on every keystroke. Registered as the renderer for
//! the popup's dynamic scope at activation; the facts' `session.cwd` entry
//! supplies the resolve base.

use jinn_cwd_msg::{CwdInputState, CwdResolution, resolve_cwd_input};
use jinn_slices::RenderFacts;
use jinn_slices::cell::TypedCell;
use marked_path::{Absolute, MarkedPathBuf};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

/// The app fact carrying the active session's working directory (the
/// resolve base for the footer). Populated per frame by the kernel's
/// `RenderCtx::facts`.
pub const SESSION_CWD_FACT: &str = "session.cwd";

/// Horizontal padding fraction for the popup (20% each side).
const POPUP_H_PAD_FRAC: f32 = 0.20;
/// Minimum popup width in cells.
const POPUP_MIN_WIDTH: u16 = 30;

/// Computes the popup rectangle for the cwd input overlay.
///
/// The popup is centered horizontally and placed one-third down the screen.
/// It is tall enough for the title border, the input line, and the
/// live-validation footer: `border(2) + input(1) + footer(1) = 4` rows.
#[must_use]
pub fn cwd_input_popup_rect(area: Rect) -> Rect {
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

/// The overlay-rect function registered on the slice host: the popup rect,
/// or `None` when the frame is too small to fit it.
#[must_use]
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "the overlay registry's OverlayFn signature passes `&Rect`"
)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "OverlayFn contract allows `None` for too-small frames; the cwd popup always fits today"
)]
pub fn cwd_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(cwd_input_popup_rect(*area))
}

/// Builds the footer line for live validation: a green check and the resolved
/// path on success, a red x and the offending path on failure, or a muted hint
/// when the input is empty.
fn validation_footer<'a>(resolution: &'a CwdResolution, theme: &jinn_theme::Theme) -> Line<'a> {
    match resolution {
        CwdResolution::Ok(path) => Line::from(vec![
            Span::styled("✓ ", Style::default().fg(theme.success)),
            Span::styled(path.to_string(), Style::default().fg(theme.success)),
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

/// Renders the cwd input popup over the frame's popup rect.
fn draw(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    input_state: &CwdInputState,
    theme: &jinn_theme::Theme,
    current_cwd: &MarkedPathBuf<Absolute>,
) {
    let title = Line::from(Span::styled(
        " Change Cwd ",
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

    // Input line: "> {input}{ghost}" - the ">" uses focus_accent for
    // consistency; the ghost (when present) renders muted, immediately
    // after the typed text, without moving the cursor.
    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let ghost_text = input_state
        .ghost
        .as_ref()
        .map(|winner| {
            let before_cursor = input_state
                .text
                .input
                .get(..input_state.text.cursor_pos)
                .unwrap_or_default();
            let (_parent, segment) = jinn_cwd_msg::split_at_cursor(before_cursor);
            let remainder = crate::suggest::ghost_remainder(segment, winner);
            format!("{remainder}{}", std::path::MAIN_SEPARATOR)
        })
        .unwrap_or_default();
    let mut spans = vec![prefix, Span::raw(input_state.text.input.clone())];
    if !ghost_text.is_empty() {
        spans.push(Span::styled(
            ghost_text,
            Style::default().fg(theme.muted_text),
        ));
    }
    let input_line = Line::from(spans);
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

/// The overlay view registered on the host: renders only when the cwd cell
/// is present (activated). Reads the resolve base from the `session.cwd`
/// fact.
///
/// # Panics
///
/// Panics if the cwd slot is not registered — the overlay only renders when
/// the slice that owns it activated.
#[expect(
    clippy::expect_used,
    reason = "the overlay only renders when the cwd scope registered its cell"
)]
pub fn render_cwd_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderFacts) {
    // `area` is the overlay rect the geometry fn computed (the centered
    // popup rect) — draw into it directly.
    let cell: TypedCell<CwdInputState> = ctx
        .slices
        .reader(&jinn_cwd_msg::cwds_slot())
        .expect("cwd overlay renders only when its cell is registered");
    let state = cell.read();
    let current_cwd =
        MarkedPathBuf::<Absolute>::new(ctx.fact(SESSION_CWD_FACT).unwrap_or_default())
            .expect("the session.cwd fact is an absolute path");
    draw(frame, area, &state, &ctx.theme, &current_cwd);
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use super::*;
    use jinn_cwd_msg::cwds_slot;
    use jinn_slices::Slices;
    use jinn_slices::TypedCell;
    use jinn_testutil::setup_term;
    use marked_path::MarkedPathAccess as _;

    /// Test wiring: a slices registry with the cwd cell seeded from typed
    /// text, cursor position, and ghost. Returns (slices, cell).
    fn cwd_ctx(
        input: &str,
        cursor_pos: usize,
        ghost: Option<&str>,
    ) -> (Slices, TypedCell<CwdInputState>) {
        let slices = Slices::new();
        let cell = slices
            .register(cwds_slot(), CwdInputState::default())
            .expect("fresh registry");
        cell.update(|s| {
            s.text.set(input.to_owned());
            s.text.cursor_pos = cursor_pos;
            s.ghost = ghost.map(str::to_owned);
        });
        (slices, cell)
    }

    fn facts_for(slices: &Slices) -> RenderFacts {
        let mut facts = RenderFacts::new(jinn_theme::default_theme(), slices);
        // The footer's live validation resolves against this fact; a temp
        // root is a real existing directory, so the footer renders green.
        facts.set_facts([jinn_slices::AppFact {
            key: SESSION_CWD_FACT,
            value: jinn_testutil::volume_root().to_string_lossy().to_string(),
        }]);
        facts
    }

    /// Renders the popup into a fresh terminal; the popup rect is derived
    /// from the terminal area the way the overlay geometry does.
    fn render_popup(slices: &Slices) -> (ratatui::Terminal<ratatui::backend::TestBackend>, Rect) {
        let (mut terminal, area) = setup_term(80, 24);
        let popup_area = cwd_input_popup_rect(area);
        terminal
            .draw(|frame| {
                let facts = facts_for(slices);
                render_cwd_input(frame, popup_area, &facts);
            })
            .expect("test draw never fails");
        (terminal, popup_area)
    }

    /// Collects (symbol, foreground) per column of one row.
    fn row_cells(
        terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
        area: Rect,
        y: u16,
    ) -> Vec<(String, Option<ratatui::style::Color>)> {
        let buffer = terminal.backend().buffer();
        (area.x..area.x + area.width)
            .map(|x| {
                buffer
                    .cell((x, y))
                    .map(|c| (c.symbol().to_owned(), c.style().fg))
                    .unwrap_or_default()
            })
            .collect()
    }

    #[rstest::rstest]
    fn ghost_renders_after_the_cursor_in_muted_text() {
        // Given a popup seeded with a typed segment and a prefix ghost.
        let (slices, _cell) = cwd_ctx("~/co", "~/co".len(), Some("completed-projects"));
        let (terminal, popup_area) = render_popup(&slices);

        // When reading the input row.
        let input_y = popup_area.y + 1;
        let row = row_cells(&terminal, popup_area, input_y);
        let rendered: String = row.iter().map(|(sym, _)| sym.as_str()).collect();

        // Then the ghost remainder appears right after the typed text.
        let ghost = format!("mpleted-projects{}", std::path::MAIN_SEPARATOR);
        assert!(
            rendered.contains(&format!("~/co{ghost}")),
            "input row was {rendered:?}"
        );
    }

    #[rstest::rstest]
    fn ghost_is_styled_muted_and_the_typed_text_is_not() {
        // Given the same popup.
        let (slices, cell) = cwd_ctx("~/co", "~/co".len(), Some("completed-projects"));
        let muted = facts_for(&slices).theme.muted_text;
        let _ = cell;
        let (terminal, popup_area) = render_popup(&slices);

        // When locating the ghost columns (after "> ~/co").
        let input_y = popup_area.y + 1;
        let row = row_cells(&terminal, popup_area, input_y);
        // Relative index: inner edge (border) + "> " prefix + typed text.
        let ghost_idx = 1 + 2 + "~/co".len();

        // Then every ghost cell carries the muted foreground and the cell
        // just before it (the typed 'o') does not.
        let (before_sym, before_fg) = &row[ghost_idx - 1];
        assert_eq!(before_sym, "o");
        assert_ne!(*before_fg, Some(muted));
        for (sym, fg) in row.iter().skip(ghost_idx) {
            if sym == " " {
                break; // ghost text ends; padding cells follow
            }
            assert_eq!(*fg, Some(muted), "ghost cell {sym:?} not muted");
        }
    }

    #[rstest::rstest]
    fn ghost_render_does_not_move_the_cursor() {
        // Given the same popup.
        let (slices, _cell) = cwd_ctx("~/co", "~/co".len(), Some("completed-projects"));
        let (mut terminal, popup_area) = render_popup(&slices);

        // When reading the terminal cursor.
        let cursor = terminal
            .get_cursor_position()
            .expect("test backend reports cursor");

        // Then the cursor sits at the end of the typed text — the ghost
        // extended nothing.
        let input_y = popup_area.y + 1;
        assert_eq!(cursor.y, input_y);
        // Inner edge (border) + prefix + typed text.
        let expected_x = popup_area.x + 1 + 2 + u16::try_from("~/co".len()).expect("small");
        assert_eq!(cursor.x, expected_x);
    }

    #[rstest::rstest]
    fn no_ghost_renders_no_extra_span() {
        // Given a popup with no ghost.
        let (slices, _cell) = cwd_ctx("~/co", "~/co".len(), None);
        let (terminal, popup_area) = render_popup(&slices);

        // When reading the input row.
        let input_y = popup_area.y + 1;
        let rendered: String = row_cells(&terminal, popup_area, input_y)
            .iter()
            .map(|(sym, _)| sym.as_str())
            .collect();

        // Then only the typed text renders — no ghost residue.
        assert!(rendered.contains("> ~/co"), "row was {rendered:?}");
        assert!(
            !rendered.contains("mpleted"),
            "no ghost text expected, row was {rendered:?}"
        );
    }

    #[rstest::rstest]
    fn type_over_ghost_renders_the_whole_winner_name() {
        // Given a popup whose segment does not prefix the winner.
        let (slices, _cell) = cwd_ctx("~/mkn", "~/mkn".len(), Some("mkdir"));
        let (terminal, popup_area) = render_popup(&slices);

        // When reading the input row.
        let input_y = popup_area.y + 1;
        let rendered: String = row_cells(&terminal, popup_area, input_y)
            .iter()
            .map(|(sym, _)| sym.as_str())
            .collect();

        // Then the whole winner name ghosts (type-over display).
        let ghost = format!("mkdir{}", std::path::MAIN_SEPARATOR);
        assert!(
            rendered.contains(&format!("~/mkn{ghost}")),
            "input row was {rendered:?}"
        );
    }
}
