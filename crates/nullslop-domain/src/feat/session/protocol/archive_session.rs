//! Archive a session without running teardown.
//!
//! Sent by the intent handler when the user presses `a` in the sessions
//! sidebar. The session-persistence actor handles the archive: marks the
//! session as archived in SQLite, removes it from the sessions map, and
//! emits [`SessionArchived`] + [`SessionClosed`].
//!
//! [`SessionArchived`]: super::session_archived::SessionArchived
//! [`SessionClosed`]: super::session_closed::SessionClosed

use serde::{Deserialize, Serialize};

use crate::protocol::{CommandMsg, SessionId};

/// Archive a session without running teardown.
#[derive(Debug, Clone, Serialize, Deserialize, CommandMsg)]
#[cmd("session")]
pub struct ArchiveSession {
    /// The session to archive.
    pub session_id: SessionId,
}
