//! Archive session handler.

use crate::sections::sessions::state::sorted_open_sessions;
use jinn_domain::common::app_state::AppState;
use jinn_domain::feat::session::sessions_list::close::validate_session_close;

/// Handles `SidebarSessionArchive` - archives the selected session without teardown.
///
/// Validates that the close can proceed, then emits an `ArchiveSession` command.
/// The actor handles DB archival and memory removal.
///
/// # Panics
/// Panics if `sessions_section.selected_index` is `None`.
pub fn handle_session_archive(state: &mut AppState) -> jinn_domain::protocol::IntentResult {
    use jinn_domain::feat::session::protocol::archive_session::ArchiveSession;

    // Validate - same preconditions as session close.
    if validate_session_close(state).is_err() {
        return jinn_domain::protocol::IntentResult::empty();
    }

    let index = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
        .unwrap();
    let sessions = sorted_open_sessions(state);
    let Some(target) = sessions.get(index) else {
        return jinn_domain::protocol::IntentResult::empty();
    };
    let target_id = target.id.clone();

    // Emit ArchiveSession - the actor handles archival without teardown.
    jinn_domain::protocol::IntentResult::new_message(ArchiveSession {
        session_id: target_id,
    })
}
