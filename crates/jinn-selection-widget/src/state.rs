//! Selection state - the core state machine for the picker widget.
//!
//! [`SelectionState`] holds the filter text, cursor position, selection index, scroll offset,
//! the full item list, and a cached filtered index list. Filter input methods trigger
//! fuzzy re-filtering and reset the selection. Navigation methods move the selection
//! within the filtered results and adjust the scroll window.

use fuzzy_matcher::FuzzyMatcher as _;
use fuzzy_matcher::skim::SkimMatcherV2;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::PickerItem;

/// State machine for a search+filter+select picker.
///
/// Generic over any type implementing [`PickerItem`]. Owns the item list and caches
/// filtered results as **indices** into that list (no cloning on every keystroke).
///
/// # Examples
///
/// See the tests in this crate for usage patterns.
#[derive(Debug)]
#[expect(
    clippy::field_scoped_visibility_modifiers,
    reason = "fields need pub(crate) for cross-module access within the crate"
)]
pub struct SelectionState<T>
where
    T: PickerItem,
{
    /// Current filter text typed by the user.
    pub(crate) filter: String,
    /// Cursor position as a grapheme-cluster index within `filter` (0 = before first grapheme).
    pub(crate) cursor_pos: usize,
    /// Index of the currently highlighted item in the filtered list.
    pub(crate) selection: usize,
    /// Index of the first visible result row (scroll window top).
    pub(crate) scroll_offset: usize,
    /// The full item list provided by the consumer (pre-sorted).
    items: Vec<T>,
    /// Cached indices into `items` for matching entries, recomputed on filter change.
    filtered_indices: Vec<usize>,
    /// Cached byte-index vectors from fuzzy matching, one per filtered entry.
    /// Empty when filter is empty (no highlighting). Recomputed alongside `filtered_indices`.
    filtered_match_indices: Vec<Vec<usize>>,
}

impl<T> Default for SelectionState<T>
where
    T: PickerItem,
{
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    clippy::same_name_method,
    reason = "public API mirrors PickerOps trait methods for ergonomics"
)]
impl<T> SelectionState<T>
where
    T: PickerItem,
{
    /// Creates a new, empty selection state with no items.
    #[must_use]
    pub fn new() -> Self {
        Self {
            filter: String::new(),
            cursor_pos: 0,
            selection: 0,
            scroll_offset: 0,
            items: Vec::new(),
            filtered_indices: Vec::new(),
            filtered_match_indices: Vec::new(),
        }
    }

    /// Creates a selection state pre-populated with items.
    ///
    /// When the filter is empty (which it is initially), all items are visible.
    #[must_use]
    pub fn with_items(items: Vec<T>) -> Self {
        let filtered_indices = (0..items.len()).collect();
        let filtered_match_indices = vec![Vec::new(); items.len()];
        Self {
            filter: String::new(),
            cursor_pos: 0,
            selection: 0,
            scroll_offset: 0,
            items,
            filtered_indices,
            filtered_match_indices,
        }
    }

    /// Replaces the full item list and re-filters against the current filter text.
    ///
    /// Does **not** reset the filter text or cursor position - the consumer may want to
    /// update items while the picker is open (e.g., after a model cache refresh).
    /// Clamps `selection` to stay within the new filtered bounds.
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.recompute_filtered();
        self.selection = self
            .selection
            .min(self.filtered_indices.len().saturating_sub(1));
    }

    /// Inserts a character at the cursor position, advances the cursor, re-filters, and
    /// resets selection and scroll offset to 0.
    pub fn insert_char(&mut self, ch: char) {
        let byte_offset = self
            .filter
            .grapheme_indices(true)
            .nth(self.cursor_pos)
            .map_or(self.filter.len(), |(i, _)| i);
        self.filter.insert(byte_offset, ch);
        self.cursor_pos += 1;
        self.recompute_filtered();
        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Bulk inserts text at the cursor position, advances the cursor, re-filters, and
    /// resets selection and scroll offset to 0.
    ///
    /// Newlines are stripped - the picker filter is a single line.
    pub fn insert_text(&mut self, text: &str) {
        let flat: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        if flat.is_empty() {
            return;
        }
        let byte_offset = self
            .filter
            .grapheme_indices(true)
            .nth(self.cursor_pos)
            .map_or(self.filter.len(), |(i, _)| i);
        self.filter.insert_str(byte_offset, &flat);
        self.cursor_pos += flat.graphemes(true).count();
        self.recompute_filtered();
        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Deletes the grapheme before the cursor, decrements the cursor, re-filters, and
    /// resets selection and scroll offset to 0.
    ///
    /// No-op when the cursor is at position 0.
    pub fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let graphemes: Vec<_> = self.filter.grapheme_indices(true).collect();
        let delete_idx = self.cursor_pos - 1;
        let Some(&(start, g)) = graphemes.get(delete_idx) else {
            return;
        };
        let end = start + g.len();
        self.filter.drain(start..end);
        self.cursor_pos -= 1;
        self.recompute_filtered();
        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Moves the cursor one grapheme to the left.
    ///
    /// No-op when the cursor is already at position 0.
    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
    }

    /// Moves the cursor one grapheme to the right.
    ///
    /// No-op when the cursor is already at the end of the filter text.
    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.filter.graphemes(true).count() {
            self.cursor_pos += 1;
        }
    }

    /// Moves the selection up by one, clamping at 0, then adjusts scroll offset.
    ///
    /// `max_visible` is the number of visible rows in the picker area, used to keep
    /// the selection within the scroll window.
    pub fn move_up(&mut self, max_visible: usize) {
        if self.selection > 0 {
            self.selection -= 1;
        }
        self.ensure_visible(max_visible);
    }

    /// Moves the selection down by one, clamping at the end of the filtered list,
    /// then adjusts scroll offset.
    ///
    /// `max_visible` is the number of visible rows in the picker area, used to keep
    /// the selection within the scroll window.
    pub fn move_down(&mut self, max_visible: usize) {
        let max = self.filtered_indices.len();
        if max > 0 && self.selection < max - 1 {
            self.selection += 1;
        }
        self.ensure_visible(max_visible);
    }

    /// Moves the selection up by half of `max_visible`, clamped at 0, then
    /// adjusts the scroll offset so the selection stays visible.
    ///
    /// Half-page navigation. `delta` is at least 1 so the call is never a
    /// no-op purely because `max_visible` is small.
    pub fn page_up(&mut self, max_visible: usize) {
        let delta = (max_visible / 2).max(1);
        self.selection = self.selection.saturating_sub(delta);
        self.ensure_visible(max_visible);
    }

    /// Moves the selection down by half of `max_visible`, clamped at the end
    /// of the filtered list, then adjusts the scroll offset so the selection
    /// stays visible.
    ///
    /// Half-page navigation. `delta` is at least 1 so the call is never a
    /// no-op purely because `max_visible` is small.
    pub fn page_down(&mut self, max_visible: usize) {
        let delta = (max_visible / 2).max(1);
        let max = self.filtered_indices.len();
        let target = self.selection.saturating_add(delta);
        self.selection = if max > 0 { target.min(max - 1) } else { 0 };
        self.ensure_visible(max_visible);
    }

    /// Adjusts `scroll_offset` so that `selection` is within the visible window.
    ///
    /// `max_visible` is the number of rows that fit in the picker area.
    pub fn ensure_visible(&mut self, max_visible: usize) {
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if max_visible > 0 && self.selection >= self.scroll_offset + max_visible {
            self.scroll_offset = self.selection - max_visible + 1;
        } else {
            // Selection is within the visible window - no adjustment needed.
        }
    }

    /// Clears the filter text and resets selection, cursor, and scroll offset to 0.
    ///
    /// Does **not** clear the item list - the consumer manages the item lifecycle.
    /// The filtered list is recomputed (all items are visible when filter is empty).
    pub fn reset(&mut self) {
        self.filter.clear();
        self.selection = 0;
        self.cursor_pos = 0;
        self.scroll_offset = 0;
        self.recompute_filtered();
    }

    /// Clears the filter text and resets selection, cursor, and scroll offset.
    ///
    /// Semantically equivalent to [`SelectionState::reset`], exposed under a
    /// name that documents the user-intent ("I pressed the clear key") rather
    /// than the underlying mechanism ("I reset the widget").
    pub fn clear_filter(&mut self) {
        self.reset();
    }

    /// Returns the current filter text.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Returns the current cursor position as a grapheme index.
    #[must_use]
    pub fn cursor_pos(&self) -> usize {
        self.cursor_pos
    }

    /// Returns the current scroll offset (index of first visible result row).
    #[must_use]
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Returns the current selection index within the filtered list.
    #[must_use]
    pub fn selection(&self) -> usize {
        self.selection
    }

    /// Sets the selection index directly.
    ///
    /// Clamps to the valid range `[0, filtered_count - 1]`.
    /// Primarily for test setup - production code should use [`move_up`](Self::move_up)
    /// and [`move_down`](Self::move_down).
    pub fn set_selection(&mut self, idx: usize) {
        let max = self.filtered_indices.len();
        self.selection = if max > 0 { idx.min(max - 1) } else { 0 };
    }

    /// Returns the currently selected item, or `None` if the filtered list is empty.
    #[must_use]
    pub fn selected_item(&self) -> Option<&T> {
        let &i = self.filtered_indices.get(self.selection)?;
        self.items.get(i)
    }

    /// Mutably accesses the currently selected item via a closure.
    ///
    /// Use this for in-place mutations (e.g., toggling a boolean field).
    /// The filter and selection state are preserved.
    ///
    /// Returns `false` if no item is selected, `true` otherwise.
    pub fn with_selected_mut<F>(&mut self, f: F) -> bool
    where
        F: FnOnce(&mut T),
    {
        let Some(&i) = self.filtered_indices.get(self.selection) else {
            return false;
        };
        let Some(item) = self.items.get_mut(i) else {
            return false;
        };
        f(item);
        true
    }

    /// Returns the number of items in the filtered list.
    #[must_use]
    pub fn filtered_count(&self) -> usize {
        self.filtered_indices.len()
    }

    /// Returns the filtered item at the given index, or `None` if out of bounds.
    #[must_use]
    pub fn filtered_item(&self, idx: usize) -> Option<&T> {
        let &i = self.filtered_indices.get(idx)?;
        self.items.get(i)
    }

    /// Returns the full item list (all items, not just filtered).
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Returns the fuzzy match byte indices for the filtered item at `idx`,
    /// or `None` if out of bounds.
    ///
    /// When the filter is empty, returns `Some(&[])` (no highlighting).
    #[must_use]
    pub fn filtered_match_indices(&self, idx: usize) -> Option<&[usize]> {
        self.filtered_match_indices.get(idx).map(Vec::as_slice)
    }

    /// Recomputes the filtered index cache based on the current filter text.
    ///
    /// Delegates to [`fuzzy_filter`] over the items' display labels; see that
    /// function for the matching and ordering semantics.
    fn recompute_filtered(&mut self) {
        let labels: Vec<&str> = self.items.iter().map(PickerItem::display_label).collect();
        let matches = fuzzy_filter(&labels, &self.filter);
        self.filtered_indices = matches.iter().map(|m| m.index).collect();
        self.filtered_match_indices = matches.into_iter().map(|m| m.match_indices).collect();
    }
}

/// One fuzzy-filtered label: which input position matched, and where.
#[derive(Debug)]
pub struct FilteredMatch {
    /// The matched label's position in the input slice.
    pub index: usize,
    /// Sorted, deduplicated byte offsets into the label that the filter matched.
    /// Empty when the filter is empty (no highlighting).
    pub match_indices: Vec<usize>,
}

/// Fuzzy-filters `labels` by `filter`, returning the matches in display order.
///
/// This is the picker's shared match core, extracted so every surface that shows
/// a filtered picker list (the TUI widget and any rendered variant) matches and
/// orders identically.
///
/// An empty filter passes every label through in input order with no match
/// indices. Otherwise the filter is split on whitespace into terms, and each
/// term must independently fuzzy-match the label ([`SkimMatcherV2`], AND logic).
/// Results are sorted by cumulative match score (descending), with ties broken
/// by input order. Match byte indices from all terms are union'd, sorted, and
/// deduplicated for highlighting.
#[must_use]
pub fn fuzzy_filter(labels: &[&str], filter: &str) -> Vec<FilteredMatch> {
    if filter.is_empty() {
        return (0..labels.len())
            .map(|index| FilteredMatch {
                index,
                match_indices: Vec::new(),
            })
            .collect();
    }

    let matcher = SkimMatcherV2::default();
    let terms: Vec<&str> = filter.split_whitespace().collect();

    let mut scored: Vec<(i64, usize, Vec<usize>)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, label)| {
            match_label_terms(&matcher, label, &terms).map(|(score, indices)| (score, i, indices))
        })
        .collect();

    // Sort by score descending, then by original index ascending for stable ordering.
    scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    scored
        .into_iter()
        .map(|(_, index, match_indices)| FilteredMatch {
            index,
            match_indices,
        })
        .collect()
}

/// Attempts to fuzzy-match all `terms` against one label.
///
/// Returns `Some((cumulative_score, unioned_byte_indices))` when every term matches,
/// or `None` if any term fails to match (short-circuits on first miss).
fn match_label_terms(
    matcher: &SkimMatcherV2,
    label: &str,
    terms: &[&str],
) -> Option<(i64, Vec<usize>)> {
    let mut total_score: i64 = 0;
    let mut all_byte_indices: Vec<usize> = Vec::new();

    for term in terms {
        let (score, byte_indices) = matcher.fuzzy_indices(label, term)?;
        total_score += score;
        all_byte_indices.extend_from_slice(&byte_indices);
    }

    all_byte_indices.sort_unstable();
    all_byte_indices.dedup();
    Some((total_score, all_byte_indices))
}

/// One run of a label's text: a whole number of grapheme clusters, and
/// whether the filter matched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRun {
    /// The run's text.
    pub text: String,
    /// Whether this run matched the filter (render highlighted).
    pub matched: bool,
}

/// Splits `text` into [`TextRun`]s of matched and plain text, cutting on
/// grapheme-cluster boundaries — the shared presentation partner of
/// [`fuzzy_filter`].
///
/// `matched` holds **sorted** byte offsets into `text` (the positions
/// [`fuzzy_filter`] returns for one label). An offset takes effect only
/// when it lands exactly on a grapheme boundary; an offset inside a
/// multi-byte cluster (SkimMatcherV2 matches per `char`, and a grapheme
/// may group chars) or beyond the text is absorbed into the surrounding
/// plain run. An empty `matched` or empty `text` yields one plain run.
///
/// This is the single splitting definition every rendered surface shares:
/// a surface cutting a label into per-field runs remaps the offsets to
/// each field's own slice and calls this per field, so highlight runs
/// cannot drift between surfaces.
#[must_use]
pub fn split_matched_runs(text: &str, matched: &[usize]) -> Vec<TextRun> {
    if matched.is_empty() || text.is_empty() {
        return vec![TextRun {
            text: text.to_owned(),
            matched: false,
        }];
    }
    let mut runs = Vec::new();
    let mut current = String::new();
    let mut in_match = false;
    for (byte_offset, grapheme) in text.grapheme_indices(true) {
        let is_matched = matched.binary_search(&byte_offset).is_ok();
        if is_matched != in_match {
            if !current.is_empty() {
                runs.push(TextRun {
                    text: std::mem::take(&mut current),
                    matched: in_match,
                });
            }
            in_match = is_matched;
        }
        current.push_str(grapheme);
    }
    if !current.is_empty() {
        runs.push(TextRun {
            text: current,
            matched: in_match,
        });
    }
    runs
}
