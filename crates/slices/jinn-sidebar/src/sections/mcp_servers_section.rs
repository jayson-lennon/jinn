//! [`McpServersSection`] — the MCP servers sidebar section.
//!
//! Implements [`SidebarSection`] for displaying the active session's MCP
//! servers. Reads the global catalog from the configuration layer's
//! `[mcp]` section and shows only the servers enabled for the active
//! session, overlaying their
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
use jinn_kernel::common::app_state::AppState;
use jinn_mcp_msg::McpConnectionStatus;
use jinn_preferences_config::schemas::mcp::{McpServerConfig, McpServersConfig};
use jinn_slices::ConfigLayer;
use jinn_slices::DrawContext;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

pub use jinn_sidebar_msg::McpServersSectionState;

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
pub(crate) fn enabled_server_names(state: &AppState, config: &ConfigLayer) -> Vec<String> {
    let enabled = state.active_session().enabled_mcp_servers();
    config
        .get::<McpServersConfig>()
        .unwrap_or_default()
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
pub fn navigate(
    intent: &SidebarIntent,
    state: &mut AppState,
    config: &ConfigLayer,
) -> SectionNavResult {
    let count = enabled_server_names(state, config).len();
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
pub fn receive_cursor(state: &mut AppState, enter_from: EnterFrom, config: &ConfigLayer) {
    let count = enabled_server_names(state, config).len();
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
        jinn_sidebar_msg::SidebarSectionId::McpServers
    }

    fn render(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        skip_rows: u16,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) {
        let state = ctx.state();
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_sidebar_msg::SidebarSectionId::McpServers)
            );

        let cursor = state
            .frontend
            .with_sections(|s| s.mcp_servers.selected_index, || None);
        let theme = &state.frontend.theme;

        let lines = {
            let enabled = state.active_session().enabled_mcp_servers().clone();
            let session_id = state.active_session().session_id().clone();
            let statuses = ctx
                .slices()
                .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot())
                .map_or_default(|runtime| runtime.read().effective_statuses(&session_id));
            // Only enabled servers are surfaced; disabled ones are omitted entirely.
            let configured = ctx.config().get::<McpServersConfig>().unwrap_or_default();
            let servers: Vec<(String, McpServerConfig)> = configured
                .iter()
                .filter(|(name, _)| enabled.contains(name.as_str()))
                .map(|(name, server)| (name.clone(), server.clone()))
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

                // The status label keeps its own color unselected — the state
                // signal — and yields to the band when the row is selected.
                let status = if is_selected {
                    Span::styled(row_state.label(), Style::new())
                } else {
                    Span::styled(row_state.label(), Style::default().fg(row_state.color()))
                };

                // The gutter column is kept: one dark cell, then a gap cell,
                // then the name.
                let content_width =
                    2 + name.chars().count() + 1 + row_state.label().chars().count();
                let mut spans = vec![
                    crate::sections::session_row_style::gutter_span(theme),
                    crate::sections::session_row_style::gutter_span(theme),
                    Span::raw(name.clone()),
                    Span::raw(" "),
                    status,
                ];
                if is_selected {
                    // The pad carries the band to the row's last cell —
                    // `Paragraph` does not extend a line's style past the
                    // last grapheme.
                    spans.push(crate::sections::session_row_style::band_pad(
                        content_width,
                        usize::from(area.width),
                        theme,
                    ));
                }
                let row = Line::from(spans);
                let row = if is_selected {
                    row.style(crate::sections::session_row_style::selected_row_style(
                        theme,
                    ))
                } else {
                    row
                };
                lines.push(row);
            }
            lines
        };

        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((skip_rows, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
        // Collapsed to 0 when no servers are enabled for the active session,
        // matching the Pins/TaskList pattern so disabled servers waste no space.
        let enabled = ctx.state().active_session().enabled_mcp_servers();
        let count = ctx
            .config()
            .get::<McpServersConfig>()
            .unwrap_or_default()
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
    use jinn_config::ConfigLayer;
    use jinn_kernel::common::app_state::AppState;

    use jinn_kernel::common::render_test::{render_draw_with_config, with_render_ctx_and_config};
    use jinn_mcp_msg::McpConnectionStatus;
    use jinn_preferences_config::schemas::mcp::McpServerConfig;

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

    /// A layer whose `[mcp]` section configures the given servers — the
    /// section the section reads enabled names from.
    fn config_with_servers(servers: &[(String, McpServerConfig)]) -> ConfigLayer {
        let document = servers
            .iter()
            .map(|(name, _)| format!("[mcp.{name}]\ncommand = \"echo\"\n"))
            .collect::<Vec<_>>()
            .concat();
        jinn_config::testutil::config_layer(&document)
    }

    fn render_rows(state: &AppState, config: &ConfigLayer, width: u16, height: u16) -> Vec<String> {
        let slices = jinn_slices::Slices::new();
        render_rows_with_slices(state, config, width, height, &slices)
    }

    fn render_rows_with_slices(
        state: &AppState,
        config: &ConfigLayer,
        width: u16,
        height: u16,
        slices: &jinn_slices::Slices,
    ) -> Vec<String> {
        let mut section = McpServersSection;
        let terminal = render_draw_with_config(
            state,
            slices,
            config,
            width,
            height,
            |frame, area, frame_ctx| {
                section.render(frame, area, 0, frame_ctx);
            },
        );
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

    /// State over a document configuring `servers`, plus that document's
    /// layer — the servers live in config, not in the state snapshot.
    fn state_with_servers(servers: &[(String, McpServerConfig)]) -> (AppState, ConfigLayer) {
        (
            AppState::default_with_scope_focus(),
            config_with_servers(servers),
        )
    }

    #[rstest::rstest]
    fn section_id_is_mcp_servers() {
        // Given an McpServersSection.
        let section = McpServersSection;

        // When asking for its ID.
        // Then it returns McpServers.
        assert_eq!(section.id(), jinn_sidebar_msg::SidebarSectionId::McpServers);
    }

    #[rstest::rstest]
    fn render_shows_header() {
        // Given state with no configured servers.
        let (state, config) = state_with_servers(&[]);

        // When rendering.
        let rows = render_rows(&state, &config, 30, 5);

        // Then the first row contains the MCP servers header.
        assert!(rows[0].contains("MCP servers"));
    }

    #[rstest::rstest]
    fn render_disabled_server_is_hidden() {
        // Given a configured server not enabled for the active session.
        let (state, config) = state_with_servers(&[server("excalimate")]);

        // When rendering.
        let rows = render_rows(&state, &config, 40, 5);

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
        let (mut state, config) = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");

        // When rendering.
        let rows = render_rows(&state, &config, 40, 5);

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
        let (mut state, config) = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        let slices = jinn_slices::Slices::new();
        let session_id = state.active_session().session_id().clone();
        let runtime = slices
            .register(
                jinn_mcp_msg::mcp_runtime_slot(),
                jinn_mcp_msg::McpRuntimeState::default(),
            )
            .expect("MCP runtime cell");
        runtime.update(|runtime| {
            runtime.set_status(&session_id, "excalimate", McpConnectionStatus::Running);
        });

        // When rendering.
        let rows = render_rows_with_slices(&state, &config, 40, 5, &slices);

        // Then the row shows the running label.
        let combined = rows.join("\n");
        assert!(combined.contains("running"));
    }

    #[rstest::rstest]
    fn render_singleton_status_shows_running_without_a_session_entry() {
        // Given an enabled server whose Running status was published to the
        // singleton bucket (a singleton actor has no owning session).
        let (mut state, config) = state_with_servers(&[server("bundled-example")]);
        state
            .active_session_mut()
            .enable_mcp_server("bundled-example");
        let slices = jinn_slices::Slices::new();
        let runtime = slices
            .register(
                jinn_mcp_msg::mcp_runtime_slot(),
                jinn_mcp_msg::McpRuntimeState::default(),
            )
            .expect("MCP runtime cell");
        runtime.update(|runtime| {
            runtime.set_singleton_status("bundled-example", McpConnectionStatus::Running);
        });

        // When rendering.
        let rows = render_rows_with_slices(&state, &config, 40, 5, &slices);

        // Then the row shows running — the singleton status resolves for
        // every session, not just the one that spawned it.
        let combined = rows.join("\n");
        assert!(
            combined.contains("running"),
            "singleton Running must render as running; got: {combined}"
        );
    }

    #[rstest::rstest]
    fn render_dead_status_shows_dead() {
        // Given an enabled server reporting Dead.
        let (mut state, config) = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        let slices = jinn_slices::Slices::new();
        let session_id = state.active_session().session_id().clone();
        let runtime = slices
            .register(
                jinn_mcp_msg::mcp_runtime_slot(),
                jinn_mcp_msg::McpRuntimeState::default(),
            )
            .expect("MCP runtime cell");
        runtime.update(|runtime| {
            runtime.set_status(&session_id, "excalimate", McpConnectionStatus::Dead);
        });

        // When rendering.
        let rows = render_rows_with_slices(&state, &config, 40, 5, &slices);

        // Then the row shows the dead label.
        let combined = rows.join("\n");
        assert!(combined.contains("dead"));
    }

    #[rstest::rstest]
    fn render_only_active_session_servers() {
        // Given two configured servers: alpha enabled for the active session (A),
        // beta enabled only for a different session (B).
        use jinn_core_types::SessionId;
        let (mut state, config) = state_with_servers(&[server("alpha"), server("beta")]);
        let session_b = SessionId::new();
        state
            .session
            .get_or_create(&session_b)
            .enable_mcp_server("beta");
        state.active_session_mut().enable_mcp_server("alpha");

        // When rendering the active session (A).
        let rows = render_rows(&state, &config, 40, 6);

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
        let (state, config) = state_with_servers(&[server("alpha"), server("beta")]);
        let mut section = McpServersSection;

        // When computing the content height.
        let slices = jinn_slices::Slices::new();
        let height =
            with_render_ctx_and_config(&state, &slices, &config, |ctx| section.content_height(ctx));

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
        let (mut state, config) =
            state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");
        let mut section = McpServersSection;

        // When computing the content height.
        let slices = jinn_slices::Slices::new();
        let height =
            with_render_ctx_and_config(&state, &slices, &config, |ctx| section.content_height(ctx));

        // Then it counts only the enabled servers:
        // header(1) + blank(1) + 2 rows + trailing gap(1) = 5.
        assert_eq!(height, 5, "height must count enabled servers only");
    }

    #[rstest::rstest]
    #[test]
    fn navigate_exhausts_at_enabled_subset_boundary() {
        // Given two enabled servers with the cursor on the last one.
        let (mut state, config) =
            state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");
        state
            .frontend
            .update_sections(|s| s.mcp_servers.selected_index = Some(1)); // last enabled

        // When moving down past the last enabled server.
        let result = navigate(&SidebarIntent::MoveDown, &mut state, &config);

        // Then navigation exhausts (only 2 enabled servers, indices 0 and 1).
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    #[test]
    fn receive_cursor_enters_enabled_subset_from_top() {
        // Given enabled servers (alpha, gamma) with no cursor.
        let (mut state, config) =
            state_with_servers(&[server("alpha"), server("beta"), server("gamma")]);
        state.active_session_mut().enable_mcp_server("alpha");
        state.active_session_mut().enable_mcp_server("gamma");

        // When entering the section from the top.
        receive_cursor(&mut state, EnterFrom::Top, &config);

        // Then the cursor lands on the first enabled server (index 0).
        assert_eq!(
            state
                .frontend
                .with_sections(|s| s.mcp_servers.selected_index, || None),
            Some(0)
        );
    }

    /// Renders the section and returns the buffer, so a test can read the
    /// styles the user actually sees.
    fn render_buffer(
        state: &AppState,
        config: &ConfigLayer,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let mut section = McpServersSection;
        let slices = jinn_slices::Slices::new();
        let terminal = render_draw_with_config(
            state,
            &slices,
            config,
            width,
            height,
            |frame, area, frame_ctx| {
                section.render(frame, area, 0, frame_ctx);
            },
        );
        terminal.backend().buffer().clone()
    }

    #[rstest::rstest]
    #[test]
    fn a_selected_mcp_row_bands_the_full_width_with_a_dark_gutter() {
        // Given an enabled server and the section's cursor on it.
        let (mut state, config) = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::McpServers.focus_scope());
        state
            .frontend
            .update_sections(|s| s.mcp_servers.selected_index = Some(0));
        let theme = state.frontend.theme.clone();

        // When rendering wide.
        let width = 50u16;
        let buffer = render_buffer(&state, &config, width, 8);

        // Then the selected row carries the band...
        let band_y = (0..8)
            .find(|&y| {
                (0..width).any(|x| {
                    buffer
                        .cell((x, y))
                        .is_some_and(|cell| cell.bg == theme.selection_fg)
                })
            })
            .unwrap_or_else(|| panic!("no selection band rendered"));
        let last_banded_x = (0..width)
            .filter(|&x| {
                buffer
                    .cell((x, band_y))
                    .is_some_and(|cell| cell.bg == theme.selection_fg)
            })
            .max();
        assert_eq!(
            last_banded_x,
            Some(width.saturating_sub(1)),
            "the band must reach the row's last cell"
        );
        // And the gutter cell at column 0 stays on the dark sidebar
        // background.
        let gutter = buffer.cell((0, band_y)).expect("gutter cell");
        assert_eq!(gutter.bg, theme.gutter_bg, "the gutter cell stays dark");
        assert_eq!(gutter.symbol(), " ", "the gutter is a blank");
    }

    #[rstest::rstest]
    #[test]
    fn an_mcp_status_label_takes_the_selection_band() {
        // Given an enabled, running server selected by the cursor.
        let (mut state, config) = state_with_servers(&[server("excalimate")]);
        state.active_session_mut().enable_mcp_server("excalimate");
        let slices = jinn_slices::Slices::new();
        let session_id = state.active_session().session_id().clone();
        let runtime = slices
            .register(
                jinn_mcp_msg::mcp_runtime_slot(),
                jinn_mcp_msg::McpRuntimeState::default(),
            )
            .expect("MCP runtime cell");
        runtime.update(|runtime| {
            runtime.set_status(&session_id, "excalimate", McpConnectionStatus::Running);
        });
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::McpServers.focus_scope());
        state
            .frontend
            .update_sections(|s| s.mcp_servers.selected_index = Some(0));
        let theme = state.frontend.theme.clone();

        // When rendering.
        let width = 50u16;
        let terminal = render_draw_with_config(
            &state,
            &slices,
            &config,
            width,
            8,
            |frame, area, frame_ctx| {
                McpServersSection.render(frame, area, 0, frame_ctx);
            },
        );
        let buffer = terminal.backend().buffer().clone();

        // Then the status label's cells sit on the selection background — the
        // state color yields to the band when the row is selected.
        let band_y = (0..8)
            .find(|&y| {
                (0..width).any(|x| {
                    buffer
                        .cell((x, y))
                        .is_some_and(|cell| cell.bg == theme.selection_fg)
                })
            })
            .unwrap_or_else(|| panic!("no selection band rendered"));
        let text: String = (0..width)
            .filter_map(|x| buffer.cell((x, band_y)).map(ratatui::buffer::Cell::symbol))
            .collect();
        let label_at = text.find("running").expect("running label visible");
        let label_cell = buffer
            .cell((u16::try_from(label_at).unwrap_or(0), band_y))
            .expect("label cell");
        assert_eq!(label_cell.bg, theme.selection_fg);
        // And the label's text is the band's text color, not the state color.
        assert_eq!(label_cell.fg, theme.gutter_bg);
    }
}
