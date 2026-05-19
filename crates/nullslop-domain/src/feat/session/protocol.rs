//! Session protocol — session identity and lifecycle types.

pub mod archive_session;
pub mod close_session;
pub mod load_session_picker_entries;
pub mod session_archived;
pub mod session_closed;
pub mod session_fork_requested;
pub mod session_id;
pub mod session_load_completed;
pub mod session_load_requested;
pub mod session_new;

pub use archive_session::ArchiveSession;
pub use close_session::CloseSession;
pub use session_archived::SessionArchived;
pub use session_closed::SessionClosed;
