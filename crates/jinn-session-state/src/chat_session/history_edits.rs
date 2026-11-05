//! History inspection and editing: ignore/override marking, sweeps, raw editor hooks.

use super::*;

impl ChatSessionState {
    /// Raw tail push used by the history editor. Applies user-entry token
    /// expansion and cursor/scroll bookkeeping. Do not call directly.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn push_entry_raw(&mut self, entry: &mut ChatEntry) -> usize {
        self.core.identity.last_history_activity_at = Timestamp::now();
        let (cwd_view, home_view) = (
            self.core.lifecycle.cwd.as_marked_path(),
            self.core.lifecycle.home.as_marked_path(),
        );
        let ctx = PathResolveContext::new(&cwd_view, &home_view);
        expand_user_entry(
            entry,
            &self.core.ephemeral.discovered_prompt_templates,
            &ctx,
        );
        let cursor_at_last = self.with_view(
            |v| {
                v.selected_cursor_id.as_ref().is_none_or(|id| {
                    self.core
                        .history_work
                        .history
                        .last()
                        .is_some_and(|e| &e.id == id)
                })
            },
            || true,
        );
        let index = self.core.history_work.history.len();
        self.core.history_work.history.push(entry.clone());
        if cursor_at_last {
            self.scroll_to_bottom();
            if let Some(entry) = self.core.history_work.history.last() {
                let id = entry.id.clone();
                self.update_view(|v| v.selected_cursor_id = Some(id));
            }
        }
        index
    }

    /// Removes the history entry at `index`. Returns whether it existed.
    ///
    /// Editor-only. Callers must remove in descending index order.
    ///
    /// Every live streaming index above `index` is shifted down by one, because
    /// the removal moves those entries with it. Skipping this would silently
    /// aim the next argument delta at whatever entry slid into the vacated
    /// slot — the delta is accepted, so nothing reports the mistake.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn remove_history_entry_at(&mut self, index: usize) -> bool {
        if index < self.core.history_work.history.len() {
            self.core.history_work.history.remove(index);
            self.core
                .ephemeral
                .shift_streaming_indices_after_remove_at(index);
            true
        } else {
            false
        }
    }

    /// Mutable access to the history entry at `index` for the editor.
    ///
    /// In-place writes (streaming lifecycle) can never reorder entries or
    /// split a tool loop, so the editor exposes them without chunk logic.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub(crate) fn history_get_mut(&mut self, index: usize) -> Option<&mut ChatEntry> {
        self.core.history_work.history.get_mut(index)
    }

    /// Read-only access to the conversation history.
    #[expect(
        clippy::same_name_method,
        reason = "trait impl delegates to this inherent method; callers use both"
    )]
    pub fn history(&self) -> &[ChatEntry] {
        &self.core.history_work.history
    }

    /// Fills the persisted token count for entries that don't have one yet.
    ///
    /// A token count is a content-derived fact about the (immutable) entry
    /// text, so entries whose count is already `Some` are never recomputed.
    /// Entry ids not present in `counts` (or not in this history) are skipped.
    ///
    /// Returns the number of entries filled.
    pub fn fill_missing_token_counts(&mut self, counts: &HashMap<ChatEntryId, u32>) -> usize {
        let mut filled = 0;
        for entry in self.core.history_work.history.iter_mut() {
            if entry.token_count.is_some() {
                continue;
            }
            if let Some(count) = counts.get(&entry.id) {
                entry.token_count = Some(*count);
                filled += 1;
            }
        }
        filled
    }

    /// Mark entries at the given indices as ignored.
    ///
    /// Used by the compaction actor to mark entries that have been summarized.
    /// Ignores any indices that are out of bounds.
    pub fn mark_entries_ignored(&mut self, indices: &[usize]) {
        for &i in indices {
            if let Some(entry) = self.core.history_work.history.get_mut(i) {
                entry.apply_context_override(
                    ContextOverride::ForcedExclude,
                    ChangeSource::Internal {
                        label: "mark_entries_ignored".into(),
                    },
                );
            }
        }
    }

    /// Toggle the context override on the currently selected entry, always
    /// flipping the entry's *effective* in-context state.
    ///
    /// `Forced*` states flip between include and exclude. `Default` resolves to
    /// the opposite of the current effective state ([`is_in_context`]), so the
    /// toggle always lands on an explicit `Forced*` value — it never produces
    /// `Default`. Resetting to `Default` is the job of the `r` reset intent.
    ///
    /// [`is_in_context`]: ChatEntry::is_in_context
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already in the toggled state) or no entry is selected.
    pub fn toggle_entry_ignored(&mut self) -> Option<ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history_work.history.get(hist_idx)?;
        let pressed_id = entry.id.clone();
        let new_value = match entry.context_override() {
            ContextOverride::ForcedInclude => ContextOverride::ForcedExclude,
            ContextOverride::ForcedExclude => ContextOverride::ForcedInclude,
            ContextOverride::Default => {
                if entry.is_in_context() {
                    ContextOverride::ForcedExclude
                } else {
                    ContextOverride::ForcedInclude
                }
            }
        };
        // Chunk semantics: the toggle applies to the whole tool loop.
        let changed = self
            .edit_history()
            .set_context(&pressed_id, new_value, &ChangeSource::User);
        (!changed.is_empty()).then_some(pressed_id)
    }

    /// Set the context override on the currently selected entry to a specific
    /// value (not a toggle). Used by the x-sweep to apply a captured state.
    ///
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already at the target state) or no entry is selected.
    pub fn set_entry_context_override(
        &mut self,
        override_state: ContextOverride,
    ) -> Option<ChatEntryId> {
        let hist_idx = self.selected_history_index()?;
        let entry = self.core.history_work.history.get(hist_idx)?;
        let pressed_id = entry.id.clone();
        // Chunk semantics: the sweep applies to the whole tool loop.
        let changed =
            self.edit_history()
                .set_context(&pressed_id, override_state, &ChangeSource::User);
        (!changed.is_empty()).then_some(pressed_id)
    }

    /// Set the context override on the entry with the given id to a specific
    /// value. Unlike [`Self::set_entry_context_override`] this targets an
    /// entry by id rather than the cursor, so callers can write chunks
    /// without moving the selection.
    ///
    /// Returns `Some(entry_id)` if the override was changed, `None` if no-op
    /// (entry was already at the target state) or the id is unknown.
    pub fn set_entry_context_override_by_id(
        &mut self,
        id: &ChatEntryId,
        override_state: ContextOverride,
    ) -> Option<ChatEntryId> {
        // Chunk semantics: the write applies to the whole tool loop.
        let changed = self
            .edit_history()
            .set_context(id, override_state, &ChangeSource::User);
        (!changed.is_empty()).then(|| id.clone())
    }

    /// After a sweep changes an entry from excluded to in-context, propagate
    /// `shown_ignored_blocks` to any new sub-blocks created by the split.
    ///
    /// When an entry inside a shown (expanded) ignored block becomes in-context,
    /// it splits the block. If the original block was shown, the new forward
    /// sub-block should also be shown so entries remain visible.
    ///
    /// No-op if the entry was not inside a shown block.
    pub fn propagate_shown_on_unignore(&mut self, entry_id: &ChatEntryId) {
        let Some(idx) = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *entry_id)
        else {
            return;
        };

        // Scan backward to find the containing block's start.
        let mut block_start = idx;
        while block_start > 0 {
            let Some(prev) = self.core.history_work.history.get(block_start - 1) else {
                break;
            };
            if prev.is_in_context() || prev.pin_position.is_some() {
                break;
            }
            block_start -= 1;
        }

        let Some(block_entry) = self.core.history_work.history.get(block_start) else {
            return;
        };
        let block_representative = block_entry.id.clone();
        let was_shown = self.with_view(
            |v| v.shown_ignored_blocks.contains(&block_representative),
            || false,
        );
        if !was_shown {
            return; // Block was not shown — nothing to propagate.
        }

        let forward_start = idx + 1;
        // Scan forward from the changed entry to find a new forward sub-block.
        let Some(forward_entry) = self.core.history_work.history.get(forward_start) else {
            return;
        };
        if forward_entry.is_in_context() || forward_entry.pin_position.is_some() {
            return; // No excluded entries after — no sub-block to create.
        }

        // The forward sub-block's representative is its first entry.
        let forward_representative = forward_entry.id.clone();
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(forward_representative);
        });
    }

    /// Rebuild the visual items list from the current history and
    /// `shown_ignored_blocks`. Needed during sweep operations to keep the
    /// visual items consistent with mutated entry state between render passes.
    pub fn rebuild_visual_items(&self) {
        let shown = self.shown_ignored_blocks_snapshot();
        let items = build_visual_items(
            &self.core.history_work.history,
            &shown,
            PROXIMITY_COUNT,
            DEFAULT_MIN_COLLAPSE_COUNT,
        );
        self.set_visual_items(items);
    }

    /// Returns the sweep target state if an active sweep exists and has not
    /// expired (>100ms since last press). Consumes (clears) the sweep state
    /// regardless of expiry - the caller must re-store it if continuing.
    pub fn take_ignore_sweep(&mut self) -> Option<ContextOverride> {
        let sweep = self.update_view_taking(|v| v.ignore_sweep.take());
        let (instant, override_state) = sweep?;
        (instant.elapsed() < std::time::Duration::from_millis(100)).then_some(override_state)
    }

    /// Starts or continues a sweep by storing the target state and current time.
    pub fn set_ignore_sweep(&mut self, target: ContextOverride) {
        self.update_view(|v| v.ignore_sweep = Some((std::time::Instant::now(), target)));
    }

    /// Clears the sweep state, resetting to normal toggle behavior.
    pub fn clear_ignore_sweep(&mut self) {
        self.update_view(|v| v.ignore_sweep = None);
    }

    /// Removes and returns the raw sweep state (timestamp + target),
    /// bypassing the expiry check.
    ///
    /// Test seam for sweep expiry: [`Self::take_ignore_sweep`] discards
    /// sweeps older than 100ms, so expiry cannot be observed without a way
    /// to plant and retrieve a stale timestamp.
    #[doc(hidden)]
    pub fn take_ignore_sweep_raw(&mut self) -> Option<(std::time::Instant, ContextOverride)> {
        self.update_view_taking(|v| v.ignore_sweep.take())
    }

    /// Plants the sweep state with an explicit timestamp.
    ///
    /// Test seam companion to [`Self::take_ignore_sweep_raw`]; production
    /// sweeps always stamp `Instant::now` (see [`Self::set_ignore_sweep`]).
    #[doc(hidden)]
    pub fn set_ignore_sweep_at(&mut self, instant: std::time::Instant, target: ContextOverride) {
        self.update_view(|v| v.ignore_sweep = Some((instant, target)));
    }

    /// Restore conversation history from a persisted snapshot.
    ///
    /// Replaces the current history with the given entries. Used by session
    /// persistence to rehydrate a session from disk.
    pub fn restore_history(&mut self, entries: Vec<ChatEntry>) {
        self.core.history_work.history.replace_all(entries);
        let new_cursor = self.core.history_work.history.last().map(|e| e.id.clone());
        self.update_view(|v| v.selected_cursor_id = new_cursor);
        self.scroll_to_bottom();
    }
}
