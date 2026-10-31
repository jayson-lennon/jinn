//! Per-path file locks — serialization for the file-mutating tools.
//!
//! `edit`, `write`, and `save_plan` are read-modify-write tools, and the
//! orchestrator runs every tool call in its own task, so one batch routinely
//! contains several calls against the same file. Without serialization the
//! calls race: two `edit`s both read the original content and the second
//! write clobbers the first, while the first still reports success.
//!
//! Holding a lock keyed by the resolved path across a tool's whole file
//! access — read through write — makes each call apply against the previous
//! one's result instead. The key is the lexically resolved absolute path,
//! with no canonicalization: two spellings of the same file are different
//! keys. Entries live for the process lifetime; one per distinct path ever
//! touched is bounded by real usage.
//!
//! The map is process-global and behind a sync mutex that is held only for
//! the get-or-insert, never across an await. The per-path locks themselves
//! are async, and the returned guard releases on drop — a cancelled or
//! timed-out tool task cannot strand it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use marked_path::{Absolute, MarkedPathBuf};
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::OwnedMutexGuard;

/// A held per-path lock. Released when dropped.
pub(crate) type PathLock = OwnedMutexGuard<()>;

/// The per-path lock table: one lazily-created async mutex per absolute path.
type LockMap = HashMap<MarkedPathBuf<Absolute>, Arc<AsyncMutex<()>>>;

/// The process-wide path → lock map.
static LOCKS: OnceLock<Mutex<LockMap>> = OnceLock::new();

/// Returns the map, initializing it on first use.
fn locks() -> &'static Mutex<LockMap> {
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Acquires the lock for `path`, one guard per caller.
///
/// Callers hold the guard across their entire file access — for `edit` from
/// the content read through the rename, for `write`/`save_plan` from
/// directory creation through the write — so no two tool calls can
/// interleave their reads and writes on one file.
pub(crate) async fn acquire(path: &MarkedPathBuf<Absolute>) -> PathLock {
    // Clone (or create) the entry's mutex under the sync map lock, then drop
    // the map lock before awaiting so no sync guard is ever held across an
    // await.
    let mutex = {
        let mut map = locks().lock();
        Arc::clone(map.entry(path.clone()).or_default())
    };
    mutex.lock_owned().await
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn same_path_acquires_serialize() {
        // Given a lock held for a path.
        let dir = jinn_testutil::tempdir();
        let path = dir
            .path
            .join("target.txt")
            .expect("temp root joins a bare name");
        let first = acquire(&path).await;

        // When a second acquire for the same path is attempted.
        let second = {
            let path = path.clone();
            tokio::spawn(async move { acquire(&path).await })
        };
        let raced = tokio::time::timeout(std::time::Duration::from_millis(50), second).await;

        // Then it does not complete while the first is held.
        assert!(
            raced.is_err(),
            "second acquire ran while the first was held"
        );

        // And it completes once the first guard is dropped.
        drop(first);
        let second = {
            let path = path.clone();
            tokio::spawn(async move { acquire(&path).await })
        };
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(1), second).await;
        assert!(acquired.is_ok(), "second acquire never ran after release");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn different_paths_acquire_independently() {
        // Given a lock held for one path.
        let dir = jinn_testutil::tempdir();
        let held = dir
            .path
            .join("held.txt")
            .expect("temp root joins a bare name");
        let other = dir
            .path
            .join("other.txt")
            .expect("temp root joins a bare name");
        let _first = acquire(&held).await;

        // When acquiring a different path.
        let acquired =
            tokio::time::timeout(std::time::Duration::from_secs(1), acquire(&other)).await;

        // Then it succeeds without waiting on the held lock.
        assert!(acquired.is_ok(), "unrelated path blocked on the held lock");
    }
}
