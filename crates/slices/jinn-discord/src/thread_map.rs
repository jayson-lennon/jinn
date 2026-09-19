//! Thread → session mapping persistence for the Discord bot frontend.
//!
//! The Discord bot associates each forum thread with exactly one jinn session
//! so a thread can resume its session across bot restarts (and auto-un-archive
//! on inbound message). This module owns the `discord_thread` table created by
//! migration v21; its columns are independent of the session schema proper.
//!
//! Backed by the same `daow::Pool` as the session store (the table lives in
//! `sessions.db`), but as a separate DAO so the discord layer's reads/writes
//! don't entangle with session-lifecycle writes.

use async_trait::async_trait;
use daow::{Entity, Pool, dao};

/// A row of the `discord_thread` mapping table.
///
/// `thread_id` is the Discord forum-thread id (stringified snowflake).
/// `session_id` is the jinn `SessionId` (stringified).
/// `guild_id` is nullable because DM threads have no guild.
/// `created_at` is unix seconds (bot-owned convention; the session tables use
/// TEXT timestamps, but this table is self-contained).
#[derive(Debug, Clone, Entity)]
#[dao(table = "discord_thread")]
pub(crate) struct DiscordThreadRow {
    #[dao(pk)]
    thread_id: String,
    session_id: String,
    guild_id: Option<String>,
    created_at: i64,
}

/// Queries for the `discord_thread` table, validated against the post-v21
/// schema at compile time by the `#[dao]` macro.
#[dao]
#[async_trait]
trait DiscordThreadDao {
    #[execute(
        "INSERT OR REPLACE INTO discord_thread (thread_id, session_id, guild_id, created_at) \
         VALUES (?, ?, ?, ?)"
    )]
    async fn upsert(
        &self,
        thread_id: String,
        session_id: String,
        guild_id: Option<String>,
        created_at: i64,
    ) -> daow::Result<daow::ExecuteResult>;

    #[query(
        "SELECT thread_id, session_id, guild_id, created_at FROM discord_thread WHERE thread_id = ?"
    )]
    async fn session_by_thread(&self, thread_id: String) -> daow::Result<Option<DiscordThreadRow>>;

    #[query(
        "SELECT thread_id, session_id, guild_id, created_at FROM discord_thread WHERE session_id = ?"
    )]
    async fn row_by_session(&self, session_id: String) -> daow::Result<Option<DiscordThreadRow>>;
}

use error_stack::{Report, ResultExt as _};
use wherror::Error;

/// Error type for `discord_thread` table operations.
#[derive(Debug, Error)]
#[error(debug)]
pub struct DiscordThreadMapError;

/// The discord side of a thread↔session mapping.
#[derive(Debug, Clone)]
pub struct ThreadMapping {
    /// Discord forum-thread id (stringified snowflake).
    pub thread_id: String,
    /// Jinn session id bound to this thread.
    pub session_id: String,
    /// Guild the thread lives in (`None` for DM threads).
    pub guild_id: Option<String>,
    /// When the mapping was first recorded (unix seconds).
    pub created_at: i64,
}

/// DAO-backed access to the `discord_thread` mapping table.
///
/// Wraps the same `daow::Pool` as the session store. Methods map onto the
/// auto-generated [`DiscordThreadDao`] and attach `error_stack` context.
#[derive(Clone)]
pub struct DiscordThreadMap {
    pool: Pool,
}

impl DiscordThreadMap {
    /// Create a new map bound to a connection pool.
    #[must_use]
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Record (or overwrite) the thread→session mapping.
    ///
    /// `INSERT OR REPLACE` keyed on `thread_id`: re-running `/new` in an
    /// existing thread rebinds it to a fresh session.
    ///
    /// # Errors
    ///
    /// Returns [`DiscordThreadMapError`] if the write fails.
    pub async fn set(
        &self,
        thread_id: &str,
        session_id: &str,
        guild_id: Option<&str>,
        created_at: i64,
    ) -> Result<(), Report<DiscordThreadMapError>> {
        let dao = DiscordThreadDao::new(self.pool.clone());
        dao.upsert(
            thread_id.to_owned(),
            session_id.to_owned(),
            guild_id.map(str::to_owned),
            created_at,
        )
        .await
        .change_context(DiscordThreadMapError)
        .attach("failed to set discord_thread mapping")?;
        Ok(())
    }

    /// Forward lookup: which session id is this thread bound to?
    ///
    /// # Errors
    ///
    /// Returns [`DiscordThreadMapError`] if the read fails.
    pub async fn get_session_by_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<String>, Report<DiscordThreadMapError>> {
        let dao = DiscordThreadDao::new(self.pool.clone());
        let row = dao
            .session_by_thread(thread_id.to_owned())
            .await
            .change_context(DiscordThreadMapError)
            .attach("failed to read discord_thread mapping")?
            .map(|r| r.session_id);
        Ok(row)
    }

    /// Reverse lookup: which thread is bound to this session?
    ///
    /// # Errors
    ///
    /// Returns [`DiscordThreadMapError`] if the read fails.
    pub async fn get_thread_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<ThreadMapping>, Report<DiscordThreadMapError>> {
        let dao = DiscordThreadDao::new(self.pool.clone());
        let row = dao
            .row_by_session(session_id.to_owned())
            .await
            .change_context(DiscordThreadMapError)
            .attach("failed to read discord_thread mapping")?
            .map(ThreadMapping::from);
        Ok(row)
    }
}

impl std::fmt::Debug for DiscordThreadMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordThreadMap")
            .field("backend", &"daow::Pool<sqlite>")
            .finish()
    }
}

impl From<DiscordThreadRow> for ThreadMapping {
    fn from(row: DiscordThreadRow) -> Self {
        Self {
            thread_id: row.thread_id,
            session_id: row.session_id,
            guild_id: row.guild_id,
            created_at: row.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "test code: assertion helpers are fine to panic"
    )]

    use super::DiscordThreadMap;
    use jinn_session_store::sqlite::SqliteSessionStore;
    use tempfile::TempDir;

    async fn make_map() -> (TempDir, DiscordThreadMap) {
        let dir = TempDir::new().expect("temp dir");
        let store = SqliteSessionStore::new_in(dir.path()).await.expect("store");
        let map = DiscordThreadMap::new(store.pool().clone());
        (dir, map)
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dao_set_then_forward_lookup_returns_session_id() {
        // Given an empty map with one mapping inserted.
        let (_dir, map) = make_map().await;
        map.set("thread-1", "session-1", Some("guild-1"), 1_700_000_000)
            .await
            .expect("set");

        // When doing the forward lookup.
        let session = map.get_session_by_thread("thread-1").await.expect("lookup");

        // Then the stored session id is returned.
        assert_eq!(session.as_deref(), Some("session-1"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dao_set_then_reverse_lookup_returns_mapping() {
        // Given a map with one mapping.
        let (_dir, map) = make_map().await;
        map.set("thread-1", "session-1", Some("guild-1"), 1_700_000_000)
            .await
            .expect("set");

        // When doing the reverse lookup.
        let mapping = map
            .get_thread_by_session("session-1")
            .await
            .expect("lookup");

        // Then the full mapping is returned.
        let mapping = mapping.expect("mapping exists");
        assert_eq!(mapping.thread_id, "thread-1");
        assert_eq!(mapping.session_id, "session-1");
        assert_eq!(mapping.guild_id.as_deref(), Some("guild-1"));
        assert_eq!(mapping.created_at, 1_700_000_000);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dao_unknown_thread_returns_none() {
        // Given an empty map.
        let (_dir, map) = make_map().await;

        // When looking up a thread that was never recorded.
        let session = map
            .get_session_by_thread("never-seen")
            .await
            .expect("lookup");

        // Then None is returned.
        assert!(session.is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn dao_set_rebinds_existing_thread_to_new_session() {
        // Given a thread already mapped to session-1.
        let (_dir, map) = make_map().await;
        map.set("thread-1", "session-1", None, 1)
            .await
            .expect("first set");

        // When re-running set for the same thread with a new session.
        map.set("thread-1", "session-2", None, 2)
            .await
            .expect("rebind");

        // Then the forward lookup returns the new session.
        let session = map.get_session_by_thread("thread-1").await.expect("lookup");
        assert_eq!(session.as_deref(), Some("session-2"));
    }
}
