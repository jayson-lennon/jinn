//! Preview selection widget - picker with a split-pane preview area.
//!
//! [`PreviewSelectionWidget`] renders a telescope-style popup overlay with a list
//! on one side and a preview pane on the other. On wide terminals the split is
//! vertical (list left 20%, preview right 80%). On narrow terminals the split is
//! horizontal (list on top, preview below).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::preview_content::{PreviewCache, PreviewContent};
use crate::{PickerItem, SelectionColors, SelectionState, compute_popup_rect};

/// Popup width threshold for vertical (side-by-side) split.
/// Below this, the layout switches to horizontal (stacked) split.
pub const VERTICAL_SPLIT_MIN_WIDTH: u16 = 101;

/// Fraction of the list area allocated to the list in vertical split.
pub const LIST_FRACTION: u16 = 20;

/// Number of visible list rows in horizontal split mode.
pub const HORIZONTAL_LIST_ROWS: u16 = 5;

/// Horizontal separator character.
const HORIZONTAL_SEPARATOR: char = '\u{2500}'; // ─

/// Vertical separator character.
const VERTICAL_SEPARATOR: char = '\u{2502}'; // │

/// Filter prompt displayed before the user's input text.
const PROMPT: &str = "> ";

/// Renders a picker popup with a preview pane.
///
/// Generic over any type implementing both [`PickerItem`] and [`PreviewContent`].
/// Use the builder pattern to customize the title, footer, and colors, then call
/// [`render`](Self::render).
pub struct PreviewSelectionWidget<'a, T>
where
    T: PickerItem + PreviewContent,
{
    /// Title displayed in the popup border.
    title: Line<'a>,
    /// The selection state to render.
    state: &'a SelectionState<T>,
    /// Optional footer lines rendered at the bottom of the popup, top to
    /// bottom. Set via [`Self::footer`] (one line) or [`Self::footers`]
    /// (several); the popup reserves one row per line bottom-up.
    footers: Vec<Line<'a>>,
    /// Theme-dependent colors.
    colors: SelectionColors,
    /// Optional style override for the border title.
    title_style: Option<Style>,
    /// Preview pane scroll offset.
    preview_scroll: usize,
    /// Optional preview-line cache. When set, the preview pane uses
    /// [`PreviewContent::preview_lines_cached`] to skip re-rendering lines
    /// already stored under the selected item's cache key.
    preview_cache: Option<&'a dyn PreviewCache>,
}

impl<'a, T> PreviewSelectionWidget<'a, T>
where
    T: PickerItem + PreviewContent,
{
    /// Creates a new preview widget rendering the given selection state.
    pub fn new(state: &'a SelectionState<T>) -> Self {
        Self {
            title: Line::from(""),
            state,
            footers: Vec::new(),
            colors: SelectionColors::default(),
            title_style: None,
            preview_scroll: 0,
            preview_cache: None,
        }
    }

    /// Sets the popup border title.
    #[must_use]
    pub fn title(mut self, title: Line<'a>) -> Self {
        self.title = title;
        self
    }

    /// Sets a single footer line rendered at the bottom of the popup.
    /// Replaces any previously set footers. For multiple footer lines use
    /// [`Self::footers`].
    #[must_use]
    pub fn footer(mut self, footer: Line<'a>) -> Self {
        self.footers = vec![footer];
        self
    }

    /// Sets multiple footer lines rendered at the bottom of the popup, top
    /// to bottom. Each line occupies one terminal row; the popup allocates
    /// one row per footer line bottom-up (more footers -> fewer content
    /// rows). Replaces any previously set footers.
    #[must_use]
    pub fn footers(mut self, footers: Vec<Line<'a>>) -> Self {
        self.footers = footers;
        self
    }

    /// Sets the theme-dependent colors for the widget.
    #[must_use]
    pub fn colors(mut self, colors: SelectionColors) -> Self {
        self.colors = colors;
        self
    }

    /// Sets an optional style override for the popup border title.
    #[must_use]
    pub fn title_style(mut self, style: Style) -> Self {
        self.title_style = Some(style);
        self
    }

    /// Sets the preview pane scroll offset.
    #[must_use]
    pub fn preview_scroll(mut self, scroll: usize) -> Self {
        self.preview_scroll = scroll;
        self
    }

    /// Supplies a preview-line cache so the preview pane reuses already-rendered
    /// lines instead of re-rendering every frame.
    #[must_use]
    pub fn preview_cache(mut self, cache: &'a dyn PreviewCache) -> Self {
        self.preview_cache = Some(cache);
        self
    }

    /// Renders the preview selection popup within the given frame area.
    pub fn render(self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = compute_popup_rect(area);
        frame.render_widget(Clear, popup_area);

        // Destructure self to take ownership of individual fields.
        let Self {
            title,
            state,
            footers,
            colors,
            title_style,
            preview_scroll,
            preview_cache,
        } = self;

        let block = {
            let mut b = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors.border))
                .title(title);
            if let Some(style) = title_style {
                b = b.title_style(style);
            }
            b
        };
        frame.render_widget(block, popup_area);

        let inner = {
            let b = Block::default().borders(Borders::ALL);
            b.inner(popup_area)
        };

        // Reserve one row at the bottom per footer line.
        let footer_rows = footers.len() as u16;
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(footer_rows)]).areas(inner);

        // Build a temporary struct holding the remaining fields for the split renderers.
        let borrowed = RenderCtx::<T> {
            state,
            footers: &footers,
            colors: &colors,
            preview_scroll,
        };

        if popup_area.width >= VERTICAL_SPLIT_MIN_WIDTH {
            borrowed.render_vertical_split(frame, content_area, preview_cache);
        } else {
            borrowed.render_horizontal_split(frame, content_area, preview_cache);
        }

        borrowed.render_footer(frame, footer_area);
    }

    // Forward to RenderCtx methods below are replaced by the RenderCtx struct.
}

/// Borrowed rendering context used after the block/title are consumed.
struct RenderCtx<'a, T: PickerItem + PreviewContent> {
    /// Shared picker state.
    state: &'a SelectionState<T>,

    /// Footer lines, top to bottom.
    footers: &'a [Line<'a>],
    /// Color configuration.
    colors: &'a SelectionColors,
    /// Scroll offset for the preview pane.
    preview_scroll: usize,
}

impl<T: PickerItem + PreviewContent> RenderCtx<'_, T> {
    /// Renders the list and preview stacked vertically (narrow terminals).
    fn render_vertical_split(
        &self,
        frame: &mut Frame<'_>,
        inner: Rect,
        cache: Option<&dyn PreviewCache>,
    ) {
        let [list_area, preview_area] = Layout::horizontal([
            Constraint::Percentage(LIST_FRACTION),
            Constraint::Percentage(100 - LIST_FRACTION),
        ])
        .areas(inner);

        self.render_list(frame, list_area);

        // Vertical separator as the first column of the preview area.
        let [sep_area, preview_body] =
            Layout::horizontal([Constraint::Length(1), Constraint::Min(0)]).areas(preview_area);
        let sep_lines: Vec<Line<'static>> = std::iter::repeat_n(
            Line::from(VERTICAL_SEPARATOR.to_string()),
            sep_area.height as usize,
        )
        .collect();
        let sep_paragraph =
            Paragraph::new(sep_lines).style(Style::default().fg(self.colors.separator));
        frame.render_widget(sep_paragraph, sep_area);

        self.render_preview(frame, preview_body, cache);
    }

    /// Renders the list and preview stacked vertically (narrow terminals).
    fn render_horizontal_split(
        &self,
        frame: &mut Frame<'_>,
        inner: Rect,
        cache: Option<&dyn PreviewCache>,
    ) {
        let list_height = HORIZONTAL_LIST_ROWS + 2;
        let [list_area, sep_area, preview_area] = Layout::vertical([
            Constraint::Length(list_height),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);

        self.render_list(frame, list_area);

        let separator = HORIZONTAL_SEPARATOR
            .to_string()
            .repeat(sep_area.width as usize);
        let sep_paragraph =
            Paragraph::new(separator).style(Style::default().fg(self.colors.separator));
        frame.render_widget(sep_paragraph, sep_area);

        self.render_preview(frame, preview_area, cache);
    }

    /// Render the filter input and result list.
    fn render_list(&self, frame: &mut Frame<'_>, area: Rect) {
        let [input_area, sep_area, results_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

        let filter_text = format!("{}{}", PROMPT, self.state.filter());
        let filter_paragraph =
            Paragraph::new(filter_text).style(Style::default().fg(self.colors.filter_text));
        frame.render_widget(filter_paragraph, input_area);
        let cursor_col = input_area.x + (PROMPT.len() + self.state.cursor_pos()) as u16;
        frame.set_cursor_position((cursor_col, input_area.y));

        let separator = HORIZONTAL_SEPARATOR.to_string().repeat(area.width as usize);
        let sep_paragraph =
            Paragraph::new(separator).style(Style::default().fg(self.colors.separator));
        frame.render_widget(sep_paragraph, sep_area);

        self.render_results(frame, results_area);
    }

    /// Render the filtered result rows.
    fn render_results(&self, frame: &mut Frame<'_>, area: Rect) {
        let max_visible = area.height as usize;
        let scroll_offset = self.state.scroll_offset();
        let selection = self.state.selection();
        let mut result_lines = Vec::with_capacity(max_visible);

        for row in 0..max_visible {
            let entry_idx = scroll_offset + row;
            if let Some(item) = self.state.filtered_item(entry_idx) {
                let is_selected = entry_idx == selection;
                let match_indices = self.state.filtered_match_indices(entry_idx).unwrap_or(&[]);
                let ranges = super::widget::fuzzy_bytes_to_ranges(match_indices);
                result_lines.push(item.render_row_with_highlight(is_selected, &ranges));
            } else {
                result_lines.push(Line::from(""));
            }
        }
        frame.render_widget(Paragraph::new(result_lines), area);
    }

    /// Renders the preview pane for the currently selected item, consulting
    /// the optional `cache` to avoid re-rendering unchanged content.
    fn render_preview(&self, frame: &mut Frame<'_>, area: Rect, cache: Option<&dyn PreviewCache>) {
        let width = area.width as usize;
        let max_visible = area.height as usize;

        let lines = if let Some(item) = self.state.selected_item() {
            item.preview_lines_cached(width, cache)
        } else {
            Vec::new()
        };

        let visible: Vec<Line<'static>> = lines
            .iter()
            .skip(self.preview_scroll)
            .take(max_visible)
            .cloned()
            .collect();

        let mut padded = visible;
        while padded.len() < max_visible {
            padded.push(Line::from(""));
        }

        let paragraph = Paragraph::new(padded).style(Style::default().fg(self.colors.filter_text));
        frame.render_widget(paragraph, area);
    }

    /// Renders the footer line at the bottom of the popup, right-aligned.
    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.footers.is_empty() {
            return;
        }
        let rows = Layout::vertical(vec![Constraint::Length(1); self.footers.len()])
            .split(area);
        for (line, row) in self.footers.iter().zip(rows.iter()) {
            let paragraph = Paragraph::new(line.clone())
                .style(Style::default().fg(self.colors.footer))
                .right_aligned();
            frame.render_widget(paragraph, *row);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use crate::{PickerItem, SelectionState};
    use ratatui::text::Line;

    /// A simple test item implementing both PickerItem and PreviewContent.
    #[derive(Debug, Clone)]
    struct TestItem {
        name: String,
        body: String,
    }

    impl PickerItem for TestItem {
        fn display_label(&self) -> &str {
            &self.name
        }
        fn render_row(&self, _is_selected: bool) -> Line<'static> {
            Line::from(self.name.clone())
        }
    }

    impl PreviewContent for TestItem {
        fn preview_lines(&self, _width: usize) -> Vec<Line<'static>> {
            self.body
                .lines()
                .map(|l| Line::from(l.to_owned()))
                .collect()
        }
    }

    fn make_state(items: Vec<TestItem>) -> SelectionState<TestItem> {
        SelectionState::with_items(items)
    }

    #[rstest::rstest]
    fn vertical_split_used_on_wide_terminal() {
        // Given a wide popup rect (popup_width from 140-col terminal = 112).
        let area = Rect::new(0, 0, 140, 30);
        let popup = compute_popup_rect(area);

        // Then the popup is wide enough for vertical split.
        assert!(
            popup.width >= VERTICAL_SPLIT_MIN_WIDTH,
            "popup width {} < {}",
            popup.width,
            VERTICAL_SPLIT_MIN_WIDTH
        );
    }

    #[rstest::rstest]
    fn horizontal_split_used_on_narrow_terminal() {
        // Given a narrow terminal.
        let area = Rect::new(0, 0, 60, 30);
        let popup = compute_popup_rect(area);

        // Then the popup is too narrow for vertical split.
        assert!(popup.width < VERTICAL_SPLIT_MIN_WIDTH);
    }

    #[rstest::rstest]
    fn preview_lines_called_for_selected_item() {
        // Given a state with an item that has preview content.
        let state = make_state(vec![TestItem {
            name: "test".to_owned(),
            body: "Hello preview".to_owned(),
        }]);
        let selected = state.selected_item().expect("should have selection");

        // When getting preview lines.
        let lines = selected.preview_lines(40);

        // Then the body content is returned.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "Hello preview");
    }

    #[rstest::rstest]
    fn preview_lines_empty_for_empty_body() {
        // Given an item with no body.
        let item = TestItem {
            name: "empty".to_owned(),
            body: String::new(),
        };

        // When getting preview lines.
        let lines = item.preview_lines(40);

        // Then no lines are returned.
        assert!(lines.is_empty());
    }
}
