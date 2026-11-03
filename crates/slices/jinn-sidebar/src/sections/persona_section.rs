//! [`PersonaSection`] - the persona sidebar section.
//!
//! Implements [`SidebarSection`] for displaying the active persona.
//! Shows a header line and a single selectable entry with the persona name.
//! Pressing `e` while this section is focused opens the persona picker.

use crate::sections::section_trait::{
    EnterFrom, SectionNavResult, SidebarIntent, SidebarSection, SidebarSectionId,
};
use jinn_kernel::common::app_state::AppState;
use jinn_slices::DrawContext;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

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
                Some(jinn_sidebar_msg::SidebarSectionId::Persona)
            );
        let theme = &state.frontend.theme;

        let is_selected = section_focused
            && state
                .frontend
                .with_sections(|s| s.persona.cursor.is_some(), || false);

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
            // Entry line. Content is shifted one column right of the header:
            // gutter(1), gap(1), then the name. Selection is the shared
            // full-width band; the pad carries the band to the row's last
            // cell, since `Paragraph` does not extend a line's style past
            // the last grapheme.
            let entry_line = {
                let content_width = 3 + persona_name.chars().count();
                let mut spans = vec![
                    crate::sections::session_row_style::gutter_span(theme),
                    crate::sections::session_row_style::gutter_span(theme),
                    Span::raw(persona_name),
                ];
                if is_selected {
                    spans.push(crate::sections::session_row_style::band_pad(
                        content_width,
                        usize::from(area.width),
                        theme,
                    ));
                }
                let name = Line::from(spans);
                if is_selected {
                    name.style(crate::sections::session_row_style::selected_row_style(
                        theme,
                    ))
                } else {
                    name
                }
            };
            lines.push(entry_line);
            lines
        };

        let widget = Paragraph::new(lines)
            .block(Block::default().borders(Borders::NONE))
            .scroll((skip_rows, 0));
        frame.render_widget(widget, area);
    }

    fn content_height(
        &mut self,
        _ctx: &dyn DrawContext<jinn_kernel::common::app_state::AppState>,
    ) -> u16 {
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
    use jinn_kernel::KernelIntent;
    use jinn_kernel::common::app_state::AppState;

    use jinn_kernel::common::render_test::{render_draw, with_render_ctx};
    use jinn_slices::Persona;

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
        // Given a PersonaSection and state whose session carries a persona.
        let mut section = PersonaSection;
        let mut state = AppState::default_with_scope_focus();
        state
            .persona_selection()
            .expect("persona cell attached")
            .update(|p| {
                p.entries.push(Persona {
                    name: "coding-assistant".to_owned(),
                    description: "Expert coder".to_owned(),
                    body: String::new(),
                });
            });
        state
            .active_session_mut()
            .set_persona_name("coding-assistant".to_owned());

        // When asking for content height.
        let slices = jinn_slices::Slices::new();
        let height = with_render_ctx(&state, &slices, |ctx| section.content_height(ctx));

        // Then it returns 4 (header + blank + entry + trailing gap).
        assert_eq!(height, 4);
    }

    #[rstest::rstest]
    fn content_height_is_four_without_persona() {
        // Given a PersonaSection and state with no active persona.
        let mut section = PersonaSection;
        let state = AppState::default_with_scope_focus();

        // When asking for content height.
        let slices = jinn_slices::Slices::new();
        let height = with_render_ctx(&state, &slices, |ctx| section.content_height(ctx));

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
        let result = navigate(&SidebarIntent::Action(KernelIntent::Quit), &mut state);

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

    fn render_rows(
        section: &mut PersonaSection,
        state: &AppState,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let terminal = render_draw(state, width, height, |frame, area, frame_ctx| {
            section.render(frame, area, 0, frame_ctx);
        });
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
