//! Sidebar section state — the five sidebar sections' cursor/selection state.
//!
//! These are the payload of the sidebar slice's one cell. The structs carry
//! only cursor/selection state (which entry is selected, scroll offsets,
//! render-measured preview geometry); section data lives elsewhere. The
//! kernel's `FrontendState` exposes them through its facade, and the sidebar
//! slice's modules read and write them.

use std::collections::HashMap;

use crate::sidebar_section_id::SidebarSectionId;
use jinn_core_types::SessionId;
use jinn_slices::SlotKey;

/// The slot key the sidebar sections cell lives under.
#[must_use]
pub fn sidebar_sections_slot() -> SlotKey {
    SlotKey::builtin("sidebar", "state")
}

/// The five sidebar sections' state, stored as one aggregate in the sidebar
/// slice's cell.
/// State for the rename session input popup - editing a session title.
///
/// The editable text and cursor live in [`LineInput`] (shared with other popup
/// inputs) under the [`RenameSessionInputState::text`] field; access via
/// `.text.input` / `.text.cursor_pos`.
#[derive(Debug, Clone, Default)]
pub struct RenameSessionInputState {
    /// The editable text + cursor.
    pub text: jinn_slices::line_input::LineInput,
}

#[derive(Debug, Clone, Default)]
pub struct SidebarSections {
    /// Which section has keyboard focus. `None` while the sidebar itself
    /// is unfocused; the dynamic sidebar scope being on the stack is the
    /// authoritative "sidebar focused" fact, this is the intra-sidebar
    /// cursor.
    pub focused_section: Option<SidebarSectionId>,
    /// Pinned-entry selection state.
    pub pins: PinsState,
    /// Open-sessions list cursor state.
    pub sessions: SessionsSectionState,
    /// Persona section cursor state.
    pub persona: PersonaSectionState,
    /// Task-list section cursor + preview geometry state.
    pub task_list: TaskListSectionState,
    /// MCP servers section cursor state.
    pub mcp_servers: McpServersSectionState,
    /// In-progress text for the rename-session popup.
    pub rename_input: RenameSessionInputState,
}

use jinn_core_types::ChatEntryId;

/// State for the pins sidebar section.
///
/// Tracks which pinned entry is currently selected by ID.
/// The sidebar section uses this for rendering highlights and
/// dispatching pin/unpin actions.
#[derive(Debug, Clone, Default)]
pub struct PinsState {
    /// ID of the currently selected pinned entry, if any.
    selected_id: Option<ChatEntryId>,
}

impl PinsState {
    /// Resolves the current selection to an index into the given sorted ID list.
    ///
    /// Returns 0 if the selected ID is not found (or `None`), clamped to list length.
    /// Returns 0 if the list is empty.
    #[must_use]
    pub fn selection_index(&self, sorted_ids: &[ChatEntryId]) -> usize {
        match &self.selected_id {
            None => 0,
            Some(id) => sorted_ids.iter().position(|sid| sid == id).unwrap_or(0),
        }
    }

    /// Returns the currently selected entry ID, if any.
    #[must_use]
    pub fn selected_id(&self) -> Option<&ChatEntryId> {
        self.selected_id.as_ref()
    }

    /// Moves the selection to the next pinned entry in the sorted list.
    #[expect(
        clippy::indexing_slicing,
        reason = "indices are bounds-checked above via .is_empty() and .min()"
    )]
    pub fn select_next(&mut self, sorted_ids: &[ChatEntryId]) {
        if sorted_ids.is_empty() {
            return;
        }
        let current = self.selection_index(sorted_ids);
        let next = (current + 1).min(sorted_ids.len() - 1);
        self.selected_id = Some(sorted_ids[next].clone());
    }

    /// Moves the selection to the previous pinned entry in the sorted list.
    #[expect(
        clippy::indexing_slicing,
        reason = "indices are bounds-checked above via .is_empty() and current > 0"
    )]
    pub fn select_prev(&mut self, sorted_ids: &[ChatEntryId]) {
        if sorted_ids.is_empty() {
            return;
        }
        let current = self.selection_index(sorted_ids);
        if current > 0 {
            self.selected_id = Some(sorted_ids[current - 1].clone());
        }
    }

    /// Sets the selection to a specific entry by ID.
    pub fn select_by_id(&mut self, id: ChatEntryId) {
        self.selected_id = Some(id);
    }

    /// Adjusts selection after a mutation (pin/unpin).
    ///
    /// If the current ID is still in the sorted list, keep it.
    /// If not, move to the nearest valid ID by clamping `old_index` to the new list bounds.
    /// If the list is empty, set to `None`.
    ///
    /// `old_index` should be the index of the previously selected entry in the
    /// *pre-mutation* sorted list. Callers should resolve this before mutating state.
    #[expect(
        clippy::indexing_slicing,
        reason = "indices are bounds-checked above via .is_empty() and .min()"
    )]
    pub fn clamp_to_nearest(&mut self, sorted_ids: &[ChatEntryId], old_index: usize) {
        if sorted_ids.is_empty() {
            self.selected_id = None;
            return;
        }
        // If the current ID is still in the list, keep it.
        if let Some(id) = &self.selected_id
            && sorted_ids.iter().any(|sid| sid == id)
        {
            return;
        }
        // Otherwise, clamp the old index to the new list bounds.
        let clamped = old_index.min(sorted_ids.len() - 1);
        self.selected_id = Some(sorted_ids[clamped].clone());
    }

    /// Clears the selection (no entry selected).
    pub fn clear_selection(&mut self) {
        self.selected_id = None;
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use jinn_core_types::ChatEntryId;

    use super::*;

    /// Helper: create a list of unique `ChatEntryIds`.
    fn make_ids(n: usize) -> Vec<ChatEntryId> {
        std::iter::repeat_with(ChatEntryId::new).take(n).collect()
    }

    #[rstest::rstest]
    fn default_has_no_selection() {
        // Given a default pins state.
        let state = PinsState::default();

        // Then no entry is selected.
        assert!(state.selected_id().is_none());
    }

    #[rstest::rstest]
    fn selection_index_returns_zero_when_none_selected() {
        // Given a default pins state and sorted IDs.
        let ids = make_ids(2);
        let state = PinsState::default();

        // When resolving selection index.
        let index = state.selection_index(&ids);

        // Then it returns 0.
        assert_eq!(index, 0);
    }

    #[rstest::rstest]
    fn selection_index_resolves_id_to_position() {
        // Given sorted IDs [A, B, C] with B selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[1].clone());

        // When resolving selection index.
        let index = state.selection_index(&ids);

        // Then it returns 1 (position of B).
        assert_eq!(index, 1);
    }

    #[rstest::rstest]
    fn selection_index_returns_zero_when_id_not_found() {
        // Given sorted IDs [A, B] with an unknown ID selected.
        let ids = make_ids(2);
        let mut state = PinsState::default();
        state.select_by_id(ChatEntryId::new());

        // When resolving selection index.
        let index = state.selection_index(&ids);

        // Then it returns 0 (fallback).
        assert_eq!(index, 0);
    }

    #[rstest::rstest]
    fn select_next_advances_to_second_id() {
        // Given sorted IDs [A, B, C] with A selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[0].clone());

        // When selecting next.
        state.select_next(&ids);

        // Then B is selected.
        assert_eq!(state.selected_id(), Some(&ids[1]));
    }

    #[rstest::rstest]
    fn select_next_clamps_at_last() {
        // Given sorted IDs [A, B, C] with C selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[2].clone());

        // When selecting next.
        state.select_next(&ids);

        // Then C is still selected.
        assert_eq!(state.selected_id(), Some(&ids[2]));
    }

    #[rstest::rstest]
    fn select_next_is_noop_when_empty() {
        // Given an empty sorted ID list.
        let mut state = PinsState::default();

        // When selecting next.
        state.select_next(&[]);

        // Then no panic and no selection.
        assert!(state.selected_id().is_none());
    }

    #[rstest::rstest]
    fn select_prev_decrements() {
        // Given sorted IDs [A, B, C] with B selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[1].clone());

        // When selecting previous.
        state.select_prev(&ids);

        // Then A is selected.
        assert_eq!(state.selected_id(), Some(&ids[0]));
    }

    #[rstest::rstest]
    fn select_prev_clamps_at_zero() {
        // Given sorted IDs [A, B, C] with A selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[0].clone());

        // When selecting previous.
        state.select_prev(&ids);

        // Then A is still selected.
        assert_eq!(state.selected_id(), Some(&ids[0]));
    }

    #[rstest::rstest]
    fn select_by_id_sets_selection() {
        // Given a default pins state.
        let mut state = PinsState::default();
        let id = ChatEntryId::new();

        // When selecting by ID.
        state.select_by_id(id.clone());

        // Then the ID is selected.
        assert_eq!(state.selected_id(), Some(&id));
    }

    #[rstest::rstest]
    fn clear_selection_sets_none() {
        // Given a pins state with a selection.
        let mut state = PinsState::default();
        state.select_by_id(ChatEntryId::new());
        assert!(state.selected_id().is_some());

        // When clearing selection.
        state.clear_selection();

        // Then no entry is selected.
        assert!(state.selected_id().is_none());
    }

    #[rstest::rstest]
    fn clamp_to_nearest_keeps_valid_id() {
        // Given sorted IDs [A, B, C] with B selected.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[1].clone());

        // When clamping on the same list [A, B, C] with old_index 1.
        state.clamp_to_nearest(&ids, 1);

        // Then B is still selected (it's still in the list).
        assert_eq!(state.selected_id(), Some(&ids[1]));
    }

    #[rstest::rstest]
    fn clamp_to_nearest_moves_to_nearest_when_id_removed() {
        // Given sorted IDs [A, B, C] with B selected at old_index 1.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[1].clone());

        // When clamping on new list [A, C] with old_index 1.
        let new_ids = vec![ids[0].clone(), ids[2].clone()];
        state.clamp_to_nearest(&new_ids, 1);

        // Then C is selected (index 1 in new list).
        assert_eq!(state.selected_id(), Some(&ids[2]));
    }

    #[rstest::rstest]
    fn clamp_to_nearest_sets_none_when_empty() {
        // Given sorted IDs [A] with A selected at old_index 0.
        let ids = make_ids(1);
        let mut state = PinsState::default();
        state.select_by_id(ids[0].clone());

        // When clamping on an empty list.
        state.clamp_to_nearest(&[], 0);

        // Then no entry is selected.
        assert!(state.selected_id().is_none());
    }

    #[rstest::rstest]
    fn clamp_to_nearest_clamps_old_index_to_last_when_id_removed() {
        // Given 5 IDs [A, B, C, D, E] with E selected at old_index 4.
        let ids = make_ids(5);
        let mut state = PinsState::default();
        state.select_by_id(ids[4].clone());

        // When clamping on new list [A, B, C] (D and E removed) with old_index 4.
        // old_index(4).min(sorted_ids.len() - 1) = 4.min(2) = 2.
        let new_ids = vec![ids[0].clone(), ids[1].clone(), ids[2].clone()];
        state.clamp_to_nearest(&new_ids, 4);

        // Then C is selected (last element in new list, clamped from 4 to 2).
        assert_eq!(state.selected_id(), Some(&ids[2]));
    }

    #[rstest::rstest]
    fn clamp_to_nearest_preserves_existing_valid_id_on_shrink() {
        // Given [A, B, C] with C selected, old_index 2.
        let ids = make_ids(3);
        let mut state = PinsState::default();
        state.select_by_id(ids[2].clone());

        // When clamping on new list [A, B, C] - C is still present.
        state.clamp_to_nearest(&ids, 2);

        // Then C remains selected (kept because it's still in the list).
        assert_eq!(state.selected_id(), Some(&ids[2]));
    }
}
/// Discriminator for sidebar list entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEntryKind {
    Session,
}

/// Sessions section cursor state - stored on `FrontendState`.
///
/// Tracks the selected index within the sorted open sessions list.
/// `None` means no cursor (section not focused).
#[derive(Debug, Clone, Default)]
pub struct SessionsSectionState {
    /// Index into the sorted open sessions list.
    pub selected_index: Option<usize>,
    /// Scroll offset: the first session entry index that is visible.
    pub scroll_offset: usize,
    /// Visual-parent index: maps a loaded session to its nearest loaded ancestor
    /// when the direct parent has been archived/removed from memory.
    /// Updated reactively in `remove_and_replace()`, invalidated on session load.
    /// Empty when no intermediate parents have been hidden.
    pub visual_parents: HashMap<SessionId, SessionId>,
}

/// Persona section cursor state - stored on `FrontendState`.
///
/// Tracks whether the persona section currently has the cursor.
/// `None` means no cursor (section not focused). `Some(0)` means
/// the single entry is selected.
#[derive(Debug, Clone, Default)]
pub struct PersonaSectionState {
    /// Which entry the cursor is on. Always `None` or `Some(0)`.
    pub cursor: Option<usize>,
}
/// State for the task list sidebar section.
///
/// Tracks which phase is selected (has cursor). `None` means the section
/// is unfocused — all phases are collapsed.
///
/// The preview popup scroll is driven by three fields, all written by the render
/// pre-pass so keypress handlers can page and clamp without re-wrapping:
/// - `preview_scroll`           — current top line offset (the only field mutated by
///   scroll intents and reset on navigation).
/// - `preview_viewport_height`  — popup inner height (rows) for page size.
/// - `preview_content_line_count` — total wrapped lines in the selected phase,
///   used to clamp `preview_scroll` to `[0, max_offset]`.
#[derive(Debug, Clone, Default)]
pub struct TaskListSectionState {
    /// Index into the task list's phases vector.
    /// `None` when the section is unfocused.
    pub selected_phase_index: Option<usize>,
    /// Current scroll offset (top line index) of the task list preview popup.
    pub preview_scroll: usize,
    /// Inner height (rows) of the preview popup's content area, as measured by
    /// the last render pass. Used by `PageUp`/`PageDown` to page by a viewport.
    /// Zero before the first render; scroll handlers no-op when it is zero.
    pub preview_viewport_height: u16,
    /// Total wrapped content line count of the selected phase, as measured by
    /// the last render pass. Lets the scroll handlers clamp `preview_scroll` to
    /// `[0, max_offset]` without re-wrapping the tasks.
    pub preview_content_line_count: usize,
}
/// MCP servers section cursor state — stored on `FrontendState`.
///
/// Tracks the selected index into the configured-servers list.
/// `None` means no cursor (section not focused).
#[derive(Debug, Clone, Default)]
pub struct McpServersSectionState {
    /// Index into the configured MCP servers list.
    pub selected_index: Option<usize>,
}
