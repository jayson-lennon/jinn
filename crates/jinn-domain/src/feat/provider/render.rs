//! Provider picker rendering - renders the provider picker overlay.

use crate::common::render_ctx::RenderCtx;
use jinn_selection_widget::SelectionWidget;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;

use super::entries;

/// Renders the provider picker overlay using [`SelectionWidget`].
///
/// Telescope-style layout: bordered popup with filter input at top,
/// horizontal separator, scrollable results, and a footer line.
pub fn render_provider_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let selected_count = state
        .provider
        .provider_picker
        .items()
        .iter()
        .filter(|e| e.selected)
        .count();
    let footers = entries::format_footers(
        state.provider.model_cache.as_ref(),
        area.width as usize,
        &state.frontend.theme,
        selected_count,
        state.provider.is_alloy_mode(),
    );
    let widget = SelectionWidget::new(&state.provider.provider_picker)
        .title(ratatui::text::Line::from(" Model "))
        .title_style(Style::default().fg(state.frontend.theme.popup_title))
        .footers(footers);
    widget.render(frame, area);
}
