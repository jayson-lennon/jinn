//! Line count cache for virtualized chat log rendering.
//!
//! Caches the wrapped line count *and rendered lines* per entry so the renderer
//! can cheaply determine which entries are visible without calling
//! `entry_to_lines()` for the entire history. On a cache hit, the pre-rendered
//! `Vec<Line>` is reused in Pass 2 - skipping both parsing and rendering.
//!
//! The cache is invalidated on content changes (streaming tokens),
//! expand/collapse toggles, content width changes (terminal resize), and
//! render-variant changes (status-derived look that alters the rendered lines
//! without touching the entry's content — e.g. a paired tool result landing
//! or a subagent's running state flipping).
//! Theme changes are handled centrally by [`FrontendCaches::invalidate_all`]
//! which calls [`EntryLineCache::clear`] directly.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::text::Line;

use crate::protocol::{ChatEntry, ChatEntryId};

/// Cached wrapped line count and rendered lines for a single entry.
#[derive(Debug, Clone)]
pub struct CachedEntryCount {
    /// Fingerprint of the entry's content when this count was computed.
    pub fingerprint: u64,
    /// Whether the entry was expanded when this count was computed.
    pub is_expanded: bool,
    /// Hash of the status-derived render inputs (paired result status,
    /// streaming flag, subagent-waiting flag) at compute time.
    pub variant: u64,
    /// The wrapped line count for this entry.
    pub wrapped_count: u16,
    /// Pre-rendered lines for this entry, if available.
    ///
    /// `None` when inserted via [`EntryLineCache::insert`] (count-only).
    /// `Some` when inserted via [`EntryLineCache::insert_with_lines`].
    #[expect(
        clippy::rc_buffer,
        reason = "Vec<Line> not Send, Arc used for cheap clone within same thread"
    )]
    pub lines: Option<Arc<Vec<Line<'static>>>>,
}

/// Result of a successful cache hit.
pub struct CacheHit {
    /// The wrapped line count for this entry.
    pub wrapped_count: u16,
    /// Pre-rendered lines for this entry, if they were cached.
    #[expect(
        clippy::rc_buffer,
        reason = "Vec<Line> not Send, Arc used for cheap clone within same thread"
    )]
    pub lines: Option<Arc<Vec<Line<'static>>>>,
}

/// Cache mapping entry IDs to their cached wrapped line counts and rendered lines.
///
/// Owned by [`FrontendCaches`] - populated during the render pass, used
/// to determine which entries overlap the viewport without re-rendering
/// the entire history.
///
/// # Invalidation
///
/// - **Content width change:** clears all entries.
/// - **Theme change:** cleared by [`FrontendCaches::invalidate_all`].
/// - **Streaming (content change):** detected by fingerprint mismatch → automatic miss.
/// - **Expand/collapse:** detected by `is_expanded` mismatch → automatic miss.
/// - **New entry:** no cache entry exists → automatic miss.
#[derive(Debug, Clone, Default)]
pub struct EntryLineCache {
    /// The content width used when cache entries were computed.
    /// If the current width differs, the entire cache is invalid.
    content_width: Option<u16>,
    /// Per-entry cached counts.
    entries: HashMap<ChatEntryId, CachedEntryCount>,
}

impl EntryLineCache {
    /// Create a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the cached wrapped line count and optional rendered lines for an entry.
    ///
    /// Returns `None` if:
    /// - No cache entry exists for this ID (new entry).
    /// - The entry's fingerprint has changed (content changed during streaming).
    /// - The entry's expanded state has changed (expand/collapse toggle).
    /// - The entry's render variant has changed (status-derived look changed).
    /// - The content width has changed (terminal resize).
    pub fn get(
        &mut self,
        entry: &ChatEntry,
        is_expanded: bool,
        variant: u64,
        content_width: u16,
    ) -> Option<CacheHit> {
        // If content width changed, clear everything.
        if self.content_width != Some(content_width) {
            self.entries.clear();
            self.content_width = Some(content_width);
            return None;
        }

        let cached = self.entries.get(&entry.id)?;
        (cached.fingerprint == entry.content_fingerprint()
            && cached.is_expanded == is_expanded
            && cached.variant == variant)
            .then(|| CacheHit {
                wrapped_count: cached.wrapped_count,
                lines: cached.lines.clone(),
            })
    }

    /// Store a wrapped line count for an entry (without rendered lines).
    pub fn insert(
        &mut self,
        entry: &ChatEntry,
        is_expanded: bool,
        variant: u64,
        content_width: u16,
        wrapped_count: u16,
    ) {
        self.sync_invalidation(content_width);
        self.entries.insert(
            entry.id.clone(),
            CachedEntryCount {
                fingerprint: entry.content_fingerprint(),
                is_expanded,
                variant,
                wrapped_count,
                lines: None,
            },
        );
    }

    #[expect(
        clippy::rc_buffer,
        reason = "Vec<Line> not Send, Arc used for cheap clone within same thread"
    )]
    /// Store a wrapped line count and rendered lines for an entry.
    pub fn insert_with_lines(
        &mut self,
        entry: &ChatEntry,
        is_expanded: bool,
        variant: u64,
        content_width: u16,
        wrapped_count: u16,
        lines: Arc<Vec<Line<'static>>>,
    ) {
        self.sync_invalidation(content_width);
        self.entries.insert(
            entry.id.clone(),
            CachedEntryCount {
                fingerprint: entry.content_fingerprint(),
                is_expanded,
                variant,
                wrapped_count,
                lines: Some(lines),
            },
        );
    }

    /// Synchronize invalidation state: clear cache if content width has changed.
    fn sync_invalidation(&mut self, content_width: u16) {
        if self.content_width != Some(content_width) {
            self.entries.clear();
            self.content_width = Some(content_width);
        }
    }

    /// Remove a specific entry from the cache.
    pub fn invalidate_entry(&mut self, id: &ChatEntryId) {
        self.entries.remove(id);
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.content_width = None;
    }

    /// Number of entries currently cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
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
    use crate::protocol::ToolResultStatus;
    use crate::protocol::{ChatEntry, ChatEntryKind};

    #[rstest::rstest]
    fn cache_hit_returns_count() {
        // Given an entry and a cache with its count.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 0, 80, 5);

        // When looking up the same entry.
        let result = cache.get(&entry, false, 0, 80);

        // Then the cached count is returned.
        assert_eq!(result.map(|h| h.wrapped_count), Some(5));
    }

    #[rstest::rstest]
    fn cache_miss_on_new_entry() {
        // Given an empty cache.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");

        // When looking up an uncached entry.
        let result = cache.get(&entry, false, 0, 80);

        // Then None is returned.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn cache_miss_on_content_change() {
        // Given a cache with an entry's count.
        let mut cache = EntryLineCache::new();
        let mut entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 0, 80, 5);

        // When the entry's content changes.
        if let crate::protocol::ChatEntryKind::Assistant(ref mut text) = entry.kind {
            text.push_str(" world");
        }

        // Then the cache misses (fingerprint mismatch).
        let result = cache.get(&entry, false, 0, 80);
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn cache_miss_on_expanded_change() {
        // Given a cache with an entry at is_expanded=false.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 0, 80, 5);

        // When looking up with is_expanded=true.
        let result = cache.get(&entry, true, 0, 80);

        // Then the cache misses.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn cache_cleared_on_content_width_change() {
        // Given a cache with entries at width 80.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 0, 80, 5);

        // When looking up at width 100.
        let result = cache.get(&entry, false, 0, 100);

        // Then the cache misses (and is cleared).
        assert!(result.is_none());
        assert!(cache.is_empty());
    }

    #[rstest::rstest]
    fn invalidate_entry_removes_specific_entry() {
        // Given a cache with two entries.
        let mut cache = EntryLineCache::new();
        let entry1 = ChatEntry::assistant("hello");
        let entry2 = ChatEntry::assistant("world");
        cache.insert(&entry1, false, 0, 80, 3);
        cache.insert(&entry2, false, 0, 80, 5);

        // When invalidating entry1.
        cache.invalidate_entry(&entry1.id);

        // Then entry1 is gone but entry2 remains.
        assert!(cache.get(&entry1, false, 0, 80).is_none());
        assert_eq!(
            cache.get(&entry2, false, 0, 80).map(|h| h.wrapped_count),
            Some(5)
        );
    }

    #[rstest::rstest]
    fn clear_removes_all_entries() {
        // Given a cache with entries.
        let mut cache = EntryLineCache::new();
        cache.insert(&ChatEntry::assistant("hello"), false, 0, 80, 3);

        // When clearing.
        cache.clear();

        // Then the cache is empty.
        assert!(cache.is_empty());
    }

    #[rstest::rstest]
    fn fingerprint_stable_for_same_content() {
        // Given two entries with the same content.
        let entry1 = ChatEntry::assistant("hello");
        let entry2 = ChatEntry::assistant("hello");

        // Then their fingerprints match.
        assert_eq!(entry1.content_fingerprint(), entry2.content_fingerprint());
    }

    #[rstest::rstest]
    fn fingerprint_differs_for_different_content() {
        // Given two entries with different content.
        let entry1 = ChatEntry::assistant("hello");
        let entry2 = ChatEntry::assistant("world");

        // Then their fingerprints differ.
        assert_ne!(entry1.content_fingerprint(), entry2.content_fingerprint());
    }

    #[rstest::rstest]
    fn fingerprint_differs_for_different_kinds() {
        // Given entries of different kinds with same text.
        let assistant = ChatEntry::assistant("hello");
        let system = ChatEntry::system("hello");

        // Then their fingerprints differ.
        assert_ne!(
            assistant.content_fingerprint(),
            system.content_fingerprint()
        );
    }

    #[rstest::rstest]
    fn cache_hit_on_pending_tool_result_when_fingerprint_matches() {
        // Given a cache with a pending ToolResult entry.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::tool_result("id", "bash", "", ToolResultStatus::Pending);
        cache.insert(&entry, false, 0, 80, 5);

        // When looking up the pending entry with unchanged content.
        let result = cache.get(&entry, false, 0, 80);

        // Then the cached count is returned (pending entries are cacheable).
        assert_eq!(result.map(|h| h.wrapped_count), Some(5));
    }

    #[rstest::rstest]
    fn cache_miss_on_pending_tool_result_content_change() {
        // Given a cache with a pending ToolResult entry.
        let mut cache = EntryLineCache::new();
        let mut entry = ChatEntry::tool_result("id", "bash", "output", ToolResultStatus::Pending);
        cache.insert(&entry, false, 0, 80, 5);

        // When the entry's content changes (simulating tool output growth).
        if let ChatEntryKind::ToolResult {
            ref mut content, ..
        } = entry.kind
        {
            content.push_str(" more");
        }

        let result = cache.get(&entry, false, 0, 80);

        // Then the cache misses (fingerprint mismatch).
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn cache_stores_and_returns_lines() {
        // Given an entry inserted with rendered lines.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        let lines = Arc::new(vec![Line::from("hello")]);
        cache.insert_with_lines(&entry, false, 0, 80, 1, lines.clone());

        // When looking up the same entry.
        let result = cache.get(&entry, false, 0, 80);

        // Then the cached lines are returned.
        let hit = result.expect("should be a cache hit");
        assert_eq!(hit.wrapped_count, 1);
        let cached_lines = hit.lines.expect("should have cached lines");
        assert_eq!(*cached_lines, *lines);
    }

    #[rstest::rstest]
    fn cache_hit_without_lines_returns_none_lines() {
        // Given an entry inserted via insert() (no lines).
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 0, 80, 5);

        // When looking up the same entry.
        let result = cache.get(&entry, false, 0, 80);

        // Then the count is returned but lines is None.
        let hit = result.expect("should be a cache hit");
        assert_eq!(hit.wrapped_count, 5);
        assert!(hit.lines.is_none());
    }

    #[rstest::rstest]
    fn cache_hit_when_variant_unchanged() {
        // Given a cache with an entry under variant 7.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 7, 80, 5);

        // When looking up with the same variant.
        let result = cache.get(&entry, false, 7, 80);

        // Then the cached count is returned.
        assert_eq!(result.map(|h| h.wrapped_count), Some(5));
    }

    #[rstest::rstest]
    fn cache_miss_when_variant_changes() {
        // Given a cache with an entry under variant 7.
        let mut cache = EntryLineCache::new();
        let entry = ChatEntry::assistant("hello");
        cache.insert(&entry, false, 7, 80, 5);

        // When looking up with a different variant.
        let result = cache.get(&entry, false, 8, 80);

        // Then the cache misses.
        assert!(result.is_none());
    }
}
