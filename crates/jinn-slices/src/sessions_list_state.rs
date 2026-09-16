//! Sessions-list view vocabulary — the entry model, tree prompt state,
//! and preview cache shared between the kernel's session list logic and
//! the sidebar slice.
//!
//! The kernel owns the list logic (building entries from the session
//! map, reconcile on removal); the sidebar slice owns the section's
//! interactions. Both speak these types.

use std::collections::HashMap;

use crate::sidebar_sections::SessionEntryKind;
use jinn_core_types::SessionId;

/// One visible row in the sessions list: a loaded session with the
/// tree-geometry fields needed to render it.
#[derive(Clone)]
pub struct SessionEntry {
    /// The kind of this entry.
    pub kind: SessionEntryKind,
    pub id: SessionId,
    pub title: String,
    pub is_active: bool,
    pub created_at: jiff::Timestamp,
    pub is_idle: bool,
    pub last_entry_is_error: bool,

    /// Parent session ID - `None` for root sessions.
    pub parent_id: Option<SessionId>,
    /// Depth in the session tree. 0 for roots, 1 for their children, etc.
    pub depth: usize,
    /// For each ancestor level (0..depth-1), `true` if that ancestor has younger siblings.
    /// Used to render `│` vs ` ` continuation characters.
    pub ancestor_continuations: Vec<bool>,
    /// Whether this entry is the last child of its parent.
    /// Used to render `└` vs `├`.
    pub is_last_child: bool,
    /// Whether this session is a subagent spawned by the `task` tool.
    /// Derived from the parent link; rendered as a symbol next to the title.
    pub is_subagent: bool,
    /// Whether this session has a live `interactive_term` terminal
    /// (from `frontend.terminal.live_terms`); rendered as a symbol.
    pub has_live_term: bool,
}

/// The tree action a confirmation prompt was armed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreePromptAction {
    /// Archive the subtree as-is (`A` key).
    Archive,
    /// Tear down the root, then archive the subtree (`X` key).
    TeardownAndArchive,
}

/// State of the archive-tree confirmation prompt.
///
/// OWNER: IntentHandler (armed on the first press of the arming key,
/// consumed when that same key is pressed again, dismissed on any other
/// intent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveTreePrompt {
    /// Armed: the subtree was fully idle at arm time; `count` is the visible
    /// subtree size (selection plus descendants).
    Confirm {
        /// Number of sessions the confirm press will archive.
        count: usize,
        /// Which tree action the confirm press will perform.
        action: TreePromptAction,
    },
    /// Blocked: at least one member is busy; nothing will archive.
    Busy,
}

/// History length component of the preview cache key.
type HistoryLen = usize;
/// Content width component of the preview cache key.
type ContentWidth = u16;

/// Cache for session preview popup rendered lines.
///
/// Keyed by `(SessionId, HistoryLen, ContentWidth)` so that:
/// - Switching sessions produces a cache miss (different `SessionId`).
/// - New completed messages produce a cache miss (different `HistoryLen`).
/// - Terminal resize produces a cache miss (different `ContentWidth`).
#[derive(Debug, Default)]
pub struct SessionPreviewCache {
    entries: HashMap<(SessionId, HistoryLen, ContentWidth), Vec<ratatui::text::Line<'static>>>,
}

impl SessionPreviewCache {
    /// Creates a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up cached preview lines for the given key.
    pub fn get(
        &self,
        session_id: &SessionId,
        history_len: HistoryLen,
        width: ContentWidth,
    ) -> Option<&Vec<ratatui::text::Line<'static>>> {
        self.entries.get(&(session_id.clone(), history_len, width))
    }

    /// Drops all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Stores preview lines for the given key.
    pub fn insert(
        &mut self,
        session_id: SessionId,
        history_len: HistoryLen,
        width: ContentWidth,
        lines: Vec<ratatui::text::Line<'static>>,
    ) {
        self.entries.insert((session_id, history_len, width), lines);
    }
}
