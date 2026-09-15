//! Theme picker entry type and rendering.

use crate::Theme;
use jinn_selection_widget::PickerItem;
use ratatui::text::Line;

/// A theme entry ready for display in the theme picker.
#[derive(Debug, Clone)]
pub struct ThemeEntry {
    /// Theme name (filename without extension, or "default").
    pub name: String,
    /// The resolved theme colors.
    pub theme: Theme,
}

impl PickerItem for ThemeEntry {
    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        use ratatui::style::Style;
        use ratatui::text::Span;

        let style = if is_selected {
            Style::default()
                .fg(self.theme.primary_text)
                .bg(self.theme.picker_selected_bg)
        } else {
            Style::default()
        };

        // Show a colored accent swatch + name.
        let swatch = Span::styled("\u{2588} ", Style::default().fg(self.theme.focus_accent));
        let name = Span::styled(self.name.clone(), style);
        Line::from(vec![swatch, name])
    }
}

impl jinn_selection_widget::TreeItem for ThemeEntry {
    fn id(&self) -> &str {
        &self.name
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.name
    }

    fn render_row(&self, is_selected: bool) -> ratatui::text::Line<'static> {
        PickerItem::render_row(self, is_selected)
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[std::ops::Range<usize>],
    ) -> ratatui::text::Line<'static> {
        PickerItem::render_row_with_highlight(self, is_selected, match_indices)
    }
}
