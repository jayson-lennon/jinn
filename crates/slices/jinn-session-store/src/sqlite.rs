//! SQLite-backed session store implementation.
//!
//! Stores session data in normalized tables with a junction table for entries.
//! This eliminates duplication - each chat entry is stored once and shared
//! across sessions. The junction table enables fork support by copying only
//! small junction rows, not entry data.
//!
//! Backed by the `dao` crate: an async `Pool`/`Transaction` over `rusqlite`.
//! Single statements use `pool.execute`/`pool.query_*`; multi-statement
//! transactional bodies (save, delete, fork) use `pool.with_conn` to drive a
//! native rusqlite `transaction(|tx| …)` on one held connection.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use daow::{Entity, FromRow, Pool, Row, dao};
use error_stack::{Report, ResultExt as _};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use daow::Param;
use jinn_bot_msg::{BotBehavior, BotReport, BotTrigger, default_seed_template};
use jinn_core_types::SessionProfile;
use jinn_core_types::{ChatEntry, ChatEntryKind};
use jinn_core_types::{ChatEntryId, ContextOverride, EntryTiming, SessionId};
use jinn_provider::Attachment;
use jinn_session_lifecycle_msg::LifecycleScriptState;
use jinn_session_msg::SessionOrigin;
use jinn_session_state::fields::{LifecycleArgs, default_prep_mode};
use jinn_session_state::{SessionRevision, SessionSnapshot, SessionSnapshotMetadata};
use jinn_session_store_msg::{
    SearchHit, SearchOutcome, SearchParams, SearchableEntry, TranscriptEntry, TranscriptWindow,
    entry_ts_key, extract_searchable,
};
use jinn_session_store_msg::{SessionState, SessionSummary};
use jinn_token_count_msg::TokenRecord;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

use super::migrator;
use jinn_session_state::{SessionStore, SessionStoreError};

/// Configuration for the SQLite connection pool.
///
/// Controls pool sizing. Use [`PoolConfig::default()`] for sensible defaults
/// or construct with a specific max size.
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Maximum number of connections in the pool.
    max_size: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_size: 4 }
    }
}

impl PoolConfig {
    /// Creates a new configuration with the given max pool size.
    #[must_use]
    pub const fn with_max_size(max_size: usize) -> Self {
        Self { max_size }
    }

    /// Returns the configured max pool size.
    #[must_use]
    pub const fn max_size(&self) -> usize {
        self.max_size
    }
}

/// SQLite-backed implementation of [`SessionStore`].
///
/// Holds a `dao` connection pool (`foreign_keys=ON`, `journal_mode=WAL`,
/// `busy_timeout=5000` applied automatically by the pool builder). Migrations
/// run on the pool before any store method is used.
pub struct SqliteSessionStore {
    pool: Pool,
    save_gates: parking_lot::Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<SessionRevision>>>>,
    archive_gate: tokio::sync::Mutex<()>,
}

impl SqliteSessionStore {
    /// Creates a new store using the platform-default sessions directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the sessions directory cannot be determined, the
    /// pool cannot be built, or migrations fail.
    pub async fn new() -> Result<Self, Report<SessionStoreError>> {
        // `sessions_dir()` already resolves to the canonical DB parent
        // (`~/.local/share/jinn` on Linux). An earlier revision appended an
        // extra `sessions` segment here, which silently split sessions across
        // two databases (`.../jinn/sessions.db` vs `.../jinn/sessions/sessions.db`).
        let dir = jinn_common::app_paths::AppPaths::default().sessions_dir();
        Self::new_with_config(&dir, PoolConfig::default()).await
    }

    /// Creates a new store in the given directory, creating it if needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the pool cannot be
    /// built, or migrations fail.
    pub async fn new_in(dir: &MarkedPathBuf<Absolute>) -> Result<Self, Report<SessionStoreError>> {
        Self::new_with_config(dir, PoolConfig::default()).await
    }

    /// Creates a new store in the given directory with a specific pool size.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created, the pool cannot be
    /// built, or migrations fail.
    ///
    /// # Panics
    ///
    /// Panics if the joined database path is not absolute, which cannot
    /// happen: `sessions.db` is a bare name joined onto a directory already
    /// typed `Absolute`.
    #[expect(
        clippy::expect_used,
        reason = "bare-name joins onto a typed-absolute sessions dir are infallible by construction"
    )]
    pub async fn new_with_config(
        dir: &MarkedPathBuf<Absolute>,
        config: PoolConfig,
    ) -> Result<Self, Report<SessionStoreError>> {
        std::fs::create_dir_all(dir)
            .change_context(SessionStoreError)
            .attach("failed to create sessions directory")?;
        let db_path = dir
            .join("sessions.db")
            .expect("absolute parent joins a bare filename");
        Self::connect_at(&db_path, config).await
    }

    /// Opens or creates a store at an explicit database file path, creating
    /// any missing parent directories.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directories cannot be created, the pool
    /// cannot be built, or migrations fail.
    pub async fn open_or_create(
        file_path: &MarkedPathBuf<Absolute>,
    ) -> Result<Self, Report<SessionStoreError>> {
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)
                .change_context(SessionStoreError)
                .attach("failed to create parent directory for database file")?;
        }
        Self::connect_at(file_path, PoolConfig::default()).await
    }

    /// Builds the pool at `db_path`, then runs migrations on it.
    ///
    /// The `dao` `Pool::builder` applies `foreign_keys=ON`, `journal_mode=WAL`,
    /// and `busy_timeout=5000` to every freshly-opened connection, so the
    /// per-connection pragma customizer is no longer needed.
    async fn connect_at(
        db_path: &MarkedPathBuf<Absolute>,
        config: PoolConfig,
    ) -> Result<Self, Report<SessionStoreError>> {
        let url = db_path.to_string();
        let pool = {
            let mut builder = Pool::builder().path(url).max_size(config.max_size);
            // The sessions store runs pragmas via the pool. Override journal_mode
            // to WAL explicitly so it is recorded even if dao's defaults change.
            builder = builder.pragma("journal_mode", "WAL");
            builder = builder.pragma("foreign_keys", "ON");
            builder = builder.pragma("busy_timeout", "5000");
            builder.build()
        }
        .change_context(SessionStoreError)
        .attach("failed to create connection pool")?;

        migrator::run_migrations(&pool)
            .await
            .change_context(SessionStoreError)
            .attach("failed to run database migrations")?;

        Ok(Self {
            pool,
            save_gates: parking_lot::Mutex::new(HashMap::new()),
            archive_gate: tokio::sync::Mutex::new(()),
        })
    }

    fn save_gate(&self, session_id: &SessionId) -> Arc<tokio::sync::Mutex<SessionRevision>> {
        self.save_gates
            .lock()
            .entry(session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(SessionRevision::default())))
            .clone()
    }

    /// Returns a handle to the underlying connection pool.
    ///
    /// Used by sibling subsystems that share the same `sessions.db` but own
    /// their own tables (e.g. the Discord bot's `discord_thread` mapping).
    #[must_use]
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl std::fmt::Debug for SqliteSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSessionStore")
            .field("backend", &"daow::Pool<sqlite>")
            .finish()
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    async fn save(&self, snapshot: &SessionSnapshot) -> Result<(), Report<SessionStoreError>> {
        if !snapshot.metadata.persist {
            return Ok(());
        }
        let gate = self.save_gate(snapshot.session_id());
        let mut saved_revision = gate.lock().await;
        if snapshot.revision <= *saved_revision {
            tracing::debug!(
                session_id = %snapshot.session_id(),
                snapshot_revision = snapshot.revision.get(),
                saved_revision = saved_revision.get(),
                "skipping stale session snapshot"
            );
            return Ok(());
        }
        let row = NewSessionRow::try_from(snapshot)?;
        save_in_transaction(&self.pool, snapshot, &row)?.await?;
        *saved_revision = snapshot.revision;
        Ok(())
    }

    async fn archive_snapshots(
        &self,
        snapshots: &[SessionSnapshot],
    ) -> Result<(), Report<SessionStoreError>> {
        let _archive = self.archive_gate.lock().await;
        let gates = snapshots
            .iter()
            .map(|snapshot| self.save_gate(snapshot.session_id()))
            .collect::<Vec<_>>();
        let mut revisions = Vec::with_capacity(snapshots.len());
        for gate in gates {
            revisions.push(gate.lock_owned().await);
        }
        if let Some((session_id, snapshot_revision, saved_revision)) = snapshots
            .iter()
            .zip(&revisions)
            .find_map(|(snapshot, saved)| {
                (snapshot.revision.get() <= saved.get()).then_some((
                    snapshot.session_id().clone(),
                    snapshot.revision.get(),
                    saved.get(),
                ))
            })
        {
            return Err(Report::new(SessionStoreError).attach(format!(
                "refusing stale archive snapshot for {session_id}: revision {snapshot_revision} <= saved revision {saved_revision}"
            )));
        }
        archive_snapshots_in_transaction(&self.pool, snapshots).await?;
        for (snapshot, revision) in snapshots.iter().zip(&mut revisions) {
            **revision = snapshot.revision;
        }
        Ok(())
    }

    async fn last_accepted_revision(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionRevision, Report<SessionStoreError>> {
        // Read-only: a session this store has never written keeps no entry, so
        // this must not mint one. Creating it would be harmless today (an
        // unwritten gate holds revision 0, the floor every fresh session
        // already starts above) but would make an unwritten session
        // indistinguishable from one written at revision 0.
        let Some(gate) = self.save_gates.lock().get(session_id).cloned() else {
            return Ok(SessionRevision::new(0));
        };
        Ok(*gate.lock().await)
    }

    async fn load_summaries(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        let dao = SessionDao::new(self.pool.clone());
        let rows: Vec<SessionRow> = dao
            .all_sessions()
            .await
            .change_context(SessionStoreError)
            .attach("failed to query summaries")?;
        rows.into_iter().map(summary_from_row).collect()
    }

    async fn library_members(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        let dao = SessionDao::new(self.pool.clone());
        let rows: Vec<LibraryMemberRow> = dao
            .library_member_rows()
            .await
            .change_context(SessionStoreError)
            .attach("failed to query library members")?;
        rows.into_iter().map(library_summary_from_row).collect()
    }

    async fn load_archive_rows(&self) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        let dao = SessionDao::new(self.pool.clone());
        let rows: Vec<ArchiveRow> = dao
            .archive_rows()
            .await
            .change_context(SessionStoreError)
            .attach("failed to query archive rows")?;
        rows.into_iter().map(archive_summary_from_row).collect()
    }

    async fn load_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionSnapshot>, Report<SessionStoreError>> {
        let session_id = session_id.clone();
        let load_context = self
            .pool
            .with_conn(move |conn| load_context_in_transaction(conn, &session_id))
            .await
            .change_context(SessionStoreError)
            .attach("failed to load coherent session snapshot")?;
        load_context.map(SessionSnapshot::try_from).transpose()
    }

    async fn delete(&self, session_id: &SessionId) -> Result<(), Report<SessionStoreError>> {
        let session_id_str = session_id.to_string();
        self.pool
            .with_conn(move |conn| delete_with_scoped_reaping(conn, &session_id_str))
            .await
            .change_context(SessionStoreError)
            .attach("failed to delete session")?;
        Ok(())
    }

    async fn delete_many(
        &self,
        session_ids: &[SessionId],
    ) -> Result<(), Report<SessionStoreError>> {
        let id_strs: Vec<String> = session_ids.iter().map(ToString::to_string).collect();
        self.pool
            .with_conn(move |conn| delete_many_in_transaction(conn, &id_strs))
            .await
            .change_context(SessionStoreError)
            .attach("failed to delete sessions")?;
        Ok(())
    }

    async fn incremental_vacuum(&self, max_pages: u32) -> Result<(), Report<SessionStoreError>> {
        // PRAGMA arguments cannot be bound parameters; the count is a u32
        // formatted into the statement.
        let statement = format!("PRAGMA incremental_vacuum({max_pages})");
        let trimmed: Option<i64> = self
            .pool
            .query_one(&statement, vec![])
            .await
            .change_context(SessionStoreError)
            .attach("failed to run incremental_vacuum")?;
        tracing::debug!(?trimmed, max_pages, "incremental vacuum pass complete");
        Ok(())
    }

    async fn fork(
        &self,
        source_session_id: &SessionId,
        at_ordinal: usize,
    ) -> Result<SessionId, Report<SessionStoreError>> {
        let Some(source) = self.load_session(source_session_id).await? else {
            return Err(Report::new(SessionStoreError).attach("source session not found for fork"));
        };
        let child = source.forked_from(SessionId::new(), at_ordinal);
        let child_id = child.session_id().clone();
        self.save(&child).await?;
        Ok(child_id)
    }

    async fn set_archived(
        &self,
        session_id: &SessionId,
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        let session_id_str = session_id.to_string();
        let dao = SessionDao::new(self.pool.clone());
        dao.set_archived(archived, session_id_str)
            .await
            .change_context(SessionStoreError)
            .attach("failed to set archived flag")?;
        Ok(())
    }

    async fn set_archived_many(
        &self,
        session_ids: &[SessionId],
        archived: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        let id_strs: Vec<String> = session_ids.iter().map(ToString::to_string).collect();
        self.pool
            .with_conn(move |conn| set_archived_many_in_transaction(conn, &id_strs, archived))
            .await
            .change_context(SessionStoreError)
            .attach("failed to set archived flag for session tree")?;
        Ok(())
    }

    async fn set_in_library(
        &self,
        session_id: &SessionId,
        in_library: bool,
    ) -> Result<(), Report<SessionStoreError>> {
        let session_id_str = session_id.to_string();
        let dao = SessionDao::new(self.pool.clone());
        dao.set_in_library(in_library, session_id_str)
            .await
            .change_context(SessionStoreError)
            .attach("failed to set in_library flag")?;
        Ok(())
    }

    async fn load_unarchived_summaries(
        &self,
    ) -> Result<Vec<SessionSummary>, Report<SessionStoreError>> {
        let dao = SessionDao::new(self.pool.clone());
        let rows: Vec<SessionRow> = dao
            .unarchived_sessions()
            .await
            .change_context(SessionStoreError)
            .attach("failed to query unarchived summaries")?;
        rows.into_iter().map(summary_from_row).collect()
    }

    async fn dirty_session_ids(&self) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
        dirty_session_ids(&self.pool).await
    }

    async fn reindex_session_chunk(
        &self,
        session_id: &SessionId,
        max_entries: usize,
    ) -> Result<bool, Report<SessionStoreError>> {
        reindex_session_chunk(&self.pool, session_id, max_entries).await
    }

    async fn pending_dirty_count(&self) -> Result<usize, Report<SessionStoreError>> {
        pending_dirty_count(&self.pool).await
    }

    async fn search(
        &self,
        params: SearchParams,
    ) -> Result<SearchOutcome, Report<SessionStoreError>> {
        search_index(&self.pool, params).await
    }

    async fn fetch_window(
        &self,
        session_id: &SessionId,
        anchor: &ChatEntryId,
        context: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        fetch_window(&self.pool, session_id, anchor, context).await
    }

    async fn fetch_tail(
        &self,
        session_id: &SessionId,
        limit: usize,
    ) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
        fetch_tail(&self.pool, session_id, limit).await
    }

    async fn shutdown(&self) -> Result<(), Report<SessionStoreError>> {
        let result: Option<CheckpointResult> = self
            .pool
            .query_one("PRAGMA wal_checkpoint(TRUNCATE)", vec![])
            .await
            .change_context(SessionStoreError)
            .attach("failed to run wal_checkpoint(TRUNCATE) during shutdown")?;
        if let Some(result) = result {
            classify_checkpoint_result(&result);
        }
        Ok(())
    }
}

// ── Row models ───────────────────────────────────────────────────────────

/// Reading model for the `sessions` table (post-v20: 9 authoritative columns).
///
/// All columns are now authoritative — the six "zombie" columns
/// (`profile`, `blobs`, `cwd`, `lifecycle_name`, `lifecycle_args`,
/// `lifecycle_script_state`) were dropped by migration v20 after the metadata
/// blob was backfilled for every row. The metadata JSON blob is the single
/// source of truth for session core fields.
#[derive(Debug, Clone, Entity)]
#[dao(table = "sessions")]
struct SessionRow {
    #[dao(pk)]
    id: String,
    title: Option<String>,
    updated_at: String,
    created_at: String,
    parent_session: Option<String>,
    archived: bool,
    in_library: bool,
    metadata: Option<String>,
    persist: bool,
}

/// Reading model for the library-members query: the scalar columns only,
/// no metadata blob.
///
/// The blob is by far the heaviest column (task list, bot reports, working
/// intervals), and the Library row renders none of it — id, title,
/// timestamps, and state. Reading it here would re-pay a full deserialize
/// per row on every membership write.
#[derive(Debug, Clone, Entity)]
#[dao(table = "sessions")]
struct LibraryMemberRow {
    #[dao(pk)]
    id: String,
    title: Option<String>,
    updated_at: String,
    created_at: String,
    parent_session: Option<String>,
    archived: bool,
    in_library: bool,
    is_bot: bool,
}

/// Reading model for the archive picker's whole-store query: the scalar
/// columns plus the one blob field its rows render.
///
/// [`SessionRow`] carries the full metadata blob, which is by far the
/// heaviest column (task list, bot reports, working intervals); over a
/// store of thousands that dominates every picker open. The picker shows
/// only `project` from the blob, so SQLite extracts that one field and the
/// blob itself never leaves the database.
#[derive(Debug, Clone, Entity)]
#[dao(table = "sessions")]
struct ArchiveRow {
    #[dao(pk)]
    id: String,
    title: Option<String>,
    updated_at: String,
    created_at: String,
    parent_session: Option<String>,
    archived: bool,
    in_library: bool,
    is_bot: bool,
    project: Option<String>,
}

/// Insert model for the `sessions` table. Built from a [`SessionSnapshot`] then
/// upserted via hand-written SQL.
struct NewSessionRow {
    id: String,
    title: Option<String>,
    updated_at: String,
    created_at: String,
    parent_session: Option<String>,
    archived: bool,
    in_library: bool,
    is_bot: bool,
    metadata: Option<String>,
}

/// A joined `entries` + `session_history` row for loading a session's entries.
///
/// Read by a manual `FromRow` that maps the aliased columns of the JOIN query.
struct JoinedEntry {
    entry_id: String,
    timing: String,
    kind: String,
    context_history: String,
    token_count: Option<i64>,
    model_used: Option<String>,
    model_requested: Option<String>,
    pin_position: Option<String>,
    ignored: bool,
    context_override: String,
}

impl FromRow for JoinedEntry {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            entry_id: row.get("entry_id")?,
            timing: row.get("timing")?,
            kind: row.get("kind")?,
            context_history: row.get("context_history")?,
            token_count: row.get("token_count")?,
            model_used: row.get("model_used")?,
            model_requested: row.get("model_requested")?,
            pin_position: row.get("pin_position")?,
            ignored: row.get("ignored")?,
            context_override: row.get("context_override")?,
        })
    }
}

/// Reading model for the `token_ledger` table.
#[derive(Debug, Clone, Entity)]
#[dao(table = "token_ledger")]
struct TokenLedgerRow {
    #[dao(pk)]
    id: i64,
    session_id: String,
    timestamp: String,
    tokens_sent: i32,
    tokens_received: i32,
    cost: Option<f64>,
    model_used: Option<String>,
    prompt_tokens: Option<i32>,
    cached_tokens: Option<i32>,
}

// ── Typed DAO traits (compile-time SQL validation via DAOW_DATABASE_URL) ───

/// Session-level queries that run directly on the pool. These use `#[query]` /
/// `#[execute]` so the `dao` macro validates the SQL against the post-v20 schema
/// at compile time (see `jinn-session-schema` + `build.rs`). Transactional multi-statement
/// bodies (`save`, `delete`, `fork`) still use `pool.with_conn` with raw rusqlite
/// because they need dynamic `IN (?, ?, …)` placeholder strings that cannot be
/// statically validated.
#[dao]
#[async_trait]
trait SessionDao {
    #[query(
        "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, metadata, persist FROM sessions"
    )]
    async fn all_sessions(&self) -> daow::Result<Vec<SessionRow>>;

    #[query(
        "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, metadata, persist FROM sessions WHERE id = ?"
    )]
    async fn session_by_id(&self, id: String) -> daow::Result<Option<SessionRow>>;

    #[query(
        "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, metadata, persist FROM sessions WHERE archived = FALSE"
    )]
    async fn unarchived_sessions(&self) -> daow::Result<Vec<SessionRow>>;

    #[query(
        "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, is_bot \
         FROM sessions WHERE in_library = TRUE"
    )]
    async fn library_member_rows(&self) -> daow::Result<Vec<LibraryMemberRow>>;

    /// Every session's scalar columns plus the one blob field the archive
    /// picker renders — `project`, pulled out by SQLite so the blob itself
    /// never leaves the database.
    ///
    /// The blob stores a project as `{"path": …, "marker": "absolute"}`, so
    /// the path is the nested `$.project.path`; extracting `$.project` would
    /// return the whole object as text and parse as no path.
    #[query(
        "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, is_bot, \
         json_extract(metadata, '$.project.path') AS project FROM sessions"
    )]
    async fn archive_rows(&self) -> daow::Result<Vec<ArchiveRow>>;

    #[execute("UPDATE sessions SET archived = ? WHERE id = ?")]
    async fn set_archived(&self, archived: bool, id: String) -> daow::Result<daow::ExecuteResult>;

    #[execute("UPDATE sessions SET in_library = ? WHERE id = ?")]
    async fn set_in_library(
        &self,
        in_library: bool,
        id: String,
    ) -> daow::Result<daow::ExecuteResult>;
}

// ── Conversions ──────────────────────────────────────────────────────────

// ── PersistableCore - JSON blob for session metadata ─────────────────────

fn default_persist() -> bool {
    true
}

/// A subset of [`SessionCore`] fields suitable for JSON blob persistence.
///
/// Excludes `history`, `token_ledger`, and `ephemeral` which are stored in
/// normalized tables or are runtime-only. This blob acts as a snapshot that
/// can be deserialized back into a full `SessionCore` with defaults for the
/// excluded fields.
#[derive(Serialize, Deserialize)]
pub(crate) struct PersistableCore {
    session_id: SessionId,
    title: Option<String>,
    updated_at: jiff::Timestamp,
    created_at: jiff::Timestamp,
    profile: SessionProfile,
    #[serde(
        default = "jinn_session_state::fields::default_cwd",
        deserialize_with = "jinn_session_state::fields::deserialize_cwd_resolving_legacy_dot"
    )]
    cwd: MarkedPathBuf<Absolute>,
    parent_session: Option<SessionId>,
    /// Highest entry ordinal inherited from parent at fork time.
    /// `None` for root sessions.
    #[serde(default)]
    fork_ordinal: Option<usize>,
    /// Identity of this session's creation path.
    /// Defaults to [`SessionOrigin::User`] for blobs written by older versions.
    #[serde(default)]
    origin: SessionOrigin,
    /// Project directory association, stamped at session creation from the
    /// projects UI. Defaults to `None` for blobs written by older versions.
    #[serde(default)]
    project: Option<MarkedPathBuf<Absolute>>,

    blobs: HashMap<String, JsonValue>,
    lifecycle_name: Option<String>,
    lifecycle_args: LifecycleArgs,
    /// The workflow-local lifecycle file the lifecycle resolved from.
    /// Absent for jinn.toml lifecycles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lifecycle_source: Option<String>,
    lifecycle_script_state: LifecycleScriptState,
    /// Phased task list for agent session planning.
    /// OWNER: tools-actor (mutated by task list tools).
    #[serde(default)]
    task_list: jinn_tools_msg::TaskList,
    /// Names of MCP servers enabled for this session.
    /// Persisted in the metadata blob; off by default.
    #[serde(default)]
    enabled_mcp_servers: std::collections::BTreeSet<String>,
    /// Whether this session should be persisted to disk.
    /// Defaults to true for blobs written by older versions.
    #[serde(default = "default_persist")]
    persist: bool,
    /// What a run in a bot sees of the conversation.
    #[serde(default)]
    behavior: BotBehavior,
    /// Whether the bot is still being composed.
    /// Defaults to composing for blobs written before prep mode existed.
    #[serde(default = "default_prep_mode")]
    prep_mode: bool,
    /// The condition that causes an automatic bot re-run.
    #[serde(default)]
    trigger: BotTrigger,
    /// User-editable seed text used to inject the prior report.
    #[serde(default = "default_seed_template")]
    seed_template: String,
    /// The bot's append-only report log.
    #[serde(default)]
    reports: Vec<BotReport>,
    /// The workflow-local TOML file a workflow bot loaded from.
    /// Absent for personal bots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow_source: Option<jinn_core_types::BotSourcePath>,
    /// Wall-clock intervals during which this session was working.
    /// Absent in blobs written before working time was recorded.
    ///
    /// Omitted when empty, so a session that never worked persists a blob
    /// byte-identical to one written before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    working_intervals: Vec<jinn_core_types::WorkingInterval>,
}

impl From<&SessionSnapshotMetadata> for PersistableCore {
    fn from(metadata: &SessionSnapshotMetadata) -> Self {
        Self {
            session_id: metadata.session_id.clone(),
            title: metadata.title.clone(),
            updated_at: metadata.updated_at,
            created_at: metadata.created_at,
            profile: metadata.profile.clone(),
            cwd: metadata.cwd.clone(),
            parent_session: metadata.parent_session.clone(),
            fork_ordinal: metadata.fork_ordinal,
            origin: metadata.origin,
            project: metadata.project.clone(),
            blobs: metadata.blobs.clone(),
            lifecycle_name: metadata.lifecycle_name.clone(),
            lifecycle_args: metadata.lifecycle_args.clone(),
            lifecycle_source: metadata.lifecycle_source.clone(),
            lifecycle_script_state: metadata.lifecycle_script_state,
            task_list: metadata.task_list.clone(),
            enabled_mcp_servers: metadata.enabled_mcp_servers.clone(),
            persist: metadata.persist,
            behavior: metadata.behavior,
            prep_mode: metadata.prep_mode,
            trigger: metadata.trigger,
            seed_template: metadata.seed_template.clone(),
            reports: metadata.reports.clone(),
            workflow_source: metadata.workflow_source.clone(),
            working_intervals: metadata.working_intervals.clone(),
        }
    }
}

impl From<PersistableCore> for SessionSnapshotMetadata {
    fn from(core: PersistableCore) -> Self {
        Self {
            session_id: core.session_id,
            title: core.title,
            updated_at: core.updated_at,
            created_at: core.created_at,
            profile: core.profile,
            cwd: core.cwd,
            parent_session: core.parent_session,
            fork_ordinal: core.fork_ordinal,
            origin: core.origin,
            project: core.project,
            blobs: core.blobs,
            lifecycle_name: core.lifecycle_name,
            lifecycle_args: core.lifecycle_args,
            lifecycle_source: core.lifecycle_source,
            lifecycle_script_state: core.lifecycle_script_state,
            task_list: core.task_list,
            enabled_mcp_servers: core.enabled_mcp_servers,
            persist: core.persist,
            behavior: core.behavior,
            prep_mode: core.prep_mode,
            trigger: core.trigger,
            seed_template: core.seed_template,
            reports: core.reports,
            workflow_source: core.workflow_source,
            working_intervals: core.working_intervals,
            session_state: SessionState::Loaded,
            in_library: false,
        }
    }
}

impl TryFrom<&SessionSnapshot> for NewSessionRow {
    type Error = Report<SessionStoreError>;

    fn try_from(snapshot: &SessionSnapshot) -> Result<Self, Self::Error> {
        let metadata = &snapshot.metadata;
        Ok(Self {
            id: metadata.session_id.to_string(),
            title: metadata.title.clone(),
            updated_at: metadata.updated_at.to_string(),
            created_at: metadata.created_at.to_string(),
            parent_session: metadata
                .parent_session
                .as_ref()
                .map(std::string::ToString::to_string),
            archived: metadata.session_state == SessionState::Archived,
            in_library: metadata.in_library,
            is_bot: metadata.origin == jinn_session_msg::SessionOrigin::Bot,
            metadata: Some(
                serde_json::to_string(&PersistableCore::from(metadata))
                    .change_context(SessionStoreError)
                    .attach("failed to serialize metadata")?,
            ),
        })
    }
}

/// Carries all durable data loaded for one session from the database.
struct SessionLoadContext {
    row: SessionRow,
    entries: Vec<ChatEntry>,
    ledger: Vec<TokenRecord>,
}

impl TryFrom<SessionLoadContext> for SessionSnapshot {
    type Error = Report<SessionStoreError>;

    fn try_from(ctx: SessionLoadContext) -> Result<Self, Self::Error> {
        let SessionRow {
            archived,
            in_library,
            metadata,
            persist: _persist,
            ..
        } = ctx.row;

        let metadata_json = metadata.ok_or_else(|| {
            Report::new(SessionStoreError)
                .attach("session row has NULL metadata after v20 (data corruption)")
        })?;
        let persistable: PersistableCore = serde_json::from_str(&metadata_json)
            .change_context(SessionStoreError)
            .attach("failed to deserialize session metadata blob")?;
        let mut metadata: SessionSnapshotMetadata = persistable.into();
        metadata.session_state = if archived {
            SessionState::Archived
        } else {
            SessionState::Loaded
        };
        metadata.in_library = in_library;

        Ok(Self {
            revision: SessionRevision::new(0),
            metadata,
            entries: ctx.entries,
            token_ledger: ctx.ledger,
        })
    }
}

// ── Transactions ─────────────────────────────────────────────────────────

/// Reads all durable session data under one SQLite read transaction.
///
/// The returned context is converted into a `SessionSnapshot` only after the
/// read transaction commits, so no partially assembled live state is visible.
#[expect(
    clippy::too_many_lines,
    reason = "one read transaction is the coherence boundary for metadata, entries, attachments, and ledger"
)]
fn load_context_in_transaction(
    conn: &mut rusqlite::Connection,
    session_id: &SessionId,
) -> daow::Result<Option<SessionLoadContext>> {
    let tx = conn.transaction()?;
    let row: Option<SessionRow> = tx
        .query_row(
            "SELECT id, title, updated_at, created_at, parent_session, archived, in_library, metadata, persist \
             FROM sessions WHERE id = ?",
            rusqlite::params![session_id.to_string()],
            |row| {
                Ok(SessionRow {
                    id: row.get("id")?,
                    title: row.get("title")?,
                    updated_at: row.get("updated_at")?,
                    created_at: row.get("created_at")?,
                    parent_session: row.get("parent_session")?,
                    archived: row.get("archived")?,
                    in_library: row.get("in_library")?,
                    metadata: row.get("metadata")?,
                    persist: row.get("persist")?,
                })
            },
        )
        .optional()?;

    let Some(row) = row else {
        tx.commit()?;
        return Ok(None);
    };

    let joined: Vec<JoinedEntry> = {
        let mut stmt = tx.prepare(
            "SELECT entries.id AS entry_id, entries.timing AS timing, entries.kind AS kind, \
             entries.context_history AS context_history, \
             session_history.pin_position AS pin_position, \
             session_history.ignored AS ignored, \
             session_history.context_override AS context_override, \
             entries.token_count AS token_count, \
             entries.model_used AS model_used, \
             entries.model_requested AS model_requested \
             FROM entries \
             INNER JOIN session_history ON entries.id = session_history.entry_id \
             WHERE session_history.session_id = ? \
             ORDER BY session_history.ordinal ASC",
        )?;
        let session_id = session_id.to_string();
        stmt.query_map([session_id], |row| {
            Ok(JoinedEntry {
                entry_id: row.get("entry_id")?,
                timing: row.get("timing")?,
                kind: row.get("kind")?,
                context_history: row.get("context_history")?,
                token_count: row.get("token_count")?,
                model_used: row.get("model_used")?,
                model_requested: row.get("model_requested")?,
                pin_position: row.get("pin_position")?,
                ignored: row.get("ignored")?,
                context_override: row.get("context_override")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let blob_rows: Vec<EntryBlobRow> = {
        let mut stmt = tx.prepare(
            "SELECT entry_blobs.entry_id AS entry_id, entry_blobs.ordinal AS ordinal, \
             entry_blobs.media_type AS media_type, entry_blobs.data AS data \
             FROM entry_blobs \
             INNER JOIN session_history ON entry_blobs.entry_id = session_history.entry_id \
             WHERE session_history.session_id = ? \
             ORDER BY session_history.ordinal ASC, entry_blobs.ordinal ASC",
        )?;
        stmt.query_map([session_id.to_string()], |row| {
            Ok(EntryBlobRow {
                entry_id: row.get("entry_id")?,
                ordinal: row.get("ordinal")?,
                media_type: row.get("media_type")?,
                data: row.get("data")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let ledger_rows: Vec<TokenLedgerRow> = {
        let mut stmt = tx.prepare(
            "SELECT id, session_id, timestamp, tokens_sent, tokens_received, cost, model_used, \
             prompt_tokens, cached_tokens FROM token_ledger WHERE session_id = ?",
        )?;
        stmt.query_map([session_id.to_string()], |row| {
            Ok(TokenLedgerRow {
                id: row.get("id")?,
                session_id: row.get("session_id")?,
                timestamp: row.get("timestamp")?,
                tokens_sent: row.get("tokens_sent")?,
                tokens_received: row.get("tokens_received")?,
                cost: row.get("cost")?,
                model_used: row.get("model_used")?,
                prompt_tokens: row.get("prompt_tokens")?,
                cached_tokens: row.get("cached_tokens")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    tx.commit()?;

    let blobs_by_entry = group_blobs_by_entry(blob_rows);
    let entries = joined
        .into_iter()
        .map(|joined| {
            let attachments = blobs_by_entry
                .get(&joined.entry_id)
                .cloned()
                .unwrap_or_default();
            entry_from_joined(&joined, attachments)
                .map_err(|report| daow::Error::Custom(report.to_string()))
        })
        .collect::<daow::Result<Vec<_>>>()?;
    let ledger = ledger_rows.into_iter().map(record_from_row).collect();

    Ok(Some(SessionLoadContext {
        row,
        entries,
        ledger,
    }))
}

/// Serialized data for one complete snapshot write inside a transaction.
///
/// `session_id` carries the typed id beside its SQL string form in `row`, so
/// the insert helpers take `&SessionId` without re-parsing `row.id` per row.
struct SnapshotWrite {
    row: NewSessionRow,
    session_id: SessionId,
    entries: Vec<PersistableEntry>,
    ledger: Vec<PersistableTokenRecord>,
}

impl TryFrom<&SessionSnapshot> for SnapshotWrite {
    type Error = Report<SessionStoreError>;

    fn try_from(snapshot: &SessionSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            row: NewSessionRow::try_from(snapshot)?,
            session_id: snapshot.metadata.session_id.clone(),
            entries: persistable_entries(&snapshot.entries)?,
            ledger: persistable_ledger(&snapshot.token_ledger),
        })
    }
}

/// Saves a complete session in a single transaction.
///
/// Upserts session metadata, replaces all junction rows and token ledger rows,
/// and inserts any new entries. Orphaned-entry reaping is intentionally not done
/// here — it belongs in `delete`/`fork`, where the removing session is known. A
/// global cleanup in the save hot-path could wipe every entry if
/// `session_history` is transiently empty (e.g. mid-migration).
fn save_in_transaction(
    pool: &Pool,
    snapshot: &SessionSnapshot,
    row: &NewSessionRow,
) -> Result<
    impl std::future::Future<Output = Result<(), Report<SessionStoreError>>> + Send + 'static,
    Report<SessionStoreError>,
> {
    let write = SnapshotWrite {
        row: clone_row(row),
        session_id: snapshot.metadata.session_id.clone(),
        entries: persistable_entries(&snapshot.entries)?,
        ledger: persistable_ledger(&snapshot.token_ledger),
    };
    let pool = pool.clone();

    Ok(async move {
        pool.with_conn(move |conn| -> daow::Result<()> {
            let tx = conn.transaction()?;
            write_snapshot(&tx, &write)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .change_context(SessionStoreError)
        .attach("failed to save session")?;
        Ok(())
    })
}

fn clone_row(row: &NewSessionRow) -> NewSessionRow {
    NewSessionRow {
        id: row.id.clone(),
        title: row.title.clone(),
        updated_at: row.updated_at.clone(),
        created_at: row.created_at.clone(),
        parent_session: row.parent_session.clone(),
        archived: row.archived,
        in_library: row.in_library,
        is_bot: row.is_bot,
        metadata: row.metadata.clone(),
    }
}

/// Writes one complete serialized snapshot into the current transaction.
fn write_snapshot(tx: &rusqlite::Transaction<'_>, write: &SnapshotWrite) -> rusqlite::Result<()> {
    upsert_session_row(
        tx,
        &write.row.id,
        &write.row.title,
        &write.row.updated_at,
        &write.row.created_at,
        &write.row.parent_session,
        write.row.archived,
        write.row.in_library,
        write.row.is_bot,
        &write.row.metadata,
    )?;
    tx.execute(
        "DELETE FROM session_history WHERE session_id = ?",
        rusqlite::params![&write.row.id],
    )?;
    tx.execute(
        "DELETE FROM token_ledger WHERE session_id = ?",
        rusqlite::params![&write.row.id],
    )?;
    for entry in &write.entries {
        insert_entry_and_junction(tx, &write.session_id, entry)?;
    }
    for record in &write.ledger {
        insert_token_ledger_row(tx, &write.session_id, record)?;
    }
    Ok(())
}

/// Archives all supplied snapshots in one transaction.
async fn archive_snapshots_in_transaction(
    pool: &Pool,
    snapshots: &[SessionSnapshot],
) -> Result<(), Report<SessionStoreError>> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let writes = snapshots
        .iter()
        .map(SnapshotWrite::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let pool = pool.clone();
    pool.with_conn(move |conn| -> daow::Result<()> {
        let tx = conn.transaction()?;
        for write in &writes {
            write_snapshot(&tx, write)?;
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .change_context(SessionStoreError)
    .attach("failed to archive session snapshots")
}

/// Builds the list of persistable entries (skipping transient UI hints).
fn persistable_entries(
    entries: &[ChatEntry],
) -> Result<Vec<PersistableEntry>, Report<SessionStoreError>> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.kind, ChatEntryKind::Transient(_)))
        .map(|(ordinal, entry)| PersistableEntry::build(entry, ordinal))
        .collect()
}

/// Extracts an entry's attachments, leaving the kind shape otherwise intact.
///
/// User entries (from `@path` sends) and tool-result entries (from tools like
/// `read_image`) can both carry attachments. Entries with none return an empty
/// vec. The attachments are persisted separately in `entry_blobs` so the
/// `kind` JSON column stays lean — see [`serialize_lean_kind`].
pub(super) fn extract_attachments(kind: &ChatEntryKind) -> Vec<Attachment> {
    // User entries (from `@path` sends) and tool-result entries (from tools
    // like `read_image`) are the attachment carriers; every other kind has
    // none, whatever shape it takes.
    match kind {
        ChatEntryKind::User { attachments, .. } | ChatEntryKind::ToolResult { attachments, .. } => {
            attachments.clone()
        }
        _ => Vec::new(),
    }
}

/// Serializes a kind to JSON with any attachments stripped out.
///
/// The persisted `kind` blob must not carry raw image bytes (base64), which
/// would bloat the `entries.kind` column on every context re-read. Attachments
/// (user-entry and tool-result alike) live in the `entry_blobs` table and are
/// rehydrated on load by [`hydrate_attachments`].
pub(super) fn serialize_lean_kind(kind: &ChatEntryKind) -> String {
    match kind {
        ChatEntryKind::User {
            display,
            expanded,
            attachments,
            outcome,
        } if !attachments.is_empty() => {
            let lean = ChatEntryKind::User {
                display: display.clone(),
                expanded: expanded.clone(),
                attachments: Vec::new(),
                outcome: outcome.clone(),
            };
            serde_json::to_string(&lean).unwrap_or_else(|_| "{}".to_owned())
        }
        ChatEntryKind::ToolResult {
            id,
            name,
            content,
            status,
            full_content,
            truncation,
            pin_position,
            is_alert,
            attachments,
        } if !attachments.is_empty() => {
            let lean = ChatEntryKind::ToolResult {
                id: id.clone(),
                name: name.clone(),
                content: content.clone(),
                status: *status,
                full_content: full_content.clone(),
                truncation: truncation.clone(),
                pin_position: *pin_position,
                is_alert: *is_alert,
                attachments: Vec::new(),
            };
            serde_json::to_string(&lean).unwrap_or_else(|_| "{}".to_owned())
        }
        _ => serde_json::to_string(kind).unwrap_or_else(|_| "{}".to_owned()),
    }
}

/// Builds the list of persistable token ledger records.
fn persistable_ledger(records: &[TokenRecord]) -> Vec<PersistableTokenRecord> {
    records.iter().map(PersistableTokenRecord::build).collect()
}

struct PersistableEntry {
    entry_id: String,
    timing: String,
    kind: String,
    context_history: String,
    ordinal: i32,
    pin_position: Option<String>,
    ignored: bool,
    context_override: String,
    token_count: Option<i64>,
    model_used: Option<String>,
    model_requested: Option<String>,
    attachments: Vec<Attachment>,
}

impl PersistableEntry {
    /// Serializes an entry's fields into the SQL-ready form.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry's ordinal exceeds the storage
    /// column's width — `i64::from` of `usize` only fits while the entry
    /// count stays under 2^31, and a session at that scale is corrupt
    /// rather than clamped.
    fn build(entry: &ChatEntry, ordinal: usize) -> Result<Self, Report<SessionStoreError>> {
        let timing = serde_json::to_string(&entry.timing).unwrap_or_else(|_| "{}".to_owned());
        let attachments = extract_attachments(&entry.kind);
        let kind = serialize_lean_kind(&entry.kind);
        let context_history =
            serde_json::to_string(&entry.context_history).unwrap_or_else(|_| "[]".to_owned());
        let pin_position = entry.pin_position.map(|p| p.to_string());
        let context_override = serde_json::to_string(&entry.context_override())
            .unwrap_or_else(|_| "\"default\"".to_owned());
        Ok(Self {
            entry_id: entry.id.to_string(),
            timing,
            kind,
            context_history,
            ordinal: i32::try_from(ordinal)
                .change_context(SessionStoreError)
                .attach(format!(
                    "entry {} ordinal {ordinal} exceeds storage width",
                    entry.id
                ))?,
            pin_position,
            ignored: entry.ignored(),
            context_override,
            token_count: entry.token_count.map(i64::from),
            model_used: entry.model_used.clone(),
            model_requested: entry.model_requested.clone(),
            attachments,
        })
    }
}
/// A pre-serialized token ledger row.
struct PersistableTokenRecord {
    timestamp: String,
    tokens_sent: i32,
    tokens_received: i32,
    cost: Option<f64>,
    model_used: Option<String>,
    prompt_tokens: Option<i32>,
    cached_tokens: Option<i32>,
}

impl PersistableTokenRecord {
    /// Serializes a `TokenRecord` into the SQL-ready form.
    fn build(record: &TokenRecord) -> Self {
        Self {
            timestamp: record.timestamp.to_string(),
            tokens_sent: record.tokens_sent as i32,
            tokens_received: record.tokens_received as i32,
            cost: record.cost,
            model_used: record.model_used.clone(),
            prompt_tokens: record.prompt_tokens.map(|t| t as i32),
            cached_tokens: record.cached_tokens.map(|t| t as i32),
        }
    }
}

/// Upserts a session row (full-column; immutable columns are no-ops on re-write).
///
/// The legacy `is_automated` column is no longer mapped to the domain; every
/// write sets it to false.
#[expect(clippy::ref_option, reason = "ignore")]
fn upsert_session_row(
    conn: &rusqlite::Connection,
    id: &str,
    title: &Option<String>,
    updated_at: &str,
    created_at: &str,
    parent_session: &Option<String>,
    archived: bool,
    in_library: bool,
    is_bot: bool,
    metadata: &Option<String>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, title, updated_at, created_at, parent_session, archived, \
         in_library, is_bot, metadata, is_automated, persist) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, FALSE, TRUE) \
         ON CONFLICT(id) DO UPDATE SET \
         title = excluded.title, \
         updated_at = excluded.updated_at, \
         created_at = excluded.created_at, \
         parent_session = excluded.parent_session, \
         archived = excluded.archived, \
         in_library = excluded.in_library, \
         is_bot = excluded.is_bot, \
         metadata = excluded.metadata, \
         is_automated = excluded.is_automated, \
         persist = excluded.persist",
        rusqlite::params![
            id,
            title,
            updated_at,
            created_at,
            parent_session,
            archived,
            in_library,
            is_bot,
            metadata
        ],
    )?;
    Ok(())
}

/// Inserts an entry row (upserting `context_history`) and its junction row.
fn insert_entry_and_junction(
    conn: &rusqlite::Connection,
    session_id: &SessionId,
    entry: &PersistableEntry,
) -> rusqlite::Result<()> {
    // Insert entry. On conflict (entry shared across sessions), update
    // context_history since it mutates after first insertion via
    // `apply_context_override`. `token_count`, `model_used`, and
    // `model_requested` keep the already-stored value when the incoming one
    // is NULL — a session saved before its actor computed the count (or
    // before model attribution existed) must not erase a value another
    // session persisted.
    conn.execute(
        "INSERT INTO entries (id, timing, kind, context_history, token_count, model_used, model_requested) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET context_history = excluded.context_history, \
         token_count = COALESCE(excluded.token_count, entries.token_count), \
         model_used = COALESCE(excluded.model_used, entries.model_used), \
         model_requested = COALESCE(excluded.model_requested, entries.model_requested)",
        rusqlite::params![
            entry.entry_id,
            entry.timing,
            entry.kind,
            entry.context_history,
            entry.token_count,
            entry.model_used,
            entry.model_requested
        ],
    )?;

    // Insert junction row.
    conn.execute(
        "INSERT INTO session_history \
         (session_id, entry_id, ordinal, pin_position, ignored, context_override) \
         VALUES (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            session_id.to_string(),
            entry.entry_id,
            entry.ordinal,
            entry.pin_position,
            entry.ignored,
            entry.context_override,
        ],
    )?;

    // Persist attachment blobs. The lean `kind` JSON carries no raw bytes;
    // `entry_blobs` holds them keyed by ordinal within the entry.
    // Replace any prior blobs for this entry (the entry is upserted above).
    conn.execute(
        "DELETE FROM entry_blobs WHERE entry_id = ?",
        rusqlite::params![entry.entry_id],
    )?;
    for (ordinal, attachment) in entry.attachments.iter().enumerate() {
        conn.execute(
            "INSERT INTO entry_blobs (entry_id, ordinal, media_type, data) \
             VALUES (?, ?, ?, ?)",
            rusqlite::params![
                entry.entry_id,
                i64::try_from(ordinal).unwrap_or(i64::MAX),
                attachment.media_type(),
                attachment.data(),
            ],
        )?;
    }
    Ok(())
}

/// Inserts a token ledger row.
fn insert_token_ledger_row(
    conn: &rusqlite::Connection,
    session_id: &SessionId,
    record: &PersistableTokenRecord,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO token_ledger \
         (session_id, timestamp, tokens_sent, tokens_received, cost, model_used, prompt_tokens, cached_tokens) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            session_id.to_string(),
            record.timestamp,
            record.tokens_sent,
            record.tokens_received,
            record.cost,
            record.model_used,
            record.prompt_tokens,
            record.cached_tokens,
        ],
    )?;
    Ok(())
}

// ── Scoped orphan reaping (delete) ───────────────────────────────────────

/// Deletes a session and reaps entries that became orphaned by this delete.
///
/// Cleanup is **scoped to this session's own former entries**: the session's
/// `entry_id`s are captured before the FK cascade removes its junction rows,
/// then only those candidates that no remaining session references are deleted.
/// A global orphan sweep is deliberately avoided — a transiently-empty global
/// `session_history` state can never cause mass reaping of other sessions' data.
fn delete_with_scoped_reaping(
    conn: &mut rusqlite::Connection,
    session_id_str: &str,
) -> daow::Result<()> {
    let tx = conn.transaction()?;
    // Capture this session's entry references before the FK cascade
    // removes them. These are the only candidates for reaping.
    let candidates: Vec<String> = tx
        .prepare("SELECT DISTINCT entry_id FROM session_history WHERE session_id = ?")?
        .query_map([session_id_str], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Delete the session. With FK=ON this cascades to remove this session's
    // session_history and token_ledger rows.
    tx.execute(
        "DELETE FROM sessions WHERE id = ?",
        rusqlite::params![session_id_str],
    )?;

    if !candidates.is_empty() {
        // After the cascade, session_history holds only OTHER sessions'
        // references. Reap a candidate only if no remaining session claims it.
        let orphaned = unreferenced_entries(&tx, &candidates)?;
        if !orphaned.is_empty() {
            delete_entries(&tx, &orphaned)?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Returns the subset of `candidates` that no remaining `session_history` row references.
fn unreferenced_entries(
    conn: &rusqlite::Connection,
    candidates: &[String],
) -> rusqlite::Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = repeat_placeholders(candidates.len());
    let sql = format!("SELECT entry_id FROM session_history WHERE entry_id IN ({placeholders})");
    let referenced: Vec<String> = {
        let mut stmt = conn.prepare(&sql)?;
        let params = candidates
            .iter()
            .map(|c| c as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let orphaned = candidates
        .iter()
        .filter(|id| !referenced.iter().any(|r| r == *id))
        .cloned()
        .collect();
    Ok(orphaned)
}

/// Deletes the given entry rows by id.
fn delete_entries(conn: &rusqlite::Connection, ids: &[String]) -> rusqlite::Result<()> {
    let placeholders = repeat_placeholders(ids.len());
    let sql = format!("DELETE FROM entries WHERE id IN ({placeholders})");
    let params = ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect::<Vec<_>>();
    conn.execute(&sql, params.as_slice())?;
    Ok(())
}

/// Builds a `?, ?, …` placeholder string of `n` elements.
fn repeat_placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(", ")
}

/// Deletes many sessions and reaps their orphaned entries in one transaction.
///
/// The bulk form of [`delete_with_scoped_reaping`] for the maintenance
/// actor's prune sweep — all-or-nothing: either every session is removed or
/// none is. Reaping stays scoped to the deleted set's own former entries, so
/// an entry shared with any surviving session (a kept fork of a pruned
/// session, most commonly) is never touched.
///
/// Free function (not a DAO method) because the dynamic `IN (…)` placeholder
/// list cannot be statically validated against the compile-time schema DB.
fn delete_many_in_transaction(
    conn: &mut rusqlite::Connection,
    session_id_strs: &[String],
) -> daow::Result<()> {
    if session_id_strs.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    // Capture the deleted set's entry references before the FK cascade
    // removes their junction rows. These are the only reap candidates.
    let placeholders = repeat_placeholders(session_id_strs.len());
    let candidates: Vec<String> = {
        let sql = format!(
            "SELECT DISTINCT entry_id FROM session_history WHERE session_id IN ({placeholders})"
        );
        let params = session_id_strs
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        tx.prepare(&sql)?
            .query_map(params.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };

    // Delete every session. With FK=ON this cascades to remove each one's
    // session_history and token_ledger rows.
    {
        let sql = format!("DELETE FROM sessions WHERE id IN ({placeholders})");
        let params = session_id_strs
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect::<Vec<_>>();
        tx.execute(&sql, params.as_slice())?;
    }

    if !candidates.is_empty() {
        // After the cascade, session_history holds only surviving sessions'
        // references. Reap a candidate only if no surviving session claims it.
        let orphaned = unreferenced_entries(&tx, &candidates)?;
        if !orphaned.is_empty() {
            delete_entries(&tx, &orphaned)?;
        }
    }
    tx.commit()?;
    Ok(())
}

// ── Archived-tree updates ────────────────────────────────────────────────
/// Sets the `archived` flag for many sessions in one transaction.
///
/// Free function (not a DAO method) because the dynamic `IN (…)` placeholder
/// list cannot be statically validated against the compile-time schema DB.
/// Unknown IDs simply match no rows.
fn set_archived_many_in_transaction(
    conn: &mut rusqlite::Connection,
    id_strs: &[String],
    archived: bool,
) -> daow::Result<()> {
    if id_strs.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let placeholders = vec!["?"; id_strs.len()].join(", ");
    let sql = format!("UPDATE sessions SET archived = ? WHERE id IN ({placeholders})");
    let params: Vec<&dyn rusqlite::ToSql> = std::iter::once(&archived as &dyn rusqlite::ToSql)
        .chain(id_strs.iter().map(|s| s as &dyn rusqlite::ToSql))
        .collect();
    tx.execute(&sql, rusqlite::params_from_iter(params))?;
    tx.commit()?;
    Ok(())
}

// ── Row → domain conversions ─────────────────────────────────────────────

/// Builds a `SessionSummary` from a loaded `SessionRow`.
///
/// # Errors
///
/// Returns an error when the row's `id` column is not a parseable session
/// id — a malformed primary key is corruption, and a summary carrying it
/// would silently poison every consumer that keys on the id.
fn summary_from_row(row: SessionRow) -> Result<SessionSummary, Report<SessionStoreError>> {
    // The project association lives only in the metadata blob; parse failure
    // (corrupt/legacy row) yields `None`, i.e. a blank project column.
    let project = row
        .metadata
        .as_deref()
        .and_then(|json| serde_json::from_str::<PersistableCore>(json).ok())
        .and_then(|core| core.project);
    summary_parts(
        &row.id,
        row.title,
        &row.updated_at,
        &row.created_at,
        row.parent_session.as_deref(),
        row.archived,
        row.in_library,
        is_bot_of(row.metadata.as_ref()),
        project,
    )
}

/// Reads the blob's `origin` for the summary's `is_bot` projection.
///
/// Parse failure (corrupt/legacy row) yields `false`: the summary stays
/// servable, and the only consequence is a bot rendered untinted — the same
/// degradation a NULL column would give.
fn is_bot_of(metadata: Option<&String>) -> bool {
    metadata
        .and_then(|json| serde_json::from_str::<PersistableCore>(json).ok())
        .is_some_and(|core| core.origin == jinn_session_msg::SessionOrigin::Bot)
}

/// Builds a `SessionSummary` from a library-members row.
///
/// The metadata blob is deliberately unread here: the Library section
/// renders none of it, and the whole point of the members query is to skip
/// its parse. `project` is therefore `None`.
///
/// # Errors
///
/// Same contract as [`summary_from_row`]: a malformed id or parent id is an
/// error, not a silently poisoned summary.
fn library_summary_from_row(
    row: LibraryMemberRow,
) -> Result<SessionSummary, Report<SessionStoreError>> {
    summary_parts(
        &row.id,
        row.title,
        &row.updated_at,
        &row.created_at,
        row.parent_session.as_deref(),
        row.archived,
        row.in_library,
        row.is_bot,
        None,
    )
}

/// Maps an [`ArchiveRow`] to its summary.
///
/// The project arrives as the path the blob stores, already extracted; a
/// value that does not parse as an absolute path yields `None`, i.e. a
/// blank project column — the same degradation [`summary_from_row`] gives
/// a corrupt blob.
fn archive_summary_from_row(row: ArchiveRow) -> Result<SessionSummary, Report<SessionStoreError>> {
    let project = row.project.and_then(|path| path.parse().ok());
    summary_parts(
        &row.id,
        row.title,
        &row.updated_at,
        &row.created_at,
        row.parent_session.as_deref(),
        row.archived,
        row.in_library,
        row.is_bot,
        project,
    )
}

/// The shared summary assembly behind [`summary_from_row`] and
/// [`library_summary_from_row`]: everything after "which columns did we read".
///
/// # Errors
///
/// Returns an error on a malformed `id` or `parent_session` value.
#[expect(
    clippy::too_many_arguments,
    reason = "one flat parameter per read column keeps the two call sites symmetric"
)]
fn summary_parts(
    id: &str,
    title: Option<String>,
    updated_at: &str,
    created_at: &str,
    parent_session: Option<&str>,
    archived: bool,
    in_library: bool,
    is_bot: bool,
    project: Option<MarkedPathBuf<Absolute>>,
) -> Result<SessionSummary, Report<SessionStoreError>> {
    let session_id = SessionId::try_from_string(id)
        .ok_or(SessionStoreError)
        .attach(format!("sessions row carries a malformed id: {id}"))?;
    let parent_session = parent_session
        .map(|id| {
            SessionId::try_from_string(id)
                .ok_or(SessionStoreError)
                .attach(format!(
                    "sessions row carries a malformed parent_session: {id}"
                ))
        })
        .transpose()?;
    Ok(SessionSummary {
        session_id,
        title: title.unwrap_or_else(|| "Untitled".to_owned()),
        updated_at: updated_at
            .parse()
            .unwrap_or_else(|_| jiff::Timestamp::now()),
        created_at: created_at
            .parse()
            .unwrap_or_else(|_| jiff::Timestamp::now()),
        session_state: if archived {
            SessionState::Archived
        } else {
            SessionState::Loaded
        },
        in_library,
        is_bot,
        parent_session,
        project,
    })
}

/// Puts persisted attachment blobs back into the kinds that carry them.
///
/// User entries (from `@path` sends) and tool-result entries (e.g. images
/// from `read_image`) were persisted with empty attachment lists; the blobs
/// live in `entry_blobs` and ride back in through this function on load.
fn hydrate_kind_attachments(kind: ChatEntryKind, attachments: Vec<Attachment>) -> ChatEntryKind {
    match kind {
        ChatEntryKind::User {
            display,
            expanded,
            outcome,
            ..
        } => ChatEntryKind::User {
            display,
            expanded,
            attachments,
            outcome,
        },
        ChatEntryKind::ToolResult {
            id,
            name,
            content,
            status,
            full_content,
            truncation,
            pin_position,
            is_alert,
            ..
        } => ChatEntryKind::ToolResult {
            id,
            name,
            content,
            status,
            full_content,
            truncation,
            pin_position,
            is_alert,
            attachments,
        },
        other => other,
    }
}

/// Reconstructs a `ChatEntry` from a joined entry/junction row.
///
/// # Errors
///
/// Returns an error when the row's `entry_id` is not a parseable entry id.
fn entry_from_joined(
    joined: &JoinedEntry,
    attachments: Vec<Attachment>,
) -> Result<ChatEntry, Report<SessionStoreError>> {
    let mut kind: ChatEntryKind = serde_json::from_str(&joined.kind).unwrap_or_else(|e| {
        tracing::warn!(entry_id = %joined.entry_id, error = %e, "failed to deserialize entry kind");
        ChatEntryKind::Error(format!("corrupt entry: {e}"))
    });
    kind = hydrate_kind_attachments(kind, attachments);
    let pin_position = joined.pin_position.as_deref().and_then(|s| match s {
        "TOP" => Some(jinn_core_types::PinPosition::Top),
        "BOTTOM" => Some(jinn_core_types::PinPosition::Bottom),
        "RELATIVE" => Some(jinn_core_types::PinPosition::Relative),
        _ => None,
    });

    let timing: jinn_core_types::EntryTiming =
        serde_json::from_str(&joined.timing).unwrap_or_else(|_| {
            // Fallback: parse raw timestamp string as Instant (legacy data).
            joined.timing.parse::<jiff::Timestamp>().map_or_else(
                |_| jinn_core_types::EntryTiming::instant_now(),
                |at| jinn_core_types::EntryTiming::Instant { at },
            )
        });
    let entry_id = ChatEntryId::try_from_string(&joined.entry_id)
        .ok_or(SessionStoreError)
        .attach(format!(
            "entry row carries a malformed entry id: {}",
            joined.entry_id
        ))?;
    let mut chat_entry = ChatEntry::new_with_kind(entry_id, timing, kind, pin_position);
    // Restored from DB - no audit event recorded.
    let override_value: ContextOverride = serde_json::from_str(&joined.context_override)
        .unwrap_or_else(|e| {
            tracing::warn!(
                entry_id = %chat_entry.id.as_uuid(),
                raw = %joined.context_override,
                error = %e,
                "failed to deserialize context_override, falling back to Default"
            );
            // Fallback: use legacy ignored column if context_override is corrupt
            if joined.ignored {
                ContextOverride::ForcedExclude
            } else {
                ContextOverride::Default
            }
        });
    chat_entry.restore_context_override(override_value);

    // Restore the persisted token count (NULL or corrupt-negative → None →
    // computed lazily by the token count actor). Content-derived fact —
    // restored, not recomputed.
    chat_entry.restore_token_count(joined.token_count.and_then(|t| u32::try_from(t).ok()));

    // Restore the persisted model stamps (NULL → None, permanent for
    // pre-v29 rows). Facts recorded at entry creation — restored, never
    // re-derived.
    chat_entry.restore_model_used(joined.model_used.clone());
    chat_entry.restore_model_requested(joined.model_requested.clone());

    // Restore audit trail. Empty array (default) loads as Vec::new().
    // Corrupt JSON falls back to empty with a warning.
    chat_entry.context_history =
        serde_json::from_str(&joined.context_history).unwrap_or_else(|e| {
            tracing::warn!(
                entry_id = %chat_entry.id.as_uuid(),
                raw = %joined.context_history,
                error = %e,
                "failed to deserialize context_history, falling back to empty"
            );
            Vec::new()
        });
    Ok(chat_entry)
}

/// Reconstructs a `TokenRecord` from a `TokenLedgerRow`.
fn record_from_row(row: TokenLedgerRow) -> TokenRecord {
    TokenRecord {
        model_used: row.model_used,
        timestamp: row
            .timestamp
            .parse()
            .unwrap_or_else(|_| jiff::Timestamp::now()),
        tokens_sent: row.tokens_sent as u32,
        tokens_received: row.tokens_received as u32,
        cost: row.cost,
        prompt_tokens: row.prompt_tokens.map(|t| t as u32),
        cached_tokens: row.cached_tokens.map(|t| t as u32),
    }
}

/// A raw `entry_blobs` row for attachment hydration.
///
/// Read by a manual `FromRow` that maps the column names of the blob query.
struct EntryBlobRow {
    entry_id: String,
    ordinal: i64,
    media_type: String,
    data: Vec<u8>,
}

/// Groups blob rows into ordered attachment vectors keyed by entry id.
fn group_blobs_by_entry(mut rows: Vec<EntryBlobRow>) -> HashMap<String, Vec<Attachment>> {
    rows.sort_by_key(|r| r.ordinal);
    let mut map: HashMap<String, Vec<Attachment>> = HashMap::new();
    for row in rows {
        map.entry(row.entry_id)
            .or_default()
            .push(Attachment::image(row.media_type, row.data));
    }
    map
}

// ── Shutdown checkpoint ────────────────────���─────────────────────────────

/// Result row of `PRAGMA wal_checkpoint(TRUNCATE)`.
///
/// Columns: `busy` (1 if the checkpoint could not complete because a reader
/// held a snapshot), `log` (frames in the WAL), `checkpointed` (frames folded
/// into the main db). Read by name via a manual `FromRow`.
struct CheckpointResult {
    busy: i64,
    log: i64,
    checkpointed: i64,
}

impl FromRow for CheckpointResult {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            busy: row.get("busy")?,
            log: row.get("log")?,
            checkpointed: row.get("checkpointed")?,
        })
    }
}

/// Classifies a `wal_checkpoint` result row as fatal or non-fatal.
///
/// Extracted from `shutdown` as a pure function so the `busy=1`
/// graceful-degradation path is unit-testable without a database. A busy
/// result (a reader held a snapshot mid-checkpoint) logs a warning and returns
/// `Ok` — the un-folded frames survive in the WAL and fold on the next open.
/// A clean result logs at info level.
///
/// This function never fails: the only fallible step in shutdown is the
/// checkpoint query itself, which stays in `shutdown`. This pure classifier
/// just chooses the log level based on the result row.
fn classify_checkpoint_result(result: &CheckpointResult) {
    if result.busy == 1 {
        tracing::warn!(
            log_frames = result.log,
            checkpointed_frames = result.checkpointed,
            "wal_checkpoint was busy; some WAL frames remain un-folded (non-fatal)"
        );
    } else {
        tracing::info!(
            log_frames = result.log,
            checkpointed_frames = result.checkpointed,
            "folded WAL into sessions.db during shutdown"
        );
    }
}

// ── FTS search index (schema v26) ────────────────────────────────────────

/// A raw `(entry_id, timing, kind)` row joined across `session_history` +
/// `entries` — the input to the reindex parse stage.
struct RawIndexedEntry {
    entry_id: String,
    timing: String,
    kind: String,
}

impl FromRow for RawIndexedEntry {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            entry_id: row.get("entry_id")?,
            timing: row.get("timing")?,
            kind: row.get("kind")?,
        })
    }
}

/// Returns the ids of all sessions with pending (dirty) FTS reindex work.
///
/// Rows whose stored id cannot be parsed as a [`SessionId`] are skipped with
/// a warning: a corrupt marker must not poison the batch, and it could never
/// be reindexed anyway. [`pending_dirty_count`] counts those rows — the two
/// primitives intentionally disagree on corrupt markers so the dashboard can
/// surface them as permanently pending.
async fn dirty_session_ids(pool: &Pool) -> Result<Vec<SessionId>, Report<SessionStoreError>> {
    let dirty: Vec<String> = pool
        .query_all("SELECT session_id AS session_id FROM fts_dirty", vec![])
        .await
        .change_context(SessionStoreError)
        .attach("failed to read dirty session markers")?;

    Ok(dirty
        .into_iter()
        .filter_map(|id_str| match SessionId::try_from_string(&id_str) {
            Some(id) => Some(id),
            None => {
                tracing::warn!(
                    raw_id = %id_str,
                    "skipping unparseable FTS dirty marker"
                );
                None
            }
        })
        .collect())
}

/// Counts all `fts_dirty` marker rows, including corrupt (unparseable) ones —
/// the number reflects the true size of the pending queue.
async fn pending_dirty_count(pool: &Pool) -> Result<usize, Report<SessionStoreError>> {
    let total: Option<i64> = pool
        .query_one("SELECT COUNT(*) AS total FROM fts_dirty", vec![])
        .await
        .change_context(SessionStoreError)
        .attach("failed to count dirty session markers")?;
    Ok(total.unwrap_or(0) as usize)
}

/// Read by a manual `FromRow` that maps the aliased dirty-marker columns.
struct DirtyResumeRow {
    resume_offset: i64,
}

impl FromRow for DirtyResumeRow {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            resume_offset: row.get("resume_offset")?,
        })
    }
}

/// Returns the resume point stored on a session's dirty marker: how many of
/// its entries the chunked rebuild has already indexed (0 = not started).
///
/// A query failure falls back to 0 (rebuild from scratch — correct, just
/// slower) but is logged loudly: this default once masked a migration drift
/// bug where the `resume_offset` column itself was missing, silently
/// restarting every partial rebuild for days.
async fn resume_offset(pool: &Pool, session_id: &SessionId) -> usize {
    let rows: Vec<DirtyResumeRow> = pool
        .query_all(
            "SELECT resume_offset AS resume_offset FROM fts_dirty WHERE session_id = ?",
            vec![Box::new(session_id.to_string())],
        )
        .await
        .inspect_err(|e| {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "failed to read FTS resume offset; restarting this session's reindex from 0"
            );
        })
        .unwrap_or_default();
    rows.first().map_or(0, |row| {
        usize::try_from(row.resume_offset.max(0)).unwrap_or(0)
    })
}

/// Indexes at most `max_entries` more of one session's entries into the FTS
/// table, resuming at the marker's stored resume point (by ordinal), as a
/// single transaction. A resume point of 0 additionally deletes the
/// session's existing FTS rows ("dirty = recompute from scratch"); later
/// chunks append. Returns `true` when the session is fully indexed — its
/// dirty marker is then deleted in the same transaction.
///
/// A partial chunk persists the advanced resume point on the marker, so the
/// drain continues where it left off on the next tick or after a restart.
/// If new writes land mid-rebuild, the `sessions` UPDATE trigger re-marks
/// the session (resume stays put; the next rebuild-from-zero repairs any
/// staleness). A session deleted since being marked yields an empty first
/// chunk and a deleted marker — a no-op rebuild, not an error.
async fn reindex_session_chunk(
    pool: &Pool,
    session_id: &SessionId,
    max_entries: usize,
) -> Result<bool, Report<SessionStoreError>> {
    let session_id_str = session_id.to_string();
    let offset = resume_offset(pool, session_id).await;
    let raw: Vec<RawIndexedEntry> = pool
        .query_all(
            "SELECT entries.id AS entry_id, entries.timing AS timing, entries.kind AS kind \
             FROM entries \
             INNER JOIN session_history ON entries.id = session_history.entry_id \
             WHERE session_history.session_id = ? \
             ORDER BY session_history.ordinal ASC \
             LIMIT ? OFFSET ?",
            vec![
                Box::new(session_id_str.clone()),
                Box::new(i64::from(i32::try_from(max_entries).unwrap_or(i32::MAX))),
                Box::new(i64::try_from(offset).unwrap_or(i64::MAX)),
            ],
        )
        .await
        .change_context(SessionStoreError)
        .attach("failed to read entries for reindex")?;

    // Kind JSON parsing can be heavy (full_content tool outputs) — keep it
    // off the async runtime. `raw` moves in; `session_id` is cloned for the
    // closure because the tx stage below needs it too.
    let parsed = {
        let session_id = session_id.clone();
        tokio::task::spawn_blocking(move || parse_searchable_rows(&session_id, &raw))
            .await
            .change_context(SessionStoreError)
            .attach("reindex parse task panicked")?
    };

    // A chunk shorter than `max_entries` means the ordinal walk ran past the
    // end: the session is fully indexed. (An exactly-full final chunk is
    // followed by one empty chunk that returns `finished` here — correct,
    // since the walk is then exhausted.)
    let finished = parsed.len() < max_entries;
    let advanced = (offset + parsed.len()) as i64;
    // The tx stage below is a `move` closure over a spawned borrow: hand it
    // an owned id.
    let session_id = session_id.clone();
    pool.with_conn(move |conn| -> daow::Result<bool> {
        let tx = conn.transaction()?;
        if offset == 0 {
            // First chunk of the rebuild: drop the session's existing rows
            // before inserting the live prefix. For a deleted session the
            // live set is empty, so this removes stale rows only.
            //
            // `session_id` is UNINDEXED in the FTS5 table, so filtering on it
            // full-scans the index; delete via the `fts_rowids` map instead
            // (primary-key rowid lookups), and skip entirely when the map has
            // no rows for this session (a never-indexed session).
            let mapped: i64 = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM fts_rowids WHERE session_id = ?)",
                rusqlite::params![session_id.to_string()],
                |row| row.get(0),
            )?;
            if mapped != 0 {
                tx.execute(
                    "DELETE FROM session_fts WHERE rowid IN \
                     (SELECT fts_rowid FROM fts_rowids WHERE session_id = ?)",
                    rusqlite::params![session_id.to_string()],
                )?;
            }
            tx.execute(
                "DELETE FROM fts_rowids WHERE session_id = ?",
                rusqlite::params![session_id.to_string()],
            )?;
        }
        for entry in &parsed {
            tx.execute(
                "INSERT INTO session_fts (body, role, session_id, entry_id, entry_ts) \
                 VALUES (?, ?, ?, ?, ?)",
                rusqlite::params![
                    entry.body,
                    entry.role.as_str(),
                    session_id.to_string(),
                    entry.entry_id.to_string(),
                    entry.entry_ts
                ],
            )?;
            // Paired map insert, same transaction: the map must reference
            // every row the index gains, or a later rebuild's delete misses
            // it. `last_insert_rowid` reflects the FTS insert above — the
            // only intervening insert on this connection.
            tx.execute(
                "INSERT INTO fts_rowids (session_id, fts_rowid) \
                 VALUES (?, last_insert_rowid())",
                rusqlite::params![session_id.to_string()],
            )?;
        }
        if finished {
            // Final chunk: the session is fully indexed — clear the marker
            // (and its resume point) in the same transaction so the queue
            // and the index agree.
            tx.execute(
                "DELETE FROM fts_dirty WHERE session_id = ?",
                rusqlite::params![session_id.to_string()],
            )?;
        } else {
            tx.execute(
                "UPDATE fts_dirty SET resume_offset = ? WHERE session_id = ?",
                rusqlite::params![advanced, session_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(finished)
    })
    .await
    .change_context(SessionStoreError)
    .attach("failed to rebuild FTS rows")
}

/// Parses raw entry rows into indexed rows. Runs inside `spawn_blocking`.
///
/// Rows whose kind JSON fails to deserialize are skipped with a warning: the
/// FTS index is derived data, and a corrupt entry must not fail the whole
/// reindex (it will be retried on the next save anyway).
fn parse_searchable_rows(session_id: &SessionId, raw: &[RawIndexedEntry]) -> Vec<SearchableEntry> {
    raw.iter()
        .filter_map(|r| {
            let kind: ChatEntryKind = match serde_json::from_str(&r.kind) {
                Ok(kind) => kind,
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        entry_id = %r.entry_id,
                        error = %e,
                        "skipping unparseable entry kind during FTS reindex"
                    );
                    return None;
                }
            };
            let (role, body) = extract_searchable(&kind)?;
            let timing: EntryTiming = serde_json::from_str(&r.timing).unwrap_or_else(|_| {
                match r.timing.parse::<jiff::Timestamp>() {
                    Ok(at) => EntryTiming::Instant { at },
                    Err(_) => EntryTiming::instant_now(),
                }
            });
            Some(SearchableEntry {
                entry_id: match ChatEntryId::try_from_string(&r.entry_id.clone()) {
                    Some(id) => id,
                    None => {
                        tracing::warn!(
                            entry_id = %r.entry_id,
                            "skipping FTS reindex of row with malformed entry id"
                        );
                        return None;
                    }
                },
                role,
                body,
                entry_ts: entry_ts_key(&timing),
            })
        })
        .collect()
}

/// A hit row read back from the FTS table.
struct FtsHitRow {
    entry_id: String,
    role: String,
    session_id: String,
    entry_ts: String,
    snippet: String,
}

impl FromRow for FtsHitRow {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            entry_id: row.get("entry_id")?,
            role: row.get("role")?,
            session_id: row.get("session_id")?,
            entry_ts: row.get("entry_ts")?,
            snippet: row.get("snippet")?,
        })
    }
}

/// A per-session match count row for the rollup.
struct SessionCountRow {
    session_id: String,
    matches: i64,
}

impl FromRow for SessionCountRow {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            session_id: row.get("session_id")?,
            matches: row.get("matches")?,
        })
    }
}

/// A joined window row: an entry plus its junction ordinal.
struct JoinedWindowEntry {
    joined: JoinedEntry,
    ordinal: i64,
}

impl FromRow for JoinedWindowEntry {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            joined: JoinedEntry::from_row(row)?,
            ordinal: row.get("ordinal")?,
        })
    }
}

/// A `(entry_id, ignored, context_override, pin_position, context_history)`
/// row — the persisted signals behind the excluded-from-context flag.
struct ExclusionRow {
    entry_id: String,
    ignored: bool,
    context_override: String,
    pin_position: Option<String>,
    context_history: String,
}

impl FromRow for ExclusionRow {
    fn from_row(row: &Row) -> daow::Result<Self> {
        Ok(Self {
            entry_id: row.get("entry_id")?,
            ignored: row.get("ignored")?,
            context_override: row.get("context_override")?,
            pin_position: row.get("pin_position")?,
            context_history: row.get("context_history")?,
        })
    }
}

impl ExclusionRow {
    /// Whether the persisted signals say this entry is out of context.
    ///
    /// Priority matches `ChatEntry::is_in_context`: pins win; an explicit
    /// `ForcedInclude` wins over a persisted worker exclusion; `ForcedExclude`
    /// (or the legacy `ignored` column) excludes.
    fn is_excluded(&self) -> bool {
        if self.pin_position.is_some() {
            return false;
        }
        match serde_json::from_str::<ContextOverride>(&self.context_override) {
            Ok(ContextOverride::ForcedInclude) => false,
            Ok(ContextOverride::ForcedExclude) => true,
            _ if self.ignored => true,
            _ => {
                // Fall back to the audit trail: a persisted worker/user
                // ForcedExclude that was never re-included.
                serde_json::from_str::<Vec<jinn_core_types::ContextChangeEvent>>(
                    &self.context_history,
                )
                .is_ok_and(|events| {
                    events
                        .last()
                        .is_some_and(|event| event.to == ContextOverride::ForcedExclude)
                })
            }
        }
    }
}

/// Clamps an anchor + context size to an inclusive ordinal window of at most
/// `context` entries, centered on the anchor with a slight forward bias
/// (reading forward is what fetch is for), clamped to the session's start.
fn clamp_window(anchor_ord: i64, context: usize) -> (i64, i64) {
    let span = i64::try_from(context.max(1)).unwrap_or(i64::MAX);
    let after = (span - 1) * 7 / 10;
    let before = span - 1 - after;
    let lo = (anchor_ord - before).max(0);
    match lo.checked_add(span - 1) {
        Some(hi) => (lo, hi),
        None => (0, span - 1),
    }
}

/// Collapses all whitespace in an FTS snippet to single spaces so one hit is
/// always one output line.
fn collapse_whitespace(snippet: &str) -> String {
    snippet.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The bind values for one search, in a cloneable form (plain owned values).
#[derive(Clone)]
enum Bind {
    Text(String),
    Int(i64),
}

impl Bind {
    fn into_param(self) -> Param {
        match self {
            Self::Text(s) => Box::new(s),
            Self::Int(i) => Box::new(i),
        }
    }
}

fn binds_to_params(binds: &[Bind]) -> Vec<Param> {
    binds.iter().cloned().map(Bind::into_param).collect()
}

/// Runs the FTS query with WHERE post-filters, then the count + rollup.
///
/// `role` and `session_id` are UNINDEXED columns: they are invisible to
/// `MATCH` and must be plain `WHERE` conditions (which also guarantees bare
/// query terms can never match a role word).
async fn search_index(
    pool: &Pool,
    params: SearchParams,
) -> Result<SearchOutcome, Report<SessionStoreError>> {
    let (where_clause, mut binds) = build_fts_filter(&params);
    binds.insert(0, Bind::Text(params.query.clone()));
    let limit_placeholder = format!("?{}", binds.len() + 1);

    let hits_sql = format!(
        "SELECT f.entry_id AS entry_id, f.role AS role, f.session_id AS session_id, \
         f.entry_ts AS entry_ts, \
         snippet(session_fts, 0, '<<', '>>', ' … ', 24) AS snippet \
         FROM session_fts f WHERE {where_clause} \
         ORDER BY bm25(session_fts) LIMIT {limit_placeholder}"
    );
    let count_sql = format!("SELECT COUNT(*) AS total FROM session_fts f WHERE {where_clause}");
    let rollup_sql = format!(
        "SELECT f.session_id AS session_id, COUNT(*) AS matches \
         FROM session_fts f WHERE {where_clause} GROUP BY f.session_id \
         ORDER BY matches DESC"
    );

    let mut hit_binds = binds.clone();
    hit_binds.push(Bind::Int(params.limit as i64));

    let rows: Vec<FtsHitRow> = pool
        .query_all(&hits_sql, binds_to_params(&hit_binds))
        .await
        .map_err(|daow_err| {
            Report::new(SessionStoreError).attach(format!("FTS query failed: {daow_err}"))
        })?;
    let total: Option<i64> = pool
        .query_one(&count_sql, binds_to_params(&binds))
        .await
        .map_err(|daow_err| {
            Report::new(SessionStoreError).attach(format!("FTS count failed: {daow_err}"))
        })?;
    let rollup: Vec<SessionCountRow> = pool
        .query_all(&rollup_sql, binds_to_params(&binds))
        .await
        .map_err(|daow_err| {
        Report::new(SessionStoreError).attach(format!("FTS rollup failed: {daow_err}"))
    })?;

    let mut hits: Vec<SearchHit> = rows
        .into_iter()
        .map(|r| -> Result<SearchHit, Report<SessionStoreError>> {
            Ok(SearchHit {
                session_id: SessionId::try_from_string(&r.session_id)
                    .ok_or(SessionStoreError)
                    .attach(format!(
                        "FTS row carries a malformed session id: {}",
                        r.session_id
                    ))?,
                entry_id: ChatEntryId::try_from_string(&r.entry_id.clone())
                    .ok_or(SessionStoreError)
                    .attach(format!(
                        "FTS row carries a malformed entry id: {}",
                        r.entry_id
                    ))?,
                role: r.role,
                snippet: collapse_whitespace(&r.snippet),
                entry_ts: r.entry_ts,
                excluded: false,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    mark_excluded_hits(pool, &mut hits).await?;

    Ok(SearchOutcome {
        total_matches: total.unwrap_or(0) as u64,
        per_session: rollup
            .into_iter()
            .map(|r| {
                SessionId::try_from_string(&r.session_id)
                    .map(|id| (id, r.matches as u64))
                    .ok_or_else(|| {
                        Report::new(SessionStoreError).attach(format!(
                            "FTS rollup row carries a malformed session id: {}",
                            r.session_id
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
        hits,
    })
}

/// Builds the shared FTS WHERE clause and its positional binds. ?1 is the
/// MATCH expression, which the caller prepends last so the hits, count,
/// and rollup queries share one bind vector.
fn build_fts_filter(params: &SearchParams) -> (String, Vec<Bind>) {
    let mut clauses: Vec<String> = vec!["session_fts MATCH ?1".to_owned()];
    let mut binds: Vec<Bind> = Vec::new();

    if !params.session_ids.is_empty() {
        let placeholders = repeat_placeholders(params.session_ids.len());
        clauses.push(format!("f.session_id IN ({placeholders})"));
        binds.extend(
            params
                .session_ids
                .iter()
                .map(|id| Bind::Text(id.to_string())),
        );
    }
    if !params.roles.is_empty() {
        let placeholders = repeat_placeholders(params.roles.len());
        clauses.push(format!("f.role IN ({placeholders})"));
        binds.extend(
            params
                .roles
                .iter()
                .map(|r| Bind::Text(r.as_str().to_owned())),
        );
    }
    if let Some(since) = params.since {
        clauses.push(format!("f.entry_ts >= ?{}", binds.len() + 2));
        binds.push(Bind::Text(since.to_string()));
    }
    if let Some(until) = params.until {
        clauses.push(format!("f.entry_ts <= ?{}", binds.len() + 2));
        binds.push(Bind::Text(until.to_string()));
    }
    (clauses.join(" AND "), binds)
}

/// Sets the `excluded` flag on each hit from its persisted context signals.
///
/// Mirrors [`ChatEntry::is_in_context`]'s priority (pin > forced-include >
/// forced-exclude > kind default) without reconstructing full entries. Kind
/// defaults never apply here — non-context kinds (`Actor`, `Thinking`,
/// `Transient`, `Annotation`) are not indexed in the first place.
async fn mark_excluded_hits(
    pool: &Pool,
    hits: &mut [SearchHit],
) -> Result<(), Report<SessionStoreError>> {
    if hits.is_empty() {
        return Ok(());
    }
    let entry_ids: Vec<String> = hits.iter().map(|h| h.entry_id.to_string()).collect();
    let placeholders = repeat_placeholders(entry_ids.len());
    let sql = format!(
        "SELECT e.id AS entry_id, h.ignored AS ignored, \
         h.context_override AS context_override, h.pin_position AS pin_position, \
         e.context_history AS context_history \
         FROM entries e INNER JOIN session_history h ON e.id = h.entry_id \
         WHERE e.id IN ({placeholders})"
    );
    let params: Vec<Param> = entry_ids
        .into_iter()
        .map(|id| Box::new(id) as Param)
        .collect();
    let rows: Vec<ExclusionRow> = pool
        .query_all(&sql, params)
        .await
        .change_context(SessionStoreError)
        .attach("failed to read exclusion signals for hits")?;
    let by_entry: HashMap<ChatEntryId, bool> = rows
        .into_iter()
        .map(|r| {
            let excluded = r.is_excluded();
            let id = ChatEntryId::try_from_string(&r.entry_id)
                .ok_or(SessionStoreError)
                .attach(format!(
                    "exclusion row carries a malformed entry id: {}",
                    r.entry_id
                ))?;
            Ok::<_, Report<SessionStoreError>>((id, excluded))
        })
        .collect::<Result<HashMap<_, _>, Report<SessionStoreError>>>()?;

    for hit in hits {
        hit.excluded = by_entry.get(&hit.entry_id).copied().unwrap_or(false);
    }
    Ok(())
}

// ── Transcript fetch (session_fetch) ─────────────────────────────────────

/// Loads the session row or `None` if the session does not exist.
async fn session_meta(
    pool: &Pool,
    session_id: &SessionId,
) -> Result<Option<SessionRow>, Report<SessionStoreError>> {
    let dao = SessionDao::new(pool.clone());
    dao.session_by_id(session_id.to_string())
        .await
        .change_context(SessionStoreError)
        .attach("failed to query session metadata")
}

/// Counts the entries in a session.
async fn count_entries(
    pool: &Pool,
    session_id: &SessionId,
) -> Result<usize, Report<SessionStoreError>> {
    let total: Option<i64> = pool
        .query_one(
            "SELECT COUNT(*) AS total FROM session_history WHERE session_id = ?",
            vec![Box::new(session_id.to_string())],
        )
        .await
        .change_context(SessionStoreError)
        .attach("failed to count session entries")?;
    Ok(total.unwrap_or(0) as usize)
}

/// Loads joined entries + ordinals for an inclusive ordinal range [lo, hi].
async fn load_joined_range(
    pool: &Pool,
    session_id: &SessionId,
    lo: i64,
    hi: i64,
) -> Result<Vec<JoinedWindowEntry>, Report<SessionStoreError>> {
    pool.query_all(
        "SELECT entries.id AS entry_id, entries.timing AS timing, entries.kind AS kind, \
         entries.context_history AS context_history, \
         session_history.pin_position AS pin_position, \
         session_history.ignored AS ignored, \
         session_history.context_override AS context_override, \
         entries.token_count AS token_count, \
         entries.model_used AS model_used, \
         entries.model_requested AS model_requested, \
         session_history.ordinal AS ordinal \
         FROM entries \
         INNER JOIN session_history ON entries.id = session_history.entry_id \
         WHERE session_history.session_id = ? \
         AND session_history.ordinal BETWEEN ? AND ? \
         ORDER BY session_history.ordinal ASC",
        vec![Box::new(session_id.to_string()), Box::new(lo), Box::new(hi)],
    )
    .await
    .change_context(SessionStoreError)
    .attach("failed to query transcript window")
}

/// Resolves an anchor entry's ordinal within a session, with a legible error
/// when the entry is not part of it (wrong session, or pre-index data).
async fn anchor_ordinal(
    pool: &Pool,
    session_id: &SessionId,
    anchor: &ChatEntryId,
) -> Result<i64, Report<SessionStoreError>> {
    let ordinal: Option<i64> = pool
        .query_one(
            "SELECT ordinal AS ordinal FROM session_history \
             WHERE session_id = ? AND entry_id = ?",
            vec![
                Box::new(session_id.to_string()),
                Box::new(anchor.to_string()),
            ],
        )
        .await
        .change_context(SessionStoreError)
        .attach("failed to resolve anchor ordinal")?;
    ordinal.ok_or_else(|| {
        Report::new(SessionStoreError).attach(format!(
            "entry {anchor} was not found in session {session_id} - it may belong to another \
             session or predate the search index; re-run session_search to locate an entry in \
             this session"
        ))
    })
}

/// Builds a [`TranscriptWindow`] from loaded joined rows.
///
/// # Errors
///
/// Returns an error when a stored ordinal does not fit the platform's
/// `usize` — corruption is surfaced, never clamped to a fake 0.
fn build_window(
    session_id: &SessionId,
    title: Option<String>,
    total_entries: usize,
    joined: Vec<JoinedWindowEntry>,
) -> Result<TranscriptWindow, Report<SessionStoreError>> {
    let entries = joined
        .into_iter()
        .map(|row| {
            let ordinal = usize::try_from(row.ordinal)
                .change_context(SessionStoreError)
                .attach(format!(
                    "entry {} has ordinal {} exceeding the platform width",
                    row.joined.entry_id, row.ordinal
                ))?;
            let entry = entry_from_joined(&row.joined, Vec::new())?;
            let excluded = !entry.is_in_context();
            Ok(TranscriptEntry {
                ordinal,
                entry,
                excluded,
            })
        })
        .collect::<Result<Vec<_>, Report<SessionStoreError>>>()?;
    Ok(TranscriptWindow {
        session_id: session_id.clone(),
        title,
        total_entries,
        entries,
    })
}

/// Fetches a window of `context` entries starting just before the anchor,
/// clamped to the session's bounds.
async fn fetch_window(
    pool: &Pool,
    session_id: &SessionId,
    anchor: &ChatEntryId,
    context: usize,
) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
    let Some(meta) = session_meta(pool, session_id).await? else {
        return Ok(None);
    };
    let anchor_ord = anchor_ordinal(pool, session_id, anchor).await?;
    let total = count_entries(pool, session_id).await?;
    let (lo, hi) = clamp_window(anchor_ord, context);
    let joined = load_joined_range(pool, session_id, lo, hi).await?;
    Ok(Some(build_window(session_id, meta.title, total, joined)?))
}

/// Fetches the last `limit` entries of a session.
async fn fetch_tail(
    pool: &Pool,
    session_id: &SessionId,
    limit: usize,
) -> Result<Option<TranscriptWindow>, Report<SessionStoreError>> {
    let Some(meta) = session_meta(pool, session_id).await? else {
        return Ok(None);
    };
    let total = count_entries(pool, session_id).await?;
    let span = i64::try_from(limit.max(1)).unwrap_or(i64::MAX);
    let lo = (i64::from(i32::try_from(total).unwrap_or(i32::MAX)) - span).max(0);
    let joined = load_joined_range(pool, session_id, lo, i64::MAX).await?;
    Ok(Some(build_window(session_id, meta.title, total, joined)?))
}
