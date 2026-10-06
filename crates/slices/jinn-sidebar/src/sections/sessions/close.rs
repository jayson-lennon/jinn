//! Session-close validation and lifecycle command flow.

use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::IntentResult;
use jinn_session_msg::PhaseKind;

use crate::sections::sessions::state::mark_in_flight;

/// Why a session close can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseError {
    /// The sessions section is not focused.
    WrongSection,
    /// No session is selected, or the one selected is not loaded.
    NoSelection,
    /// The selected session is streaming or sending.
    SessionBusy,
}

/// Validates that a session close can proceed.
///
/// # Errors
///
/// Returns [`SessionCloseError`] if the sessions section is not focused, no session is selected, or the session is busy.
pub fn validate_session_close(state: &AppState) -> Result<(), SessionCloseError> {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    ) {
        return Err(SessionCloseError::WrongSection);
    }
    let id = state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
        .ok_or(SessionCloseError::NoSelection)?;
    let session = state
        .session
        .get(&id)
        .ok_or(SessionCloseError::NoSelection)?;
    if !matches!(session.phase(), PhaseKind::Idle) {
        return Err(SessionCloseError::SessionBusy);
    }
    Ok(())
}

/// Arms the close prompt on the first press and emits `CloseSession` on the
/// second press after re-validating the selected session.
pub fn handle_session_close_arm(state: &mut AppState) -> IntentResult {
    if state.frontend.close_session_prompt {
        state.frontend.close_session_prompt = false;
        return handle_session_close_with_lifecycle(state);
    }
    state.frontend.close_session_prompt = true;
    IntentResult::empty()
}

/// Emits the lifecycle close command for the selected session.
pub fn handle_session_close_with_lifecycle(state: &mut AppState) -> IntentResult {
    use jinn_session_lifecycle_msg::CloseSession;

    if validate_session_close(state).is_err() {
        return IntentResult::empty();
    }
    // Re-read after validating rather than trusting the earlier read: the
    // cursor is a session, and if it went away between the two the close is
    // simply refused. It used to unwrap here on the strength of the
    // validation having just passed.
    let Some(selected) = state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
    else {
        return IntentResult::empty();
    };

    // Mark in flight - the row stays tinted until the teardown and its
    // following archive both conclude.
    mark_in_flight(state, std::slice::from_ref(&selected));

    IntentResult::new_message(CloseSession {
        session_id: selected,
    })
}
