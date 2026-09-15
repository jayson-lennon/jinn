//! Shared style helpers for picker entry rendering.

use crate::feat::theme::Theme;
use crate::feat::theme::contrast;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::ops::Range;

/// Renders the active-item marker: `> ` when active, `  ` when not.
pub fn active_marker(is_active: bool, theme: &Theme) -> Span<'static> {
    Span::styled(
        if is_active { "> " } else { "  " },
        if is_active {
            Style::default()
                .fg(theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    )
}

/// Returns the style for selected items (primary text on selected background).
pub fn selected_style(is_selected: bool, theme: &Theme) -> Style {
    if is_selected {
        Style::default()
            .fg(theme.primary_text)
            .bg(theme.picker_selected_bg)
    } else {
        Style::default()
    }
}

/// Returns a dimmed style for description text. When selected, uses muted text
/// on the selected background with contrast adjustment to ensure readability.
pub fn dim_style(is_selected: bool, theme: &Theme) -> Style {
    if is_selected {
        let fg = contrast::ensure_contrast(theme.muted_text, theme.picker_selected_bg);
        Style::default().fg(fg).bg(theme.picker_selected_bg)
    } else {
        Style::default().fg(theme.muted_text)
    }
}

/// Builds a footer line with a muted label and primary text value.
pub fn labeled_footer(label: &str, value: &str, theme: &Theme) -> Line<'static> {
    let gray = Style::default().fg(theme.muted_text);
    Line::from(vec![
        Span::styled(format!("{label}: "), gray),
        Span::styled(value.to_owned(), Style::default().fg(theme.primary_text)),
    ])
}

/// Promotes the first active item to the top of the list when the filter is empty.
///
/// This ensures the currently-active item (e.g., active provider, active strategy)
/// always appears first in the unfiltered picker list.
pub fn promote_active_to_top<T, F>(entries: &mut [T], is_active: F, filter: &str)
where
    F: Fn(&T) -> bool,
{
    if filter.is_empty()
        && let Some(pos) = entries.iter().position(is_active)
        && pos > 0
    {
        #[expect(
            clippy::indexing_slicing,
            reason = "pos comes from iter().position() on the same slice"
        )]
        entries[0..=pos].rotate_right(1);
    }
}

/// Splits match indices from `"{name} {description}"` into name-portion and
/// description-portion indices.
///
/// The space separator occupies byte offset `name_len`. Description indices
/// are remapped to be relative to the start of the description string.
pub(crate) fn split_match_indices(
    indices: &[Range<usize>],
    name_len: usize,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let desc_offset = name_len + 1;

    let mut name_indices = Vec::new();
    let mut desc_indices = Vec::new();

    for range in indices {
        if range.start < name_len {
            let end = range.end.min(name_len);
            name_indices.push(range.start..end);
        }

        if range.end > desc_offset {
            let start = range.start.saturating_sub(desc_offset);
            let end = range.end.saturating_sub(desc_offset);
            desc_indices.push(start..end);
        }
    }

    (name_indices, desc_indices)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::single_range_in_vec_init,
        reason = "test assertions construct one range at a time"
    )]
    use super::split_match_indices;

    #[rstest::rstest]
    #[test]
    fn split_match_indices_partitions_correctly() {
        // Given indices spanning both the name and the description of
        // search_text = "abc xyz" (name_len = 3, desc_offset = 4).
        let indices = vec![0..2, 4..6];

        // When splitting.
        let (name, desc) = split_match_indices(&indices, 3);

        // Then the name portion keeps offsets as-is and the description
        // portion is remapped relative to the description start.
        assert_eq!(name, vec![0..2]);
        assert_eq!(desc, vec![0..2]);
    }

    #[rstest::rstest]
    #[test]
    fn split_match_indices_name_only_match() {
        // Given indices entirely within the name.
        let indices = vec![1..3];

        // When splitting.
        let (name, desc) = split_match_indices(&indices, 5);

        // Then only the name portion is populated.
        assert_eq!(name, vec![1..3]);
        assert!(desc.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn split_match_indices_description_only_match() {
        // Given indices entirely within the description (search_text
        // "bash run shell", name_len = 4, desc_offset = 5).
        let indices = vec![5..14];

        // When splitting.
        let (name, desc) = split_match_indices(&indices, 4);

        // Then only the remapped description portion is populated.
        assert!(name.is_empty());
        assert_eq!(desc, vec![0..9]);
    }
}
