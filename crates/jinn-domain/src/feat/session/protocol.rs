//! Session protocol - session identity and lifecycle types.

pub mod archive_session;
pub mod archive_session_tree;
pub mod close_session;
pub mod history_snapshot_ready;
pub mod load_session_picker_entries;
pub mod mark_session_interacted;
pub mod retry_stalled_session;

/// Re-export: the history contracts live in `jinn-session-history-msg`
/// (the crossing-contract crate); the kernel paths stay stable for consumers.
pub mod history_appended {
    pub use jinn_session_history_msg::HistoryAppended;
}
pub mod citations_received {
    pub use jinn_session_history_msg::CitationsReceived;
}
pub mod submit_history_mutations {
    pub use jinn_session_history_msg::SubmitHistoryMutations;
}
pub mod task_list_updated {
    pub use jinn_session_history_msg::TaskListUpdated;
}

/// Re-export: the archived event lives in `jinn-session-msg` (the
/// crossing-contract crate); the kernel path stays stable for consumers.
pub mod session_archived {
    pub use jinn_session_msg::SessionArchived;
}
pub mod session_closed;
pub mod session_fork_requested;
pub mod session_id;
pub mod session_load_completed;
pub mod session_load_requested;
pub mod session_new;

/// Re-export: the phase-changed event lives in `jinn-session-msg` (the
/// crossing-contract crate); the kernel path stays stable for consumers.
pub mod session_phase_changed {
    pub use jinn_session_msg::SessionPhaseChanged;
}
pub mod teardown_session_tree;
pub mod trigger_compaction;
pub mod user_interacted;

pub use archive_session::ArchiveSession;
pub use archive_session_tree::ArchiveSessionTree;
pub use close_session::CloseSession;
pub use mark_session_interacted::MarkSessionInteracted;
pub use retry_stalled_session::RetryStalledSession;
pub use session_archived::SessionArchived;
pub use session_closed::SessionClosed;
pub use session_phase_changed::SessionPhaseChanged;
pub use teardown_session_tree::TeardownSessionTree;
pub use user_interacted::UserInteracted;
