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

use std::path::Path;

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
#[must_use]
pub fn is_vcs_root(dir: &Path) -> bool {
    VCS_MARKERS.iter().any(|marker| dir.join(marker).exists())
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
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join(".git")).expect("create .git");

        // Then is_vcs_root reports true.
        assert!(is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn detects_mercurial_marker() {
        // Given a directory containing a .hg marker.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join(".hg")).expect("create .hg");

        // Then is_vcs_root reports true.
        assert!(is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn detects_fossil_checkout_marker() {
        // Given a directory containing a .fslckout marker (Fossil checkout).
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(".fslckout"), b"fossil checkout db")
            .expect("create .fslckout");

        // Then is_vcs_root reports true.
        assert!(is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn detects_fossil_repo_marker() {
        // Given a directory containing a .fossil marker (Fossil repo db).
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join(".fossil"), b"fossil repo db").expect("create .fossil");

        // Then is_vcs_root reports true.
        assert!(is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn detects_jujutsu_marker() {
        // Given a directory containing a .jj marker.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join(".jj")).expect("create .jj");

        // Then is_vcs_root reports true.
        assert!(is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn plain_directory_is_not_vcs_root() {
        // Given a directory with no VCS markers.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("README.md"), "hi").expect("write file");

        // Then is_vcs_root reports false.
        assert!(!is_vcs_root(dir.path()));
    }

    #[rstest::rstest]
    fn nonexistent_directory_is_not_vcs_root() {
        // Given a path that does not exist.
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("does-not-exist");

        // Then is_vcs_root reports false (no panic).
        assert!(!is_vcs_root(&missing));
    }

    #[rstest::rstest]
    fn detects_any_one_of_multiple_markers() {
        // Given a directory containing both .git and .hg.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join(".git")).expect("create .git");
        std::fs::create_dir(dir.path().join(".hg")).expect("create .hg");

        // Then is_vcs_root reports true (presence of any marker suffices).
        assert!(is_vcs_root(dir.path()));
    }
}
