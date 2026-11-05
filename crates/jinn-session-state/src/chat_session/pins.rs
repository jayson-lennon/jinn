//! Pinned tool-loop display: pin/unpin, shown-propagation, and the display-row projection.

use super::*;

impl ChatSessionState {
    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    /// Pin an entry by ID, setting its pin position.
    ///
    /// If no entry with the given ID exists, this is a no-op.
    ///
    /// When pinning an ignored entry inside a shown (expanded) ignored block,
    /// the pinned entry becomes a block splitter in `build_visual_items`.
    /// This propagates `shown_ignored_blocks` to any new forward sub-block
    /// created by the split, keeping all entries visible.
    pub fn pin_entry(&mut self, id: &ChatEntryId, position: PinPosition) {
        let Some(entry) = self.core.history_work.history.iter().find(|e| e.id == *id) else {
            return;
        };
        // Captured before pinning: a pin makes the entry in-context, but the
        // propagation below only applies when the entry was ignored.
        let was_ignored = !entry.is_in_context();
        let index = self
            .core
            .history_work
            .history
            .iter()
            .position(|e| e.id == *id)
            .unwrap_or_default();
        // Chunk semantics: pinning a member pins the whole tool loop.
        self.edit_history().pin(id, position);
        self.propagate_shown_after_pin(index, was_ignored);
    }

    /// Propagate `shown_ignored_blocks` when pinning an ignored entry inside a
    /// shown (expanded) block split it.
    fn propagate_shown_after_pin(&mut self, idx: usize, was_ignored: bool) {
        // Propagation: only for entries that were ignored before the pin.
        if !was_ignored {
            return;
        }

        // Scan backward to find the containing block's start.
        // Same boundary rules as `build_visual_items` and `toggle_ignored_block_visibility`.
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

        let Some(block_entry) = self.core.history_work.history.get(block_start) else {
            return;
        };
        let block_representative = block_entry.id.clone();
        let was_shown = self.with_view(
            |v| v.shown_ignored_blocks.contains(&block_representative),
            || false,
        );
        if !was_shown {
            return; // Block was collapsed - nothing to propagate.
        }

        // Scan forward from the pinned entry to find the new forward sub-block.
        let forward_start = idx + 1;
        if forward_start >= self.core.history_work.history.len() {
            return; // No entries after the pin.
        }

        let Some(forward_entry) = self.core.history_work.history.get(forward_start) else {
            return;
        };
        if forward_entry.is_in_context() || forward_entry.pin_position.is_some() {
            return; // Forward entry is not part of an ignored block.
        }

        // The forward sub-block's representative is its first entry.
        let forward_representative = forward_entry.id.clone();
        self.update_view(|v| {
            v.shown_ignored_blocks.insert(forward_representative);
        });
    }

    /// Unpin an entry by ID, clearing its pin position.
    ///
    /// Chunk semantics: unpinning a member unpins the whole tool loop.
    /// If no entry with the given ID exists, this is a no-op.
    pub fn unpin_entry(&mut self, id: &ChatEntryId) {
        self.edit_history().unpin(id);
    }

    /// Returns all pinned entries in history order.
    pub fn pinned_entries(&self) -> Vec<&ChatEntry> {
        self.core
            .history_work
            .history
            .iter()
            .filter(|e| e.is_pinned())
            .collect()
    }

    /// Projects the pinned entries into pins-sidebar display rows.
    ///
    /// A pinned tool loop is pinned as a whole (provider APIs require intact
    /// call/result structure), but it displays as one row per completed
    /// `ToolResult` — plus one `⚒` row per call with no completed result in
    /// the loop — while every non-loop pinned entry keeps its own row.
    /// Rows sort TOP → RELATIVE → BOTTOM, stable by history order within
    /// each group, matching the `pin_sort_key` order of sidebar consumers.
    ///
    /// This is a display projection only: pin state itself is unchanged, so
    /// unpinning a row's entry still clears the whole loop's pins.
    #[must_use]
    pub fn pinned_display_rows(&self) -> Vec<PinDisplayRow> {
        let mut rows = display_rows(self.history());
        // Stable sort by display position: TOP, then RELATIVE/None, then
        // BOTTOM, preserving history order within each group.
        rows.sort_by_key(|row| pin_display_sort_key(row.position));
        rows
    }

    /// The number of rows the pins sidebar displays for this session.
    ///
    /// Sidebar consumers must count display rows, not pinned entries: a
    /// pinned tool loop contributes its result rows, not its member count.
    #[must_use]
    pub fn pinned_display_count(&self) -> usize {
        self.pinned_display_rows().len()
    }
}

/// One row of the pins sidebar: a representative entry plus how to label it.
///
/// The projection from pinned entries to rows lives in
/// [`ChatSessionState::pinned_display_rows`]; this type is only its output.
/// A row's `entry_id` is always a real history entry id, so selection,
/// unpin, and pin-cycle operations address it like any pinned entry, while
/// chunk pin semantics stay unchanged (unpinning still clears the loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDisplayRow {
    /// The id the row's selection, unpin, and pin-cycle operations address:
    /// the id of the representative entry itself.
    pub entry_id: ChatEntryId,
    /// The pin position the row's badge shows, inherited from the entry.
    pub position: PinPosition,
    /// What the row displays.
    pub label: PinRowLabel,
}

/// What a pins-sidebar display row shows.
///
/// The sidebar turns a label into styled text; the prefix is rendered from
/// the variant (`Verbatim` keeps the entry kind's usual prefix, `Result` and
/// `SkillResult` render with no prefix, `Call` renders with the `⚒` prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinRowLabel {
    /// Every non-tool-loop entry kind, displayed exactly as today under its
    /// existing per-kind prefix.
    Verbatim,
    /// A completed tool result: `✓ name: content` on success,
    /// `✗ name: content` on failure.
    Result {
        /// The tool's name.
        name: String,
        /// The result content.
        content: String,
        /// Whether the result completed successfully (`✗` otherwise).
        success: bool,
    },
    /// A loaded-skill tool result: the skill summary label, no prefix.
    SkillResult {
        /// The result content, parsed into the skill summary label.
        content: String,
    },
    /// A tool call without a completed result in its loop: `name(...)`.
    Call {
        /// The tool's name.
        name: String,
    },
}

/// The display sort weight for a pin position: TOP first, then RELATIVE
/// (or absent), then BOTTOM — the same order the sidebar consumers apply.
fn pin_display_sort_key(position: PinPosition) -> u8 {
    match position {
        PinPosition::Top => 0,
        PinPosition::Relative => 1,
        PinPosition::Bottom => 2,
    }
}

/// Projects a history into display rows in history order (unsorted).
///
/// Standalone pinned entries become one row each. A pinned tool loop
/// contributes one row per completed result, one row per call with no
/// completed result, and nothing for the opener or interstitials. Call rows
/// appear where the call sits; result rows where the result sits — history
/// order, so parallel calls/results interleave naturally.
fn display_rows(history: &[ChatEntry]) -> Vec<PinDisplayRow> {
    let chunks = chunking(history);

    let mut rows = Vec::new();
    for (index, entry) in history.iter().enumerate() {
        let Some(position) = entry.pin_position else {
            continue;
        };
        // Every index below history.len() lies in some chunk, so this can
        // only miss if `chunking` and the enumerate disagree — skip, not panic.
        let Some(row_chunk) = chunk_containing(&chunks, index) else {
            continue;
        };

        if is_standalone(&row_chunk) {
            rows.push(standalone_row(entry, position));
            continue;
        }

        let Some(row) = loop_member_row(entry, &row_chunk, history, position) else {
            continue;
        };
        rows.push(row);
    }
    rows
}

/// Whether a chunk is a single standalone entry rather than a tool loop.
fn is_standalone(chunk: &Chunk) -> bool {
    chunk.range.len() == 1
}

/// The display row for a standalone pinned entry (a single-entry chunk).
///
/// Every non-loop kind keeps its own row under its existing prefix; tool
/// kinds pinned on their own (never produced by normal flow, but legal in
/// restored history) project the same label text a loop member would.
fn standalone_row(entry: &ChatEntry, position: PinPosition) -> PinDisplayRow {
    let label = match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } => result_label(content, name, *status),
        ChatEntryKind::ToolCall { name, .. } => call_label(name),
        _ => PinRowLabel::Verbatim,
    };
    PinDisplayRow {
        entry_id: entry.id.clone(),
        position,
        label,
    }
}

/// The display row for one member of a pinned multi-entry tool loop.
///
/// Returns `None` for members that render no row: the assistant opener,
/// interstitial entries (system, actor, thinking, transient, annotation,
/// rule-interrupt guidance), and *pending* results — a pending result is
/// represented by its call's row, so when the result completes the call's
/// ⚒ row becomes the result's ✓ row in place and the row count is stable.
/// A completed result always renders its row; a call renders only when no
/// completed result for its id exists anywhere in the same chunk.
fn loop_member_row(
    entry: &ChatEntry,
    chunk: &Chunk,
    history: &[ChatEntry],
    position: PinPosition,
) -> Option<PinDisplayRow> {
    match &entry.kind {
        ChatEntryKind::ToolResult {
            name,
            content,
            status,
            ..
        } if *status != ToolResultStatus::Pending => Some(PinDisplayRow {
            entry_id: entry.id.clone(),
            position,
            label: result_label(content, name, *status),
        }),
        ChatEntryKind::ToolCall { id, name, .. } => {
            if chunk_has_completed_result_for(history, chunk, id) {
                return None;
            }
            Some(PinDisplayRow {
                entry_id: entry.id.clone(),
                position,
                label: call_label(name),
            })
        }
        // Assistant opener and interstitials never display a row.
        _ => None,
    }
}

/// Whether any completed (non-pending) result for `call_id` sits inside
/// `chunk`.
///
/// Matching is by id within the chunk, never by adjacency, so a loop whose
/// calls and results are separated by interstitials still resolves — and a
/// result outside the chunk can never satisfy a call inside it.
fn chunk_has_completed_result_for(
    history: &[ChatEntry],
    chunk: &Chunk,
    call_id: &ToolCallId,
) -> bool {
    let members = history.get(chunk.range.start..chunk.range.end);
    members.is_some_and(|members| {
        members.iter().any(|member| {
            matches!(
                &member.kind,
                ChatEntryKind::ToolResult { id, status, .. }
                    if id == call_id && *status != ToolResultStatus::Pending
            )
        })
    })
}

/// Assembles the label for a tool result row.
///
/// Loaded skills are pinned as `<skill name="X" ...>` XML; they display the
/// parsed skill summary instead of the raw content. Every other result is
/// `icon name: content`, `✓` for success and `✗` for failure.
fn result_label(content: &str, name: &str, status: ToolResultStatus) -> PinRowLabel {
    if name == "skill" {
        return PinRowLabel::SkillResult {
            content: loaded_skill_summary_label(content),
        };
    }
    PinRowLabel::Result {
        name: name.to_owned(),
        content: content.to_owned(),
        success: status == ToolResultStatus::Success,
    }
}

/// Assembles the label for a pending-call row.
fn call_label(name: &str) -> PinRowLabel {
    PinRowLabel::Call {
        name: name.to_owned(),
    }
}
