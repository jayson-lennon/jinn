//! TUI rendering adapter for the vertical minimap column and arrow overlay.

use jinn_domain::RenderCtx;
use jinn_domain::feat::ui::vertical_minimap;
use ratatui::Frame;
use ratatui::layout::Rect;

/// Renders the vertical minimap blocks and the `>` arrow overlay.
///
/// The minimap renders into `minimap_area`. The arrow renders as an overlay
/// on the rightmost column of `chat_log_area`, pointing at the selected entry.
pub(super) fn render_minimap(
    frame: &mut Frame<'_>,
    minimap_area: Rect,
    chat_log_area: Rect,
    ctx: &RenderCtx,
) {
    let state = ctx.state;
    let focus_scope = state.frontend.scope();
    let theme = &state.frontend.theme;

    let arrow_color = if matches!(focus_scope, jinn_domain::FocusScope::Normal) {
        theme.focus_accent
    } else {
        theme.border_unfocused
    };
    let muted_text_color = state.frontend.theme.muted_text;

    let arrow =
        vertical_minimap::render_vertical_minimap(frame, minimap_area, state, muted_text_color);

    if let Some(ref arrow) = arrow {
        vertical_minimap::render_minimap_arrow(frame, chat_log_area, arrow, arrow_color);
    }
}
