//! Tab bar — top-level strip showing one label per registered tab slice.

use jinn_domain::RenderCtx;
use jinn_slices::SliceScopeId;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// The chat tab, which is always present and first in the order.
const CHAT_TAB: &str = "Chat";

/// The active application layout, selected by the base focus scope.
///
/// Tab metadata (label, order) is declared by slices at activation; the
/// chat tab is the always-present fallback, so an app with no
/// registered tab slices renders exactly one tab.
fn tab_labels(slices: &jinn_slices::Slices) -> Vec<String> {
    let mut labels = vec![CHAT_TAB.to_owned()];
    for scope in slices.tab_scopes() {
        labels.push(tab_label(&scope));
    }
    labels
}

/// The display label for a registered tab scope.
fn tab_label(scope: &SliceScopeId) -> String {
    // A slice's tab label: derived from the scope id's slice name,
    // title-cased (e.g. "dashboard/status" → "Dashboard"). Kept as a
    // pure derivation so the slice declares identity once (its scope
    // id) and the bar renders it consistently.
    scope
        .slice()
        .split(['-', '_'])
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            let mut label: String = first.to_uppercase().collect();
            label.push_str(chars.as_str());
            label
        }
        None => String::new(),
    }
}

/// Returns the highlighted tab index for the current base scope.
///
/// Chat is `0`; a registered tab scope highlights its position in the
/// declared order (chat + 1 + index). Unregistered scopes fall back to
/// chat. The terminal is an overlay (`<M-t>`), not a tab, so it never
/// highlights a tab.
fn active_tab_index(slices: &jinn_slices::Slices, ctx: &RenderCtx) -> usize {
    match ctx.state.frontend.scope_base() {
        jinn_domain::FocusScope::Dynamic(id) => slices
            .tab_scopes()
            .iter()
            .position(|scope| *scope == id)
            .map_or(0, |idx| idx + 1),
        _ => 0,
    }
}

/// Renders the tab bar into `area`.
pub fn render_tab_bar(frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
    let theme = &ctx.state.frontend.theme;
    let labels = tab_labels(ctx.slices);
    let active = active_tab_index(ctx.slices, ctx);

    let mut spans = Vec::new();
    for (idx, label) in labels.iter().enumerate() {
        let is_active = idx == active;

        let style = if is_active {
            Style::default()
                .fg(theme.tab_active_fg)
                .bg(theme.tab_active_bg)
        } else {
            Style::default().fg(theme.tab_inactive_fg)
        };

        spans.push(Span::styled(format!(" {label} "), style));
        if idx + 1 < labels.len() {
            spans.push(Span::raw(" "));
        }
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line);
    frame.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use jinn_domain::FocusScope;
    use jinn_testutil::setup_term;
    use ratatui::style::Color;

    async fn build_app_with_scope(scope: FocusScope) -> crate::TuiApp {
        let app = crate::TuiApp::test_builder().build().await;
        app.core
            .state
            .write_test_no_cap()
            .frontend
            .scope_swap_base(scope);
        app
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn chat_tab_is_highlighted_in_normal_scope() {
        // Given an app in Normal (chat) scope.
        let mut app = build_app_with_scope(FocusScope::Normal).await;
        let (mut terminal, _area) = setup_term(80, 24);

        // When rendering.
        terminal.draw(|frame| app.render(frame)).unwrap();

        // Then the chat tab cell has an active background (non-Reset).
        let layout = crate::render::app_layout::AppLayout::new(
            ratatui::layout::Rect::new(0, 0, 80, 24),
            1,
            12,
            30,
        );
        let buffer = terminal.backend().buffer();
        let cell = buffer
            .cell((layout.tab_bar.x + 1, layout.tab_bar.y))
            .expect("chat tab cell");
        assert_ne!(
            cell.bg,
            Color::Reset,
            "chat tab should be highlighted in Normal scope"
        );
    }
}
