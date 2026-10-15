//! Project scope resolution — which past sessions belong to a project anchor.
//!
//! The v1 default ([`CwdTreeScopeResolver`]) returns the anchor session's CWD
//! descendants plus its fork-ancestor chain, within a day window. Sibling and
//! out-of-tree sessions are excluded. The resolver is a pure function over a
//! slice of [`CandidateSession`] so it is trivially testable and so a future
//! session-picker impl can replace it without touching downstream consumers.

use std::path::{Component, Path, PathBuf};

use jinn_core_types::SessionId;

/// Resolves the set of past session ids that belong to a project.
///
/// This trait is the seam for a future session picker: v1's default impl returns
/// a CWD-tree + fork-ancestor chain within a window; a picker impl returns the
/// user's fuzzy selections instead. Downstream code consumes `Vec<SessionId>`
/// and is resolver-agnostic.
pub trait ProjectScopeResolver {
    /// Returns the ids of `candidates` that belong to the project corpus.
    fn resolve(&self, anchor: &ScopeAnchor, candidates: &[CandidateSession]) -> Vec<SessionId>;
}

/// Inputs to scope resolution that every impl needs.
#[derive(Debug, Clone)]
pub struct ScopeAnchor {
    /// The CWD of the anchor session.
    pub cwd: PathBuf,
    /// The id of the anchor session.
    pub session_id: SessionId,
    /// How far back to include sessions, in days.
    pub window_days: u32,
    /// Wall-clock reference (Unix seconds) for computing the window cutoff.
    /// Carried explicitly so the resolver is deterministic and testable.
    pub now_unix: i64,
}

/// A past session considered as a scope candidate.
///
/// The caller loads these from the session store and hands them to the
/// resolver. Keeping the resolver a pure function over this slice means it has
/// no DB dependency and is trivially testable.
#[derive(Debug, Clone)]
pub struct CandidateSession {
    pub id: SessionId,
    pub cwd: PathBuf,
    pub parent_session: Option<SessionId>,
    /// Wall-clock time of the session's last update, in Unix seconds.
    pub updated_at_unix: i64,
}

/// Default v1 resolver: anchor CWD + descendants + fork-ancestor chain, within
/// the day window. Siblings and out-of-tree sessions are excluded.
pub struct CwdTreeScopeResolver;

impl ProjectScopeResolver for CwdTreeScopeResolver {
    fn resolve(&self, anchor: &ScopeAnchor, candidates: &[CandidateSession]) -> Vec<SessionId> {
        let cutoff = window_cutoff_unix(anchor.window_days, anchor.now_unix);
        let mut acc = ScopeAccumulator::default();
        collect_in_tree(&mut acc, anchor, candidates, cutoff);
        collect_fork_ancestors(&mut acc, anchor, candidates, cutoff);
        acc.into_ids()
    }
}

#[derive(Default)]
struct ScopeAccumulator {
    ids: Vec<SessionId>,
    seen: std::collections::HashSet<SessionId>,
}

impl ScopeAccumulator {
    fn insert(&mut self, id: SessionId) {
        if self.seen.insert(id.clone()) {
            self.ids.push(id);
        }
    }

    fn into_ids(self) -> Vec<SessionId> {
        self.ids
    }
}

/// Append every candidate whose CWD is the anchor or a descendant of it, and
/// whose `updated_at` falls within the window. Excludes siblings and out-of-tree
/// sessions.
fn collect_in_tree(
    acc: &mut ScopeAccumulator,
    anchor: &ScopeAnchor,
    candidates: &[CandidateSession],
    cutoff: i64,
) {
    for c in candidates {
        if within_window(c.updated_at_unix, cutoff) && is_under(&c.cwd, &anchor.cwd) {
            acc.insert(c.id.clone());
        }
    }
}

/// Append the fork-ancestor chain of the anchor session. Walks `parent_session`
/// upward, stopping on `None`, a cycle, or a session outside the window.
fn collect_fork_ancestors(
    acc: &mut ScopeAccumulator,
    anchor: &ScopeAnchor,
    candidates: &[CandidateSession],
    cutoff: i64,
) {
    let by_id = build_id_index(candidates);
    let mut cursor = anchor.session_id.clone();
    let mut guard = 0;
    while let Some(parent) = by_id.get(&cursor).and_then(|c| c.parent_session.clone()) {
        // Cycle / depth guard: the candidate set is finite, but bound defensively.
        guard += 1;
        if guard > candidates.len() + 1 {
            break;
        }
        match by_id.get(&parent) {
            Some(p) if within_window(p.updated_at_unix, cutoff) => {
                acc.insert(parent.clone());
                cursor = parent;
            }
            _ => break,
        }
    }
}

fn build_id_index(
    candidates: &[CandidateSession],
) -> std::collections::HashMap<SessionId, &CandidateSession> {
    candidates.iter().map(|c| (c.id.clone(), c)).collect()
}

/// `true` if `candidate` equals `anchor` or is a descendant directory of it.
///
/// Matches at path-component boundaries only, so `/a/jinn` is NOT considered
/// under `/a/jinn-old`. No symlink resolution and no lexical normalization of
/// the stored paths — raw stored CWDs are matched as-is.
fn is_under(candidate: &Path, anchor: &Path) -> bool {
    let c = components(candidate);
    let a = components(anchor);
    if a.len() > c.len() {
        return false;
    }
    // Anchor must be a prefix at component boundaries — covers both exact
    // match and descendant. Matches raw stored paths only: no normalization,
    // no symlink resolution.
    c.iter().take(a.len()).eq(a.iter())
}

/// Normalized component slice of a path (no `.` / `..` / prefix), as `OsString`
/// comparisons.
fn components(path: &Path) -> Vec<std::ffi::OsString> {
    path.components()
        .filter_map(|comp| match comp {
            Component::Normal(s) => Some(s.to_owned()),
            _ => None,
        })
        .collect()
}

/// Returns `true` if the candidate's `updated_at` is within the window.
fn within_window(updated_at_unix: i64, cutoff_unix: i64) -> bool {
    updated_at_unix >= cutoff_unix
}

/// Unix-seconds cutoff for a `window_days`-day lookback ending at `now_unix`.
/// Returns `i64::MIN` when `window_days == 0` (no window).
#[must_use]
pub fn window_cutoff_unix(window_days: u32, now_unix: i64) -> i64 {
    if window_days == 0 {
        return i64::MIN;
    }
    now_unix - i64::from(window_days) * SECONDS_PER_DAY
}

const SECONDS_PER_DAY: i64 = 86_400;

pub fn now_unix() -> i64 {
    // std::time is sufficient and avoids a hard dep on jiff in this leaf module.
    // The resolver stays deterministic by taking `now_unix` on the anchor;
    // only the production caller reads the real clock here.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jinn_core_types::SessionId;

    /// Deterministically maps a mnemonic tag (e.g. "s-a", "s-child") to a valid
    /// `SessionId` (a `Uuid` newtype).
    fn sid(tag: &str) -> SessionId {
        SessionId::from(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, tag.as_bytes()).to_string())
    }

    fn cand(id: &str, cwd: &str, parent: Option<&str>, updated_at_unix: i64) -> CandidateSession {
        CandidateSession {
            id: sid(id),
            cwd: PathBuf::from(cwd),
            parent_session: parent.map(sid),
            updated_at_unix,
        }
    }

    fn anchor(cwd: &str, session_id: &str, window_days: u32) -> ScopeAnchor {
        ScopeAnchor {
            cwd: PathBuf::from(cwd),
            session_id: sid(session_id),
            window_days,
            now_unix: NOW,
        }
    }

    const NOW: i64 = 1_700_000_000;

    #[rstest::rstest]
    fn descendant_cwd_is_in_scope() {
        // Given an anchor at /proj and a descendant session.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-anchor", 30);
        let candidates = [cand("s-child", "/proj/sub", None, NOW)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the descendant is included.
        assert_eq!(ids, vec![sid("s-child")]);
    }

    #[rstest::rstest]
    fn anchor_exact_cwd_is_in_scope() {
        // Given an anchor and a session at the exact same CWD.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-anchor", 30);
        let candidates = [cand("s-self", "/proj", None, NOW)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the same-CWD session is included.
        assert_eq!(ids, vec![sid("s-self")]);
    }

    #[rstest::rstest]
    fn sibling_cwd_is_excluded() {
        // Given an anchor at /proj and a sibling directory /other.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-anchor", 30);
        let candidates = [cand("s-other", "/other", None, NOW)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the sibling is excluded.
        assert_eq!(ids, [] as [jinn_core_types::SessionId; 0]);
    }

    #[rstest::rstest]
    fn name_prefix_does_not_match_sibling() {
        // Given an anchor at /a/jinn and a path that merely shares a name prefix.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/a/jinn", "s-anchor", 30);
        let candidates = [cand("s-old", "/a/jinn-old", None, NOW)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then /a/jinn-old is NOT treated as a descendant of /a/jinn.
        assert_eq!(ids, [] as [jinn_core_types::SessionId; 0]);
    }

    #[rstest::rstest]
    fn fork_ancestor_chain_is_in_scope() {
        // Given an anchor whose parent chain is in the candidate set.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-c", 30);
        let candidates = [
            cand("s-a", "/proj", None, NOW),
            cand("s-b", "/proj", Some("s-a"), NOW),
            cand("s-c", "/proj", Some("s-b"), NOW),
        ];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the anchor's parent chain (s-a, s-b) is included.
        assert!(ids.contains(&sid("s-a")));
        assert!(ids.contains(&sid("s-b")));
    }

    #[rstest::rstest]
    fn fork_chain_stops_at_missing_parent() {
        // Given an anchor whose parent id is not present in the candidate set.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-c", 30);
        let candidates = [cand("s-c", "/proj", Some("s-missing"), NOW)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the in-tree session is collected, but the fork walk stops at the
        // missing parent without panicking — s-missing is never inserted.
        assert_eq!(ids, vec![sid("s-c")]);
        assert!(!ids.contains(&sid("s-missing")));
    }

    #[rstest::rstest]
    fn session_outside_window_is_excluded() {
        // Given a candidate older than the window.
        let resolver = CwdTreeScopeResolver;
        // window_days=1 → cutoff = NOW - 86400
        let anchor = anchor("/proj", "s-anchor", 1);
        let stale = NOW - (SECONDS_PER_DAY * 10);
        let candidates = [cand("s-old", "/proj", None, stale)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then the stale session is excluded.
        assert_eq!(ids, [] as [jinn_core_types::SessionId; 0]);
    }

    #[rstest::rstest]
    fn result_is_deduplicated() {
        // Given a candidate that is both an in-tree match and a fork ancestor.
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-c", 30);
        let candidates = [
            cand("s-a", "/proj", None, NOW),
            cand("s-b", "/proj", Some("s-a"), NOW),
            cand("s-c", "/proj", Some("s-b"), NOW),
        ];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then each id appears exactly once (dedup by HashSet length).
        let unique: std::collections::HashSet<&SessionId> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[rstest::rstest]
    fn zero_window_includes_all_in_tree() {
        // Given window_days=0 (meaning "no window").
        let resolver = CwdTreeScopeResolver;
        let anchor = anchor("/proj", "s-anchor", 0);
        let ancient = NOW - (SECONDS_PER_DAY * 1000);
        let candidates = [cand("s-old", "/proj", None, ancient)];

        // When resolving.
        let ids = resolver.resolve(&anchor, &candidates);

        // Then even ancient sessions are included.
        assert_eq!(ids, vec![sid("s-old")]);
    }
}
