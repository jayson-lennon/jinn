//! [`PersonaSection`] - the persona sidebar section.
//!
//! Implements [`SidebarSection`] for displaying the active persona.
//! Shows a header line and a single selectable entry with the persona name.
//! Pressing `e` while this section is focused opens the persona picker.

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::render_ctx::RenderCtx;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Solid full block used as the selection indicator (same as pins section).
const SELECTED_INDICATOR: &str = "\u{2588}";
/// One space used as the unselected border (same as pins section).
const UNSELECTED_BORDER: &str = " ";

pub use jinn_sidebar_msg::PersonaSectionState;

/// Navigate within the persona section.
///
/// Persona has a single entry, so any directional move exhausts immediately.
/// The section does NOT modify its cursor - the sidebar decides what to do.
pub fn navigate(intent: &SidebarIntent, _state: &mut AppState) -> SectionNavResult {
    match intent {
        SidebarIntent::MoveDown | SidebarIntent::MoveUp => SectionNavResult::Exhausted,
        SidebarIntent::Action(_) => SectionNavResult::Moved,
    }
}

/// Place the cursor on this section from a given direction.
pub fn receive_cursor(state: &mut AppState, _enter_from: EnterFrom) {
    state
        .frontend
        .update_sections(|s| s.persona.cursor = Some(0));
}

/// The persona sidebar section.
///
/// Renders the active persona as a single selectable entry.
#[derive(Debug)]
pub struct PersonaSection;

impl SidebarSection for PersonaSection {
    fn id(&self) -> SidebarSectionId {
        jinn_sidebar_msg::SidebarSectionId::Persona
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &RenderCtx) {
        let state = ctx.state;
        let sidebar_focused = state.frontend.is_sidebar();
        let section_focused = sidebar_focused
            && matches!(
                state.frontend.sidebar_section(),
                Some(jinn_sidebar_msg::SidebarSectionId::Persona)
            );
        let theme = &state.frontend.theme;

        let indicator_color = if sidebar_focused {
            theme.focus_accent
        } else {
            theme.border_unfocused
        };

        let is_selected = section_focused
            && state
                .frontend
                .with_sections(|s| s.persona.cursor.is_some(), || false);
        let indicator = if is_selected {
            Span::styled(SELECTED_INDICATOR, Style::default().fg(indicator_color))
        } else {
            Span::raw(UNSELECTED_BORDER)
        };

        // Read persona from the active session, not the global default.
        // This ensures the sidebar reflects the current session's persona
        // immediately when switching between sessions.
        let persona_name = state.active_session().persona_name();

        let lines = {
            let mut lines = Vec::new();
            // Header.
            lines.push(Line::from(vec![Span::styled(
                " Persona",
                Style::default()
                    .fg(theme.primary_text)
                    .add_modifier(Modifier::BOLD),
            )]));
            // Blank separator.
            lines.push(Line::from(""));
            // Entry line.
            let name_style = if is_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                indicator,
                Span::styled(format!(" {persona_name}"), name_style),
            ]));
            lines
        };

        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
        frame.render_widget(widget, area);
    }

    fn content_height(&self, _ctx: &RenderCtx) -> u16 {
        // Header(1) + blank(1) + entry(1) + trailing gap(1) = 4.
        4
    }
}

/// Computes the persona section content height from state.
///
/// Mirrors [`PersonaSection::content_height`] so the task list preview popup
/// can determine where the task list section starts without needing the
/// section instance.
///
/// [`PersonaSection::content_height`]: PersonaSection::content_height
#[must_use]
pub fn persona_section_content_height(_state: &AppState) -> u16 {
    // Header(1) + blank(1) + entry(1) + trailing gap(1) = 4.
    4
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
    use super::{PersonaSection, navigate, receive_cursor};
    use crate::sections::section_trait::{
        EnterFrom, SectionNavResult, SidebarIntent, SidebarSection,
    };
    use jinn_domain::Intent;
    use jinn_domain::common::app_state::AppState;
    use jinn_domain::common::render_ctx::RenderCtx;
    use jinn_domain::feat::persona::Persona;

    #[rstest::rstest]
    fn section_id_is_persona() {
        // Given a PersonaSection.
        let section = PersonaSection;

        // When asking for its ID.
        // Then it returns Persona.
        assert_eq!(section.id(), jinn_sidebar_msg::SidebarSectionId::Persona);
    }

    #[rstest::rstest]
    fn content_height_is_four_with_active_persona() {
        // Given a PersonaSection and state with an active persona.
        let section = PersonaSection;
        let mut state = AppState::default_with_scope_focus();
        state.context.set_active_persona(Some(Persona {
            name: "coding-assistant".to_owned(),
            description: "Expert coder".to_owned(),
            body: String::new(),
        }));

        // When asking for content height.
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

        // Then it returns 4 (header + blank + entry + trailing gap).
        assert_eq!(height, 4);
    }

    #[rstest::rstest]
    fn content_height_is_four_without_persona() {
        // Given a PersonaSection and state with no active persona.
        let section = PersonaSection;
        let state = AppState::default_with_scope_focus();

        // When asking for content height.
        let slices = jinn_slices::Slices::new();
        let overlay_views = jinn_domain::common::overlay_views::OverlayViews::new();
        let height = section.content_height(&RenderCtx::new(&state, &slices, &overlay_views));

        // Then it returns 4 (consistent layout).
        assert_eq!(height, 4);
    }

    #[rstest::rstest]
    fn navigate_returns_exhausted_for_move_down() {
        // Given default app state.
        let mut state = AppState::default_with_scope_focus();

        // When navigating down.
        let result = navigate(&SidebarIntent::MoveDown, &mut state);

        // Then the result is Exhausted (single-entry section).
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    fn navigate_returns_exhausted_for_move_up() {
        // Given default app state.
        let mut state = AppState::default_with_scope_focus();

        // When navigating up.
        let result = navigate(&SidebarIntent::MoveUp, &mut state);

        // Then the result is Exhausted (single-entry section).
        assert_eq!(result, SectionNavResult::Exhausted);
    }

    #[rstest::rstest]
    fn navigate_returns_moved_for_action() {
        // Given default app state.
        let mut state = AppState::default_with_scope_focus();

        // When navigating with an action intent.
        let result = navigate(&SidebarIntent::Action(Intent::Quit), &mut state);

        // Then the result is Moved.
        assert_eq!(result, SectionNavResult::Moved);
    }

    #[rstest::rstest]
    fn receive_cursor_sets_cursor_to_some_zero() {
        // Given default app state (cursor is None).
        let mut state = AppState::default_with_scope_focus();

        // When receiving the cursor from the top.
        receive_cursor(&mut state, EnterFrom::Top);

        // Then the persona section cursor is set to Some(0).
        assert_eq!(
            state.frontend.with_sections(|s| s.persona.cursor, || None),
            Some(0)
        );
    }

    use jinn_testutil::setup_term;

    fn render_rows(
        section: &mut PersonaSection,
        state: &AppState,
        width: u16,
        height: u16,
    ) -> Vec<String> {
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

    #[rstest::rstest]
    fn render_shows_persona_header() {
        // Given a PersonaSection.
        let mut section = PersonaSection;
        let state = AppState::default_with_scope_focus();

        // When rendering.
        let rows = render_rows(&mut section, &state, 30, 5);

        // Then the first row contains "Persona".
        assert!(rows[0].contains("Persona"));
    }

    #[rstest::rstest]
    fn render_shows_session_persona_name() {
        // Given a PersonaSection with a session that has a custom persona.
        let mut section = PersonaSection;
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_persona_name("learning-tutor".to_owned());

        // When rendering.
        let rows = render_rows(&mut section, &state, 40, 5);

        // Then the entry row contains the session's persona name.
        let combined = rows.join("\n");
        assert!(
            combined.contains("learning-tutor"),
            "should contain 'learning-tutor', got: {combined}"
        );
    }

    #[rstest::rstest]
    fn render_shows_coding_assistant_by_default() {
        // Given a PersonaSection with default state.
        let mut section = PersonaSection;
        let state = AppState::default_with_scope_focus();

        // When rendering.
        let rows = render_rows(&mut section, &state, 40, 5);

        // Then the entry row contains "coding-assistant" (session default).
        let combined = rows.join("\n");
        assert!(
            combined.contains("coding-assistant"),
            "should contain 'coding-assistant', got: {combined}"
        );
    }
}
