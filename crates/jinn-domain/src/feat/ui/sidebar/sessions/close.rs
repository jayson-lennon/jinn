//! Session close validation and handlers.

use crate::common::app_state::AppState;
use crate::feat::session::phase_machine::PhaseKind;
use crate::feat::ui::sidebar::sessions::state::sorted_open_sessions;

/// Why a session close can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCloseError {
    /// The sessions section is not focused.
    WrongSection,
    /// No session is selected.
    NoSelection,
    /// The selected entry is not a session.
    NotASession,
    /// The selected session is streaming or sending.
    SessionBusy,
}

/// Validates that a session close can proceed.
///
/// # Errors
///
/// Returns [`SessionCloseError`] if the sessions section is not focused, no session is selected, or the session is busy.
pub fn validate_session_close(state: &AppState) -> Result<(), SessionCloseError> {
    use crate::feat::ui::sidebar::section_trait::SidebarSectionId;

    // Sessions section must be focused.
    if !matches!(
        state.frontend.sidebar_section(),
        Some(SidebarSectionId::Sessions)
    ) {
        return Err(SessionCloseError::WrongSection);
    }

    // A session must be selected.
    let index = state
        .frontend
        .sessions_section
        .selected_index
        .ok_or(SessionCloseError::NoSelection)?;

    // The selected session must be idle (not streaming/sending).
    let sessions = sorted_open_sessions(state);
    let entry = sessions.get(index).ok_or(SessionCloseError::NoSelection)?;

    let session = state
        .session
        .get(&entry.id)
        .ok_or(SessionCloseError::NoSelection)?;
    if session.is_busy() || !matches!(session.phase(), PhaseKind::Idle) {
        return Err(SessionCloseError::SessionBusy);
    }

    Ok(())
}
/// Handles `SidebarSessionClose` - closes the selected session.
///
/// Removes the session from the in-memory HashMap (keeps it in SQLite).
/// Activates the next session in the sorted list, clamping the index.
/// If the last session is closed, creates a new empty session.
///
/// # Panics
///
/// Panics if the selected index is out of bounds (should not happen after validation).
pub fn handle_session_close(state: &mut AppState) -> crate::protocol::IntentResult {
    // Validate.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let Some(closing) = sessions.get(index) else {
        return crate::protocol::IntentResult::empty();
    };
    let closing_id = closing.id.clone();
    drop(sessions);

    // Update visual-parent index before removing the session
    // (need it in memory to resolve its parent chain).
    super::update_visual_parents_on_removal(state, &closing_id);

    // Remove and replace if last session.
    let was_last = state.session.session_count() == 1;
    let mut mcp_enablement = None;
    if was_last {
        // Last session - create a new one with the last-used model.
        let (new_session, enablement) = {
            // Seed per-session defaults from jinn.toml (disablement sets +
            // auto-enabled MCP servers), matching every other creation path.
            let seed = crate::feat::session::profile::SessionSeed::from_preferences(
                &state.frontend.preferences,
            );

            let model = state
                .frontend
                .app_state
                .last_model
                .clone()
                .unwrap_or_default();

            // Seed effort from the global default, mirroring model selection:
            // the new session owns its own copy from creation onward.
            let reasoning_effort = state.frontend.app_state.reasoning_effort;

            let mut profile =
                crate::feat::session::profile::SessionProfile::from_model_selection(model);
            profile.reasoning_effort = reasoning_effort;
            {
                let p = &mut profile;
                p.disabled_tools.clone_from(&seed.disabled_tools);
                p.disabled_skills.clone_from(&seed.disabled_skills);
            }
            let mut new_session =
                crate::feat::session::chat_session::ChatSessionState::new_with_profile(profile);
            new_session.set_enabled_mcp_servers(seed.enabled_mcp.clone());

            let enablement = seed.has_auto_enabled_mcp().then(|| {
                crate::feat::mcp_coordinator_actor::protocol::McpEnablementChanged {
                    session_id: new_session.session_id().clone(),
                    enabled: seed.enabled_mcp,
                }
            });
            (new_session, enablement)
        };
        mcp_enablement = enablement;
        state.session.remove_and_replace(&closing_id, new_session);
    } else {
        state.session.remove(&closing_id);
    }

    super::reconcile_after_session_removal(state);

    let result = crate::protocol::IntentResult::empty();
    // The replacement session may carry config-seeded MCP enablement; attach
    // it so the coordinator spawns the servers for the replacement. Skipped
    // when nothing is auto-enabled (nothing to reconcile).
    match mcp_enablement {
        Some(enablement) => result.with_message(enablement),
        None => result,
    }
}

/// Handles `SidebarSessionClose` - closes the selected session.
///
/// Validates that the close can proceed, gets the selected session ID,
/// then emits a `CloseSession` command. The session actor handles teardown,
/// archival, removal, and emits `SessionClosed` for the sidebar actor to
/// clamp the cursor.
///
/// # Panics
///
/// Panics if `sessions_section.selected_index` is `None`.
pub fn handle_session_close_with_lifecycle(state: &mut AppState) -> crate::protocol::IntentResult {
    use crate::feat::session::protocol::close_session::CloseSession;

    // Validate.
    if validate_session_close(state).is_err() {
        return crate::protocol::IntentResult::empty();
    }

    let index = state.frontend.sessions_section.selected_index.unwrap();
    let sessions = sorted_open_sessions(state);
    let Some(closing) = sessions.get(index) else {
        return crate::protocol::IntentResult::empty();
    };
    let closing_id = closing.id.clone();

    // Emit CloseSession - the actor handles teardown, archive, and removal.
    crate::protocol::IntentResult::new_message(CloseSession {
        session_id: closing_id,
    })
}
