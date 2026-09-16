//! The sidebar's overlay registration: the rename-session popup.
//!
//! The popup renders through the shared overlay registry — a geometry fn
//! computes the centered rect, and the view draws into it reading the
//! in-progress text from the sections cell and the theme from the facts.

use crate::sections::rename_input::render::rename_session_popup_rect;
use jinn_sidebar_msg::{SidebarSections, sidebar_sections_slot};
use jinn_slices::RenderFacts;
use jinn_slices::cell::TypedCell;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

/// The overlay-rect function registered on the slice host: the centered
/// rename popup rect.
// The `&Rect` parameter and the `Option` return follow the overlay
// registry's `OverlayFn` contract (geometry may reject too-small frames).
pub fn rename_overlay_rect(area: &Rect) -> Option<Rect> {
    Some(rename_session_popup_rect(*area))
}

/// The overlay view: draws the rename popup into `area` (the geometry
/// rect), reading the edit state from the sections cell.
///
/// # Panics
///
/// Panics if the sidebar sections slot is not registered — the overlay
/// only renders when the slice that owns it activated.
#[expect(
    clippy::expect_used,
    reason = "the overlay only renders when the sidebar registered its cell"
)]
pub fn render_rename_overlay(frame: &mut Frame<'_>, area: Rect, ctx: &RenderFacts) {
    let cell: TypedCell<SidebarSections> = ctx
        .slices
        .reader(&sidebar_sections_slot())
        .expect("rename overlay renders only when the sidebar cell is registered");
    let state = cell.read();
    draw(
        frame,
        area,
        &state.rename_input.text.input,
        state.rename_input.text.cursor_pos,
        &ctx.theme,
    );
}

/// Draws the popup chrome + input line into `popup_area`.
fn draw(
    frame: &mut Frame<'_>,
    popup_area: Rect,
    input_text: &str,
    input_cursor: usize,
    theme: &jinn_theme::Theme,
) {
    let title = Line::from(Span::styled(
        " Rename Session ",
        Style::default().fg(theme.popup_title),
    ));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_unfocused));

    frame.render_widget(Clear, popup_area);
    frame.render_widget(block, popup_area);

    let inner = Rect {
        x: popup_area.x + 1,
        y: popup_area.y + 1,
        width: popup_area.width.saturating_sub(2),
        height: popup_area.height.saturating_sub(2),
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let prefix = Span::styled("> ", Style::default().fg(theme.focus_accent));
    let input_span = Span::raw(input_text);
    let input_line = Line::from(vec![prefix, input_span]);
    frame.render_widget(
        Paragraph::new(input_line),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    let prefix_len = 2u16;
    let grapheme_count = input_text
        .get(..input_cursor)
        .map_or(0, |s| s.graphemes(true).count());
    let cursor_x = (prefix_len + grapheme_count as u16).min(inner.width.saturating_sub(1));
    frame.set_cursor_position((inner.x.saturating_add(cursor_x), inner.y));
}
