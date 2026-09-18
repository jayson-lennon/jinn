//! Render for the project-add input popup.
//!
//! A centered overlay that lets the user type an absolute or relative path to
//! register as a new project directory. A live-validation footer shows the
//! resolved path (green check) or the reason it is invalid (red x) on every
//! keystroke. Mirrors the layout of the cwd slice's popup renderer.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

use jinn_cwd_msg::{CwdResolution, resolve_cwd_input};
use jinn_slices::RenderFacts;

use super::state::ProjectAddInputState;

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
fn validation_footer<'a>(resolution: &'a CwdResolution, theme: &jinn_theme::Theme) -> Line<'a> {
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

/// Renders the project-add input popup over the frame's popup rect.
///
/// # Panics
///
/// Panics if the project-add cell is not registered — the overlay only
/// renders when the slice that owns it activated.
#[expect(
    clippy::expect_used,
    reason = "the overlay only renders when the scope registered its cell"
)]
pub fn render_project_add_input(frame: &mut Frame<'_>, area: Rect, ctx: &RenderFacts) {
    // `area` is the overlay rect the geometry fn computed (the centered
    // popup rect) — draw into it directly.
    let cell: jinn_slices::TypedCell<ProjectAddInputState> = ctx
        .slices
        .reader(&super::intent::project_add_slot())
        .expect("project-add overlay renders only when its cell is registered");
    let state = cell.read();
    let current_cwd =
        std::path::PathBuf::from(ctx.fact(super::SESSION_CWD_FACT).unwrap_or_default());
    draw(frame, area, &state, &ctx.theme, &current_cwd);
}

/// Draws the popup: title border, input line with live cursor, and the
/// validation footer resolved against `current_cwd`.
fn draw(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    input_state: &ProjectAddInputState,
    theme: &jinn_theme::Theme,
    current_cwd: &std::path::Path,
) {
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

/// The overlay geometry fn registered on the host: the centered popup rect.
///
/// The `&Rect` argument and `Option` return mirror [`jinn_slices::OverlayFn`]'s
/// signature, which the host registers this function under.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    clippy::unnecessary_wraps,
    reason = "signature dictated by the OverlayFn registration type"
)]
pub fn project_add_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(project_add_input_popup_rect(*area))
}
