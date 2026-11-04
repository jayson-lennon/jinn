//! The apps picker's state cell payload and its entry type.
//!
//! The picker is owned entirely by the apps slice: this payload is what
//! its cell holds, so no other crate names a field for it. The rows are
//! rebuilt from the catalog cell on every open (the scope-enter hook)
//! and repainted in place when a rescan republishes the catalog — the
//! picker never reads disk itself.

use jinn_picker::RowCtx;
use jinn_picker::picker_style::{active_marker, dim_style, selected_style};
use jinn_selection_widget::highlight_text_with_bg;
use jinn_theme::Theme;
use ratatui::text::{Line, Span};

use crate::catalog::AppEntry;
use crate::scope::RESULTS_VIEWPORT_FALLBACK;

/// An app entry ready for display in the apps picker.
#[derive(Debug, Clone)]
pub struct AppPickerEntry {
    /// The app's directory id (its URL path segment).
    pub id: String,
    /// The app's display title.
    pub title: String,
    /// Theme for styling.
    pub theme: Theme,
}

impl AppPickerEntry {
    /// Builds one picker entry from a catalog entry.
    #[must_use]
    pub fn from_catalog(entry: &AppEntry, theme: &Theme) -> Self {
        Self {
            id: entry.id.clone(),
            title: entry.title.clone(),
            theme: theme.clone(),
        }
    }
}

/// Renders one apps picker row: the cursor marker and the app title.
#[must_use]
pub fn app_row(entry: &AppPickerEntry, ctx: &RowCtx<'_>) -> Line<'static> {
    let base_style = selected_style(ctx.is_selected, &entry.theme);
    let mut spans = vec![active_marker(ctx.is_selected, &entry.theme)];

    if ctx.match_ranges.is_empty() {
        spans.push(Span::styled(entry.title.clone(), base_style));
    } else {
        spans.extend(highlight_text_with_bg(
            &entry.title,
            base_style,
            ctx.match_ranges,
            entry.theme.picker_highlight_bg,
        ));
    }

    Line::from(spans)
}

impl jinn_selection_widget::TreeItem for AppPickerEntry {
    fn id(&self) -> &str {
        &self.id
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.title
    }

    fn render_row(&self, _is_selected: bool) -> Line<'static> {
        // Rows render through the spec's row hook via PickerEntry; this
        // impl only supplies tree structure (id/parent_id) and filter text.
        Line::raw(self.display_label().to_owned())
    }

    fn render_row_with_highlight(
        &self,
        _is_selected: bool,
        _match_indices: &[std::ops::Range<usize>],
    ) -> Line<'static> {
        Line::raw(self.display_label().to_owned())
    }
}

/// The apps picker's complete state: the rows, the filter, and the
/// highlight.
#[derive(Debug)]
pub struct AppsPickerState {
    /// The selection/filter state backing the picker's rows.
    pub selection:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<AppPickerEntry>>,
    /// How many result rows fit on screen, measured by the render pass.
    pub results_viewport: usize,
    /// The theme the rows were built with, so a repaint recolors them
    /// consistently instead of stranding them on a stale palette.
    pub theme: Theme,
}

impl Default for AppsPickerState {
    fn default() -> Self {
        Self {
            selection: jinn_selection_widget::SelectionState::new(),
            // Matches the other pickers' fallback, so the very first
            // keypress before any render behaves as it does everywhere.
            results_viewport: RESULTS_VIEWPORT_FALLBACK,
            theme: jinn_theme::default_theme(),
        }
    }
}

impl AppsPickerState {
    /// Clears the filter and moves the highlight back to the top, ready
    /// for a fresh open.
    pub fn reset(&mut self) {
        self.selection.clear_filter();
        self.selection.move_up(usize::MAX);
    }

    /// The highlighted app's id, when a row is highlighted.
    #[must_use]
    pub fn highlighted_id(&self) -> Option<String> {
        self.selection
            .selected_item()
            .map(|item| item.entry().id.clone())
    }

    /// The highlighted app's title, when a row is highlighted.
    #[must_use]
    pub fn highlighted_title(&self) -> Option<String> {
        self.selection
            .selected_item()
            .map(|item| item.entry().title.clone())
    }
}

/// The row's search text: the title, so filtering matches what the user
/// sees.
#[must_use]
pub fn app_search_text(entry: &AppPickerEntry) -> String {
    entry.title.clone()
}

/// Builds picker entries from the catalog, in the catalog's order (the
/// store actor has already sorted it recency-newest-first).
#[must_use]
pub fn build_app_entries(
    entries: &[AppEntry],
    theme: &Theme,
) -> Vec<jinn_picker::PickerEntry<AppPickerEntry>> {
    let picker_entries: Vec<AppPickerEntry> = entries
        .iter()
        .map(|entry| AppPickerEntry::from_catalog(entry, theme))
        .collect();

    jinn_picker::make_items_with_hooks(
        picker_entries,
        jinn_picker::PickerItemHooks::new()
            .row(app_row)
            .search(app_search_text),
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::single_range_in_vec_init,
        reason = "test code"
    )]
    use super::{AppPickerEntry, AppsPickerState, app_row, app_search_text, build_app_entries};
    use crate::catalog::AppEntry;
    use jinn_picker::RowCtx;
    use jinn_theme::default_theme;

    fn entry(id: &str, title: &str) -> AppEntry {
        AppEntry {
            id: id.to_owned(),
            title: title.to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn row_shows_the_title() {
        // Given one app entry.
        let app = AppPickerEntry {
            id: "sales-dashboard".to_owned(),
            title: "Sales Dashboard".to_owned(),
            theme: default_theme(),
        };

        // When rendering its row unselected with no filter matches.
        let line = app_row(&app, &RowCtx::flat(false, &Vec::new()));

        // Then the row starts with the unselected padding and shows the
        // title.
        let text = line.to_string();
        assert!(text.contains("Sales Dashboard"), "got: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn search_text_is_the_title() {
        // Given one entry.
        let app = AppPickerEntry {
            id: "id".to_owned(),
            title: "The Title".to_owned(),
            theme: default_theme(),
        };

        // When building its search text.
        // Then it is the title.
        assert_eq!(app_search_text(&app), "The Title");
    }

    #[rstest::rstest]
    #[test]
    fn build_entries_preserves_the_catalog_order() {
        // Given a catalog already ordered newest-first.
        let entries = vec![entry("newest", "Newest"), entry("older", "Older")];

        // When building the picker's entries.
        let items = build_app_entries(&entries, &default_theme());

        // Then the order is the catalog's — the store actor owns ordering.
        let ids: Vec<&str> = items
            .iter()
            .map(jinn_selection_widget::TreeItem::id)
            .collect();
        assert_eq!(ids, ["newest", "older"]);
    }

    #[rstest::rstest]
    #[test]
    fn reset_clears_the_filter_and_returns_to_the_top() {
        // Given a picker state with a filter and a moved highlight.
        let mut state = AppsPickerState::default();
        state.selection.set_items(build_app_entries(
            &[entry("a", "Alpha"), entry("b", "Beta")],
            &default_theme(),
        ));
        state.selection.insert_char('b');
        state.selection.move_down(state.results_viewport);

        // When resetting for a fresh open.
        state.reset();

        // Then the filter is empty and the highlight is back at the top.
        assert_eq!(state.selection.filter(), "");
        assert_eq!(state.highlighted_id().as_deref(), Some("a"));
    }

    #[rstest::rstest]
    #[test]
    fn highlighted_accessors_read_the_highlighted_row() {
        // Given a picker with rows and the highlight at the top.
        let mut state = AppsPickerState::default();
        state.selection.set_items(build_app_entries(
            &[entry("a", "Alpha"), entry("b", "Beta")],
            &default_theme(),
        ));

        // When reading the highlighted id and title.
        // Then both name the first row; on an empty picker both are None.
        assert_eq!(state.highlighted_id().as_deref(), Some("a"));
        assert_eq!(state.highlighted_title().as_deref(), Some("Alpha"));

        let empty = AppsPickerState::default();
        assert!(empty.highlighted_id().is_none());
        assert!(empty.highlighted_title().is_none());
    }
}
