//! The session-store slice — SQLite session persistence and the FTS
//! search index.
//!
//! Owns the SQLite-backed [`SqliteSessionStore`] (the production
//! [`jinn_domain::SessionStore`](jinn_domain::feat::session::SessionStore)
//! implementation) and the schema migrator, plus the background
//! search-index maintenance actor that drains the durable `fts_dirty`
//! marker table into the `session_fts` index.
//!
//! Kernel dependency (see Cargo.toml): transitional and justified — the
//! trait seam (`SessionStoreService`) and the session state vocabulary the
//! store persists live in `jinn-domain` for now.

pub mod migrator;
pub mod search_index_actor;
pub mod sqlite;

#[cfg(test)]
mod search_index_actor_tests;
#[cfg(test)]
mod sqlite_tests;
