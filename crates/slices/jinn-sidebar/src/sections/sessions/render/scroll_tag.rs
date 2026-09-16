//! Scroll indicator tag rendering - DRY helper for ↑/↓ indicators.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Renders a single-character scroll indicator tag at the right edge of the area.
///
/// Used for the ↑ (content above) and ↓ (content below) indicators in the
/// sessions sidebar section.
pub(crate) fn render_scroll_tag(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    y: u16,
    style: Style,
) {
    let tag_area = Rect {
        x: area.x + area.width.saturating_sub(1),
        y,
        width: 1,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style))),
        tag_area,
    );
}
