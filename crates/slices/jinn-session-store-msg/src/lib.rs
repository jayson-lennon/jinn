//! Session-store crossing contracts.
//!
//! The search and transcript-window data model lives here because the
//! `SessionStore` seam in `jinn-kernel` and the SQLite implementation in
//! `jinn-session-store` both use these types. Neither crate can own them
//! without depending on the other, so they live at the shared low level.

pub mod command;
pub mod event;
pub mod library_state;
pub mod projections;
pub mod session_entries;
pub mod session_picker_state;
pub mod session_search;
pub mod session_state;
pub mod session_tree_entry;

pub use command::{
    ArchiveSession, ArchiveSessionTree, LoadLibraryEntries, LoadSessionPickerEntries,
    LoadSessionTree, PersistSession, SessionForkRequested, SessionLoadRequested, SetSessionLibrary,
};
pub use event::{
    SessionLibraryMembershipChanged, SessionLoadCompleted, SessionPickerEntriesLoaded,
    StartupHydrationFinished,
};
pub use library_state::{LibraryState, library_slot};
pub use projections::{FrozenTreeNode, SessionSummary};
pub use session_entries::{session_tree_entries, sort_entries_tree_aware};
pub use session_search::{
    SearchHit, SearchOutcome, SearchParams, SearchableEntry, SearchableRole, TranscriptEntry,
    TranscriptWindow, entry_ts_key, extract_searchable,
};
pub use session_state::SessionState;
pub use session_tree_entry::{SessionTreeEntry, apply_project_column_width, session_row};

pub use session_picker_state::{
    RESULTS_VIEWPORT_FALLBACK as SESSION_PICKER_RESULTS_VIEWPORT_FALLBACK, SessionPickerState,
    session_picker_scope, session_picker_slot,
};
