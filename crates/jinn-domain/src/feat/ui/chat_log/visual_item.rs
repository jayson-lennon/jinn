//! Visual item model - represents collapsed and expanded entries in the chat log.
//!
//! The chat log renders a [`Vec<VisualItem>`] computed from the flat history at
//! each render pass. Ignored entries outside the proximity zone are collapsed
//! into a single [`VisualItem::CollapsedIgnoredBlock`] summary line unless the
//! user has explicitly expanded that block.
//!
//! The [`VisualItem`] type itself lives in `jinn-slices` (it is part of the
//! chat-log view state vocabulary, which persists per-session in the
//! chat-log-view slice's cell); the computation and resolution helpers stay
//! kernel-resident. This module re-exports the type under the kernel path.

use std::collections::HashSet;

use crate::protocol::{ChatEntry, ChatEntryId};

// Re-export shim: `VisualItem` moved to `jinn-slices` (chat-log view
// vocabulary); the kernel path stays stable for consumers.
pub use jinn_slices::VisualItem;

/// Number of entries from the end that are never hidden, regardless of `ignored`.
pub(crate) const PROXIMITY_COUNT: usize = 3;

/// Default minimum contiguous excluded entries required to collapse.
/// Blocks with fewer entries are displayed individually.
pub(crate) const DEFAULT_MIN_COLLAPSE_COUNT: usize = 3;

/// Build the list of visual items from flat history.
///
/// Walks the history once, identifying contiguous runs of `ignored` entries.
/// Each run that is not proximity-protected and not explicitly shown is
/// collapsed into a single [`VisualItem::CollapsedIgnoredBlock`].
///
/// # Rules
///
/// - Entries with `ignored == false` are always shown as `Entry`.
/// - Pinned entries (`pin_position.is_some()`) are always shown, even if `ignored`.
/// - Entries within [`PROXIMITY_COUNT`] of the end are always shown.
/// - Contiguous ignored runs whose first entry's ID is in `shown_ignored_blocks`
///   are shown as individual `Entry` items.
/// - All other contiguous ignored runs become a single `CollapsedIgnoredBlock`.
#[expect(clippy::expect_used, reason = "infallible")]
pub fn build_visual_items(
    history: &[ChatEntry],
    shown_ignored_blocks: &HashSet<ChatEntryId>,
    proximity_count: usize,
    min_collapse_count: usize,
) -> Vec<VisualItem> {
    let len = history.len();
    if len == 0 {
        return Vec::new();
    }

    let protected_start = len.saturating_sub(proximity_count);
    let mut items = Vec::with_capacity(len);

    let mut i = 0;
    while i < len {
        let Some(entry) = history.get(i) else { break };

        // Is this entry eligible for collapsing?
        if !entry.is_in_context() && entry.pin_position.is_none() && i < protected_start {
            // Start of a potential collapsed block. Accumulate contiguous
            // ignored entries that are also eligible.
            let block_start = i;
            let block_count = find_ignored_block_end(history, i, protected_start);
            i += block_count;

            // Check if the user has expanded this block, or the block is too small to collapse.
            let block_representative_id = &history.get(block_start).expect("block_start < len").id;
            if shown_ignored_blocks.contains(block_representative_id)
                || block_count < min_collapse_count
            {
                push_entry_indices(&mut items, block_start, block_count);
            } else {
                items.push(VisualItem::CollapsedIgnoredBlock {
                    start: block_start,
                    count: block_count,
                });
            }
        } else {
            items.push(VisualItem::Entry(i));
            i += 1;
        }
    }

    items
}

/// Push `count` `VisualItem::Entry`s starting at `start`.
fn push_entry_indices(items: &mut Vec<VisualItem>, start: usize, count: usize) {
    items.extend((start..start + count).map(VisualItem::Entry));
}

/// Count contiguous ignored entries starting at `start`, stopping at the first
/// protected or in-context entry.
fn find_ignored_block_end(history: &[ChatEntry], start: usize, protected_start: usize) -> usize {
    history.get(start..).map_or(0, |slice| {
        slice
            .iter()
            .take_while(|e| !e.is_in_context() && e.pin_position.is_none())
            .enumerate()
            .take_while(|(i, _)| start + i < protected_start)
            .count()
    })
}

/// Find the visual-item index for a given entry ID.
///
/// Scans the visual items list for an entry with the given ID.
/// If the entry is inside a [`CollapsedIgnoredBlock`], returns that block's
/// visual-item index (so the cursor lands on the collapsed block).
///
/// Returns `None` if the entry ID is not found in any visual item.
pub fn resolve_entry_id_to_vi_index(
    entry_id: &ChatEntryId,
    items: &[VisualItem],
    history: &[ChatEntry],
) -> Option<usize> {
    for (vi_idx, item) in items.iter().enumerate() {
        match item {
            VisualItem::Entry(hist_idx) => {
                if history.get(*hist_idx).is_some_and(|e| &e.id == entry_id) {
                    return Some(vi_idx);
                }
            }
            VisualItem::CollapsedIgnoredBlock { start, count } => {
                for j in *start..*start + count {
                    if history.get(j).is_some_and(|e| &e.id == entry_id) {
                        return Some(vi_idx);
                    }
                }
            }
        }
    }
    None
}

/// Get the representative entry ID for a visual item.
///
/// For [`Entry`], returns that entry's ID.
/// For [`CollapsedIgnoredBlock`], returns the first entry's ID (the block
/// representative).
///
/// [`Entry`]: VisualItem::Entry
/// [`CollapsedIgnoredBlock`]: VisualItem::CollapsedIgnoredBlock
pub fn entry_id_from_visual_item(item: &VisualItem, history: &[ChatEntry]) -> Option<ChatEntryId> {
    match item {
        VisualItem::Entry(hist_idx) => history.get(*hist_idx).map(|e| e.id.clone()),
        VisualItem::CollapsedIgnoredBlock { start, .. } => {
            history.get(*start).map(|e| e.id.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::map_with_unused_argument_over_ranges,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        clippy::needless_range_loop,
        reason = "test code"
    )]

    use super::*;
    use crate::feat::session::tool_result_status::ToolResultStatus;
    use crate::protocol::{ChatEntry, PinPosition};

    fn make_entries(count: usize, ignored: bool) -> Vec<ChatEntry> {
        (0..count)
            .map(|_| ChatEntry::user("msg").with_ignored(ignored))
            .collect()
    }

    fn shown_set(ids: &[&ChatEntryId]) -> HashSet<ChatEntryId> {
        ids.iter().copied().cloned().collect()
    }

    #[rstest::rstest]
    fn empty_history_produces_empty_items() {
        // Given an empty history.
        let history = vec![];

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then the result is empty.
        assert!(items.is_empty());
    }

    #[rstest::rstest]
    fn all_non_ignored_produces_all_entries() {
        // Given 5 non-ignored entries.
        let history = make_entries(5, false);

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then all are Entry items.
        assert_eq!(items.len(), 5);
        for (i, item) in items.iter().enumerate() {
            assert_eq!(*item, VisualItem::Entry(i));
        }
    }

    #[rstest::rstest]
    fn ignored_entries_collapsed_into_block() {
        // Given 3 non-ignored, 10 ignored, 7 non-ignored (total 20).
        // PROXIMITY_COUNT = 3, so protected_start = 17.
        // Ignored entries at indices 3..12 are all below protected_start.
        // So all 10 collapse into one block, then 7 non-ignored Entry.
        let mut history = make_entries(3, false);
        history.extend(make_entries(10, true));
        history.extend(make_entries(7, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then: 3 Entry, 1 CollapsedIgnoredBlock(3, 10), 7 Entry(13..19).
        assert_eq!(items.len(), 3 + 1 + 7);
        for i in 0..3 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
        assert_eq!(
            items[3],
            VisualItem::CollapsedIgnoredBlock {
                start: 3,
                count: 10
            }
        );
        // Non-ignored entries 13..19.
        for i in 0..7 {
            assert_eq!(items[4 + i], VisualItem::Entry(13 + i));
        }
    }

    #[rstest::rstest]
    fn proximity_protects_last_entries() {
        // Given 20 ignored entries.
        let history = make_entries(20, true);

        // When building visual items with proximity_count=10.
        let items = build_visual_items(&history, &HashSet::new(), 10, DEFAULT_MIN_COLLAPSE_COUNT);

        // Then: 1 CollapsedIgnoredBlock(0, 10), 10 Entry(10..19).
        assert_eq!(items.len(), 1 + 10);
        assert_eq!(
            items[0],
            VisualItem::CollapsedIgnoredBlock {
                start: 0,
                count: 10
            }
        );
        for i in 0..10 {
            assert_eq!(items[1 + i], VisualItem::Entry(10 + i));
        }
    }

    #[rstest::rstest]
    fn proximity_with_exactly_10_no_collapse() {
        // Given 10 ignored entries.
        let history = make_entries(10, true);

        // When building visual items with proximity_count=10.
        let items = build_visual_items(&history, &HashSet::new(), 10, DEFAULT_MIN_COLLAPSE_COUNT);

        // Then all are Entry (protected_start = 0).
        assert_eq!(items.len(), 10);
        for (i, item) in items.iter().enumerate() {
            assert_eq!(*item, VisualItem::Entry(i));
        }
    }

    #[rstest::rstest]
    fn pinned_overrides_ignored() {
        // Given 1 non-ignored, 5 ignored+pinned, 14 non-ignored (total 20).
        let mut history = make_entries(1, false);
        history.extend((0..5).map(|_| {
            ChatEntry::user("msg")
                .with_ignored(true)
                .with_pin(PinPosition::Top)
        }));
        history.extend(make_entries(14, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then the pinned entries are shown individually.
        assert_eq!(items.len(), 1 + 5 + 14);
        for i in 0..1 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
        for i in 1..=5 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
    }

    #[rstest::rstest]
    fn multiple_blocks_produce_multiple_collapsed() {
        // Given: 2 non-ignored, 3 ignored, 2 non-ignored, 4 ignored, 14 non-ignored (total 25).
        let mut history = make_entries(2, false);
        history.extend(make_entries(3, true));
        history.extend(make_entries(2, false));
        history.extend(make_entries(4, true));
        history.extend(make_entries(14, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then: 2 Entry, 1 Collapsed(2, 3), 2 Entry, 1 Collapsed(7, 4), 14 Entry.
        assert_eq!(items.len(), 2 + 1 + 2 + 1 + 14);
        assert_eq!(items[0], VisualItem::Entry(0));
        assert_eq!(items[1], VisualItem::Entry(1));
        assert_eq!(
            items[2],
            VisualItem::CollapsedIgnoredBlock { start: 2, count: 3 }
        );
        assert_eq!(items[3], VisualItem::Entry(5));
        assert_eq!(items[4], VisualItem::Entry(6));
        assert_eq!(
            items[5],
            VisualItem::CollapsedIgnoredBlock { start: 7, count: 4 }
        );
        for i in 0..14 {
            assert_eq!(items[6 + i], VisualItem::Entry(11 + i));
        }
    }

    #[rstest::rstest]
    fn shown_block_emits_individual_entries() {
        // Given: 2 non-ignored, 5 ignored, 20 non-ignored (total 27).
        let mut history = make_entries(2, false);
        let ignored_block: Vec<ChatEntry> = make_entries(5, true);
        let block_id = ignored_block[0].id.clone();
        history.extend(ignored_block);
        history.extend(make_entries(20, false));

        // When building visual items with the block's first entry shown.
        let shown = shown_set(&[&block_id]);
        let items = build_visual_items(
            &history,
            &shown,
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then the ignored entries are shown individually.
        assert_eq!(items.len(), 2 + 5 + 20);
        for i in 0..2 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
        for i in 2..=6 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
    }

    #[rstest::rstest]
    fn mixed_shown_and_hidden_blocks() {
        // Given: 2 non-ignored, 3 ignored (hidden), 2 non-ignored, 4 ignored (shown), 20 non-ignored.
        let mut history = make_entries(2, false);
        let hidden_block: Vec<ChatEntry> = make_entries(3, true);
        history.extend(hidden_block);
        history.extend(make_entries(2, false));
        let shown_block: Vec<ChatEntry> = make_entries(4, true);
        let shown_block_id = shown_block[0].id.clone();
        history.extend(shown_block);
        history.extend(make_entries(20, false));

        // When building with one block shown and one not.
        let shown = shown_set(&[&shown_block_id]);
        let items = build_visual_items(
            &history,
            &shown,
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then: 2 Entry, 1 Collapsed(2, 3), 2 Entry, 4 Entry(7..10), 20 Entry.
        assert_eq!(items.len(), 2 + 1 + 2 + 4 + 20);
        assert_eq!(
            items[2],
            VisualItem::CollapsedIgnoredBlock { start: 2, count: 3 }
        );
        for i in 0..4 {
            assert_eq!(items[5 + i], VisualItem::Entry(7 + i));
        }
    }

    #[rstest::rstest]
    fn all_ignored_with_len_greater_than_proximity() {
        // Given 25 ignored entries.
        let history = make_entries(25, true);

        // When building visual items with proximity_count=10.
        let items = build_visual_items(&history, &HashSet::new(), 10, DEFAULT_MIN_COLLAPSE_COUNT);

        // Then: 1 Collapsed(0, 15), 10 Entry(15..24).
        assert_eq!(items.len(), 1 + 10);
        assert_eq!(
            items[0],
            VisualItem::CollapsedIgnoredBlock {
                start: 0,
                count: 15
            }
        );
        for i in 0..10 {
            assert_eq!(items[1 + i], VisualItem::Entry(15 + i));
        }
    }

    #[rstest::rstest]
    fn pinned_entry_splits_ignored_block() {
        // Given: 5 ignored, 1 ignored+pinned, 5 ignored, 14 non-ignored (total 25).
        let mut history = make_entries(5, true);
        history.push(
            ChatEntry::user("pinned")
                .with_ignored(true)
                .with_pin(PinPosition::Top),
        );
        history.extend(make_entries(5, true));
        history.extend(make_entries(14, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then the pinned entry splits the block:
        // Collapsed(0, 5), Entry(5), Collapsed(6, 5), 14 Entry.
        assert_eq!(items.len(), 1 + 1 + 1 + 14);
        assert_eq!(
            items[0],
            VisualItem::CollapsedIgnoredBlock { start: 0, count: 5 }
        );
        assert_eq!(items[1], VisualItem::Entry(5));
        assert_eq!(
            items[2],
            VisualItem::CollapsedIgnoredBlock { start: 6, count: 5 }
        );
        for i in 0..14 {
            assert_eq!(items[3 + i], VisualItem::Entry(11 + i));
        }
    }

    #[rstest::rstest]
    fn below_threshold_shows_individually() {
        // Given 2 non-ignored, 2 ignored, 20 non-ignored (total 24).
        let mut history = make_entries(2, false);
        history.extend(make_entries(2, true));
        history.extend(make_entries(20, false));

        // When building visual items with min_collapse_count=3.
        let items = build_visual_items(&history, &HashSet::new(), 10, 3);

        // Then the 2 excluded entries at indices 2,3 are shown as Entry.
        assert_eq!(items[2], VisualItem::Entry(2));
        assert_eq!(items[3], VisualItem::Entry(3));
    }

    #[rstest::rstest]
    fn at_threshold_collapses() {
        // Given 2 non-ignored, 3 ignored, 20 non-ignored (total 25).
        let mut history = make_entries(2, false);
        history.extend(make_entries(3, true));
        history.extend(make_entries(20, false));

        // When building visual items with min_collapse_count=3.
        let items = build_visual_items(&history, &HashSet::new(), 10, 3);

        // Then the 3 excluded entries collapse into one block.
        assert_eq!(
            items[2],
            VisualItem::CollapsedIgnoredBlock { start: 2, count: 3 }
        );
    }

    #[rstest::rstest]
    fn above_count_below_threshold_shows_individually() {
        // Given 2 non-ignored, 4 ignored, 20 non-ignored (total 26).
        let mut history = make_entries(2, false);
        history.extend(make_entries(4, true));
        history.extend(make_entries(20, false));

        // When building visual items with min_collapse_count=5.
        let items = build_visual_items(&history, &HashSet::new(), 10, 5);

        // Then all 4 excluded entries are shown as Entry (below threshold).
        for i in 2..6 {
            assert_eq!(items[i], VisualItem::Entry(i));
        }
    }

    #[rstest::rstest]
    fn multiple_sub_threshold_runs_stay_separate() {
        // Given: 2 non-ignored, 2 ignored, 1 non-ignored, 2 ignored, 20 non-ignored.
        let mut history = make_entries(2, false);
        history.extend(make_entries(2, true));
        history.extend(make_entries(1, false));
        history.extend(make_entries(2, true));
        history.extend(make_entries(20, false));

        // When building visual items with min_collapse_count=3.
        let items = build_visual_items(&history, &HashSet::new(), 10, 3);

        // Then no CollapsedIgnoredBlock exists - all sub-threshold.
        assert!(
            items
                .iter()
                .all(|item| matches!(item, VisualItem::Entry(_)))
        );
    }

    #[rstest::rstest]
    fn threshold_one_collapses_everything() {
        // Given 2 non-ignored, 5 ignored, 20 non-ignored (total 27).
        let mut history = make_entries(2, false);
        history.extend(make_entries(5, true));
        history.extend(make_entries(20, false));

        // When building visual items with min_collapse_count=1.
        let items = build_visual_items(&history, &HashSet::new(), 10, 1);

        // Then the 5 excluded entries collapse (threshold 1 restores old behavior).
        assert_eq!(
            items[2],
            VisualItem::CollapsedIgnoredBlock { start: 2, count: 5 }
        );
    }

    #[rstest::rstest]
    fn empty_assistant_does_not_split_excluded_block() {
        // Given: 1 in-context, 2 excluded, 1 empty assistant (default),
        // 2 excluded, 14 in-context (total 20).
        // The empty assistant is out-of-context (is_in_context returns false
        // for empty assistants with Default override).
        let mut history = make_entries(1, false);
        history.extend(make_entries(2, true));
        history.push(ChatEntry::assistant("")); // empty assistant
        history.extend(make_entries(2, true));
        history.extend(make_entries(14, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then: 1 Entry, 1 Collapsed(1, 5), 14 Entry.
        // The empty assistant is absorbed into the surrounding excluded block.
        assert_eq!(items.len(), 1 + 1 + 14);
        assert_eq!(items[0], VisualItem::Entry(0));
        assert_eq!(
            items[1],
            VisualItem::CollapsedIgnoredBlock { start: 1, count: 5 }
        );
        for i in 0..14 {
            assert_eq!(items[2 + i], VisualItem::Entry(6 + i));
        }
    }

    #[rstest::rstest]
    fn pending_tool_result_does_not_split_excluded_block() {
        // Given: 1 in-context, 2 excluded, 1 pending tool result (default),
        // 2 excluded, 14 in-context (total 20).
        // The pending tool result is out-of-context (is_in_context returns
        // false for pending results with Default override).
        let mut history = make_entries(1, false);
        history.extend(make_entries(2, true));
        history.push(ChatEntry::tool_result(
            "tc-1",
            "bash",
            "",
            ToolResultStatus::Pending,
        ));
        history.extend(make_entries(2, true));
        history.extend(make_entries(14, false));

        // When building visual items.
        let items = build_visual_items(
            &history,
            &HashSet::new(),
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );

        // Then: 1 Entry, 1 Collapsed(1, 5), 14 Entry.
        // The pending tool result is absorbed into the surrounding excluded block.
        assert_eq!(items.len(), 1 + 1 + 14);
        assert_eq!(items[0], VisualItem::Entry(0));
        assert_eq!(
            items[1],
            VisualItem::CollapsedIgnoredBlock { start: 1, count: 5 }
        );
        for i in 0..14 {
            assert_eq!(items[2 + i], VisualItem::Entry(6 + i));
        }
    }
}
