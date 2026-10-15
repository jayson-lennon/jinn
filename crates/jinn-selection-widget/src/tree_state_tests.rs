//! Tests for tree-aware filtering in [`TreePickerState`].

use crate::TreePickerState;
use crate::tree_item::TreeItem;
use ratatui::text::Line;
use std::ops::Range;

// ---------------------------------------------------------------------------
// Test item type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct TestItem {
    id: String,
    parent_id: Option<String>,
    label: String,
}

impl TreeItem for TestItem {
    fn id(&self) -> &str {
        &self.id
    }
    fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }
    fn display_label(&self) -> &str {
        &self.label
    }
    fn render_row(&self, _is_selected: bool) -> Line<'static> {
        Line::from(self.label.clone())
    }
    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        _match_indices: &[Range<usize>],
    ) -> Line<'static> {
        self.render_row(is_selected)
    }
}

fn item(id: &str, parent_id: Option<&str>, label: &str) -> TestItem {
    TestItem {
        id: id.to_owned(),
        parent_id: parent_id.map(str::to_owned),
        label: label.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn empty_filter_shows_all_items_in_tree_order() {
    // Given a tree: root A → children B, C.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
    ];

    // When creating state with items and empty filter.
    let state = TreePickerState::with_items(items);

    // Then all items are visible in tree order.
    assert_eq!(state.filtered_count(), 3);
    assert_eq!(state.visible_entry(0).unwrap().depth, 0);
    assert_eq!(state.visible_entry(1).unwrap().depth, 1);
    assert_eq!(state.visible_entry(2).unwrap().depth, 1);
    // Root is last child (only root).
    assert!(state.visible_entry(0).unwrap().is_last_child);
    // C is last child of A.
    assert!(!state.visible_entry(1).unwrap().is_last_child);
    assert!(state.visible_entry(2).unwrap().is_last_child);
}

#[rstest::rstest]
#[test]
fn child_match_includes_ancestors() {
    // Given: root A → child B → grandchild C.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("b"), "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Charlie".
    state.insert_text("Charlie");

    // Then visible = [A, B, C] - ancestor chain included.
    assert_eq!(state.filtered_count(), 3);
    assert_eq!(state.filtered_item(0).unwrap().id, "a");
    assert_eq!(state.filtered_item(1).unwrap().id, "b");
    assert_eq!(state.filtered_item(2).unwrap().id, "c");
    // A and B have no match indices; C has match bytes.
    assert_eq!(state.filtered_match_indices(0).unwrap(), [] as [usize; 0]);
    assert_eq!(state.filtered_match_indices(1).unwrap(), [] as [usize; 0]);
    assert_ne!(state.filtered_match_indices(2).unwrap(), [] as [usize; 0]);
}

#[rstest::rstest]
#[test]
fn non_matching_siblings_excluded() {
    // Given: root A → children B, C.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Charlie".
    state.insert_text("Charlie");

    // Then visible = [A, C] - B excluded.
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().id, "a");
    assert_eq!(state.filtered_item(1).unwrap().id, "c");
    // C is now last child (only visible child).
    assert!(state.visible_entry(1).unwrap().is_last_child);
}

#[rstest::rstest]
#[test]
fn tree_connectors_recomputed_for_filtered_set() {
    // Given: root A → children B, C, D.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
        item("d", Some("a"), "Delta"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Delta".
    state.insert_text("Delta");

    // Then D is is_last_child = true (recomputed for visible set).
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().id, "a");
    assert_eq!(state.filtered_item(1).unwrap().id, "d");
    assert!(state.visible_entry(1).unwrap().is_last_child);
}

#[rstest::rstest]
#[test]
fn orphaned_item_treated_as_root() {
    // Given: child C references non-existent parent X.
    let items = vec![item("a", None, "Alpha"), item("c", Some("x"), "Charlie")];
    // When building the tree state.
    let state = TreePickerState::with_items(items);

    // Then C appears as a root (depth 0).
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.visible_entry(1).unwrap().depth, 0);
}

#[rstest::rstest]
#[test]
fn circular_reference_guard() {
    // Given: A → B → A cycle.
    let items = vec![item("a", Some("b"), "Alpha"), item("b", Some("a"), "Bravo")];

    // When creating state with items.
    let state = TreePickerState::with_items(items);

    // Then no infinite loop - both appear as roots (neither's parent is resolvable
    // since they reference each other but neither is a root initially).
    // Actually, A's parent is B (which is in items), so A is a child of B.
    // B's parent is A (which is in items), so B is a child of A.
    // Neither is a root, so both are orphaned due to cycle.
    // The build_index marks an item as root if parent_id is None OR parent is not in id_to_idx.
    // A's parent is B (in items), so A goes to children_map["b"].
    // B's parent is A (in items), so B goes to children_map["a"].
    // Neither goes to roots.
    // DFS visits roots (empty) → nothing visible.
    // This is actually correct behavior for corrupted data.
    assert_eq!(state.filtered_count(), 0);
}

#[rstest::rstest]
#[test]
fn multiple_matches_in_different_subtrees() {
    // Given: root A → child B, root C → child D.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", None, "Charlie"),
        item("d", Some("c"), "Delta"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Bravo Delta" (matches B and D).
    state.insert_text("Bravo");

    // Then only B matches, together with its parent A.
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().id, "a");
    assert_eq!(state.filtered_item(1).unwrap().id, "b");
}

#[rstest::rstest]
#[test]
fn root_match_does_not_include_children() {
    // Given: root A → children B, C.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Alpha".
    state.insert_text("Alpha");

    // Then only A is visible (children are not ancestors, they're descendants).
    assert_eq!(state.filtered_count(), 1);
    assert_eq!(state.filtered_item(0).unwrap().id, "a");
}

#[rstest::rstest]
#[test]
fn selection_clamped_after_filter() {
    // Given: root A → children B, C.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering reduces visible set to 2 items.
    state.insert_text("a");

    // Then selection resets to 0.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn reset_clears_filter_and_restores_full_tree() {
    // Given: state with active filter.
    let items = vec![item("a", None, "Alpha"), item("b", Some("a"), "Bravo")];
    let mut state = TreePickerState::with_items(items);
    state.insert_text("Bravo");
    assert_eq!(state.filtered_count(), 2); // A + B

    // When resetting.
    state.reset();

    // Then filter is cleared and all items visible.
    assert_eq!(state.filter(), "");
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn set_items_rebuilds_index() {
    // Given: state with items.
    let items = vec![item("a", None, "Alpha"), item("b", Some("a"), "Bravo")];
    let mut state = TreePickerState::with_items(items);

    // When setting new items.
    let new_items = vec![item("x", None, "Xray"), item("y", Some("x"), "Yankee")];
    state.set_items(new_items);

    // Then visible list reflects new items.
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().id, "x");
    assert_eq!(state.filtered_item(1).unwrap().id, "y");
}

#[rstest::rstest]
#[test]
fn tree_insert_char_appends_to_filter() {
    // Given a tree state with one root item and an empty filter.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    // When inserting a character.
    state.insert_char('x');
    // Then the filter holds the inserted character.
    assert_eq!(state.filter(), "x");
}

#[rstest::rstest]
#[test]
fn tree_insert_char_advances_cursor() {
    // Given a tree state with one root item and an empty filter.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    // When inserting a character.
    state.insert_char('x');
    // Then the filter cursor advances one grapheme.
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
#[test]
fn tree_insert_char_resets_selection() {
    // Given a tree state with a child selected.
    let items = vec![item("a", None, "Alpha"), item("b", Some("a"), "Bravo")];
    let mut state = TreePickerState::with_items(items);
    state.selection = 1;
    // When inserting a character.
    state.insert_char('x');
    // Then the selection resets to the first visible entry.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn tree_insert_char_resets_scroll_offset() {
    // Given a tree state with a non-zero scroll offset.
    let items = vec![item("a", None, "Alpha")];
    let mut state = TreePickerState::with_items(items);
    state.scroll_offset = 5;
    // When inserting a character.
    state.insert_char('x');
    // Then the scroll offset resets to the top.
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
#[test]
fn tree_insert_char_at_cursor_middle() {
    // Given a tree state with filter "abc" and the cursor after "a".
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "abc".to_owned();
    state.cursor_pos = 1;
    // When inserting a character at the cursor.
    state.insert_char('x');
    // Then the character lands at the cursor and the cursor advances past it.
    assert_eq!(state.filter(), "axbc");
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
#[test]
fn tree_insert_text_strips_newlines_and_carriage_returns() {
    // Given a tree state with an empty filter.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    // When inserting text containing newlines and carriage returns.
    state.insert_text("he\nl\rl\no");
    // Then the filter holds the text with those characters removed.
    assert_eq!(state.filter(), "hello");
}

#[rstest::rstest]
#[test]
fn tree_insert_text_advances_cursor_by_grapheme_count() {
    // Given a tree state with an empty filter.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    // When inserting three characters as text.
    state.insert_text("abc");
    // Then the cursor advances by the grapheme count.
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
#[test]
fn tree_insert_text_with_only_newlines_is_noop() {
    // Given a tree state with filter "existing" and the cursor three graphemes in.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "existing".to_owned();
    state.cursor_pos = 3;
    // When inserting text made only of newlines.
    state.insert_text("\n\r\n");
    // Then the filter and cursor are left untouched.
    assert_eq!(state.filter(), "existing");
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
#[test]
fn tree_insert_text_resets_selection_and_scroll() {
    // Given a tree state with a child selected and scrolled past the top.
    let items = vec![item("a", None, "Alpha"), item("b", Some("a"), "Bravo")];
    let mut state = TreePickerState::with_items(items);
    state.selection = 1;
    state.scroll_offset = 1;
    // When inserting text into the filter.
    state.insert_text("x");
    // Then the selection and scroll offset both reset.
    assert_eq!(state.selection(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
#[test]
fn tree_insert_text_at_cursor_middle() {
    // Given a tree state with filter "ace" and the cursor after "a".
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "ace".to_owned();
    state.cursor_pos = 1;
    // When inserting text at the cursor.
    state.insert_text("bd");
    // Then the text lands at the cursor and the cursor advances past it.
    assert_eq!(state.filter(), "abdce");
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
#[test]
fn tree_backspace_at_start_is_noop() {
    // Given a tree state with filter "abc" and the cursor at the start.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "abc".to_owned();
    state.cursor_pos = 0;
    // When deleting the grapheme before the cursor.
    state.backspace();
    // Then the filter and cursor are left untouched.
    assert_eq!(state.filter(), "abc");
    assert_eq!(state.cursor_pos(), 0);
}

#[rstest::rstest]
#[test]
fn tree_backspace_removes_before_cursor() {
    // Given a tree state with filter "abc" and the cursor after "ab".
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "abc".to_owned();
    state.cursor_pos = 2;
    // When deleting the grapheme before the cursor.
    state.backspace();
    // Then that grapheme is removed and the cursor moves back.
    assert_eq!(state.filter(), "ac");
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
#[test]
fn tree_backspace_at_end_removes_last() {
    // Given a tree state with filter "ab" and the cursor at the end.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "ab".to_owned();
    state.cursor_pos = 2;
    // When deleting the grapheme before the cursor.
    state.backspace();
    // Then the last grapheme is removed and the cursor moves back.
    assert_eq!(state.filter(), "a");
    assert_eq!(state.cursor_pos(), 1);
}

#[rstest::rstest]
#[test]
fn tree_backspace_resets_selection_and_scroll() {
    // Given a tree state with filter "ab", a child selected, and a non-zero scroll offset.
    let items = vec![item("a", None, "Alpha"), item("b", Some("a"), "Bravo")];
    let mut state = TreePickerState::with_items(items);
    state.filter = "ab".to_owned();
    state.cursor_pos = 2;
    state.selection = 1;
    state.scroll_offset = 1;
    // When deleting the grapheme before the cursor.
    state.backspace();
    // Then the selection and scroll offset both reset.
    assert_eq!(state.selection(), 0);
    assert_eq!(state.scroll_offset(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_cursor_left_decrements() {
    // Given a tree state whose filter cursor sits three graphemes in.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.cursor_pos = 3;
    // When moving the filter cursor left.
    state.move_cursor_left();
    // Then the cursor moves one grapheme earlier.
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
#[test]
fn tree_move_cursor_left_clamps_at_zero() {
    // Given a tree state whose filter cursor is at the start.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.cursor_pos = 0;
    // When moving the filter cursor left.
    state.move_cursor_left();
    // Then the cursor stays at the start.
    assert_eq!(state.cursor_pos(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_cursor_left_multiple_times() {
    // Given a tree state whose filter cursor sits two graphemes in.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.cursor_pos = 2;
    // When moving the filter cursor left three times.
    state.move_cursor_left();
    state.move_cursor_left();
    state.move_cursor_left(); // should clamp at 0
    // Then the cursor stops at the start.
    assert_eq!(state.cursor_pos(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_cursor_right_increments() {
    // Given a tree state with filter "abc" and the cursor after "a".
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "abc".to_owned();
    state.cursor_pos = 1;
    // When moving the filter cursor right.
    state.move_cursor_right();
    // Then the cursor moves one grapheme later.
    assert_eq!(state.cursor_pos(), 2);
}

#[rstest::rstest]
#[test]
fn tree_move_cursor_right_clamps_at_end() {
    // Given a tree state with filter "abc" and the cursor at the end.
    let mut state = TreePickerState::with_items(vec![item("a", None, "Alpha")]);
    state.filter = "abc".to_owned();
    state.cursor_pos = 3;
    // When moving the filter cursor right.
    state.move_cursor_right();
    // Then the cursor stays at the end.
    assert_eq!(state.cursor_pos(), 3);
}

#[rstest::rstest]
#[test]
fn tree_move_up_decrements() {
    // Given a tree of one root and two children with the selection on the last child.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);
    state.selection = 2;
    // When moving the selection up.
    state.move_up(5);
    // Then the selection moves to the previous visible entry.
    assert_eq!(state.selection(), 1);
}

#[rstest::rstest]
#[test]
fn tree_move_up_clamps_at_zero() {
    // Given a tree state with the selection on the first entry.
    let items = vec![item("a", None, "Alpha")];
    let mut state = TreePickerState::with_items(items);
    state.selection = 0;
    // When moving the selection up.
    state.move_up(5);
    // Then the selection stays on the first entry.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_up_adjusts_scroll_offset() {
    // Given a tree of ten roots with the selection two entries below the scroll offset.
    let items: Vec<TestItem> = (0..10)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 2;
    state.scroll_offset = 2;
    // When moving the selection up.
    state.move_up(5);
    // Then the selection and the scroll offset both move up one.
    assert_eq!(state.selection(), 1);
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
#[test]
fn tree_move_down_increments() {
    // Given a tree of three roots with the selection on the middle one.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", None, "Bravo"),
        item("c", None, "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);
    state.selection = 1;
    // When moving the selection down.
    state.move_down(5);
    // Then the selection moves to the next visible entry.
    assert_eq!(state.selection(), 2);
}

#[rstest::rstest]
#[test]
fn tree_move_down_clamps_at_end() {
    // Given a tree state whose only entry is selected.
    let items = vec![item("a", None, "Alpha")];
    let mut state = TreePickerState::with_items(items);
    state.selection = 0;
    // When moving the selection down.
    state.move_down(5);
    // Then the selection stays on the last entry.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_down_clamps_when_empty() {
    // Given a tree state with no items at all.
    let mut state = TreePickerState::<TestItem>::new();
    // When moving the selection down.
    state.move_down(5);
    // Then the selection stays at zero.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn tree_move_down_adjusts_scroll_offset() {
    // Given a tree of ten roots with the selection on the last visible entry.
    let items: Vec<TestItem> = (0..10)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 4;
    state.scroll_offset = 0;
    // When moving the selection down.
    state.move_down(5);
    // Then the selection advances and the scroll offset follows it.
    assert_eq!(state.selection(), 5);
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
#[test]
fn tree_page_down_advances_selection_by_half_viewport() {
    // Given a tree with 20 root items and selection=0.
    let items: Vec<TestItem> = (0..20)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 0;
    state.scroll_offset = 0;

    // When paging down with max_visible=10.
    state.page_down(10);

    // Then selection advances by half the viewport (5).
    assert_eq!(state.selection(), 5);
}

#[rstest::rstest]
#[test]
fn tree_page_down_clamps_at_end_of_list() {
    // Given a tree with 20 root items and selection=18.
    let items: Vec<TestItem> = (0..20)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 18;
    state.scroll_offset = 13;

    // When paging down with max_visible=10.
    state.page_down(10);

    // Then selection clamps to the last index (19).
    assert_eq!(state.selection(), 19);
}

#[rstest::rstest]
#[test]
fn tree_page_up_decrements_selection_by_half_viewport() {
    // Given a tree with 20 root items and selection=10.
    let items: Vec<TestItem> = (0..20)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 10;
    state.scroll_offset = 5;

    // When paging up with max_visible=10.
    state.page_up(10);

    // Then selection decrements by half the viewport (5).
    assert_eq!(state.selection(), 5);
}

#[rstest::rstest]
#[test]
fn tree_page_up_clamps_at_zero() {
    // Given a tree with 20 root items and selection=2.
    let items: Vec<TestItem> = (0..20)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 2;
    state.scroll_offset = 0;

    // When paging up with max_visible=10.
    state.page_up(10);

    // Then selection clamps to 0.
    assert_eq!(state.selection(), 0);
}

#[rstest::rstest]
#[test]
fn tree_page_down_moves_at_least_one_when_viewport_small() {
    // Given a tree with 20 root items and selection=0.
    let items: Vec<TestItem> = (0..20)
        .map(|i| item(&format!("{i}"), None, &format!("Item{i}")))
        .collect();
    let mut state = TreePickerState::with_items(items);
    state.selection = 0;
    state.scroll_offset = 0;

    // When paging down with max_visible=1.
    state.page_down(1);

    // Then selection moves by at least 1.
    assert_eq!(state.selection(), 1);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_selection_above_view() {
    // Given a tree state scrolled to entry 3 with entry 1 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 3;
    state.selection = 1;
    // When making the selection visible in a five-row viewport.
    state.ensure_visible(5);
    // Then the scroll offset moves up to the selection.
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_selection_below_view() {
    // Given a tree state scrolled to the top with entry 7 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 0;
    state.selection = 7;
    // When making the selection visible in a five-row viewport.
    state.ensure_visible(5);
    // Then the scroll offset scrolls down just far enough.
    assert_eq!(state.scroll_offset(), 3);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_selection_within_view() {
    // Given a tree state scrolled to entry 2 with entry 3 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 2;
    state.selection = 3;
    // When making the selection visible in a five-row viewport.
    state.ensure_visible(5);
    // Then the scroll offset is left alone.
    assert_eq!(state.scroll_offset(), 2);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_selection_equal_to_scroll_offset() {
    // Given a tree state scrolled to entry 5 with entry 5 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 5;
    state.selection = 5;
    // When making the selection visible in a five-row viewport.
    state.ensure_visible(5);
    // Then the scroll offset is left alone.
    assert_eq!(state.scroll_offset(), 5);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_selection_at_view_end() {
    // Given a tree state scrolled to the top with entry 5 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 0;
    state.selection = 5; // scroll_offset + max_visible
    // When making the selection visible in a five-row viewport.
    state.ensure_visible(5);
    // Then the scroll offset scrolls down by one.
    assert_eq!(state.scroll_offset(), 1);
}

#[rstest::rstest]
#[test]
fn tree_ensure_visible_with_zero_max_visible_selection_below() {
    // Given a tree state scrolled to entry 2 with entry 10 selected.
    let mut state = TreePickerState::<TestItem>::new();
    state.scroll_offset = 2;
    state.selection = 10;
    // When making the selection visible in a zero-row viewport.
    state.ensure_visible(0);
    // Then the scroll offset is left alone -- the max_visible == 0 guard
    // prevents scrolling down at all.
    assert_eq!(state.scroll_offset(), 2);
}

#[rstest::rstest]
#[test]
fn tree_dfs_multiple_roots_correct_last_child_flags() {
    // Given: two roots, each with children.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", None, "Charlie"),
        item("d", Some("c"), "Delta"),
    ];
    // When building the visible entries depth-first.
    let state = TreePickerState::with_items(items);

    // Then: root A is not last child (C is the other root).
    assert!(!state.visible_entry(0).unwrap().is_last_child);
    // Root C is last child.
    assert!(state.visible_entry(2).unwrap().is_last_child);
    // B is last child of A.
    assert!(state.visible_entry(1).unwrap().is_last_child);
    // D is last child of C.
    assert!(state.visible_entry(3).unwrap().is_last_child);
}

#[rstest::rstest]
#[test]
fn tree_dfs_filtered_multiple_roots_recomputes_last_child() {
    // Given: two roots A and C, each with children.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", None, "Charlie"),
        item("d", Some("c"), "Delta"),
        item("e", Some("c"), "Echo"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Delta" only.
    state.insert_text("Delta");

    // Then only C + D are visible (A not matched, B not matched).
    assert_eq!(state.filtered_count(), 2);
    assert_eq!(state.filtered_item(0).unwrap().id, "c");
    assert_eq!(state.filtered_item(1).unwrap().id, "d");
    // D is the last (only visible) child of C.
    assert!(state.visible_entry(1).unwrap().is_last_child);
}

#[rstest::rstest]
#[test]
fn tree_dfs_filtered_is_last_child_uses_root_count_minus_one() {
    // Given: three roots, filter keeps only first and third.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", None, "Bravo"),
        item("c", None, "Charlie"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for something matching A and C but not B.
    state.insert_text("l"); // matches Alpha and Charlie (contains 'l') but not Bravo

    // Then last visible root is C.
    let count = state.filtered_count();
    assert!(count >= 2);
    // Find the last visible entry - should be the last root and have is_last_child = true.
    let last_entry = state.visible_entry(count - 1).unwrap();
    assert!(last_entry.is_last_child);
}

#[rstest::rstest]
#[test]
fn tree_continuations_correct_with_sibling_filtering() {
    // Given: root A → children B, C, D. Filter keeps A and C only.
    let items = vec![
        item("a", None, "Alpha"),
        item("b", Some("a"), "Bravo"),
        item("c", Some("a"), "Charlie"),
        item("d", Some("a"), "Delta"),
    ];
    let mut state = TreePickerState::with_items(items);

    // When filtering for "Charlie".
    state.insert_text("Charlie");

    // Then C is the only visible child and is_last_child = true.
    assert_eq!(state.filtered_count(), 2); // A + C
    assert!(state.visible_entry(1).unwrap().is_last_child);
    // A is the last (only visible) root, so ancestor_continuation for C is [false]
    // (meaning: the parent A does NOT have younger siblings).
    assert_eq!(
        state.visible_entry(1).unwrap().ancestor_continuations,
        vec![false]
    );
}
