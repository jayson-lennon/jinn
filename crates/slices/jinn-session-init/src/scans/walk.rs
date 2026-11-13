//! Bounded cwd → ancestor directory walk.
//!
//! [`project_dirs`] resolves the ordered list of project directories contributing
//! skills/prompts/AGENTS.md for a given session cwd. The walk is **bounded** by two
//! rules (decision D4 of the project-locals plan):
//!
//! 1. **`$HOME` is exclusive** — the walk never collects from `$HOME`. A stray
//!    `~/AGENTS.md` can never be injected. This fixes a pre-existing bug where the
//!    unbounded AGENTS.md walk in `env_context` ran to the filesystem root.
//! 2. **VCS-marker root is inclusive** — when a directory containing a recognized
//!    VCS marker (`.git`, `.hg`, `.fslckout`, etc.) is reached, it is collected
//!    and the walk stops. "Project" == "VCS worktree."
//!
//! Whichever condition triggers first wins. The result is ordered
//! **least-local first, cwd last**, so merging "most-local wins" is a simple
//! later-insert-overwrites-earlier pass.

use jinn_common::canonicalize_display;

use super::vcs::is_vcs_root;
use super::{CONTEXT_FILE_CANDIDATES, PROMPTS_SUBDIR, SKILLS_SUBDIR};
use marked_path::{Absolute, MarkedPath, MarkedPathAccess, MarkedPathBuf};

/// Returns the ordered project directories for `cwd`, bounded by VCS root or
/// `$HOME`.
///
/// The returned list is ordered **least-local → cwd** (root ancestors first, the
/// session cwd last). Callers that merge with most-local-wins semantics should
/// iterate in returned order and let later entries overwrite earlier ones.
///
/// ## Bounding rule
///
/// Walk upward from `cwd`. At each directory:
/// - If it equals `home` → **break without collecting** (exclusive `$HOME`).
/// - Otherwise collect it.
/// - If it is a VCS root → **break after collecting** (inclusive VCS root).
///
/// First bounding condition hit wins. Both `cwd` and `home` are canonicalized
/// (best-effort) before comparison so symlinked or relative inputs compare
/// correctly.
///
/// ## Edge cases
///
/// - `cwd == home` → empty `Vec` (home is excluded; nothing collected).
/// - `cwd` outside `home` and under no VCS → walks to the filesystem root.
/// - Canonicalization failure (e.g. cwd does not exist) falls back to the input
///   path; the walk proceeds against the literal input.
///
/// # Panics
///
/// Panics if a typed join or a `MarkedPathBuf::<Absolute>::new` rebuild fails.
/// Neither can happen: every path in the walk is `Absolute` and is only ever
/// joined with bare components, so joins stay absolute and rebuilt absolutes
/// remain absolute.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "absolute paths joined with bare components / rebuilt as absolutes cannot fail; the Err is unreachable"
)]
pub fn project_dirs(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<MarkedPathBuf<Absolute>> {
    let current = canonicalize_or_input(cwd);
    let home = canonicalize_or_input(home);

    let mut dirs = collect_bounded(&current, &home);
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| {
            MarkedPathBuf::<Absolute>::new(dir)
                .expect("walk rebuilds absolutes from canonicalized inputs")
        })
        .collect()
}

/// Walks `cwd` upward, collecting each directory until a boundary is hit.
///
/// Returns dirs in **walk order (cwd → ancestor)**; the public [`project_dirs`]
/// reverses to ancestor → cwd. Collecting in walk order keeps the boundary logic
/// linear (one loop) per the one-loop-per-function rule.
#[expect(
    clippy::expect_used,
    reason = "parent of an absolute path is absolute; the typed rebuild's Err is unreachable"
)]
fn collect_bounded(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<MarkedPathBuf<Absolute>> {
    let mut dirs = Vec::new();
    let mut current = cwd.clone();
    loop {
        if current == *home {
            break;
        }
        dirs.push(current.clone());
        if is_vcs_root(&current) {
            break;
        }
        match current.as_path().parent() {
            None => break,
            Some(parent) if parent == current.as_path() => break, // filesystem root
            Some(parent) => {
                current = MarkedPathBuf::<Absolute>::new(parent)
                    .expect("parent of an absolute path is absolute");
            }
        }
    }
    dirs
}

/// Canonicalizes `path` into jinn's plain path form, falling back to the
/// input on failure.
///
/// Discovery must be robust to nonexistent or non-canonical paths (e.g. a cwd
/// that was just typed and not yet entered). Canonicalization goes through
/// [`jinn_common::canonicalize_display`] so cwd and home land in the same
/// (non-verbatim) form — on Windows, std's canonicalization returns verbatim
/// `\\?\`-prefixed paths that never compare equal to the plain parents the
/// walk rebuilds, so the home bound would never fire. When canonicalization
/// fails we use the literal input so the walk proceeds against the given path
/// rather than aborting.
fn canonicalize_or_input(path: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
    canonicalize_display(path)
        .ok()
        .and_then(|raw| MarkedPathBuf::<Absolute>::new(raw).ok())
        .unwrap_or_else(|| path.clone())
}

/// Returns the `.agents/skills` directory under each project dir for `cwd`.
///
/// Order matches [`project_dirs`]: least-local first, cwd last. Directories are
/// returned even if they don't exist yet — the scan actor filters nonexistent
/// dirs. The `.agents/skills` suffix is joined one segment at a time so the
/// result uses the platform's own separators throughout — never a
/// mixed-separator path such as `C:\proj\.agents/skills` on Windows.
#[must_use]
pub fn project_skills_dirs(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<MarkedPathBuf<Absolute>> {
    project_dirs(cwd, home)
        .into_iter()
        .map(|dir| join_segments(&dir, SKILLS_SUBDIR))
        .collect()
}

/// Returns the `.agents/prompts` directory under each project dir for `cwd`.
///
/// Order matches [`project_dirs`]: least-local first, cwd last. Directories are
/// returned even if they don't exist yet. The suffix is joined one segment at
/// a time, exactly as [`project_skills_dirs`] does.
#[must_use]
pub fn project_prompts_dirs(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<MarkedPathBuf<Absolute>> {
    project_dirs(cwd, home)
        .into_iter()
        .map(|dir| join_segments(&dir, PROMPTS_SUBDIR))
        .collect()
}

/// Folds `segments` onto `base` one component at a time.
///
/// Joining a multi-segment suffix (`.agents/skills`) as a single string
/// would bake `/` separators into a `\`-separated Windows path; folding
/// one segment per [`Path::join`] keeps every separator native.
#[expect(
    clippy::expect_used,
    reason = "absolute dir joined one bare segment at a time cannot fail; the typed join's Err is unreachable"
)]
fn join_segments(base: &MarkedPathBuf<Absolute>, segments: &[&str]) -> MarkedPathBuf<Absolute> {
    segments.iter().fold(base.clone(), |path, segment| {
        path.join(segment)
            .expect("absolute dir joins a bare subdir name")
    })
}

/// Returns the context files (AGENTS.md / CLAUDE.md) present in each project dir.
///
/// Walks the project dirs (least-local → cwd) and returns the path of the
/// **first existing** candidate in each dir (per the env_context precedent, a
/// directory contributes at most one context file). Result is ordered
/// least-local → cwd so callers can stack them root-first.
///
/// Unlike [`project_skills_dirs`] / [`project_prompts_dirs`], this returns
/// concrete file paths (already known to exist), not directories to scan.
#[must_use]
pub fn project_context_files(
    cwd: &MarkedPathBuf<Absolute>,
    home: &MarkedPathBuf<Absolute>,
) -> Vec<MarkedPathBuf<Absolute>> {
    project_dirs(cwd, home)
        .into_iter()
        .filter_map(|dir| first_existing_context_candidate(&dir.as_marked_path()))
        .collect()
}

/// Returns the first existing context-file candidate under `dir`, if any.
///
/// [`CONTEXT_FILE_CANDIDATES`] is checked in order; the first file that
/// exists wins for this directory. Returns `None` if no candidate exists.
#[expect(
    clippy::expect_used,
    reason = "absolute dir joined with a bare candidate filename cannot fail; the typed join's Err is unreachable"
)]
fn first_existing_context_candidate(
    dir: &MarkedPath<'_, Absolute>,
) -> Option<MarkedPathBuf<Absolute>> {
    CONTEXT_FILE_CANDIDATES
        .iter()
        .map(|name| dir.join(name).expect("absolute dir joins a bare filename"))
        .find(|path| path.as_path().is_file())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::create_dir,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

    /// A home directory that is nowhere near the tree under test.
    ///
    /// The walk must not reach it, so it lives in its own temp root — a
    /// sibling of the tree, never one of its ancestors. Building it from a
    /// typed root keeps the path absolute on every platform.
    fn far_away_home() -> MarkedPathBuf<Absolute> {
        let root = jinn_testutil::paths::TestRoot::new();
        root.join_str("home").expect("relative path")
    }

    /// Builds a nested temp dir tree and returns the root + a leaf path.
    struct Tree {
        root: MarkedPathBuf<Absolute>,
        leaf: MarkedPathBuf<Absolute>,
        _tmp: tempfile::TempDir,
    }

    impl Tree {
        /// `root/sub/leaf` directory structure; `marker` placed at `root`.
        ///
        /// The root is canonicalized at construction so fixture paths sit
        /// in the same form the walk emits: on macOS the temp tree lives
        /// behind a symlink (`$TMPDIR` → `/private/var/…`), and a plain
        /// `tmp.path()` would compare unequal to the walk's canonicalized
        /// output. Linux is identity; Windows loses only the verbatim
        /// prefix, which [`canonicalize_display`] already strips.
        fn new(marker: Option<&str>) -> Self {
            let tmp = jinn_testutil::tempdir();
            let root = canonicalize_display(&tmp.path).expect("temp dir canonicalizes");
            let leaf = root
                .join_all(["sub", "leaf"])
                .expect("name joins keep the base absolute");
            std::fs::create_dir_all(leaf.as_path()).expect("create dir chain");
            if let Some(m) = marker {
                // Marker is a file for .fslckout/.fossil, dir for .git/.hg/.jj.
                if m == ".git" || m == ".hg" || m == ".jj" {
                    let marker_dir = root.join(m).expect("typed parent joins a bare name");
                    std::fs::create_dir(marker_dir.as_path()).expect("create marker dir");
                } else {
                    let marker_file = root.join(m).expect("typed parent joins a bare name");
                    std::fs::write(marker_file.as_path(), b"marker").expect("create marker file");
                }
            }
            Self {
                leaf: root
                    .join_all(["sub", "leaf"])
                    .expect("name joins keep the base absolute"),
                root,
                _tmp: tmp.dir,
            }
        }
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_git() {
        // Given a tree rooted at a .git repo, cwd at a deep leaf.
        let tree = Tree::new(Some(".git"));

        // When walking up from the leaf with home far away.
        let dirs = project_dirs(&tree.leaf, &far_away_home());

        // Then the walk collects leaf -> sub, then root (VCS root, inclusive),
        // and stops. Root comes first after reverse, leaf last.
        assert_eq!(dirs.len(), 3, "should collect leaf, sub, and root");
        assert_eq!(dirs[0], tree.root, "root (VCS) is least-local");
        assert_eq!(
            dirs[1],
            tree.root
                .join("sub")
                .expect("typed parent joins a bare name")
        );
        assert_eq!(dirs[2], tree.leaf, "cwd is most-local");
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_fossil_checkout() {
        // Given a tree rooted at a Fossil checkout (.fslckout).
        let tree = Tree::new(Some(".fslckout"));

        // When walking up from the leaf.
        let dirs = project_dirs(&tree.leaf, &far_away_home());

        // Then the walk stops at the Fossil root (dogfoods this repo's VCS).
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], tree.root);
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_fossil_repo() {
        // Given a tree rooted at a .fossil repo db.
        let tree = Tree::new(Some(".fossil"));

        // When walking up from the leaf.
        let dirs = project_dirs(&tree.leaf, &far_away_home());

        // Then the walk stops at the Fossil repo root.
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], tree.root);
    }

    #[rstest::rstest]
    fn walk_includes_vcs_root_directory_itself() {
        // Given cwd IS the VCS root.
        let tree = Tree::new(Some(".git"));

        // When walking from the root itself.
        let dirs = project_dirs(&tree.root, &far_away_home());

        // Then only the root is collected (inclusive), no parents.
        assert_eq!(dirs, vec![tree.root]);
    }

    #[rstest::rstest]
    fn walk_bounds_at_home_exclusive() {
        // Given a tree with no VCS marker, and home set to the tree root.
        let tree = Tree::new(None);

        // When walking from the leaf with home == root.
        let dirs = project_dirs(&tree.leaf, &tree.root);

        // Then home (root) is excluded; only leaf and sub are collected.
        assert_eq!(
            dirs,
            vec![
                tree.root
                    .join("sub")
                    .expect("typed parent joins a bare name"),
                tree.leaf,
            ],
            "home must be excluded"
        );
    }

    #[rstest::rstest]
    fn walk_cwd_equals_home_returns_empty() {
        // Given cwd == home.
        let tree = Tree::new(None);

        // When walking from home itself.
        let dirs = project_dirs(&tree.root, &tree.root);

        // Then nothing is collected (degenerate case).
        assert!(dirs.is_empty(), "cwd == home must collect nothing");
    }

    #[rstest::rstest]
    fn walk_home_reached_before_vcs_excludes_home() {
        // Given home is an ANCESTOR of cwd, and a VCS marker exists BELOW home
        // (so home would be reached first walking up).
        let tmp = jinn_testutil::tempdir();
        let home = canonicalize_display(&tmp.path)
            .expect("temp dir canonicalizes")
            .join("home")
            .expect("typed parent joins a bare name");
        let repo = home.join("repo").expect("typed parent joins a bare name");
        let cwd = repo.join("deep").expect("typed parent joins a bare name");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        let git = repo.join(".git").expect("typed parent joins a bare name");
        std::fs::create_dir(git.as_path()).expect("git marker at repo");

        // When walking from cwd with the given home.
        let dirs = project_dirs(&cwd, &home);

        // Then the walk collects cwd -> repo (VCS root, inclusive) and stops.
        // Home is not reached because the VCS root is hit first.
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[0], repo);
        assert_eq!(dirs[1], cwd);
    }

    #[rstest::rstest]
    fn walk_vcs_root_above_home_stops_at_home() {
        // Given a VCS marker ABOVE home — home should bound first.
        let tmp = jinn_testutil::tempdir();
        let outer = canonicalize_display(&tmp.path).expect("temp dir canonicalizes");
        let home = outer.join("home").expect("typed parent joins a bare name");
        let cwd = home
            .join("project")
            .expect("typed parent joins a bare name");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        std::fs::create_dir(
            outer
                .join(".git")
                .expect("typed parent joins a bare name")
                .as_path(),
        )
        .expect("git marker above home");

        // When walking from cwd with home.
        let dirs = project_dirs(&cwd, &home);

        // Then home bounds first (exclusive); only `project` is collected.
        assert_eq!(dirs, vec![cwd], "home should bound before outer VCS root");
    }

    #[rstest::rstest]
    fn walk_nonexistent_cwd_falls_back_to_input() {
        // Given a cwd path that does not exist.
        let tmp = jinn_testutil::tempdir();
        // The base is canonicalized so the literal fallback form and home
        // compare in the same form on every platform (see `Tree::new`).
        let home = canonicalize_display(&tmp.path).expect("temp dir canonicalizes");
        let cwd = home
            .join("nope/does-not-exist")
            .expect("typed parent joins a bare name");

        // When walking.
        let dirs = project_dirs(&cwd, &home);

        // Then the walk uses the literal input; no panic. It will walk the
        // (non-canonicalized) parents up to home, collecting what it can.
        // The key assertion: no panic, home excluded.
        assert!(dirs.iter().all(|d| d != &home), "home never collected");
    }

    #[rstest::rstest]
    fn project_skills_dirs_appends_subdir() {
        // Given a VCS-rooted tree.
        let tree = Tree::new(Some(".git"));

        // When resolving skills dirs.
        let dirs = project_skills_dirs(&tree.leaf, &far_away_home());

        // Then each project dir has the .agents/skills suffix with native
        // separators (the typed join normalizes them).
        assert_eq!(dirs.len(), 3);
        assert!(
            dirs.iter().all(|d| d.to_string_lossy().ends_with("skills")
                && d.to_string_lossy().contains(".agents"))
        );
    }

    #[rstest::rstest]
    fn project_prompts_dirs_appends_subdir() {
        // Given a VCS-rooted tree.
        let tree = Tree::new(Some(".git"));

        // When resolving prompts dirs.
        let dirs = project_prompts_dirs(&tree.leaf, &far_away_home());

        // Then each project dir has the .agents/prompts suffix with native
        // separators (the typed join normalizes them).
        assert_eq!(dirs.len(), 3);
        assert!(
            dirs.iter().all(|d| d.to_string_lossy().ends_with("prompts")
                && d.to_string_lossy().contains(".agents"))
        );
    }

    /// The skills suffix must never mix separators inside one path: joining
    /// `.agents/skills` as a single string baked `/` into Windows paths
    /// (`C:\proj\.agents/skills`), which flowed into skill `base_dir`/
    /// `file_path` and the `skill` tool's user-visible result header.
    #[rstest::rstest]
    fn project_skills_dirs_string_form_is_native_separated() {
        // Given a VCS-rooted tree.
        let tree = Tree::new(Some(".git"));

        // When resolving skills dirs.
        let dirs = project_skills_dirs(&tree.leaf, &far_away_home());

        // Then the rendered string of every dir contains no separator other
        // than the platform's own.
        let native_sep = std::path::MAIN_SEPARATOR;
        for d in &dirs {
            let s = d.to_string_lossy();
            let has_foreign_sep = ['/', '\\']
                .into_iter()
                .any(|sep| sep != native_sep && s.contains(sep));
            assert!(
                !has_foreign_sep,
                "skills dir mixes separators: {s} (native is {native_sep:?})"
            );
        }
    }

    #[rstest::rstest]
    fn project_context_files_returns_existing_candidates() {
        // Given a tree with an AGENTS.md at the root (VCS root) only.
        let tree = Tree::new(Some(".git"));
        let agents = tree
            .root
            .join("AGENTS.md")
            .expect("typed parent joins a bare name");
        std::fs::write(agents.as_path(), "# root").expect("write AGENTS.md");

        // When resolving context files.
        let files = project_context_files(&tree.leaf, &far_away_home());

        // Then only the root contributes a context file.
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0],
            tree.root
                .join("AGENTS.md")
                .expect("typed parent joins a bare name")
        );
    }

    #[rstest::rstest]
    fn project_context_files_skips_dirs_without_candidates() {
        // Given a tree with no context files anywhere.
        let tree = Tree::new(Some(".git"));

        // When resolving context files.
        let files = project_context_files(&tree.leaf, &far_away_home());

        // Then no dirs contribute (all filtered out).
        assert_eq!(
            files,
            [] as [marked_path::MarkedPathBuf<marked_path::Absolute>; 0]
        );
    }

    #[rstest::rstest]
    fn project_context_files_first_candidate_wins_per_dir() {
        // Given a dir with both AGENTS.md and CLAUDE.md.
        let tmp = jinn_testutil::tempdir();
        let root = tmp.path.clone();
        std::fs::write(
            root.join("AGENTS.md")
                .expect("typed parent joins a bare name")
                .as_path(),
            "# agents",
        )
        .expect("write");
        std::fs::write(
            root.join("CLAUDE.md")
                .expect("typed parent joins a bare name")
                .as_path(),
            "# claude",
        )
        .expect("write");

        // When resolving context files.
        let files = project_context_files(&root, &far_away_home());

        // Then only AGENTS.md is returned (first candidate wins).
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("AGENTS.md"));
    }

    #[rstest::rstest]
    fn walk_canonicalizes_symlinked_cwd() {
        // Given a real dir and a symlink to it, with home elsewhere.
        let tmp = jinn_testutil::tempdir();
        let real = canonicalize_display(&tmp.path)
            .expect("temp dir canonicalizes")
            .join("real")
            .expect("typed parent joins a bare name");
        std::fs::create_dir(real.as_path()).expect("mkdir");
        #[cfg(unix)]
        let link = {
            let link = tmp.path.join("link").expect("temp root joins a bare name");
            std::os::unix::fs::symlink(real.as_path(), link.as_path()).expect("symlink");
            link
        };
        #[cfg(not(unix))]
        let link = real.clone(); // symlink test is unix-only; no-op elsewhere.

        // When walking from the symlink, with home set to the real parent
        // so the walk is bounded to the real directory only.
        let dirs = project_dirs(&link, &tmp.path);

        // Then the canonicalized real path is used (symlink resolved,
        // walk bounded by tmp parent = home).
        #[cfg(unix)]
        assert_eq!(dirs, vec![real]);

        #[cfg(not(unix))]
        {
            let _ = dirs;
        }
    }
}
