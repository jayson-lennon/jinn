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

/// Renders the cwd input popup over the frame's popup rect.
fn draw(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    input_state: &CwdInputState,
    theme: &jinn_theme::Theme,
    current_cwd: &std::path::Path,
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
    let current_cwd = std::path::PathBuf::from(ctx.fact(SESSION_CWD_FACT).unwrap_or_default());
    draw(frame, area, &state, &ctx.theme, &current_cwd);
}
