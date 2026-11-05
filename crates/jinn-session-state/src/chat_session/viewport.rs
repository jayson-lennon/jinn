//! Viewport bookkeeping: scroll offsets, line ranges, visual-item publication, dimensions.

use super::*;

impl ChatSessionState {
    /// Transition to Working phase (a background operation started).
    ///
    /// Increment the busy counter. Called when a background operation starts.
    /// The count is ephemeral (not persisted).
    /// The current scroll offset (lines to skip from top).
    ///
    /// Returns `None` when auto-scrolled to the bottom, or `Some(n)` when
    /// the user has manually scrolled to a specific offset.
    pub fn scroll_offset(&self) -> Option<u32> {
        self.with_view(|v| v.scroll_offset, || None)
    }

    /// Whether the conversation is scrolled to the bottom (auto-scroll position).
    pub fn is_at_bottom(&self) -> bool {
        self.with_view(|v| v.scroll_offset.is_none(), || true)
    }

    /// Scroll up (toward older messages) by the given number of lines.
    ///
    /// If currently at the bottom (auto-scroll), resolves to `last_max_offset` first
    /// so the scroll is relative to the actual bottom position.
    /// Seeds an explicit scroll offset (test seam for "already scrolled"
    /// arrangements; production scrolls always move relative to the
    /// current offset).
    #[doc(hidden)]
    pub fn set_scroll_offset(&mut self, offset: Option<u32>) {
        self.update_view(|v| v.scroll_offset = offset);
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.update_view(|v| {
            let current = v
                .scroll_offset
                .unwrap_or(v.last_max_offset.load(Ordering::Relaxed));
            v.scroll_offset = Some(current.saturating_sub(u32::from(amount)));
        });
    }

    /// Scroll down (toward newer messages) by the given number of lines.
    ///
    /// If the resulting offset reaches or exceeds `last_max_offset`, resets to
    /// auto-scroll (bottom).
    pub fn scroll_down(&mut self, amount: u16) {
        self.update_view(|v| {
            let current = v
                .scroll_offset
                .unwrap_or(v.last_max_offset.load(Ordering::Relaxed));
            let next = current.saturating_add(u32::from(amount));
            if next >= v.last_max_offset.load(Ordering::Relaxed) {
                v.scroll_offset = None;
            } else {
                v.scroll_offset = Some(next);
            }
        });
    }

    /// Scroll to the very top of the conversation.
    pub fn scroll_to_top(&mut self) {
        self.update_view(|v| v.scroll_offset = Some(0));
    }

    /// Scroll to the very bottom of the conversation (auto-scroll).
    ///
    /// `None` is the bottom sentinel, not an unset scroll: a session that has
    /// never been scrolled and one scrolled to its last line render the same,
    /// and both follow the streaming entry. This absorbed a second method that
    /// did exactly this and was called the same thing.
    pub fn scroll_to_bottom(&mut self) {
        self.update_view(|v| v.scroll_offset = None);
    }

    /// Scroll the chat log so that the currently selected entry is visible.
    ///
    /// Uses `entry_line_ranges` and `viewport_height` (set by the renderer
    /// each frame) to compute the scroll offset that brings the selected
    /// entry into view. This is essentially the same logic as the renderer's
    /// scroll-to-selected adjustment, but applied as a state mutation for
    /// intent handlers.
    ///
    /// No-op if no entry is selected or if line range data is unavailable.
    pub fn scroll_to_selected(&mut self) {
        let Some(selected_idx) = self.selected_entry_index() else {
            return;
        };

        // Read phase: gather the render caches, then decide.
        let decision = self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read();
                let &(start, end) = ranges.get(selected_idx)?;
                let viewport_height = v.viewport_height.load(Ordering::Relaxed);
                if viewport_height == 0 {
                    return None;
                }
                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let current_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                let abs_start = start.saturating_add(blank_count);
                let abs_end = end.saturating_add(blank_count);
                let entry_height = abs_end.saturating_sub(abs_start);

                if entry_height <= viewport_height {
                    // Entry fits in viewport - adjust only if it's outside.
                    if abs_start < current_offset {
                        Some(abs_start)
                    } else if abs_end > current_offset.saturating_add(viewport_height) {
                        Some(abs_end.saturating_sub(viewport_height))
                    } else {
                        // Already visible - no change needed.
                        None
                    }
                } else {
                    // Entry is taller than viewport - align top.
                    if abs_start >= current_offset.saturating_add(viewport_height) {
                        Some(abs_start)
                    } else if abs_end <= current_offset {
                        Some(abs_end.saturating_sub(viewport_height))
                    } else {
                        // Already overlapping - no change needed.
                        None
                    }
                }
            },
            || None,
        );

        let Some(new_offset) = decision else {
            return;
        };

        // Write phase: clamp and apply as the new scroll intent.
        self.update_view(|v| {
            let clamped = new_offset.min(v.last_max_offset.load(Ordering::Relaxed));
            if clamped >= v.last_max_offset.load(Ordering::Relaxed) {
                v.scroll_offset = None;
            } else {
                v.scroll_offset = Some(clamped);
            }
        });
    }

    /// Update the cached maximum scroll offset from the renderer.
    ///
    /// Called by the chat log element during each render so that
    /// scroll handlers can resolve the "at bottom" state into a concrete offset.
    pub fn set_last_max_offset(&self, max_offset: u32) {
        self.update_view(|v| v.last_max_offset.store(max_offset, Ordering::Relaxed));
    }

    /// The maximum scroll offset recorded by the last render.
    ///
    /// Meaningful only after the chat-log render pipeline has run for the
    /// current frame; it is the bottom of the document in wrapped lines.
    #[must_use]
    pub fn rendered_max_offset(&self) -> u32 {
        self.with_view(|v| v.last_max_offset.load(Ordering::Relaxed), || 0)
    }

    /// Returns the screen-space Y coordinate of the top of the currently-selected
    /// chat entry within the chat-log area, or `None` if no entry is selected or
    /// the render-pipeline cache is empty.
    ///
    /// The returned Y is in terminal (absolute) coordinates: it already incorporates
    /// `chat_log_area_y` and `blank_count`. Callers can pass it directly as the
    /// `entry_top_y` argument to
    /// the chat-log audit popup geometry helper.
    ///
    /// If the selected entry's top is scrolled above the viewport, returns
    /// `chat_log_area_y` (clamped to the top of the chat-log area).
    ///
    /// Returns a meaningful value only after the chat-log render pipeline has
    /// populated the cached fields for the current frame.
    pub fn selected_entry_screen_y(&self, chat_log_area_y: u16) -> Option<u16> {
        let vi_idx = self.selected_entry_index()?;
        self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read();
                let &(start, _end) = ranges.get(vi_idx)?;

                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let scroll_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                // wrapped-line coord of entry top, with bottom-alignment blank padding
                let abs_start = start.saturating_add(blank_count);

                // viewport top in the same coord space
                let viewport_top = scroll_offset;

                // visible-Y offset within viewport (0 = top of chat-log area)
                let viewport_offset = abs_start.saturating_sub(viewport_top);

                // absolute screen Y; clamped to chat-log area top. A screen row
                // is u16, so narrow from the u32 line math.
                Some(
                    chat_log_area_y
                        .saturating_add(u16::try_from(viewport_offset).unwrap_or(u16::MAX)),
                )
            },
            || None,
        )
    }

    /// Store the rendered scroll offset (actual viewport position after clamping
    /// and scroll-to-selected adjustment). Called by the render pipeline each frame.
    pub fn set_rendered_scroll_offset(&self, offset: u32) {
        self.update_view(|v| v.rendered_scroll_offset.store(offset, Ordering::Relaxed));
    }

    /// Store per-entry wrapped line ranges computed by the renderer.
    ///
    /// `entry_line_ranges[i] = (start_wrapped_line, end_wrapped_line)` in the
    /// wrapped coordinate space. Called each frame by the chat log renderer.
    pub fn set_entry_line_ranges(&self, ranges: Vec<(u32, u32)>) {
        self.update_view(|v| *v.entry_line_ranges.write() = ranges);
    }

    /// Store the viewport height (render area height) from the renderer.
    pub fn set_viewport_height(&self, height: u16) {
        self.update_view(|v| {
            v.viewport_height
                .store(u32::from(height), Ordering::Relaxed);
        });
    }

    /// Read the cached viewport height.
    pub fn viewport_height_value(&self) -> u16 {
        self.with_view(
            |v| u16::try_from(v.viewport_height.load(Ordering::Relaxed)).unwrap_or(u16::MAX),
            || 0,
        )
    }

    /// Store the blank line count prepended for bottom-alignment.
    pub fn set_blank_count(&self, count: u32) {
        self.update_view(|v| v.blank_count.store(count, Ordering::Relaxed));
    }

    /// Returns the range of entry indices visible in the current viewport.
    ///
    /// Uses `entry_line_ranges`, `blank_count`, `scroll_offset`, and
    /// `viewport_height` to determine which entries have at least one line
    /// visible. Returns an empty range if no entries are visible or viewport
    /// data is unavailable.
    pub fn visible_entry_range(&self) -> Range<usize> {
        self.with_view(
            |v| {
                let ranges = v.entry_line_ranges.read().clone();
                if ranges.is_empty() {
                    return 0..0;
                }

                let viewport_height = v.viewport_height.load(Ordering::Relaxed);
                let blank_count = v.blank_count.load(Ordering::Relaxed);
                let scroll_offset = v.rendered_scroll_offset.load(Ordering::Relaxed);

                let viewport_top = scroll_offset;
                let viewport_bottom = scroll_offset.saturating_add(viewport_height);

                let mut first_visible = None;
                let mut last_visible = None;

                for (i, &(start, end)) in ranges.iter().enumerate() {
                    let abs_start = start.saturating_add(blank_count);
                    let abs_end = end.saturating_add(blank_count);
                    if abs_end > viewport_top && abs_start < viewport_bottom {
                        if first_visible.is_none() {
                            first_visible = Some(i);
                        }
                        last_visible = Some(i);
                    }
                }

                match (first_visible, last_visible) {
                    (Some(first), Some(last)) => first..last + 1,
                    _ => 0..0,
                }
            },
            || 0..0,
        )
    }

    /// Store the visual items list computed during render.
    pub fn set_visual_items(&self, items: Vec<VisualItem>) {
        self.update_view(|v| *v.visual_items.write() = items);
    }

    /// Publish the visual items list computed during render, replacing the
    /// stored list only when it differs.
    ///
    /// The renderer recomputes the list every frame, but it changes only when
    /// the history, the ignore sets, or the collapse threshold change. An
    /// unconditional write would allocate and copy a list that is one entry
    /// per history entry on every frame. Returns `true` when the stored list
    /// was replaced.
    pub fn set_visual_items_if_changed(&self, items: &[VisualItem]) -> bool {
        self.update_view_taking(|v| Some(v.set_visual_items_if_changed(items)))
            .unwrap_or(false)
    }

    /// Publish the per-entry wrapped line ranges computed during render,
    /// replacing the stored ranges only when they differ.
    ///
    /// The ranges are one pair per visual item and are recomputed every
    /// frame, but the values only change when the wrapped layout does.
    /// Returns `true` when the stored ranges were replaced.
    pub fn set_entry_line_ranges_if_changed(&self, ranges: &[(u32, u32)]) -> bool {
        self.update_view_taking(|v| Some(v.set_entry_line_ranges_if_changed(ranges)))
            .unwrap_or(false)
    }

    /// How many times the visual items list was actually replaced.
    ///
    /// Counts replacements rather than frames, so a caller can tell that a
    /// frame over unchanged history reused the stored list.
    #[must_use]
    pub fn visual_items_writes(&self) -> u64 {
        self.with_view(
            jinn_chat_log_view_msg::ChatLogViewUi::visual_items_writes,
            || 0,
        )
    }

    /// How many times the per-entry line ranges were actually replaced.
    ///
    /// Counts replacements rather than frames, so a caller can tell that a
    /// frame over unchanged history reused the stored ranges.
    #[must_use]
    pub fn entry_line_ranges_writes(&self) -> u64 {
        self.with_view(
            jinn_chat_log_view_msg::ChatLogViewUi::entry_line_ranges_writes,
            || 0,
        )
    }

    /// Store the content width the renderer just measured at.
    ///
    /// The session load reads this back to measure a freshly loaded history
    /// at the width the next frame will use, so the measurement is not
    /// discarded as stale.
    pub fn set_content_width(&self, width: u16) {
        self.update_view(|v| v.content_width.store(u32::from(width), Ordering::Relaxed));
    }

    /// The content width the last render measured at.
    ///
    /// `0` before the first render, which the renderer also treats as "do
    /// not wrap".
    #[must_use]
    pub fn content_width(&self) -> u16 {
        self.with_view(
            |v| u16::try_from(v.content_width.load(Ordering::Relaxed)).unwrap_or(u16::MAX),
            || 0,
        )
    }

    /// Store how many consecutive tool entries the renderer just collapsed.
    ///
    /// A coverage probe reads this back so the visual items it builds match
    /// the ones the next frame will build; see
    /// [`set_content_width`] for the same arrangement.
    pub fn set_min_collapse_count(&self, count: usize) {
        self.update_view(|v| v.min_collapse_count.store(count as u32, Ordering::Relaxed));
    }

    /// The collapse threshold the last render used, or `0` before the first.
    #[must_use]
    pub fn min_collapse_count(&self) -> Option<usize> {
        self.with_view(
            |v| match v.min_collapse_count.load(Ordering::Relaxed) {
                0 => None,
                n => Some(n as usize),
            },
            || None,
        )
    }

    /// A snapshot copy of the visual items list computed by the last render.
    ///
    /// Returns an empty vec before the first render. Copying keeps the view
    /// lock out of callers' hands — navigation resolves indices against the
    /// snapshot while the renderer may publish a newer list underneath.
    #[must_use]
    pub fn visual_items_snapshot(&self) -> Vec<VisualItem> {
        self.with_view(|v| v.visual_items.read().clone(), Vec::new)
    }

    /// The visual item at the currently selected position, if any.
    pub fn selected_visual_item(&self) -> Option<VisualItem> {
        let idx = self.selected_entry_index()?;
        self.with_view(|v| v.visual_items.read().get(idx).cloned(), || None)
    }

    /// Whether the cursor is currently on a collapsed ignored block.
    ///
    /// Reads the one item it needs under the view lock instead of copying the
    /// whole list first. The snapshot exists so navigation can resolve indices
    /// against a stable copy while the renderer publishes underneath — but this
    /// asks a yes/no question about one slot, so there is nothing to stabilize:
    /// copying several thousand visual items to discard all but one bought
    /// nothing that holding the lock for one lookup does not already give.
    #[must_use]
    pub fn is_selected_collapsed_block(&self) -> bool {
        let Some(idx) = self.selected_entry_index() else {
            return false;
        };
        self.with_view(
            |v| {
                v.visual_items
                    .read()
                    .get(idx)
                    .is_some_and(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }))
            },
            || false,
        )
    }

    /// Resolve the selected visual-item index to a history index.
    ///
    /// Returns `None` if nothing is selected or the selected item is a
    /// collapsed block (not a real entry).
    pub fn selected_history_index(&self) -> Option<usize> {
        let vi_idx = self.selected_entry_index()?;
        let items = self.visual_items_snapshot();

        if items.is_empty() {
            // Before first render, visual items haven't been computed yet.
            // Fall back: selected_entry_index IS a history index in this case.
            return Some(vi_idx);
        }
        match items.get(vi_idx)? {
            VisualItem::Entry(hist_idx) => Some(*hist_idx),
            VisualItem::CollapsedIgnoredBlock { .. } => None,
        }
    }
}
