//! [`Sidebar`] - the sidebar container that manages section registration,
//! focus delegation, section-crossing navigation, and rendering.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

use super::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use super::{mcp_servers_section, persona_section, pins, sessions, task_list_section};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::render_ctx::RenderCtx;
/// The sidebar container.
///
/// Holds registered sections in order, manages focus, and handles
/// section-crossing navigation for `j`/`k` intents.
#[derive(Debug)]
pub struct Sidebar {
    sections: Vec<Box<dyn SidebarSection>>,
}

impl Sidebar {
    /// Creates a new empty sidebar.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    /// Registers a section with the sidebar.
    ///
    /// Sections are rendered and navigated in registration order.
    pub fn register(&mut self, section: Box<dyn SidebarSection>) {
        self.sections.push(section);
    }

    /// Returns the number of registered sections.
    #[must_use]
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }

    /// Renders all sections within the given area.
    ///
    /// Applies a dark gray background to the entire sidebar area,
    /// then renders each section in registration order, stacking vertically.
    /// Sections receive their computed sub-area based on content height.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        // Clear sidebar area with dark gray background.
        let background =
            Block::default().style(Style::default().bg(ctx.state.frontend.theme.gutter_bg));
        frame.render_widget(background, area);

        // Pre-compute all section heights so we don't fight the borrow checker.
        let heights: Vec<u16> = self
            .sections
            .iter()
            .map(|s| s.content_height(ctx))
            .collect();
        let n = self.sections.len();

        // Render all sections except the last top-down.
        let mut y_offset = 0u16;
        for (i, section) in self.sections.iter_mut().enumerate() {
            let height = heights.get(i).copied().unwrap_or(0);
            if i == n - 1 {
                break; // handle last section separately
            }
            if height == 0 || y_offset >= area.height {
                continue;
            }
            let available = area.height.saturating_sub(y_offset);
            let section_height = height.min(available);
            let section_area = Rect {
                x: area.x,
                y: area.y + y_offset,
                width: area.width,
                height: section_height,
            };
            section.render(frame, section_area, ctx);
            y_offset += section_height;
        }

        // Render the last section (Sessions) anchored to the bottom.
        if n > 0 {
            let last_idx = n - 1;
            let height = heights.get(last_idx).copied().unwrap_or(0);
            if height > 0 {
                let bottom_y = area.height.saturating_sub(height);
                let section_y = bottom_y.max(y_offset);
                let available = area.height.saturating_sub(section_y);
                let section_height = height.min(available);
                let section_area = Rect {
                    x: area.x,
                    y: area.y + section_y,
                    width: area.width,
                    height: section_height,
                };
                if let Some(section) = self.sections.get_mut(last_idx) {
                    section.render(frame, section_area, ctx);
                }
            }
        }
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Sidebar topology - section-crossing navigation
// ---------------------------------------------------------------------------

/// Navigate the sidebar, handling section-crossing when a section exhausts its entries.
///
/// This is the single navigation entry point called by the IntentHandler.
/// Sections report `Exhausted` when they run out of entries; this function
/// decides whether to switch to an adjacent section or keep the cursor where it is.
pub fn navigate_sidebar(direction: &SidebarIntent, state: &mut AppState) {
    let focused = state
        .frontend
        .sidebar_section()
        .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona);
    let result = dispatch_navigate(focused, direction, state);

    if result == SectionNavResult::Exhausted {
        let neighbor = match direction {
            SidebarIntent::MoveDown => next_section(focused),
            SidebarIntent::MoveUp => prev_section(focused),
            SidebarIntent::Action(_) => return,
        };

        // Scan past consecutive empty sections.
        let mut candidate = neighbor;
        while let Some(target) = candidate {
            if section_has_content(target, state) {
                // Restore history position when leaving Pins.
                if focused == jinn_sidebar_msg::SidebarSectionId::Pins {
                    state.active_session_mut().restore_history_position();
                }
                clear_cursor(focused, state);
                state.frontend.scope_set_sidebar_section(target);
                let enter_from = match direction {
                    SidebarIntent::MoveDown => EnterFrom::Top,
                    SidebarIntent::MoveUp => EnterFrom::Bottom,
                    SidebarIntent::Action(_) => return,
                };
                receive_cursor(target, enter_from, state);
                return;
            }
            candidate = match direction {
                SidebarIntent::MoveDown => next_section(target),
                SidebarIntent::MoveUp => prev_section(target),
                SidebarIntent::Action(_) => return,
            };
        }
    }
}

fn dispatch_navigate(
    section: SidebarSectionId,
    intent: &SidebarIntent,
    state: &mut AppState,
) -> SectionNavResult {
    match section {
        jinn_sidebar_msg::SidebarSectionId::Persona => persona_section::navigate(intent, state),
        jinn_sidebar_msg::SidebarSectionId::Pins => pins::navigate(intent, state),
        jinn_sidebar_msg::SidebarSectionId::TaskList => task_list_section::navigate(intent, state),
        jinn_sidebar_msg::SidebarSectionId::McpServers => {
            mcp_servers_section::navigate(intent, state)
        }
        jinn_sidebar_msg::SidebarSectionId::Sessions => sessions::navigate(intent, state),
    }
}

fn next_section(id: SidebarSectionId) -> Option<SidebarSectionId> {
    match id {
        jinn_sidebar_msg::SidebarSectionId::Persona => {
            Some(jinn_sidebar_msg::SidebarSectionId::Pins)
        }
        jinn_sidebar_msg::SidebarSectionId::Pins => {
            Some(jinn_sidebar_msg::SidebarSectionId::TaskList)
        }
        jinn_sidebar_msg::SidebarSectionId::TaskList => {
            Some(jinn_sidebar_msg::SidebarSectionId::McpServers)
        }
        jinn_sidebar_msg::SidebarSectionId::McpServers => {
            Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
        }
        jinn_sidebar_msg::SidebarSectionId::Sessions => None,
    }
}

fn prev_section(id: SidebarSectionId) -> Option<SidebarSectionId> {
    match id {
        jinn_sidebar_msg::SidebarSectionId::Persona => None,
        jinn_sidebar_msg::SidebarSectionId::Pins => {
            Some(jinn_sidebar_msg::SidebarSectionId::Persona)
        }
        jinn_sidebar_msg::SidebarSectionId::TaskList => {
            Some(jinn_sidebar_msg::SidebarSectionId::Pins)
        }
        jinn_sidebar_msg::SidebarSectionId::McpServers => {
            Some(jinn_sidebar_msg::SidebarSectionId::TaskList)
        }
        jinn_sidebar_msg::SidebarSectionId::Sessions => {
            Some(jinn_sidebar_msg::SidebarSectionId::McpServers)
        }
    }
}

fn section_has_content(id: SidebarSectionId, state: &AppState) -> bool {
    match id {
        jinn_sidebar_msg::SidebarSectionId::Persona => true,
        jinn_sidebar_msg::SidebarSectionId::Pins => !state.sorted_pinned_ids().is_empty(),
        jinn_sidebar_msg::SidebarSectionId::TaskList => {
            !state.active_session().task_list().is_empty()
        }
        jinn_sidebar_msg::SidebarSectionId::McpServers => {
            let enabled = state.active_session().enabled_mcp_servers();
            state
                .frontend
                .preferences
                .mcp_server
                .iter()
                .any(|(name, _)| enabled.contains(name.as_str()))
        }
        jinn_sidebar_msg::SidebarSectionId::Sessions => !state.session.is_empty(),
    }
}

pub(crate) fn clear_cursor(id: SidebarSectionId, state: &mut AppState) {
    state.frontend.update_sections(|s| match id {
        jinn_sidebar_msg::SidebarSectionId::Persona => s.persona.cursor = None,
        jinn_sidebar_msg::SidebarSectionId::Pins => s.pins.clear_selection(),
        jinn_sidebar_msg::SidebarSectionId::TaskList => s.task_list.selected_phase_index = None,
        jinn_sidebar_msg::SidebarSectionId::McpServers => s.mcp_servers.selected_index = None,
        jinn_sidebar_msg::SidebarSectionId::Sessions => {
            s.sessions.selected_index = None;
        }
    });
}

fn receive_cursor(id: SidebarSectionId, enter_from: EnterFrom, state: &mut AppState) {
    match id {
        jinn_sidebar_msg::SidebarSectionId::Persona => {
            persona_section::receive_cursor(state, enter_from)
        }
        jinn_sidebar_msg::SidebarSectionId::Pins => pins::receive_cursor(state, enter_from),
        jinn_sidebar_msg::SidebarSectionId::TaskList => {
            task_list_section::receive_cursor(state, enter_from)
        }
        jinn_sidebar_msg::SidebarSectionId::McpServers => {
            mcp_servers_section::receive_cursor(state, enter_from)
        }
        jinn_sidebar_msg::SidebarSectionId::Sessions => sessions::receive_cursor(state, enter_from),
    }
}

/// Check if a section has a retained cursor.
fn section_has_cursor(id: SidebarSectionId, state: &AppState) -> bool {
    state.frontend.with_sections(
        |s| match id {
            jinn_sidebar_msg::SidebarSectionId::Persona => s.persona.cursor.is_some(),
            jinn_sidebar_msg::SidebarSectionId::Pins => s.pins.selected_id().is_some(),
            jinn_sidebar_msg::SidebarSectionId::TaskList => {
                s.task_list.selected_phase_index.is_some()
            }
            jinn_sidebar_msg::SidebarSectionId::McpServers => {
                s.mcp_servers.selected_index.is_some()
            }
            jinn_sidebar_msg::SidebarSectionId::Sessions => s.sessions.selected_index.is_some(),
        },
        || false,
    )
}

/// Jump directly to the next/previous sidebar section without clearing cursors.
///
/// Uses existing [`next_section`]/[`prev_section`] helpers, skipping empty sections.
/// Retains the leaving section's cursor position. If the target section has no
/// cursor (never visited), calls [`receive_cursor`] as fallback.
/// If the target has a retained cursor, ensures scroll offset is valid.
#[expect(
    clippy::else_if_without_else,
    reason = "no-op on fallthrough is intentional"
)]
pub fn jump_to_section(direction: &SidebarIntent, state: &mut AppState) {
    let focused = state
        .frontend
        .sidebar_section()
        .unwrap_or(jinn_sidebar_msg::SidebarSectionId::Persona);
    let neighbor_fn: fn(SidebarSectionId) -> Option<SidebarSectionId> = match direction {
        SidebarIntent::MoveDown => next_section,
        SidebarIntent::MoveUp => prev_section,
        SidebarIntent::Action(_) => return,
    };

    // Find the next non-empty section.
    let mut candidate = neighbor_fn(focused);
    while let Some(target) = candidate {
        if section_has_content(target, state) {
            // Restore history position when leaving Pins.
            if focused == jinn_sidebar_msg::SidebarSectionId::Pins
                && target != jinn_sidebar_msg::SidebarSectionId::Pins
            {
                state.active_session_mut().restore_history_position();
            }

            state.frontend.scope_set_sidebar_section(target);

            // Save history position when entering Pins without receive_cursor.
            if target == jinn_sidebar_msg::SidebarSectionId::Pins
                && !state.active_session().has_saved_history_position()
            {
                state.active_session_mut().save_history_position();
            }

            // If target has no cursor, call receive_cursor as fallback.
            if !section_has_cursor(target, state) {
                let enter_from = match direction {
                    SidebarIntent::MoveDown => EnterFrom::Top,
                    SidebarIntent::MoveUp => EnterFrom::Bottom,
                    SidebarIntent::Action(_) => return,
                };
                receive_cursor(target, enter_from, state);
            } else if target == jinn_sidebar_msg::SidebarSectionId::Pins {
                // Pins has a retained cursor - sync chat log to show it.
                pins::pins_section::sync_chat_log_cursor(state);
            } else if target == jinn_sidebar_msg::SidebarSectionId::Sessions {
                // Ensure scroll offset is valid for sessions.
                sessions::scroll_to_cursor(state);
            }
            return;
        }
        candidate = neighbor_fn(target);
    }
}
