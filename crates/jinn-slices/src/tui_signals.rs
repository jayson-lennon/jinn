// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, not, see <https://www.gnu.org/licenses/>.

//! Signals from the intent handler for the outer platform layer
//! (shared vocabulary; the kernel re-exports under `jinn_kernel`).
//!
//! The intent handler sets these flags during processing. The platform layer
//! (`TuiApp` or headless runner) reads them after each `handle()` call and
//! performs the corresponding platform-specific action.
//!
//! All flags are cleared at the start of each `handle()` call so they are
//! always fresh.

/// A staged clipboard write: the payload text plus the caller-chosen label
/// the clipboard writer composes its status hints around.
///
/// The caller owns the label — short, lowercase, domain-owned ("entry",
/// "selection", "session id", "3 terminal lines") — because the writer only
/// knows how to compose the two hint phrasings, never which domain it
/// served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardRequest {
    /// The text to place on the system clipboard.
    pub text: String,
    /// The label the writer names in its copied/could-not-copy hints.
    pub label: String,
}

impl ClipboardRequest {
    /// Stages a clipboard write of `text`, to be reported as `label`.
    #[must_use]
    pub fn new<T, L>(text: T, label: L) -> Self
    where
        T: Into<String>,
        L: Into<String>,
    {
        Self {
            text: text.into(),
            label: label.into(),
        }
    }
}

/// Flags set by the intent handler for the outer platform layer to act on.
///
/// These represent requests that cannot be fulfilled by the intent handler
/// itself because they require platform-specific machinery (TUI widgets,
/// external editor, clipboard writer, split manager, etc.).
#[derive(Debug, Clone)]
pub struct TuiSignals {
    /// The which-key popup should be toggled (shown ↔ hidden).
    pub toggle_whichkey: bool,

    /// An external editor should be launched for the chat input.
    pub edit_requested: bool,

    /// A clipboard write to perform (set by yank intents): the payload text
    /// and the label the writer reports it under.
    pub yank_text: Option<ClipboardRequest>,

    /// Request to change CWD via external command. Carries the search root.
    pub change_cwd_requested: Option<crate::cwd_root::CwdRoot>,
}

impl Default for TuiSignals {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiSignals {
    /// Create with all signals cleared.
    #[must_use]
    pub fn new() -> Self {
        Self {
            toggle_whichkey: false,
            edit_requested: false,
            yank_text: None,
            change_cwd_requested: None,
        }
    }

    /// Clear all signals. Called at the start of each `handle()` call.
    pub fn clear(&mut self) {
        self.toggle_whichkey = false;
        self.edit_requested = false;
        self.yank_text = None;
        self.change_cwd_requested = None;
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
    use super::*;

    #[rstest::rstest]
    fn clear_resets_all_signals() {
        // Given a TuiSignals with some flags set.
        let mut signals = TuiSignals::new();
        signals.toggle_whichkey = true;
        signals.edit_requested = true;
        signals.change_cwd_requested = Some(crate::cwd_root::CwdRoot::Session);

        // When clearing.
        signals.clear();

        // Then all flags are false/None.
        assert!(!signals.toggle_whichkey);
        assert!(!signals.edit_requested);
        assert!(signals.change_cwd_requested.is_none());
        // And the clipboard request is cleared too.
        assert!(signals.yank_text.is_none());
    }

    #[rstest::rstest]
    fn clipboard_request_carries_text_and_label() {
        // Given a clipboard request built from text and a label.
        let request = ClipboardRequest::new("payload", "entry");

        // Then it carries both, each under its own name.
        assert_eq!(request.text, "payload");
        assert_eq!(request.label, "entry");
    }
}
