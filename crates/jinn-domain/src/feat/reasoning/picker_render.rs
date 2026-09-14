//! Rendering for the reasoning effort picker overlay.

use crate::common::render_ctx::RenderCtx;
use crate::feat::ui::picker_states::PickerExt;
use jinn_selection_widget::SelectionWidget;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

/// Renders the reasoning effort picker overlay using [`SelectionWidget`].
///
/// Telescope-style layout matching the other picker popups (bordered popup,
/// filter on top, separator, scrollable entries):
/// bordered popup with filter input at top, separator, scrollable effort entries.
/// The footer shows the currently-active effort (the one marked `is_active` by
/// the loader), or "none" when nothing is active (both stores unset).
pub fn render_reasoning_effort_picker(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let state = ctx.state;
    let theme = &state.frontend.theme;

    let footer = {
        use ratatui::text::{Line, Span};
        let gray = Style::default().fg(theme.muted_text);
        // The loader already marked the resolved entry active; find it rather
        // than recomputing resolve_effort (AppState has no prefs access here).
        let active_name = state
            .frontend
            .reasoning_effort_picker()
            .items()
            .iter()
            .find(|e| e.is_active)
            .map_or("none", |e| e.name.as_str());
        Line::from(vec![
            Span::styled("Active: ".to_owned(), gray),
            Span::styled(
                active_name.to_owned(),
                Style::default().fg(theme.primary_text),
            ),
        ])
    };

    let widget = SelectionWidget::new(state.frontend.reasoning_effort_picker())
        .title(Line::from(" Reasoning Effort "))
        .title_style(Style::default().fg(theme.popup_title))
        .footer(footer);
    widget.render(frame, area);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::*;
    use crate::common::app_state::AppState;
    use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};
    use crate::feat::theme::default_theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[rstest::rstest]
    #[test]
    fn render_reasoning_effort_picker_does_not_panic_with_populated_picker() {
        // Given a state whose reasoning effort picker has a populated entry.
        let mut state = AppState::default();
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active: true,
            theme: default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);

        // When rendering the picker.
        // Then it does not panic.
        let area = Rect::new(0, 0, 60, 20);
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("terminal");
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = crate::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(&state, &slices, &overlay_views);
                render_reasoning_effort_picker(frame, area, &ctx);
            })
            .expect("draw");
    }
}
