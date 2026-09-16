//! Chat bottom line - horizontal separator at the bottom of the content area.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;

use jinn_domain::RenderCtx;

/// Renders the horizontal separator line (`─`) at the bottom of the content area.
///
/// Color reflects the current focus scope.
pub(super) fn render_chat_bottom_line(frame: &mut Frame<'_>, content_area: Rect, ctx: &RenderCtx) {
    let focus_scope = ctx.state.frontend.scope();
    let theme = &ctx.state.frontend.theme;

    let line_y = content_area.y + content_area.height.saturating_sub(1);
    let chat_line_color = if matches!(focus_scope, jinn_domain::FocusScope::Normal) {
        theme.focus_accent
    } else {
        theme.border_unfocused
    };
    let chat_line_style = Style::default().fg(chat_line_color);
    for x in content_area.x..(content_area.x + content_area.width) {
        if let Some(cell) = frame.buffer_mut().cell_mut((x, line_y)) {
            cell.set_symbol("\u{2500}");
            cell.set_style(chat_line_style);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test code, panics are acceptable"
    )]
    use jinn_domain::FocusScope;
    use jinn_testutil::setup_term;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    use crate::render::app_layout::AppLayout;

    fn frame_area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn chat_bottom_line_is_yellow_when_normal_scope() {
        // Given a TuiApp rendered with Normal scope.
        let mut app = crate::TuiApp::test_builder().build().await;
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .scope_clear_overlays();
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .unwrap();

        // Then the chat bottom line (last row of content area) is Yellow.
        let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
        let line_y = layout.content.y + layout.content.height - 1;
        let buffer = terminal.backend().buffer();
        let cell = buffer
            .cell((layout.content.x, line_y))
            .expect("chat bottom line cell");
        assert_eq!(cell.symbol(), "\u{2500}");
        assert_eq!(cell.fg, Color::Yellow);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn chat_bottom_line_is_darkgray_when_input_scope() {
        // Given a TuiApp rendered with Input scope.
        let mut app = crate::TuiApp::test_builder().build().await;
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .scope_push(FocusScope::Input);
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .unwrap();

        // Then the chat bottom line is DarkGray.
        let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
        let line_y = layout.content.y + layout.content.height - 1;
        let buffer = terminal.backend().buffer();
        let cell = buffer
            .cell((layout.content.x, line_y))
            .expect("chat bottom line cell");
        assert_eq!(cell.fg, Color::DarkGray);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn chat_bottom_line_is_darkgray_when_sidebar_scope() {
        // Given a TuiApp rendered with Sidebar scope.
        let mut app = crate::TuiApp::test_builder().build().await;
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .scope_push(jinn_slices::SidebarSectionId::Persona.focus_scope());
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .unwrap();

        // Then the chat bottom line is DarkGray.
        let layout = AppLayout::new(frame_area(80, 24), 1, 12, 30);
        let line_y = layout.content.y + layout.content.height - 1;
        let buffer = terminal.backend().buffer();
        let cell = buffer
            .cell((layout.content.x, line_y))
            .expect("chat bottom line cell");
        assert_eq!(cell.fg, Color::DarkGray);
    }
}
