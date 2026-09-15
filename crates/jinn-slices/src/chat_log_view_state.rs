//! The chat-log-view slice's shared cell vocabulary.
//!
//! [`ChatLogViewUi`] bundles the per-session chat log *view* state — scroll
//! intent and render caches, cursor selection, expand/ignore sets, the saved
//! pins position, and the ignore-sweep — into one payload. The concrete
//! types live beside it in `jinn-slices` ([`crate::VisualItem`]) so the
//! kernel writes them without depending on the slice crate.
//!
//! Writers are the exempt IntentHandler (scroll/selection/expand/ignore
//! arms, via `ChatSession`'s semantic methods) and the renderer (per-frame
//! write-back through the same methods). There is no actor and no route row:
//! exactly as the migration docs prescribe for this slice.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU16, Ordering};

use parking_lot::RwLock;

use crate::SlotKey;
use jinn_core_types::{ChatEntryId, ContextOverride, SessionId};

/// A visual item in the chat log, computed from the flat history at render
/// time.
///
/// Each item is either a real entry (referenced by its index in the flat
/// history) or a collapsed block of consecutive ignored entries displayed as
/// a single summary line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisualItem {
    /// A real entry, referenced by its index in the flat history.
    Entry(usize),
    /// A collapsed block of consecutive ignored entries.
    CollapsedIgnoredBlock {
        /// Index of the first ignored entry in the block (in flat history).
        start: usize,
        /// Number of consecutive ignored entries in this block.
        count: usize,
    },
}

/// Snapshot of chat log scroll position captured before entering the Pins
/// section.
///
/// Used to restore the history viewport when the user navigates away from
/// Pins to another sidebar section (Persona/Sessions). Discarded without
/// restoring when the user leaves the sidebar entirely to Normal scope,
/// indicating they wanted to view the pinned entry in the history.
#[derive(Debug, Clone, Default)]
pub struct SavedHistoryPosition {
    /// The scroll offset at the time of capture.
    pub scroll_offset: Option<u16>,
    /// The entry ID of the cursor at the time of capture.
    pub selected_cursor_id: Option<ChatEntryId>,
}

/// The chat-log-view slice's per-session cell payload: everything about how
/// one session's chat log is currently *displayed*.
///
/// Defaults match the historical `SessionUi` defaults exactly: auto-scroll
/// (`scroll_offset: None`), no selection, empty caches and sets.
#[derive(Debug, Default)]
pub struct ChatLogViewUi {
    /// Number of lines to skip from the top when rendering (ratatui scroll
    /// offset).
    ///
    /// `None` means "show the bottom of the conversation" (auto-scroll).
    /// `Some(n)` means the user has manually scrolled to offset `n`.
    pub scroll_offset: Option<u16>,
    /// The entry ID of the currently selected cursor position, if any.
    ///
    /// This is the source of truth for selection. The visual-item index
    /// is resolved on demand. `None` means no entry is selected.
    pub selected_cursor_id: Option<ChatEntryId>,
    /// The maximum scroll offset computed during the last render.
    ///
    /// Used by scroll handlers to resolve the "at bottom" sentinel into
    /// a concrete offset so `scroll_up` / `scroll_down` work correctly.
    /// Uses `AtomicU16` for interior mutability since the element receives
    /// `&self`.
    pub last_max_offset: AtomicU16,
    /// The actual viewport scroll offset after clamping and
    /// scroll-to-selected adjustment, as computed by the render pipeline.
    ///
    /// Unlike `scroll_offset` (the user's intent), this reflects what's
    /// actually displayed. Written by the renderer each frame, read by
    /// intent handlers to determine visible entries.
    pub rendered_scroll_offset: AtomicU16,
    /// Per-entry wrapped line ranges computed by the renderer each frame.
    ///
    /// `entry_line_ranges[i] = (start_wrapped_line, end_wrapped_line)` in
    /// wrapped coordinate space. Used by intent handlers to determine which
    /// entries are visible in the viewport.
    pub entry_line_ranges: RwLock<Vec<(u16, u16)>>,
    /// The viewport height (render area height) set by the renderer each
    /// frame.
    pub viewport_height: AtomicU16,
    /// Number of blank lines prepended by the renderer for bottom-alignment.
    pub blank_count: AtomicU16,
    /// The set of chat entry IDs whose tool result content is expanded.
    ///
    /// When a tool result entry is expanded, its full content is shown
    /// instead of being truncated. This is ephemeral UI state - not
    /// persisted.
    pub expanded_entries: HashSet<ChatEntryId>,
    /// Snapshot of chat log position before entering Pins sidebar section.
    ///
    /// `None` when not in a Pins browsing session. Set when the cursor
    /// enters Pins, restored when the cursor leaves to another section,
    /// discarded when leaving the sidebar to Normal.
    pub saved_history_position: Option<SavedHistoryPosition>,
    /// Entry IDs whose ignored blocks are currently *shown* (expanded).
    ///
    /// Default: empty (all ignored blocks are collapsed).
    /// Key: the ID of the first entry in the contiguous ignored block.
    /// Ephemeral - not persisted across restarts.
    pub shown_ignored_blocks: HashSet<ChatEntryId>,
    /// The visual items list computed from flat history during render.
    ///
    /// Maps visual-item positions to either real entries or collapsed
    /// ignored blocks. Set by the renderer each frame, read by intent
    /// handlers for navigation and toggle.
    pub visual_items: RwLock<Vec<VisualItem>>,
    /// Tracks an active "x-sweep": holding `x` to apply a fixed ignore state
    /// across consecutive entries.
    ///
    /// `Some((instant, override))` means a sweep is active:
    /// - `instant`: timestamp of the last `x` press in this sweep
    /// - `override`: the `ContextOverride` to apply to subsequent entries
    ///
    /// Cleared by: >100ms gap, or any non-`ChatEntryIgnoreSelected` intent.
    pub ignore_sweep: Option<(std::time::Instant, ContextOverride)>,
}

impl Clone for ChatLogViewUi {
    fn clone(&self) -> Self {
        Self {
            scroll_offset: self.scroll_offset,
            selected_cursor_id: self.selected_cursor_id.clone(),
            last_max_offset: AtomicU16::new(self.last_max_offset.load(Ordering::Relaxed)),
            rendered_scroll_offset: AtomicU16::new(
                self.rendered_scroll_offset.load(Ordering::Relaxed),
            ),
            entry_line_ranges: RwLock::new(self.entry_line_ranges.read().clone()),
            viewport_height: AtomicU16::new(self.viewport_height.load(Ordering::Relaxed)),
            blank_count: AtomicU16::new(self.blank_count.load(Ordering::Relaxed)),
            expanded_entries: self.expanded_entries.clone(),
            saved_history_position: self.saved_history_position.clone(),
            shown_ignored_blocks: self.shown_ignored_blocks.clone(),
            visual_items: RwLock::new(self.visual_items.read().clone()),
            ignore_sweep: self.ignore_sweep,
        }
    }
}

/// The chat-log-view cell's payload: one view state per session, keyed by
/// session id.
///
/// Readers must not grow the map (a session with no entry reads as its
/// default view); only writers get-or-insert. Entries persist for the life
/// of the process — bounded by the number of sessions opened, and cleaned
/// up with the session family later.
pub type ChatLogViews = HashMap<SessionId, ChatLogViewUi>;

/// The slot key the chat-log-view cell lives under.
#[must_use]
pub fn chat_log_views_slot() -> SlotKey {
    SlotKey::builtin("chat-log-view", "state")
}
