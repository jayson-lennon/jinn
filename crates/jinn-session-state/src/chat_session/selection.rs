//! Cursor and selection state: movement, saved positions, expansion, ignored-block visibility.

use super::*;

impl ChatSessionState {
    /// A snapshot copy of the expanded entry ids.
    ///
    /// The off-thread chat log layout reads the set once per job rather than
    /// testing membership per entry, matching
    /// [`Self::streaming_tool_call_ids`]'s single-snapshot approach.
    #[must_use]
    pub fn expanded_entry_ids(&self) -> std::collections::HashSet<ChatEntryId> {
        self.with_view(|v| v.expanded_entries.clone(), Default::default)
    }

    /// A snapshot copy of the shown-ignored-blocks set. Builders that read
    /// the set alongside history (`build_visual_items`, sweep propagation)
    /// work on the copy so the view lock is never held across computation.
    #[must_use]
    pub fn shown_ignored_blocks_snapshot(&self) -> std::collections::HashSet<ChatEntryId> {
        self.with_view(|v| v.shown_ignored_blocks.clone(), Default::default)
    }

    /// Move the cursor to the first entry visible in the viewport.
    ///
    /// Resolves through visual items. Collapsed blocks are selectable.
    /// No-op if no entries are visible.
    pub fn move_cursor_to_first_visible(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Fallback: use raw history index when visual items not yet computed.
            self.set_selected_entry_index(range.start);
        } else {
            let mut idx = range.start;
            while idx < range.end {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history_work
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    break;
                }
                idx += 1;
            }
            if idx < items.len() {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history_work
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    self.set_selected_entry_index(idx);
                }
            }
        }
    }

    /// Move the cursor to the last entry visible in the viewport.
    ///
    /// Resolves through visual items. Collapsed blocks are selectable.
    /// No-op if no entries are visible.
    pub fn move_cursor_to_last_visible(&mut self) {
        let range = self.visible_entry_range();
        if range.is_empty() {
            return;
        }
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Fallback: use raw history index when visual items not yet computed.
            self.set_selected_entry_index(range.end.saturating_sub(1));
        } else {
            let mut idx = range.end.saturating_sub(1);
            while idx > range.start {
                let selectable = match items.get(idx) {
                    Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                    Some(VisualItem::Entry(hist_idx)) => self
                        .core
                        .history_work
                        .history
                        .get(*hist_idx)
                        .is_some_and(|e| !e.is_empty_assistant()),
                    None => false,
                };
                if selectable {
                    break;
                }
                idx = idx.saturating_sub(1);
            }
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history_work
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                self.set_selected_entry_index(idx);
            }
        }
    }

    /// Select the next entry (moving toward newer messages).
    ///
    /// If nothing is selected, selects the first visual item.
    /// Walks the visual items list, not the raw history.
    /// Collapsed blocks are selectable (so user can press `h` to expand).
    /// Skips empty assistant entries.
    /// Clamps to the last visual-item index.
    /// No-op if visual items list is empty.
    pub fn select_next_entry(&mut self) {
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, fall back to direct history walking.
            self.select_next_entry_fallback();
            return;
        }

        let max = items.len() - 1;
        let start = self
            .selected_entry_index()
            .map_or(0, |i| i.saturating_add(1).min(max));
        let mut idx = start;
        while idx < max {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history_work
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx = idx.saturating_add(1);
        }
        let selectable = match items.get(idx) {
            Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
            Some(VisualItem::Entry(hist_idx)) => self
                .core
                .history_work
                .history
                .get(*hist_idx)
                .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
            None => false,
        };
        if selectable {
            self.set_selected_entry_index(idx);
        }
    }

    /// Select the previous entry (moving toward older messages).
    ///
    /// If nothing is selected, selects the last visual item.
    /// Walks the visual items list, not the raw history.
    /// Collapsed blocks are selectable (so user can press `h` to expand).
    /// Skips empty assistant entries.
    /// Clamps to 0.
    /// No-op if visual items list is empty.
    pub fn select_prev_entry(&mut self) {
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, fall back to direct history walking.
            self.select_prev_entry_fallback();
            return;
        }

        let start = self
            .selected_entry_index()
            .map_or(items.len().saturating_sub(1), |i| i.saturating_sub(1));
        let mut idx = start;
        while idx > 0 {
            let selectable = match items.get(idx) {
                Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
                Some(VisualItem::Entry(hist_idx)) => self
                    .core
                    .history_work
                    .history
                    .get(*hist_idx)
                    .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
                None => false,
            };
            if selectable {
                break;
            }
            idx = idx.saturating_sub(1);
        }
        let selectable = match items.get(idx) {
            Some(VisualItem::CollapsedIgnoredBlock { .. }) => true,
            Some(VisualItem::Entry(hist_idx)) => self
                .core
                .history_work
                .history
                .get(*hist_idx)
                .is_some_and(|e: &ChatEntry| !e.is_empty_assistant()),
            None => false,
        };
        if selectable {
            self.set_selected_entry_index(idx);
        }
    }

    /// Clear the entry selection.
    pub fn clear_selection(&mut self) {
        self.update_view(|v| v.selected_cursor_id = None);
    }

    /// Set the selected entry index directly.
    ///
    /// Use for programmatic selection (e.g., sidebar pin sync).
    /// Does not validate bounds - caller must ensure index is valid.
    pub fn set_selected_entry_index(&mut self, index: usize) {
        let id = {
            let items = self.visual_items_snapshot();
            if items.is_empty() {
                self.core
                    .history_work
                    .history
                    .get(index)
                    .map(|e| e.id.clone())
            } else {
                items.get(index).and_then(|item| {
                    entry_id_from_visual_item(item, &self.core.history_work.history)
                })
            }
        };
        if let Some(id) = id {
            self.update_view(|v| v.selected_cursor_id = Some(id));
        }
    }

    /// Fallback: select next entry by walking raw history.
    /// Used when visual items haven't been computed yet (before first render).
    fn select_next_entry_fallback(&mut self) {
        if self.core.history_work.history.is_empty() {
            return;
        }
        let max = self.core.history_work.history.len() - 1;
        let start = self
            .selected_entry_index()
            .map_or(0, |i| i.saturating_add(1).min(max));
        let mut idx = start;
        while idx < max
            && self
                .core
                .history_work
                .history
                .get(idx)
                .is_none_or(ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_add(1);
        }
        if self
            .core
            .history_work
            .history
            .get(idx)
            .is_some_and(|e| !e.is_empty_assistant())
        {
            self.set_selected_entry_index(idx);
        }
    }

    /// Fallback: select prev entry by walking raw history.
    /// Used when visual items haven't been computed yet (before first render).
    fn select_prev_entry_fallback(&mut self) {
        if self.core.history_work.history.is_empty() {
            return;
        }
        let start = self.selected_entry_index().map_or(
            self.core.history_work.history.len().saturating_sub(1),
            |i| i.saturating_sub(1),
        );
        let mut idx = start;
        while idx > 0
            && self
                .core
                .history_work
                .history
                .get(idx)
                .is_none_or(ChatEntry::is_empty_assistant)
        {
            idx = idx.saturating_sub(1);
        }
        if self
            .core
            .history_work
            .history
            .get(idx)
            .is_some_and(|e| !e.is_empty_assistant())
        {
            self.set_selected_entry_index(idx);
        }
    }

    /// Saves the current chat log scroll position as the "pre-pin" snapshot.
    ///
    /// Call this before `sync_chat_log_cursor` changes the viewport.
    /// No-op if a position is already saved (prevents overwriting during
    /// a single Pins visit).
    pub fn save_history_position(&mut self) {
        self.update_view(|v| {
            if v.saved_history_position.is_some() {
                return;
            }
            v.saved_history_position = Some(SavedHistoryPosition {
                scroll_offset: v.scroll_offset,
                selected_cursor_id: v.selected_cursor_id.clone(),
            });
        });
    }

    /// Restores the chat log to the saved "pre-pin" position, if one exists.
    ///
    /// Consumes the saved position (take semantics).
    pub fn restore_history_position(&mut self) {
        self.update_view(|v| {
            if let Some(saved) = v.saved_history_position.take() {
                v.scroll_offset = saved.scroll_offset;
                v.selected_cursor_id = saved.selected_cursor_id;
            }
        });
    }

    /// Discards the saved position without restoring.
    ///
    /// Used when leaving the sidebar to Normal scope - the pin's position
    /// should persist in the chat log.
    pub fn discard_saved_history_position(&mut self) {
        self.update_view(|v| v.saved_history_position = None);
    }

    /// Returns whether there is a saved history position.
    pub fn has_saved_history_position(&self) -> bool {
        self.with_view(|v| v.saved_history_position.is_some(), || false)
    }

    /// The saved pre-pin position, if any (test seam for restore
    /// assertions; production observes it through
    /// [`Self::restore_history_position`]).
    #[doc(hidden)]
    pub fn saved_history_position(&self) -> Option<SavedHistoryPosition> {
        self.with_view(|v| v.saved_history_position.clone(), || None)
    }

    /// The index of the currently selected entry, if any.
    pub fn selected_entry_index(&self) -> Option<usize> {
        let cursor_id = self.selected_cursor_id_owned()?;
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            return self
                .core
                .history_work
                .history
                .iter()
                .position(|e| e.id == cursor_id);
        }
        resolve_entry_id_to_vi_index(&cursor_id, &items, &self.core.history_work.history)
    }

    /// The stored cursor ID (source of truth for selection).
    ///
    /// Unlike `selected_entry_id()` which returns `None` for collapsed blocks,
    /// this always returns the stored ID even when a collapsed block is selected.
    pub fn selected_cursor_id(&self) -> Option<ChatEntryId> {
        self.selected_cursor_id_owned()
    }

    /// The stored cursor ID, cloned. Internal spelling of
    /// [`Self::selected_cursor_id`]; keeps call sites allocation-free when
    /// the copy can be avoided.
    fn selected_cursor_id_owned(&self) -> Option<ChatEntryId> {
        self.with_view(|v| v.selected_cursor_id.clone(), || None)
    }

    /// Set the selected cursor to a specific entry by ID.
    ///
    /// Sets [`SessionUi::selected_cursor_id`] directly, bypassing
    /// visual-item index resolution. Use when the entry ID is already
    /// known (e.g., sidebar pin sync).
    pub fn set_selected_cursor_id(&mut self, id: ChatEntryId) {
        self.update_view(|v| v.selected_cursor_id = Some(id));
    }

    /// The currently selected entry, if any.
    ///
    /// Resolves through the visual items list: returns `None` if the
    /// selected item is a collapsed block rather than a real entry.
    /// Falls back to direct history indexing when visual items are empty
    /// (before the first render).
    pub fn selected_entry(&self) -> Option<&ChatEntry> {
        let vi_idx = self.selected_entry_index()?;
        let items = self.visual_items_snapshot();
        if items.is_empty() {
            // Before first render, visual items haven't been computed yet.
            // Fall back to direct history indexing.
            return self.core.history_work.history.get(vi_idx);
        }
        match items.get(vi_idx)? {
            VisualItem::Entry(hist_idx) => self.core.history_work.history.get(*hist_idx),
            VisualItem::CollapsedIgnoredBlock { .. } => None,
        }
    }

    /// The ID of the currently selected entry, if any.
    pub fn selected_entry_id(&self) -> Option<&ChatEntryId> {
        self.selected_entry().map(|e| &e.id)
    }

    /// Toggles the expanded state of a tool result entry.
    ///
    /// If the entry is currently expanded, it collapses. Otherwise, it expands.
    pub fn toggle_expand_entry(&mut self, id: ChatEntryId) {
        self.update_view(|v| {
            if v.expanded_entries.contains(&id) {
                v.expanded_entries.remove(&id);
            } else {
                v.expanded_entries.insert(id);
            }
        });
    }

    /// Whether a tool result entry is currently expanded to show full content.
    pub fn is_entry_expanded(&self, id: &ChatEntryId) -> bool {
        self.with_view(|v| v.expanded_entries.contains(id), || false)
    }

    /// Shows the ignored block whose representative is `block_representative`
    /// (test seam: bypasses block-boundary resolution, which production
    /// always goes through `toggle_ignored_block_visibility` for).
    #[doc(hidden)]
    pub fn show_ignored_block(&mut self, block_representative: ChatEntryId) {
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(block_representative);
        });
    }

    /// Toggle visibility of the ignored block containing the given entry.
    ///
    /// Finds the contiguous run of ignored entries containing the entry
    /// identified by `entry_id`, takes the first entry's ID as the block
    /// representative, and toggles it in `shown_ignored_blocks`.
    ///
    /// No-op if the entry is not found or is not ignored.
    pub fn toggle_ignored_block_visibility(&mut self, entry_id: &ChatEntryId) {
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return;
        };
        let Some(entry) = self.core.history_work.history.get(idx) else {
            return;
        };
        if entry.is_in_context() {
            return;
        }
        // Scan backward to find the start of the contiguous ignored block.
        // Must match `build_visual_items` block definition: pinned entries
        // act as block splitters even when ignored.
        let mut block_start = idx;
        while block_start > 0
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| !e.is_in_context())
            && self
                .core
                .history_work
                .history
                .get(block_start - 1)
                .is_some_and(|e| e.pin_position.is_none())
        {
            block_start -= 1;
        }
        let Some(block_rep) = self.core.history_work.history.get(block_start) else {
            return;
        };
        let block_representative = block_rep.id.clone();
        self.update_view(|v| {
            if v.shown_ignored_blocks.contains(&block_representative) {
                v.shown_ignored_blocks.remove(&block_representative);
            } else {
                v.shown_ignored_blocks.insert(block_representative);
            }
        });
    }

    /// Returns `true` if the given entry is a `ToolCall` that is still
    /// actively streaming arguments from the LLM.
    pub fn is_tool_call_streaming(&self, entry_id: &ChatEntryId) -> bool {
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return false;
        };
        self.core.ephemeral.is_tool_call_at_history_index(idx)
    }

    /// The ids of every `ToolCall` entry currently streaming arguments from the LLM.
    ///
    /// A single snapshot of the streaming state, so callers that need to test many
    /// entries (such as the chat log's per-frame layout pass) pay one map walk instead
    /// of a history scan per entry. Indices with no corresponding history entry are
    /// skipped, matching [`Self::is_tool_call_streaming`]'s treatment of unknown ids.
    pub fn streaming_tool_call_ids(&self) -> HashSet<ChatEntryId> {
        let history: &[ChatEntry] = &self.core.history_work.history;
        self.core
            .ephemeral
            .streaming_tool_call_indices
            .values()
            .filter_map(|&history_index| history.get(history_index))
            .map(|entry| entry.id.clone())
            .collect()
    }
}
