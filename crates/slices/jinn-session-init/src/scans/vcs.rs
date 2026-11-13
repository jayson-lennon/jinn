//! VCS root detection.
//!
//! [`is_vcs_root`] reports whether a directory is the root of a recognized
//! version-control working copy. Detection is marker-file based: a directory is a
//! VCS root if it contains any of [`VCS_MARKERS`].
//!
//! ## Why markers (not `git rev-parse`)
//!
//! The codebase must work across VCSes (this very repo uses Fossil, not git).
//! Shell-outs to per-VCS CLIs would couple discovery to installed tooling and
//! vary in exit-code semantics. A marker-file check is VCS-agnostic, dependency
//! free, and cheap (`std::fs::metadata`).

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// Files/directories that indicate a directory is a VCS working-copy root.
///
/// Order is irrelevant; presence of any one marker is sufficient.
///
/// - `.git`     — Git (working tree root for non-bare repos, and worktrees).
/// - `.hg`      — Mercurial.
/// - `.fslckout` — Fossil checkout database (this repo's marker).
/// - `.fossil`  — Fossil repository database (when stored at the checkout root).
/// - `.jj`      — Jujutsu.
pub const VCS_MARKERS: &[&str] = &[".git", ".hg", ".fslckout", ".fossil", ".jj"];

/// Returns `true` if `dir` contains any recognized VCS marker.
///
/// Uses [`std::fs::metadata`] (follows symlinks). A nonexistent directory, or one
/// that cannot be stat'd, returns `false` rather than panicking — discovery must
/// be robust to transient/missing directories during a walk.
///
/// # Panics
///
/// Panics if the typed join of `dir` with a bare marker name fails. This
/// cannot happen: `dir` is `Absolute` and every [`VCS_MARKERS`] entry is a
/// bare component, so the join always stays absolute.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "absolute dir joined with a bare marker name cannot fail; the typed join's Err is unreachable"
)]
pub fn is_vcs_root(dir: &MarkedPathBuf<Absolute>) -> bool {
    VCS_MARKERS.iter().any(|marker| {
        dir.join(marker)
            .expect("absolute root joins a bare marker")
            .exists()
    })
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

    #[rstest::rstest]
    fn detects_git_marker() {
        // Given a directory containing a .git marker.
        let dir = jinn_testutil::tempdir();
        std::fs::create_dir(dir.path.join(".git").expect("temp root joins a bare name"))
            .expect("create .git");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true.
        assert!(is_root);
    }

    #[rstest::rstest]
    fn detects_mercurial_marker() {
        // Given a directory containing a .hg marker.
        let dir = jinn_testutil::tempdir();
        std::fs::create_dir(dir.path.join(".hg").expect("temp root joins a bare name"))
            .expect("create .hg");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true.
        assert!(is_root);
    }

    #[rstest::rstest]
    fn detects_fossil_checkout_marker() {
        // Given a directory containing a .fslckout marker (Fossil checkout).
        let dir = jinn_testutil::tempdir();
        std::fs::write(
            dir.path
                .join(".fslckout")
                .expect("temp root joins a bare name"),
            b"fossil checkout db",
        )
        .expect("create .fslckout");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true.
        assert!(is_root);
    }

    #[rstest::rstest]
    fn detects_fossil_repo_marker() {
        // Given a directory containing a .fossil marker (Fossil repo db).
        let dir = jinn_testutil::tempdir();
        std::fs::write(
            dir.path
                .join(".fossil")
                .expect("temp root joins a bare name"),
            b"fossil repo db",
        )
        .expect("create .fossil");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true.
        assert!(is_root);
    }

    #[rstest::rstest]
    fn detects_jujutsu_marker() {
        // Given a directory containing a .jj marker.
        let dir = jinn_testutil::tempdir();
        std::fs::create_dir(dir.path.join(".jj").expect("temp root joins a bare name"))
            .expect("create .jj");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true.
        assert!(is_root);
    }

    #[rstest::rstest]
    fn plain_directory_is_not_vcs_root() {
        // Given a directory with no VCS markers.
        let dir = jinn_testutil::tempdir();
        std::fs::write(
            dir.path
                .join("README.md")
                .expect("temp root joins a bare name"),
            "hi",
        )
        .expect("write file");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports false.
        assert!(!is_root);
    }

    #[rstest::rstest]
    fn nonexistent_directory_is_not_vcs_root() {
        // Given a path that does not exist.
        let dir = jinn_testutil::tempdir();
        let missing = dir
            .path
            .join("does-not-exist")
            .expect("temp root joins a bare name");

        // When checking the missing path for VCS markers.
        let is_root = is_vcs_root(&missing);

        // Then is_vcs_root reports false (no panic).
        assert!(!is_root);
    }

    #[rstest::rstest]
    fn detects_any_one_of_multiple_markers() {
        // Given a directory containing both .git and .hg.
        let dir = jinn_testutil::tempdir();
        std::fs::create_dir(dir.path.join(".git").expect("temp root joins a bare name"))
            .expect("create .git");
        std::fs::create_dir(dir.path.join(".hg").expect("temp root joins a bare name"))
            .expect("create .hg");

        // When checking the directory for VCS markers.
        let is_root = is_vcs_root(&dir.path);

        // Then is_vcs_root reports true (presence of any marker suffices).
        assert!(is_root);
    }
}
