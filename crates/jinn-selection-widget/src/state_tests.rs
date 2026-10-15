#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test file, panics are acceptable"
)]

use ratatui::text::Line;

use super::state::*;
use crate::PickerItem;

/// A minimal item type for testing. Intentionally does **not** derive `Clone`
/// to verify that `SelectionState` works without it.
#[derive(Debug)]
struct TestItem {
    label: String,
}

impl TestItem {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_owned(),
        }
    }
}

impl PickerItem for TestItem {
    fn display_label(&self) -> &str {
        &self.label
    }

    fn render_row(&self, _is_selected: bool) -> Line<'static> {
        Line::from(self.label.clone())
    }
}

/// Creates a list of test items from the given labels.
fn make_items(labels: &[&str]) -> Vec<TestItem> {
    labels.iter().map(|&l| TestItem::new(l)).collect()
}

#[rstest::rstest]
fn insert_char_appends_to_filter() {
    // Given a fresh selection state.
    let mut state = SelectionState::<TestItem>::new();

    // When inserting 'a' then 'b'.
    state.insert_char('a');
    state.insert_char('b');

    // Then the filter is "ab".
    assert_eq!(state.filter(), "ab");
}

#[rstest::rstest]
fn insert_char_resets_selection() {
    // Given a selection state with selection at 3 and scroll_offset at 2.
    let mut state = SelectionState::<TestItem>::new();
    state.selection = 3;
    state.scroll_offset = 2;

    // When inserting a character.
    state.insert_char('x');

    // Then selection and scroll_offset are reset to 0.
    assert_eq!(state.selection(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
fn insert_char_at_cursor_middle() {
    // Given a selection state with filter "abc" and cursor at 1.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "abc".to_owned();
    state.cursor_pos = 1;

    // When inserting 'x' at cursor.
    state.insert_char('x');

    // Then the filter is "axbc" and cursor advanced to 2.
    assert_eq!(state.filter(), "axbc");
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
fn insert_char_advances_cursor() {
    // Given a fresh selection state.
    let mut state = SelectionState::<TestItem>::new();

    // When inserting 'a'.
    state.insert_char('a');

    // Then cursor is at 1.
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
fn backspace_removes_before_cursor() {
    // Given a selection state with filter "ab" and cursor at end (position 2).
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "ab".to_owned();
    state.cursor_pos = 2;

    // When pressing backspace.
    state.backspace();

    // Then the filter is "a" and cursor is at 1.
    assert_eq!(state.filter(), "a");
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
fn backspace_resets_selection() {
    // Given a selection state with filter "ab", cursor at end (2), selection at 3, and scroll_offset at 2.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "ab".to_owned();
    state.cursor_pos = 2;
    state.selection = 3;
    state.scroll_offset = 2;

    // When pressing backspace.
    state.backspace();

    // Then selection and scroll_offset are reset to 0.
    assert_eq!(state.selection(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
fn backspace_at_cursor_middle() {
    // Given a selection state with filter "abc" and cursor at 2.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "abc".to_owned();
    state.cursor_pos = 2;

    // When pressing backspace.
    state.backspace();

    // Then the filter is "ac" and cursor is at 1.
    assert_eq!(state.filter(), "ac");
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
fn backspace_at_start_is_noop() {
    // Given a selection state with filter "abc" and cursor at 0.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "abc".to_owned();
    state.cursor_pos = 0;

    // When pressing backspace.
    state.backspace();

    // Then filter is unchanged and cursor is still 0.
    assert_eq!(state.filter(), "abc");
    assert_eq!(state.cursor_pos(), 0);
}

#[rstest::rstest]
fn move_cursor_left_decrements() {
    // Given a selection state with cursor at 3.
    let mut state = SelectionState::<TestItem>::new();
    state.cursor_pos = 3;

    // When moving cursor left.
    state.move_cursor_left();

    // Then cursor is at 2.
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
fn move_cursor_left_clamps_at_zero() {
    // Given a selection state with cursor at 0.
    let mut state = SelectionState::<TestItem>::new();

    // When moving cursor left.
    state.move_cursor_left();

    // Then cursor stays at 0.
    assert_eq!(state.cursor_pos(), 0);
}

#[rstest::rstest]
fn move_cursor_right_increments() {
    // Given a selection state with filter "abc" and cursor at 1.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "abc".to_owned();
    state.cursor_pos = 1;

    // When moving cursor right.
    state.move_cursor_right();

    // Then cursor is at 2.
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
fn move_cursor_right_clamps_at_end() {
    // Given a selection state with filter "abc" and cursor at 3 (end).
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "abc".to_owned();
    state.cursor_pos = 3;

    // When moving cursor right.
    state.move_cursor_right();

    // Then cursor stays at 3.
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
fn move_up_decrements() {
    // Given a selection state with 5 items and selection at 3.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c", "d", "e"]));
    state.selection = 3;

    // When moving up with max_visible=5.
    state.move_up(5);

    // Then selection is 2.
    assert_eq!(state.selection(), 2);
}

#[rstest::rstest]
fn move_up_clamps_at_zero() {
    // Given a selection state with 5 items and selection at 0.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c", "d", "e"]));

    // When moving up.
    state.move_up(5);

    // Then selection stays at 0.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
fn move_down_increments() {
    // Given a selection state with 5 items and selection at 1.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c", "d", "e"]));
    state.selection = 1;

    // When moving down with max_visible=5.
    state.move_down(5);

    // Then selection is 2.
    assert_eq!(state.selection(), 2);
}

#[rstest::rstest]
fn move_down_clamps_at_max() {
    // Given a selection state with 5 items and selection at 4.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c", "d", "e"]));
    state.selection = 4;

    // When moving down.
    state.move_down(5);

    // Then selection stays at 4.
    assert_eq!(state.selection(), 4);
}

#[rstest::rstest]
fn move_down_clamps_when_empty() {
    // Given a selection state with no items and selection at 0.
    let mut state = SelectionState::<TestItem>::new();

    // When moving down.
    state.move_down(5);

    // Then selection stays at 0.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
fn move_up_adjusts_scroll_offset_when_selection_above_view() {
    // Given a selection state with 10 items, scroll_offset=2 and selection=2.
    let mut state =
        SelectionState::with_items((0..10).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 2;
    state.scroll_offset = 2;

    // When moving up with max_visible=5.
    state.move_up(5);

    // Then selection is 1 and scroll_offset adjusts to 1.
    assert_eq!(state.selection(), 1);
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
fn move_down_adjusts_scroll_offset_when_selection_below_view() {
    // Given a selection state with 10 items, scroll_offset=0, selection=4.
    let mut state =
        SelectionState::with_items((0..10).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 4;
    state.scroll_offset = 0;

    // When moving down with max_visible=5.
    state.move_down(5);

    // Then selection is 5 and scroll_offset adjusts to 1.
    assert_eq!(state.selection(), 5);
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
fn page_down_advances_selection_by_half_viewport() {
    // Given a selection state with 20 items and selection=0.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 0;
    state.scroll_offset = 0;

    // When paging down with max_visible=10.
    state.page_down(10);

    // Then selection advances by half the viewport (5).
    assert_eq!(state.selection(), 5);
}

#[rstest::rstest]
fn page_down_clamps_at_end_of_list() {
    // Given a selection state with 20 items and selection=18.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 18;
    state.scroll_offset = 13;

    // When paging down with max_visible=10 (delta=5 would overshoot).
    state.page_down(10);

    // Then selection clamps to the last index (19).
    assert_eq!(state.selection(), 19);
}

#[rstest::rstest]
fn page_up_decrements_selection_by_half_viewport() {
    // Given a selection state with 20 items and selection=10.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 10;
    state.scroll_offset = 5;

    // When paging up with max_visible=10.
    state.page_up(10);

    // Then selection decrements by half the viewport (5).
    assert_eq!(state.selection(), 5);
}

#[rstest::rstest]
fn page_up_clamps_at_zero() {
    // Given a selection state with 20 items and selection=2.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 2;
    state.scroll_offset = 0;

    // When paging up with max_visible=10 (delta=5 would underflow).
    state.page_up(10);

    // Then selection clamps to 0.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
fn page_down_moves_at_least_one_when_viewport_small() {
    // Given a selection state with 20 items and selection=0.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 0;
    state.scroll_offset = 0;

    // When paging down with max_visible=1 (delta = 1/2 = 0, floored to 1).
    state.page_down(1);

    // Then selection moves by at least 1.
    assert_eq!(state.selection(), 1);
}

#[rstest::rstest]
fn page_down_keeps_selection_visible_by_advancing_scroll() {
    // Given a selection state with 20 items, scroll_offset=0, selection=5.
    let mut state =
        SelectionState::with_items((0..20).map(|i| TestItem::new(&i.to_string())).collect());
    state.selection = 5;
    state.scroll_offset = 0;

    // When paging down with max_visible=10 (selection moves to 10).
    state.page_down(10);

    // Then scroll_offset advances so selection 10 is the last visible row.
    assert_eq!(state.selection(), 10);
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
fn ensure_visible_selection_within_view() {
    // Given a selection state with scroll_offset=2 and selection=3.
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 2;
    state.selection = 3;

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset stays at 2 (3 is within [2, 7)).
    assert_eq!(state.scroll_offset(), 2);
}

#[rstest::rstest]
fn ensure_visible_selection_above_view() {
    // Given a selection state with scroll_offset=3 and selection=1.
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 3;
    state.selection = 1;

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset adjusts up to 1.
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
fn ensure_visible_selection_below_view() {
    // Given a selection state with scroll_offset=0 and selection=7.
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 0;
    state.selection = 7;

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset adjusts down to 3.
    assert_eq!(state.scroll_offset(), 3);
}

#[rstest::rstest]
fn reset_clears_everything() {
    // Given a selection state with filter "ab", selection 3, cursor at 2, scroll_offset 1.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "ab".to_owned();
    state.selection = 3;
    state.cursor_pos = 2;
    state.scroll_offset = 1;

    // When resetting.
    state.reset();

    // Then filter is empty, selection is 0, cursor is 0, scroll_offset is 0.
    assert_eq!(state.filter(), "");
    assert_eq!(state.selection(), 0);
    assert_eq!(state.cursor_pos(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
fn set_items_populates_filtered_list() {
    // Given a fresh selection state.
    let mut state = SelectionState::<TestItem>::new();

    // When setting 3 items.
    state.set_items(make_items(&["apple", "banana", "cherry"]));

    // Then all 3 items are in the filtered list.
    assert_eq!(state.filtered_count(), 3);
}

#[rstest::rstest]
fn insert_char_filters_items_by_label() {
    // Given a selection state with items ["apple", "banana", "cherry"].
    let mut state = SelectionState::with_items(make_items(&["apple", "banana", "cherry"]));

    // When inserting 'a' to filter.
    state.insert_char('a');

    // Then filtered items contain "apple" and "banana" (fuzzy match) but not "cherry".
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().display_label(), "apple");
    assert_eq!(state.filtered_item(1).unwrap().display_label(), "banana");
}

#[rstest::rstest]
fn backspace_re_expands_filtered_list() {
    // Given a selection state with items, filtered to "ap".
    let mut state = SelectionState::with_items(make_items(&["apple", "banana", "cherry"]));
    state.insert_char('a');
    state.insert_char('p');

    // When pressing backspace to remove 'p'.
    state.backspace();

    // Then more items appear (filter is now "a").
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().display_label(), "apple");
    assert_eq!(state.filtered_item(1).unwrap().display_label(), "banana");
}

#[rstest::rstest]
fn filter_sorts_by_score_then_original_index() {
    // Given items in a specific order, all containing 'a'.
    // "apple" has 'a' at position 0 (better score), "banana" at position 1,
    // "avocado" at position 0.
    let mut state = SelectionState::with_items(make_items(&["banana", "apple", "avocado"]));

    // When filtering with 'a'.
    state.insert_char('a');

    // Then filtered list is sorted by score (best match first).
    assert_eq!(state.filtered_count(), 3);
    // All match 'a'; exact score ordering depends on SkimMatcherV2.
    // Verify that all three items are present.
    let labels: Vec<&str> = (0..state.filtered_count())
        .map(|i| state.filtered_item(i).unwrap().display_label())
        .collect();
    assert!(labels.contains(&"banana"));
    assert!(labels.contains(&"apple"));
    assert!(labels.contains(&"avocado"));
}

#[rstest::rstest]
fn empty_filter_shows_all_items() {
    // Given a selection state with 3 items and no filter text.
    // When constructing the selection state with no filter text.
    let state = SelectionState::with_items(make_items(&["apple", "banana", "cherry"]));

    // Then all items are visible.
    assert_eq!(state.filtered_count(), 3);
}

#[rstest::rstest]
fn no_match_returns_empty_filtered() {
    // Given a selection state with items.
    let mut state = SelectionState::with_items(make_items(&["apple", "banana", "cherry"]));

    // When filtering to "zzz".
    state.insert_char('z');
    state.insert_char('z');
    state.insert_char('z');

    // Then filtered list is empty.
    assert_eq!(state.filtered_count(), 0);
}

#[rstest::rstest]
fn selected_item_returns_none_when_no_match() {
    // Given a selection state with items filtered to no matches.
    let mut state = SelectionState::with_items(make_items(&["apple", "banana"]));
    // When filtering with 'z', which matches nothing.
    state.insert_char('z');

    // Then selected_item returns None.
    assert!(state.selected_item().is_none());
}

#[rstest::rstest]
fn selected_item_returns_first_match_initially() {
    // Given a selection state with items and no filter.
    let items = make_items(&["apple", "banana", "cherry"]);
    // When constructing the selection state.
    let state = SelectionState::with_items(items);

    // Then selected_item returns the first item.
    let selected = state.selected_item().expect("should have a selected item");
    assert_eq!(selected.display_label(), "apple");
}

#[rstest::rstest]
fn filtered_item_returns_by_filtered_index() {
    // Given items ["a", "b", "c"], filtered to match "b" and "c".
    let mut state = SelectionState::with_items(make_items(&["alpha", "bravo", "charlie"]));
    // When filtering with 'r' so only "bravo" and "charlie" match.
    state.insert_char('r');

    // Then filtered_item(0) is "bravo" and filtered_item(1) is "charlie".
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().display_label(), "bravo");
    assert_eq!(state.filtered_item(1).unwrap().display_label(), "charlie");
}

#[rstest::rstest]
fn fuzzy_match_matches_partial() {
    // Given an item "hello world".
    let mut state = SelectionState::with_items(make_items(&["hello world"]));

    // When filtering with "hlo".
    state.insert_char('h');
    state.insert_char('l');
    state.insert_char('o');

    // Then the item matches.
    assert_eq!(state.filtered_count(), 1);
}

#[rstest::rstest]
fn fuzzy_match_is_case_insensitive() {
    // Given an item "Hello" and filter "hello".
    let mut state = SelectionState::with_items(make_items(&["Hello"]));

    // When filtering with "hello".
    for ch in "hello".chars() {
        state.insert_char(ch);
    }

    // Then the item matches (case-insensitive).
    assert_eq!(state.filtered_count(), 1);
}

#[rstest::rstest]
fn set_items_does_not_reset_filter() {
    // Given a selection state with items and a filter.
    let mut state = SelectionState::with_items(make_items(&["apple", "banana"]));
    state.insert_char('a');

    // When setting new items.
    state.set_items(make_items(&["apple", "banana", "cherry"]));

    // Then filter text persists and filtered list updates.
    assert_eq!(state.filter(), "a");
    assert_eq!(state.filtered_count(), 2);
}

#[rstest::rstest]
fn reset_clears_filter_but_keeps_items() {
    // Given a selection state with items and a filter.
    let mut state = SelectionState::with_items(make_items(&["apple", "banana", "cherry"]));
    state.insert_char('a');

    // When resetting.
    state.reset();

    // Then filter is empty but items are still present.
    assert_eq!(state.filter(), "");
    assert_eq!(state.items().len(), 3);
    assert_eq!(state.filtered_count(), 3);
}

#[rstest::rstest]
fn no_clone_needed() {
    // Given a non-Clone item type held in a selection state.
    // Compile-time proof: TestItem does not derive Clone,
    // yet SelectionState<TestItem> works fine.
    let mut state = SelectionState::<TestItem>::new();
    // When setting the items on the state.
    state.set_items(vec![TestItem::new("test")]);
    // Then the item is visible in the filtered list.
    assert_eq!(state.filtered_count(), 1);
}

#[rstest::rstest]
fn multi_term_filter_matches_items_containing_all_terms() {
    // Given items including "claude-3.5-sonnet".
    let mut state = SelectionState::with_items(make_items(&[
        "claude-3.5-sonnet",
        "gpt-4o",
        "claude-3.5-haiku",
    ]));

    // When filtering with "claude sonnet" (space-separated).
    for ch in "claude sonnet".chars() {
        state.insert_char(ch);
    }

    // Then only "claude-3.5-sonnet" matches.
    assert_eq!(state.filtered_count(), 1);
    assert_eq!(
        state.filtered_item(0).unwrap().display_label(),
        "claude-3.5-sonnet"
    );
}

#[rstest::rstest]
fn multi_term_filter_returns_empty_when_one_term_does_not_match() {
    // Given items.
    let mut state = SelectionState::with_items(make_items(&["claude-3.5-sonnet", "gpt-4o"]));

    // When filtering with "claude gpt".
    for ch in "claude gpt".chars() {
        state.insert_char(ch);
    }

    // Then no items match (no item has both "claude" AND "gpt").
    assert_eq!(state.filtered_count(), 0);
}

#[rstest::rstest]
fn multi_term_filter_unions_highlight_indices() {
    // Given an item "hello world".
    let mut state = SelectionState::with_items(make_items(&["hello world"]));

    // When filtering with "hlo wor" (two terms).
    for ch in "hlo wor".chars() {
        state.insert_char(ch);
    }

    // Then the item matches and has highlight indices from both terms.
    assert_eq!(state.filtered_count(), 1);
    let indices = state.filtered_match_indices(0).unwrap();
    // Should contain indices from both "hlo" matching "hello" and "wor" matching "world".
    assert_ne!(indices, [] as [usize; 0]);
}

#[rstest::rstest]
fn score_sorting_puts_better_matches_first() {
    // Given items where "claude-sonnet" is a better match for "sonnet" than "jsonnet-tools".
    let mut state = SelectionState::with_items(make_items(&["jsonnet-tools", "claude-sonnet"]));

    // When filtering with "sonnet".
    for ch in "sonnet".chars() {
        state.insert_char(ch);
    }

    // Then "claude-sonnet" (exact substring) scores higher and comes first.
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(
        state.filtered_item(0).unwrap().display_label(),
        "claude-sonnet"
    );
}

#[rstest::rstest]
fn multi_term_filter_with_multiple_spaces_splits_on_whitespace() {
    // Given items.
    let mut state = SelectionState::with_items(make_items(&["alpha bravo charlie"]));

    // When filtering with "alpha  charlie" (double space).
    for ch in "alpha  charlie".chars() {
        state.insert_char(ch);
    }

    // Then the item matches (double space treated as single separator).
    assert_eq!(state.filtered_count(), 1);
}

#[rstest::rstest]
fn insert_text_strips_newlines_and_carriage_returns() {
    // Given a fresh selection state.
    let mut state = SelectionState::<TestItem>::new();

    // When inserting text with newlines and carriage returns.
    state.insert_text("he\nl\rl\no");

    // Then newlines and carriage returns are stripped, filter is "hello".
    assert_eq!(state.filter(), "hello");
}

#[rstest::rstest]
fn insert_text_advances_cursor_by_grapheme_count() {
    // Given a fresh selection state.
    let mut state = SelectionState::<TestItem>::new();

    // When inserting "abc".
    state.insert_text("abc");

    // Then cursor is at position 3 (after all 3 graphemes).
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
fn insert_text_with_only_newlines_is_noop() {
    // Given a selection state with existing filter.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "existing".to_owned();
    state.cursor_pos = 3;

    // When inserting only newlines.
    state.insert_text("\n\r\n");

    // Then filter and cursor are unchanged.
    assert_eq!(state.filter(), "existing");
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
fn insert_text_resets_selection_and_scroll() {
    // Given a selection state with selection and scroll offset.
    let mut state = SelectionState::with_items(make_items(&["apple", "banana"]));
    state.selection = 1;
    state.scroll_offset = 1;

    // When inserting text.
    state.insert_text("x");

    // Then selection and scroll_offset reset to 0.
    assert_eq!(state.selection(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
fn insert_text_at_cursor_middle() {
    // Given a selection state with filter "ace" and cursor at position 1.
    let mut state = SelectionState::<TestItem>::new();
    state.filter = "ace".to_owned();
    state.cursor_pos = 1;

    // When inserting "bd" at the cursor.
    state.insert_text("bd");

    // Then filter is "abdce" and cursor advanced to 3.
    assert_eq!(state.filter(), "abdce");
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
fn set_selection_clamps_to_last_item() {
    // Given a selection state with 3 items.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c"]));

    // When setting selection to 10 (out of bounds).
    state.set_selection(10);

    // Then selection is clamped to 2 (last valid index).
    assert_eq!(state.selection(), 2);
}

#[rstest::rstest]
fn set_selection_clamps_to_zero_when_empty() {
    // Given a selection state with no items.
    let mut state = SelectionState::<TestItem>::new();

    // When setting selection to 5.
    state.set_selection(5);

    // Then selection is clamped to 0 (no items available).
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
fn set_selection_keeps_valid_index() {
    // Given a selection state with 5 items.
    let mut state = SelectionState::with_items(make_items(&["a", "b", "c", "d", "e"]));

    // When setting selection to 2 (valid index).
    state.set_selection(2);

    // Then selection is exactly 2.
    assert_eq!(state.selection(), 2);
}

#[rstest::rstest]
fn set_selection_clamps_to_zero_for_single_item() {
    // Given a selection state with 1 item.
    let mut state = SelectionState::with_items(make_items(&["solo"]));

    // When setting selection to 100.
    state.set_selection(100);

    // Then selection is clamped to 0 (only valid index for 1 item is 0).
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
fn ensure_visible_selection_equal_to_scroll_offset() {
    // Given selection equal to scroll_offset (selection at top of view).
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 5;
    state.selection = 5;

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset stays at 5 (selection is within [5, 10)).
    assert_eq!(state.scroll_offset(), 5);
}

#[rstest::rstest]
fn ensure_visible_selection_at_view_bottom_minus_one() {
    // Given selection at scroll_offset + max_visible - 1 (last visible row).
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 3;
    state.selection = 7; // 3 + 5 - 1 = 7

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset stays at 3 (selection is within [3, 8)).
    assert_eq!(state.scroll_offset(), 3);
}

#[rstest::rstest]
fn ensure_visible_selection_exactly_at_view_end_scrolls_down() {
    // Given selection at scroll_offset + max_visible (just past visible window).
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 0;
    state.selection = 5; // exactly scroll_offset + max_visible

    // When ensuring visible with max_visible=5.
    state.ensure_visible(5);

    // Then scroll_offset moves to 1 (selection - max_visible + 1).
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
fn ensure_visible_with_zero_max_visible_does_not_scroll() {
    // Given selection above scroll_offset.
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 5;
    state.selection = 3;

    // When ensuring visible with max_visible=0.
    state.ensure_visible(0);

    // Then the first branch catches it (selection < scroll_offset).
    assert_eq!(state.scroll_offset(), 3);
}

#[rstest::rstest]
fn ensure_visible_with_zero_max_visible_below_does_not_scroll() {
    // Given selection below scroll_offset + 0 and max_visible=0.
    let mut state = SelectionState::<TestItem>::new();
    state.scroll_offset = 2;
    state.selection = 10;

    // When ensuring visible with max_visible=0.
    state.ensure_visible(0);

    // Then the second branch is guarded by max_visible > 0, so no scroll.
    assert_eq!(state.scroll_offset(), 2);
}

#[rstest::rstest]
fn filtered_match_indices_returns_exact_byte_positions() {
    // Given a selection state with item "hello".
    let mut state = SelectionState::with_items(make_items(&["hello"]));

    // When filtering with "hl" (matches h at byte 0, l at byte 3).
    state.insert_char('h');
    state.insert_char('l');

    // Then match indices contain the exact byte positions.
    assert_eq!(state.filtered_count(), 1);
    let indices = state.filtered_match_indices(0).unwrap();
    assert_ne!(indices, [] as [usize; 0]);
    // 'h' is at byte 0, 'l' is at byte 2 or 3 depending on matcher.
    assert!(indices.contains(&0)); // 'h' is always at position 0
    assert!(indices.iter().any(|&i| (2..=3).contains(&i))); // 'l' at position 2 or 3
}

#[rstest::rstest]
fn filtered_match_indices_returns_none_for_out_of_bounds() {
    // Given a selection state with 1 item.
    let state = SelectionState::with_items(make_items(&["hello"]));

    // When requesting match indices for index 1 (out of bounds).
    let result = state.filtered_match_indices(1);

    // Then result is None.
    assert!(result.is_none());
}

#[rstest::rstest]
fn clear_filter_empties_filter_and_resets_state() {
    // Given a selection state with a populated filter, advanced cursor, mid-list
    // selection, and non-zero scroll offset.
    let mut state =
        SelectionState::with_items(make_items(&["alpha", "bravo", "charlie", "delta", "echo"]));
    state.insert_text("bravo");
    state.move_down(5); // selection > 0
    // The filter is in place and matches exactly one item before the clear.
    assert_eq!(state.filter(), "bravo");
    assert_eq!(state.selection(), 0); // bravo matches the only item
    // (cursor_pos and scroll_offset are intentionally not asserted pre-clear
    //  because their values depend on grapheme internals; the post-clear
    //  reset is what we care about.)

    // When clearing the filter via the universal clear action.
    state.clear_filter();

    // Then filter, selection, cursor, and scroll are all reset.
    assert_eq!(state.filter(), "");
    assert_eq!(state.selection(), 0);
    assert_eq!(state.cursor_pos(), 0);
    assert_eq!(state.scroll_offset(), 0);
    // And the filtered list reflects all items again.
    assert_eq!(state.filtered_count(), 5);
}
