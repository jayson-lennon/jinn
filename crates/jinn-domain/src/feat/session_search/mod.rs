//! Cross-session content search over the FTS index.
//!
//! This feature owns the data model and plumbing for `session_search` /
//! `session_fetch`: the query/answer types that cross the session-store
//! boundary, the entry-kind → (role, body) extraction used when (re)indexing,
//! and the background actor that drains the dirty-marker table.
//!
//! The index itself is the `session_fts` FTS5 virtual table created by schema
//! v26, keyed by `(session_id, entry_id)`. Freshness is asynchronous: triggers
//! on `sessions` mark sessions dirty and [`SearchIndexActor`] recomputes each
//! dirty session's rows every few seconds, so results may trail the newest
//! saves by one poll interval.

mod extract;
pub mod model;

pub use extract::{SearchableEntry, entry_ts_key, extract_searchable};
pub use model::{
    SearchHit, SearchOutcome, SearchParams, SearchableRole, TranscriptEntry, TranscriptWindow,
};
