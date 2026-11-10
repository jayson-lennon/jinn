//! The applets picker's state cell payload and its entry type.
//!
//! The picker is owned entirely by the applets slice: this payload is what
//! its cell holds, so no other crate names a field for it. The rows are
//! rebuilt from the catalog cell on every open (the scope-enter hook)
//! and repainted in place when a rescan republishes the catalog — the
//! picker never reads disk itself.

use jinn_picker::RowCtx;
use jinn_picker::picker_style::{active_marker, selected_style};
use jinn_selection_widget::highlight_text_with_bg;
use jinn_theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::catalog::AppletEntry;
use crate::scope::RESULTS_VIEWPORT_FALLBACK;

/// An applet entry ready for display in the applets picker.
#[derive(Debug, Clone)]
pub struct AppletPickerEntry {
    /// The app's directory id (its URL path segment).
    pub id: String,
    /// The app's display title.
    pub title: String,
    /// Theme for styling.
    pub theme: Theme,
}

impl AppletPickerEntry {
    /// Builds one picker entry from a catalog entry.
    #[must_use]
    pub fn from_catalog(entry: &AppletEntry, theme: &Theme) -> Self {
        Self {
            id: entry.id.clone(),
            title: entry.title.clone(),
            theme: theme.clone(),
        }
    }
}

/// Renders one applets picker row: the cursor marker and the applet title.
#[must_use]
pub fn applet_row(entry: &AppletPickerEntry, ctx: &RowCtx<'_>) -> Line<'static> {
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

impl jinn_selection_widget::TreeItem for AppletPickerEntry {
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

/// The applets picker's complete state: the rows, the filter, the
/// highlight, the in-progress rename input, and the server's serving
/// phase for the footer.
#[derive(Debug)]
pub struct AppletsPickerState {
    /// The selection/filter state backing the picker's rows.
    pub selection:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<AppletPickerEntry>>,
    /// How many result rows fit on screen, measured by the render pass.
    pub results_viewport: usize,
    /// The theme the rows were built with, so a repaint recolors them
    /// consistently instead of stranding them on a stale palette.
    pub theme: Theme,
    /// The in-progress rename (`<c-r>` popup), owned here so the popup
    /// renders from the same cell the picker does.
    pub rename_input: jinn_slices::LineInput,
    /// The saved-applet server's serving phase, folded in from the
    /// server actor's [`AppletsServingChanged`](crate::AppletsServingChanged)
    /// events by the republisher. `None` is the offline phase — the
    /// footer says so. Not cleared by [`AppletsPickerState::reset`]: the
    /// reset runs on every picker open, and the phase outlives the menu.
    pub serving: Option<ServingAddress>,
}

/// The server's serving phase as the picker cell holds it: the resolved
/// bind while online.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingAddress {
    /// The resolved `host:port` the listener accepted (a configured
    /// port `0` already resolved by the OS).
    pub bind: String,
}

impl Default for AppletsPickerState {
    fn default() -> Self {
        Self {
            selection: jinn_selection_widget::SelectionState::new(),
            // Matches the other pickers' fallback, so the very first
            // keypress before any render behaves as it does everywhere.
            results_viewport: RESULTS_VIEWPORT_FALLBACK,
            theme: jinn_theme::default_theme(),
            rename_input: jinn_slices::LineInput::new(),
            serving: None,
        }
    }
}

impl AppletsPickerState {
    /// Clears the filter and moves the highlight back to the top, ready
    /// for a fresh open.
    ///
    /// The serving phase is deliberately untouched: the reset is the
    /// picker's scope-enter hook, and the server's phase outlives the
    /// menu — clearing it here would flash `<server offline>` between
    /// the reset and the wake event's fold on every open.
    pub fn reset(&mut self) {
        self.selection.clear_filter();
        self.selection.move_up(usize::MAX);
    }

    /// Folds a serving fact into the phase: online carries `bind`,
    /// offline clears it.
    pub fn apply_serving(&mut self, serving: bool, bind: &str) {
        self.serving = serving.then(|| ServingAddress {
            bind: bind.to_owned(),
        });
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

/// The picker footer's status line: where the saved-applet server is.
///
/// Online: the serving URL, in the theme's success color. Offline: the
/// `<server offline>` sentence, muted. Pure — takes the state and the
/// theme, returns the line; the render pass and the tests build through
/// it so a test's second construction is identical to what the frame
/// drew.
#[must_use]
pub fn applets_server_status_line(
    serving: Option<&ServingAddress>,
    theme: &Theme,
) -> Line<'static> {
    match serving {
        Some(address) => Line::from(Span::styled(
            format!("serving http://{}", address.bind),
            Style::default().fg(theme.success),
        )),
        None => Line::from(Span::styled(
            "<server offline>",
            Style::default().fg(theme.muted_text),
        )),
    }
}

/// The row's search text: the title, so filtering matches what the user
/// sees.
#[must_use]
pub fn applet_search_text(entry: &AppletPickerEntry) -> String {
    entry.title.clone()
}

/// Builds picker entries from the catalog, in the catalog's order (the
/// store actor has already sorted it recency-newest-first).
#[must_use]
pub fn build_applet_entries(
    entries: &[AppletEntry],
    theme: &Theme,
) -> Vec<jinn_picker::PickerEntry<AppletPickerEntry>> {
    let picker_entries: Vec<AppletPickerEntry> = entries
        .iter()
        .map(|entry| AppletPickerEntry::from_catalog(entry, theme))
        .collect();

    jinn_picker::make_items_with_hooks(
        picker_entries,
        jinn_picker::PickerItemHooks::new()
            .row(applet_row)
            .search(applet_search_text),
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
    use super::{
        AppletPickerEntry, AppletsPickerState, ServingAddress, applet_row, applet_search_text,
        applets_server_status_line, build_applet_entries,
    };
    use crate::catalog::AppletEntry;
    use jinn_picker::RowCtx;
    use jinn_theme::default_theme;

    fn entry(id: &str, title: &str) -> AppletEntry {
        AppletEntry {
            id: id.to_owned(),
            title: title.to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[rstest::rstest]
    #[test]
    fn row_shows_the_title() {
        // Given one applet entry.
        let app = AppletPickerEntry {
            id: "sales-dashboard".to_owned(),
            title: "Sales Dashboard".to_owned(),
            theme: default_theme(),
        };

        // When rendering its row unselected with no filter matches.
        let line = applet_row(&app, &RowCtx::flat(false, &Vec::new()));

        // Then the row starts with the unselected padding and shows the
        // title.
        let text = line.to_string();
        assert!(text.contains("Sales Dashboard"), "got: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn search_text_is_the_title() {
        // Given one entry.
        let app = AppletPickerEntry {
            id: "id".to_owned(),
            title: "The Title".to_owned(),
            theme: default_theme(),
        };

        // When building its search text.
        // Then it is the title.
        assert_eq!(applet_search_text(&app), "The Title");
    }

    #[rstest::rstest]
    #[test]
    fn build_entries_preserves_the_catalog_order() {
        // Given a catalog already ordered newest-first.
        let entries = vec![entry("newest", "Newest"), entry("older", "Older")];

        // When building the picker's entries.
        let items = build_applet_entries(&entries, &default_theme());

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
        let mut state = AppletsPickerState::default();
        state.selection.set_items(build_applet_entries(
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
        let mut state = AppletsPickerState::default();
        state.selection.set_items(build_applet_entries(
            &[entry("a", "Alpha"), entry("b", "Beta")],
            &default_theme(),
        ));

        // When reading the highlighted id and title.
        // Then both name the first row; on an empty picker both are None.
        assert_eq!(state.highlighted_id().as_deref(), Some("a"));
        assert_eq!(state.highlighted_title().as_deref(), Some("Alpha"));

        let empty = AppletsPickerState::default();
        assert!(empty.highlighted_id().is_none());
        assert!(empty.highlighted_title().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn reset_preserves_the_serving_phase() {
        // Given a picker whose server is serving at a resolved bind.
        let mut state = AppletsPickerState::default();
        state.apply_serving(true, "127.0.0.1:41234");

        // When resetting for a fresh open.
        state.reset();

        // Then the serving phase survived — the reset clears the menu,
        // not the server.
        assert_eq!(
            state.serving,
            Some(ServingAddress {
                bind: "127.0.0.1:41234".to_owned(),
            })
        );
    }

    #[rstest::rstest]
    #[test]
    fn apply_serving_folds_online_and_offline_facts() {
        // Given a default picker state.
        let mut state = AppletsPickerState::default();

        // When folding an online serving fact.
        state.apply_serving(true, "127.0.0.1:4321");

        // Then the phase carries the resolved bind.
        assert_eq!(
            state.serving,
            Some(ServingAddress {
                bind: "127.0.0.1:4321".to_owned(),
            })
        );

        // When folding an offline fact.
        state.apply_serving(false, "");

        // Then the phase is cleared — no dead address survives.
        assert_eq!(state.serving, None);
    }

    #[rstest::rstest]
    #[test]
    fn status_line_reads_offline_while_dormant() {
        // Given a dormant server.
        let theme = default_theme();

        // When building the footer's status line.
        let line = applets_server_status_line(None, &theme);

        // Then it says the server is offline, muted.
        assert_eq!(line.to_string(), "<server offline>");
        let span = line.spans.first().expect("one styled span");
        assert_eq!(span.style.fg, Some(theme.muted_text));
    }

    #[rstest::rstest]
    #[test]
    fn status_line_names_the_serving_url_when_bound() {
        // Given a serving server at a resolved bind.
        let theme = default_theme();
        let address = ServingAddress {
            bind: "127.0.0.1:41234".to_owned(),
        };

        // When building the footer's status line.
        let line = applets_server_status_line(Some(&address), &theme);

        // Then it reads `serving http://<bind>`, in the success color.
        assert_eq!(line.to_string(), "serving http://127.0.0.1:41234");
        let span = line.spans.first().expect("one styled span");
        assert_eq!(span.style.fg, Some(theme.success));
    }
}
