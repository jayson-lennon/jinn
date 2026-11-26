//! The snapshot: everything a freshly-connected page needs.
//!
//! Built on demand from `State` — the active session, its phase, model
//! label, title, and the newest part of its transcript, plus the
//! loaded-session rows the sidebar shows and the library members its
//! Library view shows. Nothing is cached; a snapshot is always a read of
//! the current state, so a reconnect can never resume onto stale data.
//!
//! The transcript is a bounded tail, not the whole history: the newest
//! entries within the window rule in [`crate::page`], with a count of how
//! many precede them. Older entries load a page at a time as the reader
//! scrolls up.

use crate::{page, revision::RevisionTracker};
use jinn_core_types::SessionId;
use jinn_kernel::{State, StateReadGuard};
use jinn_webui2_msg::{PhaseBody, SessionRowBody, SnapshotBody, WEBUI2_PROTOCOL_VERSION};

/// The title a session row shows when the session has none.
const UNTITLED: &str = "Untitled Session";

/// A snapshot together with the window of entries it carried, so the
/// caller records as delivered exactly what the frame holds.
#[derive(Debug, Clone)]
pub struct SnapshotFrame {
    /// The body, as the frame carries it.
    pub body: SnapshotBody,
    /// The transcript window the body's `entries` came from.
    pub window: crate::page::EntryWindow,
}

/// Reads the current snapshot out of state, stamped with the session's revision.
///
/// One read of state: the transcript tail, the session's phase, model and
/// title, and the sidebar rows are all drawn from the same instant, so a
/// frame can never describe two different moments.
#[must_use]
pub fn snapshot(state: &State, revisions: &RevisionTracker) -> SnapshotFrame {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    let (sessions, library) = rows_from(&guard, &active_id);
    let revision = revisions.current(&active_id);
    let theme = persisted_theme(&guard);

    let Some(active) = guard.session.get(&active_id) else {
        return SnapshotFrame {
            window: crate::page::EntryWindow::empty(),
            body: SnapshotBody {
                protocol: WEBUI2_PROTOCOL_VERSION,
                session_id: Some(active_id.clone()),
                phase: PhaseBody {
                    session_id: active_id,
                    phase: jinn_slices::PhaseView::Idle,
                    queue_len: 0,
                },
                model_label: String::new(),
                title: String::new(),
                revision,
                entries: Vec::new(),
                entries_before: 0,
                sessions,
                library,
                theme,
            },
        };
    };

    let window = page::tail(active.history());
    let entries_before = window.entries_before;
    // The body is built first: the window's entries are cloned into it
    // before the window itself moves into the frame.
    let body = SnapshotBody {
        protocol: WEBUI2_PROTOCOL_VERSION,
        session_id: Some(active_id.clone()),
        phase: PhaseBody {
            session_id: active_id,
            phase: active.phase(),
            queue_len: active.queue_len(),
        },
        model_label: active.model_selection().to_string(),
        title: active.title().unwrap_or(UNTITLED).to_owned(),
        revision,
        entries: window.entries.clone(),
        entries_before,
        sessions,
        library,
        theme,
    };
    SnapshotFrame { body, window }
}

/// Builds both sidebar lists from live state: the loaded sessions that are
/// not library members, and the loaded library members.
fn rows_from(
    guard: &StateReadGuard<'_>,
    active_id: &SessionId,
) -> (Vec<SessionRowBody>, Vec<SessionRowBody>) {
    let mut sessions = Vec::new();
    let mut library = Vec::new();
    for (id, session) in guard.session.iter() {
        let row = SessionRowBody {
            session_id: id.clone(),
            title: session.title().unwrap_or(UNTITLED).to_owned(),
            is_active: id == active_id,
            is_busy: session.phase().is_working(),
            in_library: session.in_library(),
        };
        if session.in_library() {
            library.push(row);
        } else {
            sessions.push(row);
        }
    }
    (sessions, library)
}

/// The persisted color mode: `light`, `dark`, or `system`. An unset or
/// unrecognized value reads as `system`, the stylesheet's default.
#[must_use]
pub fn persisted_theme(guard: &StateReadGuard<'_>) -> String {
    match guard.frontend.app_state.web_theme.as_deref() {
        Some(mode @ ("light" | "dark" | "system")) => mode.to_owned(),
        _ => "system".to_owned(),
    }
}

/// Reads the loaded-session rows on their own (for a `Sessions` push).
#[must_use]
pub fn session_rows(state: &State) -> (Vec<SessionRowBody>, Vec<SessionRowBody>) {
    let guard = state.read();
    let active_id = guard.session.active_session_id().clone();
    rows_from(&guard, &active_id)
}
