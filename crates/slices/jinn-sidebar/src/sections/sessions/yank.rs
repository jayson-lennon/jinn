//! Session-ID yank — copying the highlighted session's UUID to the clipboard.

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::IntentResult;
use jinn_sidebar_msg::SidebarSectionId;
use jinn_sidebar_msg::SidebarSections;

/// Why a session-ID yank can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionYankError {
    /// The focused sidebar section lists no session in its cursor.
    WrongSection,
    /// No session is highlighted.
    NoSelection,
}

/// Validates that a session-ID yank can proceed.
///
/// # Errors
///
/// Returns [`SessionYankError::WrongSection`] if the focused section is not
/// one of the three that keep a session in their cursor (sessions, bots,
/// library), and [`SessionYankError::NoSelection`] if such a section is
/// focused but its cursor names nothing.
pub fn validate_session_yank_id(state: &AppState) -> Result<(), SessionYankError> {
    let Some(focused) = yankable_section(state.frontend.sidebar_section()) else {
        return Err(SessionYankError::WrongSection);
    };
    let has_selection = state
        .frontend
        .with_sections(|s| section_cursor(s, focused).is_some(), || false);
    if !has_selection {
        return Err(SessionYankError::NoSelection);
    }
    Ok(())
}

/// Yanks the highlighted session's ID to the clipboard.
///
/// Three sidebar sections keep a session in their cursor — sessions, bots,
/// and library — each over its own list, so "the highlighted session" is
/// whichever cursor belongs to the focused section (the same resolution the
/// activation uses). A `y` press in any other section, or outside the
/// sidebar, is inert.
///
/// An archived library member yanks from its cursor alone: the cursor names
/// the session's id directly, so nothing here reads the in-memory session and
/// no load is requested — copying an id is not opening it.
///
/// The clipboard write rides [`jinn_slices::TuiSignals::yank_text`], staged
/// under the label `session id`: the TUI layer's clipboard writer drains the
/// signal after dispatch, writes the id, and raises the copied/could-not-copy
/// hint from its write thread — so this slice touches no clipboard backend
/// and writes no clipboard-success claim of its own. The status hint is the
/// yank's only confirmation; there is no banner.
pub fn handle_session_yank_id(state: &mut AppState) -> IntentResult {
    let Some(selected) = yankable_session_id(state) else {
        return IntentResult::empty();
    };
    state.frontend.update_scope(|s| {
        s.signals.yank_text = Some(jinn_slices::ClipboardRequest::new(
            selected.to_string(),
            "session id",
        ));
    });
    IntentResult::empty()
}

/// The focused section, when it is one of the three that keep a session in
/// their cursor.
fn yankable_section(section: Option<SidebarSectionId>) -> Option<SidebarSectionId> {
    match section? {
        section @ (SidebarSectionId::Sessions
        | SidebarSectionId::Bot
        | SidebarSectionId::Library) => Some(section),
        _ => None,
    }
}

/// The session id under the cursor, or `None` outside the sections that list
/// a session to yank.
fn yankable_session_id(state: &AppState) -> Option<jinn_core_types::SessionId> {
    let focused = yankable_section(state.frontend.sidebar_section())?;
    state
        .frontend
        .with_sections(|s| section_cursor(s, focused), || None)
}

/// A section's own cursor, read as a choice between identities: the three
/// sections keep separate cursors over separate lists, so the section — not
/// a row position — says which cursor names "the highlighted session".
fn section_cursor(
    sections: &SidebarSections,
    section: SidebarSectionId,
) -> Option<jinn_core_types::SessionId> {
    match section {
        SidebarSectionId::Sessions => sections.sessions.selected_id.clone(),
        SidebarSectionId::Bot => sections.bot.selected_id.clone(),
        SidebarSectionId::Library => sections.library.selected_id.clone(),
        _ => None,
    }
}
