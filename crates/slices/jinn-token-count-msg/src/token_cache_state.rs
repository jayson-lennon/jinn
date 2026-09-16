//! Shared per-session, per-entry token-count cache.
//!
//! Entries are append-only and immutable in this codebase, so a token count
//! is a pure function of [`jinn_core_types::ChatEntryId`]. The cache is
//! keyed two-deep so that a session close can evict an entire session in
//! O(1).
//!
//! Consumers receive a clone of [`HistoryWorkerChatEntryTokenCache`] and
//! access it only via its named methods — the underlying [`DashMap`] is
//! never exposed. The token-count slice's eviction actor holds a separate
//! clone and removes a session's inner map when the session closes; the
//! session actor's accumulation gate and the prune workers hold clones for
//! their reads.
//!
//! Thread-safe via [`DashMap`]'s internal sharding. No `RwLock` needed.
//!
//! # Naming
//!
//! Named `HistoryWorkerChatEntryTokenCache` (deliberately long) to
//! distinguish it from the persisted `token_count` field on the chat
//! entry: history workers use this memoization cache for their evaluation
//! passes, while the entry field is the durable per-entry count.

use std::sync::Arc;

use dashmap::DashMap;
use jinn_core_types::chat_entry_id::ChatEntryId;
use jinn_core_types::session_id::SessionId;

use jinn_slices::SlotKey;

/// Shared per-session, per-entry token-count cache for history workers.
///
/// Construct with [`HistoryWorkerChatEntryTokenCache::new`]; clone cheaply
/// (inner state is `Arc`-shared) to hand to multiple consumers.
#[derive(Clone, Default)]
pub struct HistoryWorkerChatEntryTokenCache {
    inner: Arc<DashMap<SessionId, DashMap<ChatEntryId, u32>>>,
}

impl HistoryWorkerChatEntryTokenCache {
    /// Create an empty shared cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Look up a cached token count.
    ///
    /// Returns `Some(count)` if a count was previously inserted for this
    /// `(session_id, entry_id)` pair and the session has not since been
    /// evicted. Returns `None` otherwise.
    #[must_use]
    pub fn get(&self, session_id: &SessionId, entry_id: &ChatEntryId) -> Option<u32> {
        let session_map = self.inner.get(session_id)?;
        session_map.get(entry_id).map(|v| *v)
    }

    /// Insert a token count for a `(session_id, entry_id)` pair.
    ///
    /// Overwrites any existing count for the same pair. Callers should
    /// cast from the estimator's native `usize` via `tokens as u32`.
    pub fn insert(&self, session_id: SessionId, entry_id: ChatEntryId, count: u32) {
        self.inner
            .entry(session_id)
            .or_default()
            .insert(entry_id, count);
    }

    /// Look up a cached count, or compute and store one.
    ///
    /// `compute` is called at most once per `(session_id, entry_id)` pair
    /// across the lifetime of the session's inner map. After eviction of
    /// the session via [`HistoryWorkerChatEntryTokenCache::remove_session`],
    /// a subsequent call for the same pair will re-invoke `compute`.
    ///
    /// Note: `compute` runs under a [`DashMap`] shard guard briefly. For
    /// our use case (cache hit rate → 100% after first snapshot, and
    /// tiktoken counting is microseconds) this is acceptable. If it ever
    /// becomes a hotspot, switch to a probe-then-insert pattern.
    pub fn get_or_insert_with<F>(
        &self,
        session_id: &SessionId,
        entry_id: &ChatEntryId,
        compute: F,
    ) -> u32
    where
        F: FnOnce() -> u32,
    {
        let session_id = session_id.clone();
        let entry_id = entry_id.clone();
        // outer entry() holds the outer shard guard briefly;
        // or_insert_with builds the inner DashMap only on first call.
        let session_map = self.inner.entry(session_id).or_default();

        // inner entry() holds the inner shard guard briefly;
        // or_insert_with runs the closure only on first call for this key.
        *session_map.entry(entry_id).or_insert_with(compute)
    }

    /// Evict all cached counts for a session.
    ///
    /// Called by the token-count slice's eviction actor on session close.
    /// Safe to call for a session that has no cached entries (no-op).
    /// Safe to call concurrently with `get` / `insert` / `get_or_insert_with`
    /// — [`DashMap`] operations are atomic per shard.
    pub fn remove_session(&self, session_id: &SessionId) {
        self.inner.remove(session_id);
    }
}

/// The slot key the token-count slice's cache cell lives under.
#[must_use]
pub fn token_cache_slot() -> SlotKey {
    SlotKey::builtin("token-count", "cache")
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// Deterministic session id for tests (SessionId is a Uuid newtype).
    fn test_session_id(n: u8) -> SessionId {
        serde_json::from_str(&format!("\"10000000-0000-0000-0000-{n:012x}\""))
            .expect("valid SessionId JSON")
    }

    /// Deterministic entry id for tests (ChatEntryId is a Uuid newtype).
    fn test_entry_id(n: u8) -> ChatEntryId {
        serde_json::from_str(&format!("\"00000000-0000-0000-0000-{n:012x}\""))
            .expect("valid ChatEntryId JSON")
    }

    #[rstest::rstest]
    #[test]
    fn new_cache_returns_none_for_get() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        assert_eq!(cache.get(&s, &e), None);
    }

    #[rstest::rstest]
    #[test]
    fn default_cache_returns_none_for_get() {
        let cache = HistoryWorkerChatEntryTokenCache::default();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        assert_eq!(cache.get(&s, &e), None);
    }

    #[rstest::rstest]
    #[test]
    fn insert_then_get_returns_value() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        cache.insert(s.clone(), e.clone(), 42);
        assert_eq!(cache.get(&s, &e), Some(42));
    }

    #[rstest::rstest]
    #[test]
    fn insert_overwrites_previous_value() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        cache.insert(s.clone(), e.clone(), 42);
        cache.insert(s.clone(), e.clone(), 99);
        assert_eq!(cache.get(&s, &e), Some(99));
    }

    #[rstest::rstest]
    #[test]
    fn get_or_insert_with_invokes_closure_on_first_call() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let result = cache.get_or_insert_with(&s, &e, move || {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            7
        });
        assert_eq!(result, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[rstest::rstest]
    #[test]
    fn get_or_insert_with_does_not_reinvoke_on_second_call() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_a = calls.clone();
        let first = cache.get_or_insert_with(&s, &e, move || {
            calls_a.fetch_add(1, Ordering::SeqCst);
            10
        });
        let calls_b = calls.clone();
        let second = cache.get_or_insert_with(&s, &e, move || {
            calls_b.fetch_add(1, Ordering::SeqCst);
            999 // should not be returned
        });

        assert_eq!(first, 10);
        assert_eq!(second, 10);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[rstest::rstest]
    #[test]
    fn get_or_insert_with_distinguishes_entries_within_session() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e1 = test_entry_id(1);
        let e2 = test_entry_id(2);

        let v1 = cache.get_or_insert_with(&s, &e1, || 100);
        let v2 = cache.get_or_insert_with(&s, &e2, || 200);

        assert_eq!(v1, 100);
        assert_eq!(v2, 200);
        assert_eq!(cache.get(&s, &e1), Some(100));
        assert_eq!(cache.get(&s, &e2), Some(200));
    }

    #[rstest::rstest]
    #[test]
    fn get_or_insert_with_distinguishes_sessions() {
        // ChatEntryId uniqueness is global, so this is a defensive test —
        // the same ChatEntryId in two sessions must produce independent
        // counts because the outer key differs.
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s1 = test_session_id(1);
        let s2 = test_session_id(2);
        let e = test_entry_id(0);

        let v1 = cache.get_or_insert_with(&s1, &e, || 11);
        let v2 = cache.get_or_insert_with(&s2, &e, || 22);

        assert_eq!(v1, 11);
        assert_eq!(v2, 22);
        assert_eq!(cache.get(&s1, &e), Some(11));
        assert_eq!(cache.get(&s2, &e), Some(22));
    }

    #[rstest::rstest]
    #[test]
    fn remove_session_evicts_inner_map() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        let e = test_entry_id(0);

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_a = calls.clone();
        cache.get_or_insert_with(&s, &e, move || {
            calls_a.fetch_add(1, Ordering::SeqCst);
            5
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        cache.remove_session(&s);
        assert_eq!(cache.get(&s, &e), None);

        // Re-insert after eviction must invoke closure again.
        let calls_b = calls.clone();
        let v = cache.get_or_insert_with(&s, &e, move || {
            calls_b.fetch_add(1, Ordering::SeqCst);
            9
        });
        assert_eq!(v, 9);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[rstest::rstest]
    #[test]
    fn remove_session_is_noop_if_session_not_present() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s = test_session_id(0);
        // Must not panic.
        cache.remove_session(&s);
        assert_eq!(cache.get(&s, &test_entry_id(0)), None);
    }

    #[rstest::rstest]
    #[test]
    fn remove_session_does_not_affect_other_sessions() {
        let cache = HistoryWorkerChatEntryTokenCache::new();
        let s1 = test_session_id(1);
        let s2 = test_session_id(2);
        let e = test_entry_id(0);

        cache.insert(s1.clone(), e.clone(), 111);
        cache.insert(s2.clone(), e.clone(), 222);

        cache.remove_session(&s1);

        assert_eq!(cache.get(&s1, &e), None);
        assert_eq!(cache.get(&s2, &e), Some(222));
    }

    #[rstest::rstest]
    #[test]
    fn clone_shares_underlying_state() {
        let cache_a = HistoryWorkerChatEntryTokenCache::new();
        let cache_b = cache_a.clone();

        let s = test_session_id(0);
        let e = test_entry_id(0);
        cache_a.insert(s.clone(), e.clone(), 50);

        // Read via the clone — shared state.
        assert_eq!(cache_b.get(&s, &e), Some(50));

        // Mutate via the clone — visible to the original.
        cache_b.remove_session(&s);
        assert_eq!(cache_a.get(&s, &e), None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn concurrent_get_or_insert_with_invokes_closure_once() {
        // N tasks all race on the same (session_id, entry_id) pair. The
        // closure increments an AtomicUsize and sleeps briefly to widen the
        // race window. Assertion: closure ran exactly once across all
        // tasks, and every task observed the same value.
        const N: usize = 32;
        let cache = Arc::new(HistoryWorkerChatEntryTokenCache::new());
        let s = Arc::new(test_session_id(0));
        let e = Arc::new(test_entry_id(0));
        let calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let cache = Arc::clone(&cache);
            let s = Arc::clone(&s);
            let e = Arc::clone(&e);
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                cache.get_or_insert_with(&s, &e, move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Widen the race window so concurrent callers actually
                    // contend on the shard guard.
                    std::thread::sleep(Duration::from_millis(5));
                    777
                })
            }));
        }

        let mut results = Vec::with_capacity(N);
        for h in handles {
            results.push(h.await.expect("task did not panic"));
        }

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "closure must run exactly once"
        );
        assert!(
            results.iter().all(|&v| v == 777),
            "all tasks must observe 777"
        );
    }
}
