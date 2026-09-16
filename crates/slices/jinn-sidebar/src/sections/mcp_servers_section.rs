//! [`McpServersSection`] — the MCP servers sidebar section.
//!
//! Implements [`SidebarSection`] for displaying the active session's MCP
//! servers. Reads the global catalog from `frontend.preferences.mcp_server`
//! and shows only the servers enabled for the active session, overlaying their
//! live connection status. Disabled servers are omitted entirely — they appear
//! only once enabled. Each enabled server renders one row with a visual
//! treatment matching its status:
//!
//! - **starting** (enabled but no status yet, or status `Starting`) — yellow
//! - **running** (status `Running`) — green
//! - **dead** (status `Dead`) — red
//!
//! The section is read-only: navigation works (j/k), but there are no
//! section-specific actions (enable/disable is done via the picker).

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::render_ctx::RenderCtx;
use jinn_domain::feat::mcp_actor::protocol::McpConnectionStatus;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Solid full block used as the selection indicator (same as other sections).
const SELECTED_INDICATOR: &str = "\u{2588}";
/// One space used as the unselected border (same as other sections).
const UNSELECTED_BORDER: &str = " ";
pub use jinn_slices::McpServersSectionState;

/// The effective visual state of a single (enabled) server row.
///
/// Derived from the optional live status. A server that is enabled but has
/// not yet reported a status is treated as [`Self::Starting`] — it occupies
/// the gap between "user toggled on" and "first `McpServerStatus` arrived".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerRowState {
    /// Enabled and coming up (or explicitly reporting `Starting`).
    Starting,
    /// Live connection established and tools registered.
    Running,
    /// Connection failed or was torn down.
    Dead,
}

impl ServerRowState {
    /// Maps the optional live status into a row state.
    fn derive(status: Option<McpConnectionStatus>) -> Self {
        match status {
            None | Some(McpConnectionStatus::Starting) => Self::Starting,
            Some(McpConnectionStatus::Running) => Self::Running,
            Some(McpConnectionStatus::Dead) => Self::Dead,
        }
    }

    /// Returns the textual label shown after the server name.
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Dead => "dead",
        }
    }

    /// Returns the color used for the status label and indicator.
    fn color(self) -> Color {
        match self {
            Self::Starting => Color::Yellow,
            Self::Running => Color::Green,
            Self::Dead => Color::Red,
        }
    }
}

/// Collects the names of servers enabled for the active session, in catalog order.
///
/// Only enabled servers are surfaced in the sidebar; disabled ones are omitted
/// entirely (they are toggled on via the picker).
pub(crate) fn enabled_server_names(state: &AppState) -> Vec<String> {
    let enabled = state.active_session().enabled_mcp_servers();
    state
        .frontend
        .preferences
        .mcp_server
        .iter()
        .filter(|(name, _)| enabled.contains(name.as_str()))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Navigate within the MCP servers section.
///
/// Moves the cursor up/down through the enabled-servers list. Exhausts at
/// the list boundaries so the sidebar can move focus to the neighbor section.
/// The section does NOT modify its cursor on exhaustion.
pub fn navigate(intent: &SidebarIntent, state: &mut AppState) -> SectionNavResult {
    let count = enabled_server_names(state).len();
    if count == 0 {
        return SectionNavResult::Exhausted;
    }
    let max_index = count - 1;
    let current = state
        .frontend
        .with_sections(|s| s.mcp_servers.selected_index, || None)
        .unwrap_or(0);
    match intent {
        SidebarIntent::MoveDown => {
            if current >= max_index {
                SectionNavResult::Exhausted
            } else {
                state
                    .frontend
                    .update_sections(|s| s.mcp_servers.selected_index = Some(current + 1));
                SectionNavResult::Moved
            }
        }
        SidebarIntent::MoveUp => {
            if current == 0 {
                SectionNavResult::Exhausted
            } else {
                state
                    .frontend
                    .update_sections(|s| s.mcp_servers.selected_index = Some(current - 1));
                SectionNavResult::Moved
            }
        }
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    }
}

/// Place the cursor on this section from a given direction.
///
/// Positions at the edge of the list: index 0 from top, last index from bottom.
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom) {
    let count = enabled_server_names(state).len();
    if count == 0 {
        return;
    }
    let index = match enter_from {
        EnterFrom::Top => 0,
        EnterFrom::Bottom => count - 1,
    };
    state
        .frontend
        .update_sections(|s| s.mcp_servers.selected_index = Some(index));
}

/// The MCP servers sidebar section.
///
/// Renders a header followed by one row per server enabled for the active
/// session, overlaying live status. Disabled servers are omitted entirely.
#[derive(Debug)]
pub struct McpServersSection;

impl SidebarSection for McpServersSection {
    fn id(&self) -> SidebarSectionId {
        jinn_slices::SidebarSectionId::McpServers
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_slices::SidebarSectionId::McpServers)
            );

        let cursor = state
            .frontend
            .with_sections(|s| s.mcp_servers.selected_index, || None);
        let theme = &state.frontend.theme;

        let indicator_color = if sidebar_focused {
            theme.focus_accent
        } else {
            theme.border_unfocused
        };

        let lines = {
            let enabled = state.active_session().enabled_mcp_servers();
            let statuses = state.active_session().mcp_server_status();
            // Only enabled servers are surfaced; disabled ones are omitted entirely.
            let servers: Vec<_> = state
                .frontend
                .preferences
                .mcp_server
                .iter()
                .filter(|(name, _)| enabled.contains(name.as_str()))
                .collect();

            let mut lines = Vec::new();
            // Header.
            lines.push(Line::from(vec![Span::styled(
                " MCP servers",
                Style::default()
                    .fg(theme.primary_text)
                    .add_modifier(Modifier::BOLD),
            )]));
            // Blank separator.
            lines.push(Line::from(""));

            for (index, (name, _server)) in servers.iter().enumerate() {
                let is_selected = section_focused && cursor == Some(index);
                let row_state = ServerRowState::derive(statuses.get(name.as_str()).copied());

                let indicator = if is_selected {
                    Span::styled(SELECTED_INDICATOR, Style::default().fg(indicator_color))
                } else {
                    Span::raw(UNSELECTED_BORDER)
                };

                let name_style = if is_selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };

                lines.push(Line::from(vec![
                    indicator,
                    Span::styled(format!(" {name}"), name_style),
                    Span::raw(" "),
                    Span::styled(row_state.label(), Style::default().fg(row_state.color())),
                ]));
            }
            lines
        };

        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
        frame.render_widget(widget, area);
    }

    fn content_height(&self, ctx: &RenderCtx) -> u16 {
        // Collapsed to 0 when no servers are enabled for the active session,
        // matching the Pins/TaskList pattern so disabled servers waste no space.
        let enabled = ctx.state.active_session().enabled_mcp_servers();
        let count = ctx
            .state
            .frontend
            .preferences
            .mcp_server
            .iter()
            .filter(|(name, _)| enabled.contains(name.as_str()))
            .count();
        if count == 0 {
            return 0;
        }
        // header(1) + blank(1) + one row per enabled server + trailing gap(1).
        let rows = u16::try_from(count).unwrap_or(u16::MAX);
        rows.saturating_add(3)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::McpServersSection;
    use super::{navigate, receive_cursor};
    use crate::sections::section_trait::{
        EnterFrom, SectionNavResult, SidebarIntent, SidebarSection,
    };
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::render_ctx::RenderCtx;
    use jinn_domain::feat::mcp::McpServerConfig;
    use jinn_domain::feat::mcp_actor::protocol::McpConnectionStatus;
    use jinn_testutil::setup_term;

    fn server(name: &str) -> (String, McpServerConfig) {
        (
            name.to_owned(),
            McpServerConfig {
                command: Some("echo".to_owned()),
                args: vec![],
                ..Default::default()
            },
        )
    }

    fn render_rows(state: &AppState, width: u16, height: u16) -> Vec<String> {
        let mut section = McpServersSection;
        let (mut terminal, area) = setup_term(width, height);
        terminal
            .draw(|frame| {
                let slices = jinn_slices::Slices::new();
                let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
                let ctx = RenderCtx::new(state, &slices, &overlay_views);
                section.render(frame, area, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map_or(" ", ratatui::buffer::Cell::symbol)
                    })
                    .collect()
            })
            .collect()
    }

    fn state_with_servers(servers: &[(String, McpServerConfig)]) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.preferences.mcp_server = servers.iter().cloned().collect();
        state
    }

    #[rstest::rstest]
    fn section_id_is_mcp_servers() {
        // Given an McpServersSection.
        let section = McpServersSection;

        // When asking for its ID.
        // Then it returns McpServers.
        assert_eq!(section.id(), jinn_slices::SidebarSectionId::McpServers);
    }

    #[rstest::rstest]
    fn render_shows_header() {
        // Given state with no configured servers.
        let state = state_with_servers(&[]);

        // When rendering.
        let rows = render_rows(&state, 30, 5);

        // Then the first row contains the MCP servers header.
        assert!(rows[0].contains("MCP servers"));
    }

    #[rstest::rstest]
    fn render_disabled_server_is_hidden() {
        // Given a configured server not enabled for the active session.
        let state = state_with_servers(&[server("excalimate")]);

        // When rendering.
        let rows = render_rows(&state, 40, 5);

        // Then the disabled server does not appear at all.
        let combined = rows.join("\n");
        assert!(
            !combined.contains("excalimate"),
            "disabled servers must not render; got: {combined}"
        );
    }

    #[rstest::rstest]
    fn render_enabled_no_status_shows_starting() {
        // Given an enabled server with no status event yet.
        let mut state = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");

        // When rendering.
        let rows = render_rows(&state, 40, 5);

        // Then the row shows the starting label.
        let combined = rows.join("\n");
        assert!(
            combined.contains("starting"),
            "enabled-but-no-status should render as starting; got: {combined}"
        );
    }

    #[rstest::rstest]
    fn render_running_status_shows_running() {
        // Given an enabled server reporting Running.
        let mut state = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        state
            .active_session_mut()
            .set_mcp_server_status("excalimate", McpConnectionStatus::Running);

        // When rendering.
        let rows = render_rows(&state, 40, 5);

        // Then the row shows the running label.
        let combined = rows.join("\n");
        assert!(combined.contains("running"));
    }

    #[rstest::rstest]
    fn render_dead_status_shows_dead() {
        // Given an enabled server reporting Dead.
        let mut state = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        state
            .active_session_mut()
            .set_mcp_server_status("excalimate", McpConnectionStatus::Dead);

        // When rendering.
        let rows = render_rows(&state, 40, 5);

        // Then the row shows the dead label.
        let combined = rows.join("\n");
        assert!(combined.contains("dead"));
    }

    #[rstest::rstest]
    fn render_only_active_session_servers() {
        // Given two configured servers: alpha enabled for the active session (A),
        // beta enabled only for a different session (B).
        use jinn_domain::protocol::SessionId;
        let mut state = state_with_servers(&[server("alpha"), server("beta")]);
        let session_b = SessionId::new();
        state
            .session
            .get_or_create(&session_b)
            .enable_mcp_server("beta");
        state.active_session_mut().enable_mcp_server("alpha");

        // When rendering the active session (A).
        let rows = render_rows(&state, 40, 6);

        // Then alpha (enabled for A) renders, and beta (enabled only for B)
        // does not leak into the active session's render.
        let combined = rows.join("\n");
        assert!(combined.contains("alpha"));
        assert!(
            !combined.contains("beta"),
            "a server enabled only for another session must not render; got: {combined}"
        );
        assert!(combined.contains("starting"));
    }

    #[rstest::rstest]
    #[test]
    fn content_height_is_zero_when_none_enabled() {
        // Given configured servers, none enabled for the active session.
        let state = state_with_servers(&[server("alpha"), server("beta")]);
        let section = McpServersSection;

        // When computing the content height.
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

        // Then the section collapses to zero height (hidden).
        assert_eq!(
            height, 0,
            "section must be hidden when no servers are enabled"
        );
    }

    #[rstest::rstest]
    #[test]
    fn content_height_counts_only_enabled_servers() {
        // Given three configured servers, two enabled for the active session.
        let mut state = state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");
        let section = McpServersSection;

        // When computing the content height.
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

        // Then it counts only the enabled servers:
        // header(1) + blank(1) + 2 rows + trailing gap(1) = 5.
        assert_eq!(height, 5, "height must count enabled servers only");
    }

    #[rstest::rstest]
    #[test]
    fn navigate_exhausts_at_enabled_subset_boundary() {
        // Given two enabled servers with the cursor on the last one.
        let mut state = state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");
        state
            .frontend
            .update_sections(|s| s.mcp_servers.selected_index = Some(1)); // last enabled

        // When moving down past the last enabled server.
        let result = navigate(&SidebarIntent::MoveDown, &mut state);

        // Then navigation exhausts (only 2 enabled servers, indices 0 and 1).
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_enters_enabled_subset_from_top() {
        // Given enabled servers (alpha, gamma) with no cursor.
        let mut state = state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");

        // When entering the section from the top.
        receive_cursor(&mut state, EnterFrom::Top);

        // Then the cursor lands on the first enabled server (index 0).
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.mcp_servers.selected_index, || None),
            Some(0)
        );
    }
}
