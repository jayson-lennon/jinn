//! Service wrapper for session storage.
//!
//! Wraps `Arc<dyn SessionStore>` for shared ownership across the application.
//! Follows the service wrapper pattern from the project style guide.

use std::sync::Arc;

use error_stack::Report;

use crate::snapshot::SessionSnapshot;
use jinn_core_types::ChatEntryId;
use jinn_core_types::SessionId;
use jinn_session_store_msg::SessionSummary;
use jinn_session_store_msg::{SearchOutcome, SearchParams, TranscriptWindow};

use crate::session_store::{SessionStore, SessionStoreError};

/// Service wrapper for session storage.
///
/// Wraps `Arc<dyn SessionStore>` for shared ownership across the application.
/// Follows the service wrapper pattern from the project style guide.
#[derive(Debug, Clone)]
pub struct SessionStoreService {
    /// The underlying session store implementation.
    svc: Arc<dyn SessionStore>,
}

impl SessionStoreService {
    /// Creates a new session store service.
    #[must_use]
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self { svc: store }
    }

    /// Save a complete session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the write fails.
    pub async fn save(&self, snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
        self.svc.save(snapshot).await
    }

    /// Archive one or more complete session snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if any archive write fails.
    pub async fn archive_snapshots(
        &self,
        snapshots: &[SessionSnapshot],
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.archive_snapshots(snapshots).await
    }

    /// Returns the highest revision the store has already accepted for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn last_accepted_revision(
        &self,
        session_id: &SessionId,
    ) -> Result<crate::snapshot::SessionRevision, Report<SessionStoreError>> {
        self.svc.last_accepted_revision(session_id).await
    }

    /// Load lightweight summaries for all sessions.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    pub async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        self.svc.load_summaries().await
    }

    /// Load summaries for library members only.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    pub async fn library_members(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        self.svc.library_members().await
    }

    /// Load summaries for every session, loaded and archived alike.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    pub async fn load_archive_rows(
        &self,
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        self.svc.load_archive_rows().await
    }

    /// Load a full session by ID.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
        self.svc.load_session(session_id).await
    }

    /// Delete a session and all its data.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the delete fails.
    pub async fn delete(&self, session_id: &SessionId) -> Result<(), Report<SessionStoreError>> {
        self.svc.delete(session_id).await
    }

    /// Delete many sessions in one atomic operation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the bulk delete fails.
    pub async fn delete_many(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.delete_many(session_ids).await
    }

    /// Return freed pages to the filesystem (incremental auto-vacuum).
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the vacuum command fails.
    pub async fn incremental_vacuum(
        &self,
        max_pages: u32,
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.incremental_vacuum(max_pages).await
    }

    /// Fork a session from a specific entry ordinal.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the source session doesn't exist or
    /// the fork fails.
    pub async fn fork(
        &self,
        source_session_id: &SessionId,
        at_ordinal: usize,
    ) -> Result<SessionId, Report<SessionStoreError>> {
        self.svc.fork(source_session_id, at_ordinal).await
    }

    /// Set the `archived` flag for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the update fails.
    pub async fn set_archived(
        &self,
        session_id: &SessionId,
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.set_archived(session_id, archived).await
    }

    /// Set the `archived` flag for many sessions in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the update fails.
    pub async fn set_archived_many(
        &self,
        session_ids: &[SessionId],
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.set_archived_many(session_ids, archived).await
    }

    /// Set the `in_library` flag for a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the update fails.
    pub async fn set_in_library(
        &self,
        session_id: &SessionId,
        in_library: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        self.svc.set_in_library(session_id, in_library).await
    }

    /// Load lightweight summaries for all unarchived sessions.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the database cannot be read.
    pub async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        self.svc.load_unarchived_summaries().await
    }

    /// Returns the ids of all sessions with pending (dirty) FTS reindex work.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
        self.svc.dirty_session_ids().await
    }

    /// Advance one session's chunked FTS reindex by up to `max_entries`;
    /// returns `true` when the session is fully indexed.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if any read or write fails.
    pub async fn reindex_session_chunk(
        &self,
        session_id: &SessionId,
        max_entries: usize,
    ) -> Result<bool, Report<SessionStoreError>> {
        self.svc
            .reindex_session_chunk(session_id, max_entries)
            .await
    }

    /// Returns how many sessions have pending (dirty) FTS reindex work.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>> {
        self.svc.pending_dirty_count().await
    }

    /// Run an FTS query over the search index.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the query fails, including FTS5
    /// syntax errors surfaced with the verbatim SQLite message.
    pub async fn search(
        &self,
        params: SearchParams,
    ) -> Result<SearchOutcome, Report<SessionStoreError>> {
        self.svc.search(params).await
    }

    /// Load a window of entries around an anchor entry.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn fetch_window(
        &self,
        session_id: &SessionId,
        anchor: &ChatEntryId,
        context: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        self.svc.fetch_window(session_id, anchor, context).await
    }

    /// Load the last `limit` entries of a session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the read fails.
    pub async fn fetch_tail(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        self.svc.fetch_tail(session_id, limit).await
    }

    /// Loads all non-archived judge sessions targeting the given origin.
    ///
    /// Shut down the store, performing any cleanup or flush operations.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] if the cleanup fails.
    pub async fn shutdown(&self) -> Result<(), Report<SessionStoreError>> {
        self.svc.shutdown().await
    }
}
