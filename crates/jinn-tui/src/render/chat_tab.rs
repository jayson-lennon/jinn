//! Chat tab rendering - dispatches to individual chat sub-components.

pub mod audit_popup;
pub mod autocomplete;
pub mod border;
pub mod chat_bottom_line;
pub mod chat_log;
pub mod input_box;
pub mod minimap;

pub mod sidebar;
pub mod streaming_indicator;

use jinn_domain::AppUiRegistry;
use jinn_domain::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::app_layout::AppLayout;

/// Renders the Chat tab content - chat log, streaming indicator,
/// queue, bottom line, input box, and autocomplete popup.
///
/// Called from the top-level renderer when the chat view is active.
/// Does NOT render the sidebar or border (those are rendered at top level).
/// Computes sub-areas from the layout and delegates to individual render functions.
/// Selectable rects are collected into `rects` for mouse selection support.
pub(super) fn render_chat_tab(
    ui_registry: &mut AppUiRegistry,
    frame: &mut Frame<'_>,
    layout: &AppLayout,
    ctx: &RenderCtx,
    rects: &mut Vec<Rect>,
) {
    let sidebar_focused = ctx.state.frontend.is_sidebar();

    // Compute sub-areas at the bottom of the content area.
    let content_area = layout.content;
    let bottom_lines = 2; // indicator + chat bottom line

    let chat_log_area = if content_area.height > bottom_lines {
        Rect {
            x: content_area.x,
            y: content_area.y,
            width: content_area.width,
            height: content_area.height - bottom_lines,
        }
    } else {
        content_area
    };

    // Chat log.
    chat_log::render_chat_log(
        ui_registry,
        frame,
        chat_log_area,
        content_area,
        sidebar_focused,
        ctx,
        rects,
    );

    // Audit popup overlay - renders above the chat log when toggled on.
    audit_popup::render_audit_popup(frame, chat_log_area, ctx, rects);

    // Streaming indicator.
    let indicator_y = content_area.y + content_area.height.saturating_sub(bottom_lines);
    let indicator_area = Rect {
        x: content_area.x,
        y: indicator_y,
        width: content_area.width,
        height: 1,
    };
    streaming_indicator::render_streaming_indicator(ui_registry, frame, indicator_area, ctx);

    // Cancel stream prompt - overlay at bottom of chat log area.

    if ctx.state.frontend.cancel_stream_prompt {
        let prompt_area = Rect {
            x: chat_log_area.x,
            y: chat_log_area.y + chat_log_area.height.saturating_sub(1),
            width: chat_log_area.width,
            height: 1,
        };
        let prompt = Paragraph::new(Line::from(Span::styled(
            " Press ESC again to cancel ",
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )));
        frame.render_widget(prompt, prompt_area);
    }

    // Chat bottom line.
    chat_bottom_line::render_chat_bottom_line(frame, content_area, ctx);

    // Input box.
    input_box::render_input_box(ui_registry, frame, layout.input, ctx);

    // Autocomplete popup.
    autocomplete::render_autocomplete(frame, layout.input, ctx);

    // Vertical minimap column and `>` arrow overlay.
    minimap::render_minimap(frame, layout.minimap, chat_log_area, ctx);
}
