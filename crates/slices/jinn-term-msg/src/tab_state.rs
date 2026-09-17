//! Per-session terminal mirror state — the `term/tabs` cell payload.
//!
//! The term coordinator actor writes per-session mirrors keyed by the
//! owning **chat** session id; the TUI overlay and the sidebar's
//! live-terminal symbol read them.

use std::collections::{HashMap, HashSet};

use jinn_core_types::SessionId;

use jinn_slices::SlotKey;

use crate::cells::ScreenCells;

/// One chat session's mirrored terminal screen.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TerminalMirror {
    /// Last rendered screen (plain text, newline-separated rows).
    pub screen: String,
    /// Styled cell grid matching `screen` (for the colored overlay).
    pub cells: ScreenCells,
    /// Cursor position (row, col) on the mirrored screen.
    pub cursor: (u16, u16),
    /// Whether the program hid the cursor (TUIs hide it while redrawing).
    pub cursor_hidden: bool,
}

/// Frontend mirror of all `interactive_term` sessions, keyed by chat session.
#[derive(Debug, Clone, Default)]
pub struct TerminalTabState {
    /// Per-chat-session terminal mirrors.
    pub mirrors: HashMap<SessionId, TerminalMirror>,
    /// Chat sessions with a **live** terminal (spawned, not exited/killed).
    /// Drives the sidebar's live-terminal symbol.
    pub live_terms: HashSet<SessionId>,
    /// Inner rect of the overlay, last reported as `(rows, cols)`; dedupes
    /// resize publications and seeds the spawn size before the first frame.
    pub last_layout_size: (u16, u16),
}

/// Spawn/pty size used before the overlay has ever been laid out (the
/// mirror's `last_layout_size` is `(0, 0)` until the first frame renders).
pub const DEFAULT_PTY_SIZE: (u16, u16) = (24, 80);

impl TerminalTabState {
    /// Replaces one session's mirrored screen and cursor from an update.
    pub fn apply_screen(
        &mut self,
        chat_session_id: &SessionId,
        screen: String,
        cells: ScreenCells,
        cursor: (u16, u16),
        cursor_hidden: bool,
    ) {
        self.mirrors.insert(
            chat_session_id.clone(),
            TerminalMirror {
                screen,
                cells,
                cursor,
                cursor_hidden,
            },
        );
    }

    /// Removes a session's mirror (session closed/teardown).
    pub fn remove_mirror(&mut self, chat_session_id: &SessionId) {
        self.mirrors.remove(chat_session_id);
    }

    /// Returns the mirror for a chat session, if any.
    #[must_use]
    pub fn mirror(&self, chat_session_id: &SessionId) -> Option<&TerminalMirror> {
        self.mirrors.get(chat_session_id)
    }

    /// Marks (or clears) a chat session's live-terminal flag.
    pub fn set_live(&mut self, chat_session_id: &SessionId, live: bool) {
        if live {
            self.live_terms.insert(chat_session_id.clone());
        } else {
            self.live_terms.remove(chat_session_id);
        }
    }

    /// Records the overlay's inner size, returning `true` when it changed and
    /// a resize should be published.
    pub fn record_layout_size(&mut self, rows: u16, cols: u16) -> bool {
        let changed = self.last_layout_size != (rows, cols);
        if changed {
            self.last_layout_size = (rows, cols);
        }
        changed
    }

    /// The size a new pty should be spawned at: the overlay's inner rect once
    /// a frame has laid it out, the VT100 default before that. Never zero —
    /// vt100's grid panics (`rows - 1` underflow) on a 0-row terminal.
    #[must_use]
    pub fn spawn_size(&self) -> (u16, u16) {
        if self.last_layout_size.0 == 0 || self.last_layout_size.1 == 0 {
            DEFAULT_PTY_SIZE
        } else {
            self.last_layout_size
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    fn apply_screen_replaces_mirror() {
        // Given an empty terminal state.
        let mut state = TerminalTabState::default();
        let chat = SessionId::new();

        // When applying a screen update.
        state.apply_screen(
            &chat,
            "hello\nworld".to_owned(),
            ScreenCells::default(),
            (1, 3),
            true,
        );

        // Then the mirror carries the session, screen, cursor, and visibility.
        let mirror = state.mirror(&chat).expect("mirror");
        assert_eq!(mirror.screen, "hello\nworld");
        assert_eq!(mirror.cursor, (1, 3));
        assert!(mirror.cursor_hidden);
    }

    #[rstest::rstest]
    fn mirrors_are_keyed_by_chat_session() {
        // Given a state with two sessions' mirrors.
        let mut state = TerminalTabState::default();
        let a = SessionId::new();
        let b = SessionId::new();
        state.apply_screen(
            &a,
            "alpha".to_owned(),
            ScreenCells::default(),
            (0, 0),
            false,
        );
        state.apply_screen(&b, "beta".to_owned(), ScreenCells::default(), (0, 0), false);

        // When reading each mirror back.
        // Then each session sees only its own screen.
        assert_eq!(state.mirror(&a).expect("a").screen, "alpha");
        assert_eq!(state.mirror(&b).expect("b").screen, "beta");

        // When removing one mirror.
        state.remove_mirror(&a);

        // Then only the other remains.
        assert!(state.mirror(&a).is_none());
        assert!(state.mirror(&b).is_some());
    }

    #[rstest::rstest]
    fn record_layout_size_reports_change_once() {
        // Given a default terminal state (0, 0).
        let mut state = TerminalTabState::default();

        // When recording a new layout size.
        let first = state.record_layout_size(24, 100);

        // Then the first report signals change.
        assert!(first);

        // When recording the same size again.
        let second = state.record_layout_size(24, 100);

        // Then no change is signaled.
        assert!(!second);
    }

    #[rstest::rstest]
    fn spawn_size_falls_back_to_default_before_first_frame() {
        // Given a default terminal state (no overlay ever laid out).
        let state = TerminalTabState::default();

        // Then the spawn size is the VT100 default, not the zeroed layout.
        assert_eq!(state.spawn_size(), DEFAULT_PTY_SIZE);
    }

    #[rstest::rstest]
    fn spawn_size_never_zero_after_a_zeroed_layout() {
        // Given a state whose recorded layout size was zeroed (degenerate
        // frame geometry).
        let mut state = TerminalTabState::default();
        state.record_layout_size(0, 0);

        // Then the spawn size is still the VT100 default.
        assert_eq!(state.spawn_size(), DEFAULT_PTY_SIZE);
    }

    #[rstest::rstest]
    fn spawn_size_uses_the_laid_out_overlay_size() {
        // Given a state with a laid-out overlay inner rect.
        let mut state = TerminalTabState::default();
        state.record_layout_size(30, 110);

        // Then the spawn size is that rect (WYSIWYG).
        assert_eq!(state.spawn_size(), (30, 110));
    }
}

/// The slot key the terminal tab state cell lives under.
#[must_use]
pub fn term_tabs_slot() -> SlotKey {
    SlotKey::builtin("term", "tabs")
}
