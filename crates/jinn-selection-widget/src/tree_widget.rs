//! Tree picker widget - ratatui renderer for the tree-structured picker popup.
//!
//! [`TreePickerWidget`] renders a telescope-style popup overlay: a bordered block containing
//! a filter input row with a real cursor, a horizontal separator, scrollable result rows
//! with tree prefixes, and an optional footer. Each result row is rendered via
//! [`TreeItem::render_row_with_tree`], which receives the tree connector string and
//! style computed from the [`VisibleEntry`] metadata and decides its placement.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tree_item::TreeItem;
use crate::tree_state::{TreePickerState, VisibleEntry};
use crate::widget::SelectionColors;
use crate::widget::compute_popup_rect;

/// Filter prompt displayed before the user's input text.
pub(crate) const PROMPT: &str = "> ";

/// Builds the tree connector prefix string for a visible entry.
///
/// For root entries (depth 0), returns an empty string.
/// For non-root entries, constructs non-compacted 3-char-wide segments:
/// - For each ancestor level: `│  ` if continuing, `   ` if not
/// - For the entry's own level: `├─ ` if has younger siblings, `└─ ` if last child
///
/// `ancestor_continuations[0]` is the root-level slot and is always skipped:
/// roots occupy the leftmost column, so no continuation line is drawn through
/// them (mirrors the sidebar's `entry_line` renderer).
pub fn tree_prefix(entry: &VisibleEntry) -> String {
    if entry.depth == 0 {
        return String::new();
    }
    let mut prefix = String::with_capacity(entry.depth * 3);
    for &continues in entry.ancestor_continuations.get(1..).unwrap_or(&[]) {
        prefix.push_str(if continues { "│  " } else { "   " });
    }
    prefix.push_str(if entry.is_last_child {
        "└─ "
    } else {
        "├─ "
    });
    prefix
}

/// Converts a sorted list of byte indices from fuzzy matching into sorted,
/// non-overlapping `Range<usize>` ranges.
fn fuzzy_bytes_to_ranges(indices: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut iter = indices.iter().copied();
    let Some(first) = iter.next() else {
        return Vec::new();
    };

    let mut ranges = Vec::new();
    let mut start = first;
    let mut end = start + 1;
    for idx in iter {
        if idx == end {
            end = idx + 1;
        } else {
            ranges.push(start..end);
            start = idx;
            end = idx + 1;
        }
    }
    ranges.push(start..end);
    ranges
}

/// Configuration for rendering the tree picker popup.
///
/// Generic over any type implementing [`TreeItem`]. Use the builder pattern to
/// customize the title and footer, then call [`render`](TreePickerWidget::render).
pub struct TreePickerWidget<'a, I>
where
    I: TreeItem,
{
    /// Title displayed in the popup border.
    title: Line<'a>,
    /// The tree picker state to render.
    state: &'a TreePickerState<I>,
    /// Optional footer line.
    footer: Option<Line<'a>>,
    /// Optional footer lines rendered at the bottom of the popup, top to
    /// bottom, one row each (more footers -> fewer result rows). Takes
    /// precedence over `footer` when non-empty.
    footers: Vec<Line<'a>>,
    /// Theme-dependent colors for border, text, separator, etc.
    colors: SelectionColors,
    /// Optional style override for the border title.
    title_style: Option<Style>,
    /// Color for tree connector characters.
    tree_prefix_color: ratatui::style::Color,
}

impl<'a, I> TreePickerWidget<'a, I>
where
    I: TreeItem,
{
    /// Creates a new widget rendering the given tree picker state.
    pub fn new(state: &'a TreePickerState<I>) -> Self {
        Self {
            title: Line::from(""),
            state,
            footer: None,
            footers: Vec::new(),
            colors: SelectionColors::default(),
            title_style: None,
            tree_prefix_color: ratatui::style::Color::DarkGray,
        }
    }

    /// Sets the popup border title.
    #[must_use]
    pub fn title(mut self, title: Line<'a>) -> Self {
        self.title = title;
        self
    }

    /// Sets an optional footer line rendered at the bottom of the popup.
    #[must_use]
    pub fn footer(mut self, footer: Line<'a>) -> Self {
        self.footer = Some(footer);

        self
    }

    /// Sets optional footer lines rendered at the bottom of the popup, top
    /// to bottom, one row each (more footers -> fewer result rows).
    /// Replaces any previously set footers; a non-empty stack takes
    /// precedence over [`footer`](Self::footer).
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

    /// Sets the color for tree connector characters (├─/└─/│).
    #[must_use]
    pub fn tree_prefix_color(mut self, color: ratatui::style::Color) -> Self {
        self.tree_prefix_color = color;
        self
    }

    /// Renders the tree picker popup within the given frame area.
    pub fn render(self, frame: &mut Frame<'_>, area: Rect) {
        let popup_area = compute_popup_rect(area);

        // Clear the popup area so content behind it doesn't show through.
        frame.render_widget(Clear, popup_area);

        // Bordered block with muted border.
        let block = {
            let mut b = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(self.colors.border))
                .title(self.title);
            if let Some(style) = self.title_style {
                b = b.title_style(style);
            }
            b
        };
        frame.render_widget(block, popup_area);

        // Layout: input line -> separator -> results -> footer stack.
        // One row per footer line bottom-up (more footers -> fewer result rows).
        let inner = {
            let b = Block::default().borders(Borders::ALL);
            b.inner(popup_area)
        };
        let footer_rows = if self.footers.is_empty() {
            1
        } else {
            u16::try_from(self.footers.len()).unwrap_or(u16::MAX)
        };
        let [input_area, separator_area, results_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(footer_rows),
        ])
        .areas(inner);

        // Filter input with real cursor.
        let filter_text = format!("{}{}", PROMPT, self.state.filter());
        let filter_paragraph =
            Paragraph::new(filter_text).style(Style::default().fg(self.colors.filter_text));
        frame.render_widget(filter_paragraph, input_area);
        let cursor_col = input_area.x + (PROMPT.len() + self.state.cursor_pos()) as u16;
        frame.set_cursor_position((cursor_col, input_area.y));

        // Separator line.
        let separator = "\u{2500}".repeat(separator_area.width as usize);
        let sep_paragraph =
            Paragraph::new(separator).style(Style::default().fg(self.colors.separator));
        frame.render_widget(sep_paragraph, separator_area);

        // Results area - windowed display with scroll_offset.
        let max_visible = results_area.height as usize;
        let scroll_offset = self.state.scroll_offset();
        let selection = self.state.selection();
        let mut result_lines = Vec::with_capacity(max_visible);

        let prefix_style = Style::default().fg(self.tree_prefix_color);

        for row in 0..max_visible {
            let entry_idx = scroll_offset + row;
            if let Some(entry) = self.state.visible_entry(entry_idx) {
                let is_selected = entry_idx == selection;
                let match_indices = self.state.filtered_match_indices(entry_idx).unwrap_or(&[]);
                let ranges = fuzzy_bytes_to_ranges(match_indices);

                let item = self.state.filtered_item(entry_idx);
                let Some(item) = item else {
                    result_lines.push(Line::from(""));
                    continue;
                };

                let prefix = tree_prefix(entry);
                let content_line =
                    item.render_row_with_tree(is_selected, &ranges, &prefix, prefix_style);
                result_lines.push(content_line);
            } else {
                // Empty row to maintain fixed height.
                result_lines.push(Line::from(""));
            }
        }
        frame.render_widget(Paragraph::new(result_lines), results_area);

        // Footer: a non-empty stack renders one right-aligned line per row
        // (mirroring SelectionWidget); otherwise the single footer or an
        // empty row.
        if self.footers.is_empty() {
            let footer_paragraph = match &self.footer {
                Some(line) => Paragraph::new(line.clone())
                    .style(Style::default().fg(self.colors.footer))
                    .right_aligned(),
                None => Paragraph::new(""),
            };
            frame.render_widget(footer_paragraph, footer_area);
        } else {
            let rows = Layout::vertical(vec![Constraint::Length(1); self.footers.len()])
                .split(footer_area);
            for (line, row_area) in self.footers.iter().zip(rows.iter()) {
                let paragraph = Paragraph::new(line.clone())
                    .style(Style::default().fg(self.colors.footer))
                    .right_aligned();
                frame.render_widget(paragraph, *row_area);
            }
        }
    }
}
