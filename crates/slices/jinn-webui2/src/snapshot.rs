//! The snapshot: everything a freshly-connected page needs.
//!
//! Built on demand from `State` — the active session, its phase, model
//! label, title and transcript, plus the loaded-session rows the sidebar
//! shows. Nothing is cached; a snapshot is always a read of the current
//! state, so a reconnect can never resume onto stale data.

use crate::revision::RevisionTracker;
use jinn_core_types::SessionId;
use jinn_kernel::{State, StateReadGuard};
use jinn_webui2_msg::{
    EntryEnvelope, PhaseBody, SessionRowBody, SnapshotBody, WEBUI2_PROTOCOL_VERSION,
};

/// The title a session row shows when the session has none.
const UNTITLED: &str = "Untitled Session";

/// Reads the current snapshot out of state, stamped with the session's revision.
#[must_use]
pub fn snapshot(state: &State, revisions: &RevisionTracker) -> SnapshotBody {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    let active = guard.session.get(&active_id);
    let sessions = rows_from(&guard, &active_id);
    let revision = revisions.current(&active_id);

    let Some(active) = active else {
        return SnapshotBody {
            protocol: WEBUI2_PROTOCOL_VERSION,
            session_id: Some(active_id.clone()),
            phase: PhaseBody {
                session_id: active_id,
                phase: jinn_slices::PhaseView::Idle,
            },
            model_label: String::new(),
            title: String::new(),
            revision,
            entries: Vec::new(),
            sessions,
        };
    };

    SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(active_id.clone()),
        phase: PhaseBody {
            session_id: active_id,
            phase: active.phase(),
        },
        model_label: active.model_selection().to_string(),
        title: active.title().unwrap_or(UNTITLED).to_owned(),
        revision,
        entries: active
            .history()
            .iter()
            .cloned()
            .map(EntryEnvelope::new)
            .collect(),
        sessions,
    }
}

/// Builds the sidebar rows, one per loaded session.
fn rows_from(guard: &StateReadGuard<'_>, active_id: &SessionId) -> Vec<SessionRowBody> {
    guard
        .session
        .iter()
        .map(|(id, session)| SessionRowBody {
            session_id: id.clone(),
            title: session.title().unwrap_or(UNTITLED).to_owned(),
            is_active: id == active_id,
            is_busy: session.phase().is_working(),
        })
        .collect()
}

/// Reads the loaded-session rows on their own (for a `Sessions` push).
#[must_use]
pub fn session_rows(state: &State) -> Vec<SessionRowBody> {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    rows_from(&guard, &active_id)
}
