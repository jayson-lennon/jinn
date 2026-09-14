//! Persona picker entry type and rendering.

use std::ops::Range;

use crate::feat::picker::style::dim_style;
use crate::feat::theme::Theme;
use jinn_selection_widget::PickerItem;
use jinn_selection_widget::highlight_text_with_bg;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// A persona entry ready for display in the picker.
#[derive(Debug, Clone)]
pub struct PersonaEntry {
    /// Human-readable display name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Whether this is the currently active persona.
    pub is_active: bool,
    /// Theme for rendering.
    pub theme: Theme,
}

impl PickerItem for PersonaEntry {
    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_persona_row(
            &self.name,
            &self.description,
            self.is_active,
            is_selected,
            &[],
            &self.theme,
        )
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        render_persona_row(
            &self.name,
            &self.description,
            self.is_active,
            is_selected,
            match_indices,
            &self.theme,
        )
    }
}

/// Renders a persona picker row.
pub(crate) fn render_persona_row(
    name: &str,
    description: &str,
    is_active: bool,
    is_selected: bool,
    match_indices: &[Range<usize>],
    theme: &Theme,
) -> Line<'static> {
    let active_marker = Span::styled(
        if is_active { "> " } else { "  " },
        if is_active {
            Style::default()
                .fg(theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    );

    let name_style = if is_selected {
        Style::default()
            .fg(theme.primary_text)
            .bg(theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let desc_style = dim_style(is_selected, theme);

    let name_spans = if match_indices.is_empty() {
        vec![Span::styled(format!("{name}  "), name_style)]
    } else {
        let mut spans =
            highlight_text_with_bg(name, name_style, match_indices, theme.picker_highlight_bg);
        spans.push(Span::styled("  ".to_owned(), name_style));
        spans
    };

    let mut all_spans = vec![active_marker];
    all_spans.extend(name_spans);
    all_spans.push(Span::styled(description.to_owned(), desc_style));
    Line::from(all_spans)
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
    use super::*;
    use crate::feat::theme::default_theme;

    fn theme() -> Theme {
        default_theme()
    }

    #[rstest::rstest]
    fn display_label_returns_name() {
        // Given a persona entry.
        let entry = PersonaEntry {
            name: "coding-assistant".to_owned(),
            description: "Expert coder".to_owned(),
            is_active: true,
            theme: theme(),
        };

        // When reading display_label.
        // Then it returns the name.
        assert_eq!(entry.display_label(), "coding-assistant");
    }

    #[rstest::rstest]
    fn render_row_produces_line() {
        // Given a persona entry.
        let entry = PersonaEntry {
            name: "coding-assistant".to_owned(),
            description: "Expert coder".to_owned(),
            is_active: false,
            theme: theme(),
        };

        // When rendering a non-selected row.
        let line = entry.render_row(false);

        // Then the line contains name and description spans.
        assert!(!line.spans.is_empty());
    }

    #[rstest::rstest]
    fn render_row_active_has_green_marker() {
        // Given an active persona entry.
        let entry = PersonaEntry {
            name: "coding-assistant".to_owned(),
            description: "Expert coder".to_owned(),
            is_active: true,
            theme: theme(),
        };

        // When rendering.
        let line = entry.render_row(false);

        // Then the first span contains "> ".
        assert_eq!(line.spans.first().unwrap().content, "> ");
    }

    #[rstest::rstest]
    fn render_row_inactive_has_blank_marker() {
        // Given an inactive persona entry.
        let entry = PersonaEntry {
            name: "coding-assistant".to_owned(),
            description: "Expert coder".to_owned(),
            is_active: false,
            theme: theme(),
        };

        // When rendering.
        let line = entry.render_row(false);

        // Then the first span contains "  ".
        assert_eq!(line.spans.first().unwrap().content, "  ");
    }
}
