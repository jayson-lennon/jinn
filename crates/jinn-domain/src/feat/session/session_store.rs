//! Session store abstraction.
//!
//! Defines [`SessionStore`] as the async trait for session persistence.
//! The SQLite implementation (`SqliteSessionStore`) and the schema
//! migrator live in the `jinn-session-store` slice crate; this module
//! owns the seam (`SessionStore` + [`SessionStoreService`]) that the
//! `Services` container carries.

mod service;

pub use service::SessionStoreService;

use async_trait::async_trait;
use error_stack::Report;
use wherror::Error;

use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::session_summary::SessionSummary;
use crate::feat::session_search::{SearchOutcome, SearchParams, TranscriptWindow};
use crate::protocol::{ChatEntryId, SessionId};

/// Error type for session store operations.
#[derive(Debug, Error)]
#[error(debug)]
pub struct SessionStoreError;

/// Abstraction for session persistence.
///
/// Every external dependency must have a trait abstraction (AGENTS.md §2).
/// SQLite I/O is an external dependency - this trait abstracts it so
/// tests can swap in-memory storage.
///
/// All methods are async. Implementations use `tokio::task::spawn_blocking`
/// to bridge synchronous SQLite calls into the async runtime.
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Returns the storage backend name (for debugging).
    fn name(&self) -> &'static str;

    /// Save a complete session.
    ///
    /// Upserts session metadata, entries, and token ledger in one transaction.
    /// Entries are deduplicated across sessions via the junction table.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the write fails.
    async fn save(&self, session: &ChatSessionState) -> Result<(), Report<SessionStoreError>>;

    /// Load lightweight summaries for all sessions.
    ///
    /// Returns one [`SessionSummary`] per session, suitable for picker display.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>>;

    /// Load a full session by ID.
    ///
    /// Returns `None` if no session with the given ID exists.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ChatSessionState>, Report<SessionStoreError>>;

    /// Delete a session and all its data.
    ///
    /// Removes the session row, its junction rows, and any orphaned entries
    /// (entries no longer referenced by any session). Token ledger rows for
    /// the session are also deleted via `ON DELETE CASCADE`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the delete fails.
    async fn delete(&self, session_id: &SessionId) -> Result<(), Report<SessionStoreError>>;

    /// Fork a session from a specific entry ordinal into a new session.
    ///
    /// Creates a new session with `parent_session` = `source_session_id`.
    /// Copies junction rows from the source session for entries with
    /// ordinal <= `at_ordinal`. Entry data is shared, not duplicated.
    /// The new session gets its own independent token ledger.
    ///
    /// Returns the new session's ID.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the source session doesn't exist or
    /// the fork fails.
    async fn fork(
        &self,
        source_session_id: &SessionId,
        at_ordinal: usize,
    ) -> Result<SessionId, Report<SessionStoreError>>;

    /// Set the `archived` flag for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the update fails.
    async fn set_archived(
        &self,
        session_id: &SessionId,
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>>;

    /// Set the `archived` flag for many sessions in one transaction.
    ///
    /// Used by the sidebar archive-tree action: the whole subtree is written
    /// atomically or not at all. Unknown IDs match no rows and are not an
    /// error (a session that was never persisted, or one already deleted,
    /// needs no writeback).
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the update fails.
    async fn set_archived_many(
        &self,
        session_ids: &[SessionId],
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>>;

    /// Load lightweight summaries for all unarchived sessions.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>>;

    /// Returns the ids of all sessions with pending (dirty) FTS reindex work,
    /// in queue order (oldest marker first).
    ///
    /// Rows whose stored id cannot be parsed as a [`SessionId`] are skipped
    /// with a warning — a corrupt marker must not poison the batch, and it
    /// could never be reindexed anyway.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>>;

    /// Advance one session's chunked FTS reindex by up to `max_entries`.
    ///
    /// The store tracks the rebuild's resume point on the session's dirty
    /// marker: the first chunk of a rebuild (resume point 0) deletes the
    /// session's existing FTS rows, later chunks append. Each call runs one
    /// bounded transaction and persists the advanced resume point, so a huge
    /// session cannot hold the write lock (or the caller) for an unbounded
    /// time, and progress survives restarts. Returns `true` when the session
    /// is fully indexed and its marker cleared — callers repeat across ticks
    /// until then. A failed chunk leaves the marker (and resume point)
    /// untouched, so the next call retries it.
    ///
    /// While a rebuild is partial the index holds a valid prefix of the
    /// session's entries; the dirty marker stays set. If a save lands
    /// mid-rebuild the `sessions` UPDATE trigger re-marks the session (its
    /// resume point stays put) and the next rebuild-from-zero repairs any
    /// staleness. A session deleted since being marked finishes as an empty
    /// rebuild — a no-op, not an error.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if any read or write fails.
    async fn reindex_session_chunk(
        &self,
        session_id: &SessionId,
        max_entries: usize,
    ) -> Result<bool, Report<SessionStoreError>>;

    /// Returns how many sessions currently have pending (dirty) FTS reindex
    /// work.
    ///
    /// Unlike [`SessionStore::dirty_session_ids`], this counts every marker
    /// row — including ones whose stored id cannot be parsed — so the number
    /// reflects the true size of the pending queue.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>>;

    /// Run an FTS query over the index.
    ///
    /// Returns flat bm25-ranked hits plus total and per-session match counts.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the query fails — including FTS5
    /// syntax errors in `params.query`, which surface with the verbatim
    /// SQLite message attached.
    async fn search(
        &self,
        params: SearchParams,
    ) -> Result<SearchOutcome, Report<SessionStoreError>>;

    /// Load a window of entries around an anchor entry.
    ///
    /// `context` entries total are returned, centered on the anchor and
    /// clamped to the session's bounds. Returns `None` if the session does
    /// not exist, or a legible error if the anchor entry is not part of it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    async fn fetch_window(
        &self,
        session_id: &SessionId,
        anchor: &ChatEntryId,
        context: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>>;

    /// Load the last `limit` entries of a session.
    ///
    /// Returns `None` if the session does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    async fn fetch_tail(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>>;

    /// Shut down the store, performing any cleanup or flush operations.
    ///
    /// Called once during application shutdown. Implementations may use
    /// this to delete stale data, fsync, flush WAL, etc. The default
    /// implementation is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the cleanup fails.
    async fn shutdown(&self) -> Result<(), Report<SessionStoreError>> {
        Ok(())
    }
}

impl std::fmt::Debug for dyn SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore")
            .field("name", &self.name())
            .finish()
    }
}
