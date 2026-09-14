//! Persona picker entry type and rendering.

use std::ops::Range;

use crate::feat::picker::style::dim_style;
use crate::feat::theme::Theme;
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
