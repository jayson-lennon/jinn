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

use std::path::{Path, PathBuf};

use super::vcs::is_vcs_root;
use super::{CONTEXT_FILE_CANDIDATES, PROMPTS_SUBDIR, SKILLS_SUBDIR};

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
#[must_use]
pub fn project_dirs(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let current = canonicalize_or_input(cwd);
    let home = canonicalize_or_input(home);

    let mut dirs = collect_bounded(&current, &home);
    dirs.reverse();
    dirs
}

/// Walks `cwd` upward, collecting each directory until a boundary is hit.
///
/// Returns dirs in **walk order (cwd → ancestor)**; the public [`project_dirs`]
/// reverses to ancestor → cwd. Collecting in walk order keeps the boundary logic
/// linear (one loop) per the one-loop-per-function rule.
fn collect_bounded(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        if current == home {
            break;
        }
        dirs.push(current.clone());
        if is_vcs_root(&current) {
            break;
        }
        match current.parent() {
            None => break,
            Some(parent) if parent == current => break, // filesystem root
            Some(parent) => current = parent.to_path_buf(),
        }
    }
    dirs
}

/// Canonicalizes `path`, falling back to the input on failure.
///
/// Discovery must be robust to nonexistent or non-canonical paths (e.g. a cwd
/// that was just typed and not yet entered). When `canonicalize` fails we use the
/// literal input so the walk proceeds against the given path rather than
/// aborting.
fn canonicalize_or_input(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Returns the `.agents/skills` directory under each project dir for `cwd`.
///
/// Order matches [`project_dirs`]: least-local first, cwd last. Directories are
/// returned even if they don't exist yet — the scan actor filters nonexistent
/// dirs.
#[must_use]
pub fn project_skills_dirs(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    project_dirs(cwd, home)
        .into_iter()
        .map(|dir| dir.join(SKILLS_SUBDIR))
        .collect()
}

/// Returns the `.agents/prompts` directory under each project dir for `cwd`.
///
/// Order matches [`project_dirs`]: least-local first, cwd last. Directories are
/// returned even if they don't exist yet.
#[must_use]
pub fn project_prompts_dirs(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    project_dirs(cwd, home)
        .into_iter()
        .map(|dir| dir.join(PROMPTS_SUBDIR))
        .collect()
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
pub fn project_context_files(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    project_dirs(cwd, home)
        .into_iter()
        .filter_map(|dir| first_existing_context_candidate(&dir))
        .collect()
}

/// Returns the first existing context-file candidate under `dir`, if any.
///
/// [`CONTEXT_FILE_CANDIDATES`] is checked in order; the first file that
/// exists wins for this directory. Returns `None` if no candidate exists.
fn first_existing_context_candidate(dir: &Path) -> Option<PathBuf> {
    CONTEXT_FILE_CANDIDATES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
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

    /// Builds a nested temp dir tree and returns the root + a leaf path.
    struct Tree {
        root: PathBuf,
        leaf: PathBuf,
        _tmp: tempfile::TempDir,
    }

    impl Tree {
        /// `root/sub/leaf` directory structure; `marker` placed at `root`.
        fn new(marker: Option<&str>) -> Self {
            let tmp = tempfile::tempdir().expect("temp dir");
            let root = tmp.path().to_path_buf();
            for seg in ["sub", "leaf"] {
                std::fs::create_dir_all(root.join(seg)).expect("create dir");
            }
            if let Some(m) = marker {
                // Marker is a file for .fslckout/.fossil, dir for .git/.hg/.jj.
                if m == ".git" || m == ".hg" || m == ".jj" {
                    std::fs::create_dir(root.join(m)).expect("create marker dir");
                } else {
                    std::fs::write(root.join(m), b"marker").expect("create marker file");
                }
            }
            Self {
                leaf: root.join("sub").join("leaf"),
                root,
                _tmp: tmp,
            }
        }
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_git() {
        // Given a tree rooted at a .git repo, cwd at a deep leaf.
        let tree = Tree::new(Some(".git"));

        // When walking up from the leaf with home far away.
        let dirs = project_dirs(&tree.leaf, Path::new("/nonexistent/home"));

        // Then the walk collects leaf -> sub, then root (VCS root, inclusive),
        // and stops. Root comes first after reverse, leaf last.
        assert_eq!(dirs.len(), 3, "should collect leaf, sub, and root");
        assert_eq!(dirs[0], tree.root, "root (VCS) is least-local");
        assert_eq!(dirs[1], tree.root.join("sub"));
        assert_eq!(dirs[2], tree.leaf, "cwd is most-local");
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_fossil_checkout() {
        // Given a tree rooted at a Fossil checkout (.fslckout).
        let tree = Tree::new(Some(".fslckout"));

        // When walking up from the leaf.
        let dirs = project_dirs(&tree.leaf, Path::new("/nonexistent/home"));

        // Then the walk stops at the Fossil root (dogfoods this repo's VCS).
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], tree.root);
    }

    #[rstest::rstest]
    fn walk_stops_at_vcs_root_fossil_repo() {
        // Given a tree rooted at a .fossil repo db.
        let tree = Tree::new(Some(".fossil"));

        // When walking up from the leaf.
        let dirs = project_dirs(&tree.leaf, Path::new("/nonexistent/home"));

        // Then the walk stops at the Fossil repo root.
        assert_eq!(dirs.len(), 3);
        assert_eq!(dirs[0], tree.root);
    }

    #[rstest::rstest]
    fn walk_includes_vcs_root_directory_itself() {
        // Given cwd IS the VCS root.
        let tree = Tree::new(Some(".git"));

        // When walking from the root itself.
        let dirs = project_dirs(&tree.root, Path::new("/nonexistent/home"));

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
            vec![tree.root.join("sub"), tree.leaf],
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
        let tmp = tempfile::tempdir().expect("temp dir");
        let home = tmp.path().join("home");
        let repo = home.join("repo");
        let cwd = repo.join("deep");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        std::fs::create_dir(repo.join(".git")).expect("git marker at repo");

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
        let tmp = tempfile::tempdir().expect("temp dir");
        let outer = tmp.path();
        let home = outer.join("home");
        let cwd = home.join("project");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        std::fs::create_dir(outer.join(".git")).expect("git marker above home");

        // When walking from cwd with home.
        let dirs = project_dirs(&cwd, &home);

        // Then home bounds first (exclusive); only `project` is collected.
        assert_eq!(dirs, vec![cwd], "home should bound before outer VCS root");
    }

    #[rstest::rstest]
    fn walk_nonexistent_cwd_falls_back_to_input() {
        // Given a cwd path that does not exist.
        let tmp = tempfile::tempdir().expect("temp dir");
        let cwd = tmp.path().join("nope/does-not-exist");
        let home = tmp.path().to_path_buf();

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
        let dirs = project_skills_dirs(&tree.leaf, Path::new("/nonexistent/home"));

        // Then each project dir has the .agents/skills suffix.
        assert_eq!(dirs.len(), 3);
        assert!(dirs.iter().all(|d| d.ends_with(".agents/skills")));
    }

    #[rstest::rstest]
    fn project_prompts_dirs_appends_subdir() {
        // Given a VCS-rooted tree.
        let tree = Tree::new(Some(".git"));

        // When resolving prompts dirs.
        let dirs = project_prompts_dirs(&tree.leaf, Path::new("/nonexistent/home"));

        // Then each project dir has the .agents/prompts suffix.
        assert_eq!(dirs.len(), 3);
        assert!(dirs.iter().all(|d| d.ends_with(".agents/prompts")));
    }

    #[rstest::rstest]
    fn project_context_files_returns_existing_candidates() {
        // Given a tree with an AGENTS.md at the root (VCS root) only.
        let tree = Tree::new(Some(".git"));
        std::fs::write(tree.root.join("AGENTS.md"), "# root").expect("write AGENTS.md");

        // When resolving context files.
        let files = project_context_files(&tree.leaf, Path::new("/nonexistent/home"));

        // Then only the root contributes a context file.
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], tree.root.join("AGENTS.md"));
    }

    #[rstest::rstest]
    fn project_context_files_skips_dirs_without_candidates() {
        // Given a tree with no context files anywhere.
        let tree = Tree::new(Some(".git"));

        // When resolving context files.
        let files = project_context_files(&tree.leaf, Path::new("/nonexistent/home"));

        // Then no dirs contribute (all filtered out).
        assert!(files.is_empty());
    }

    #[rstest::rstest]
    fn project_context_files_first_candidate_wins_per_dir() {
        // Given a dir with both AGENTS.md and CLAUDE.md.
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("AGENTS.md"), "# agents").expect("write");
        std::fs::write(root.join("CLAUDE.md"), "# claude").expect("write");

        // When resolving context files.
        let files = project_context_files(&root, Path::new("/nonexistent/home"));

        // Then only AGENTS.md is returned (first candidate wins).
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("AGENTS.md"));
    }

    #[rstest::rstest]
    fn walk_canonicalizes_symlinked_cwd() {
        // Given a real dir and a symlink to it, with home elsewhere.
        let tmp = tempfile::tempdir().expect("temp dir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).expect("mkdir");
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        #[cfg(not(unix))]
        let link = real.clone(); // symlink test is unix-only; no-op elsewhere.

        // When walking from the symlink, with home set to the real parent
        // so the walk is bounded to the real directory only.
        let dirs = project_dirs(&link, tmp.path());

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
