//! Cache for rendered skill-preview lines.
//!
//! The skill picker re-renders the selected skill's full markdown body — including
//! tree-sitter syntax highlighting — on every render frame. That is expensive
//! enough to noticeably slow down rendering. This cache stores the already-rendered
//! `Line` vectors so repeated frames (and back-and-forth navigation between skills)
//! skip the markdown render entirely.
//!
//! Mirrors the shape of [`SessionPreviewCache`] but keys on `(body_hash, width)`
//! because rendered output depends only on the skill body and the wrap width —
//! never on the session viewing it. A content hash (rather than the skill name)
//! means changed bodies and project/global shadowing of the same name produce
//! different keys, so the cache is safe across sessions and rescans without any
//! explicit invalidation on the scan path.
//!
//! Cache invalidation:
//! - **Theme change** (`FrontendCaches::invalidate_all`): rendered lines embed
//!   theme colors → cleared.
//! - **Rescan** (the session-init discovery worker): NOT cleared. A changed body hashes to a new
//!   key, so stale markdown is never redisplayed.
//! - **Picker open/close**: cache is preserved so the user does not pay a
//!   re-render cost when reopening the picker.
//!
//! [`SessionPreviewCache`]: jinn_slices::SessionPreviewCache

use parking_lot::Mutex;
use std::collections::HashMap;

use jinn_selection_widget::PreviewCache;
use ratatui::text::Line;

/// Cache for skill-preview rendered lines.
///
/// Keyed by `(body_hash, content_width)` so that:
/// - Editing a skill's body produces a cache miss (different content hash).
/// - Switching skills usually produces a cache miss (different body).
/// - Terminal resize produces a cache miss (different width).
/// - Sessions with different cwds shadowing a same-named skill never collide
///   (different bodies hash differently).
///
/// Interior mutability ([`parking_lot::Mutex`]) is used because the [`PreviewCache`] trait
/// methods take `&self` — the cache is borrowed immutably (`Option<&dyn PreviewCache>`)
/// as it is threaded through the widget's render pipeline. The standalone `clear` method
/// takes `&mut self` (matching `SessionPreviewCache`), so `invalidate_all` acquires a
/// write lock for clarity and consistency.
///
/// NOTE: currently using unbounded memory. Revisit if memory consumption becomes a problem.
///
/// [`FrontendCaches`]: crate::feat::ui::frontend_state::FrontendCaches
#[derive(Debug, Default)]
pub struct SkillPreviewCache {
    entries: Mutex<HashMap<(u64, usize), Vec<Line<'static>>>>,
}

impl SkillPreviewCache {
    /// Creates a new empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Clears all cached preview lines.
    ///
    /// Called when the active theme changes (via `FrontendCaches::invalidate_all`)
    /// so preview popups re-render with the new colors. Takes `&self` — the
    /// cache is shared as an `Arc` handle with the picker host, so clearing
    /// must work through the shared reference.
    pub fn clear(&self) {
        self.entries.lock().clear();
    }

    /// Returns the number of cached entries (for testing).
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// Returns `true` if the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }
}

impl PreviewCache for SkillPreviewCache {
    fn get(&self, key: &str, width: usize) -> Option<Vec<Line<'static>>> {
        // The key is the decimal body hash produced by `SkillEntry::cache_key`.
        let hash: u64 = key.parse().ok()?;
        self.entries.lock().get(&(hash, width)).cloned()
    }

    /// NOTE: currently using unbounded memory. Revisit if memory consumption becomes a problem.
    fn insert(&self, key: String, width: usize, lines: Vec<Line<'static>>) {
        // The key is the decimal body hash produced by `SkillEntry::cache_key`.
        if let Ok(hash) = key.parse::<u64>() {
            self.entries.lock().insert((hash, width), lines);
        }
    }
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
    use super::*;
    use ratatui::text::Line;

    /// Hashes a body the same way `SkillEntry::cache_key` does, for tests.
    fn body_key(body: &str) -> String {
        crate::feat::skills::skill_entry::body_hash_key(body)
    }

    fn line(s: &str) -> Line<'static> {
        Line::from(s.to_owned())
    }

    #[rstest::rstest]
    #[test]
    fn get_on_empty_cache_returns_none() {
        let cache = SkillPreviewCache::new();
        assert!(cache.get(&body_key("any body"), 80).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn insert_then_get_returns_stored_lines() {
        let cache = SkillPreviewCache::new();
        cache.insert(body_key("# bash"), 80, vec![line("rendered bash preview")]);
        let got = cache
            .get(&body_key("# bash"), 80)
            .expect("entry should exist");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].spans.len(), 1);
        assert_eq!(got[0].spans[0].content, "rendered bash preview");
    }

    #[rstest::rstest]
    #[test]
    fn width_is_part_of_the_key() {
        let cache = SkillPreviewCache::new();
        cache.insert(body_key("# rust"), 80, vec![line("width 80")]);
        // Same body, different width -> miss.
        assert!(cache.get(&body_key("# rust"), 100).is_none());
        // Insert at the new width.
        cache.insert(body_key("# rust"), 100, vec![line("width 100")]);
        // Both widths now hit.
        assert!(cache.get(&body_key("# rust"), 80).is_some());
        assert!(cache.get(&body_key("# rust"), 100).is_some());
    }

    #[rstest::rstest]
    #[test]
    fn different_bodies_are_independent() {
        let cache = SkillPreviewCache::new();
        cache.insert(body_key("# alpha"), 80, vec![line("a")]);
        // beta's body is not cached.
        assert!(cache.get(&body_key("# beta"), 80).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn clear_empties_all_entries() {
        let cache = SkillPreviewCache::new();
        cache.insert(body_key("# a"), 80, vec![line("a")]);
        cache.insert(body_key("# b"), 100, vec![line("b")]);
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
        assert!(cache.get(&body_key("# a"), 80).is_none());
        assert!(cache.get(&body_key("# b"), 100).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn get_returns_an_owned_clone_not_a_reference() {
        // The PreviewCache trait returns owned Vec<Line>, so callers can hold
        // the result across the cache being mutated.
        let cache = SkillPreviewCache::new();
        cache.insert(body_key("# k"), 80, vec![line("v")]);
        let got = cache.get(&body_key("# k"), 80).expect("entry should exist");
        cache.clear();
        // The clone survives the clear.
        assert_eq!(got.len(), 1);
    }
}
