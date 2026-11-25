//! Per-session history revisions — the one piece of bookkeeping resume needs.
//!
//! Nothing in `State` records that a session's history was mutated in
//! place (a settled entry rewritten, a compaction relocating entries), so
//! a cursor alone cannot tell the server whether an incremental resume is
//! safe. This tracker closes that gap: every history-affecting event the
//! outbound actor observes bumps the session's revision, and a page that
//! reconnects names the revision it last saw. Matching cursor and revision
//! mean "only appends happened since" — the one case an incremental resume
//! is correct for. Everything else falls back to a full snapshot.
//!
//! In memory only: a restarted server starts every session at the initial
//! revision, so a page's stale revision never matches and it gets a
//! snapshot — which is the intended behaviour after a restart.

use std::collections::HashMap;
use std::sync::Arc;

use jinn_core_types::SessionId;
use parking_lot::Mutex;

/// The revision a session has before any history event is observed.
pub const INITIAL_REVISION: u64 = 0;

#[derive(Debug, Clone, Default)]
pub struct RevisionTracker {
    revisions: Arc<Mutex<HashMap<SessionId, u64>>>,
}

impl RevisionTracker {
    /// The current revision of a session (`INITIAL_REVISION` if none observed).
    #[must_use]
    pub fn current(&self, session_id: &SessionId) -> u64 {
        self.revisions
            .lock()
            .get(session_id)
            .copied()
            .unwrap_or(INITIAL_REVISION)
    }

    /// Records that a session's history changed and returns the new revision.
    pub fn bump(&self, session_id: &SessionId) -> u64 {
        *self
            .revisions
            .lock()
            .entry(session_id.clone())
            .or_insert(INITIAL_REVISION) += 1;
        self.current(session_id)
    }
}
