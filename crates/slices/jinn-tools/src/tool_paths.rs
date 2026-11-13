//! How a tool turns a user-supplied path into one it can touch.
//!
//! Every filesystem tool takes a `path` argument that the model writes, and
//! that path is usually relative — to whatever directory the session's working
//! directory happens to be. Resolving it is the same arithmetic for every tool
//! (`read`, `write`, `edit`, `save_plan`), so it lives here rather than being
//! restated four times: a tool that resolved paths its own way would be a tool
//! that could disagree with its siblings about which file a name refers to.
//!
//! Resolution is purely lexical. It does not canonicalize, expand `~`, or touch
//! the filesystem, so it cannot fail and cannot be surprised by a symlink.

use marked_path::{Absolute, MarkedPathBuf};

/// Resolves a tool's `path` argument against the session's working directory.
///
/// An absolute path is returned as-is; a relative one is joined onto `cwd`.
#[must_use]
pub(crate) fn resolve_path(path: &str, cwd: &MarkedPathBuf<Absolute>) -> MarkedPathBuf<Absolute> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        #[expect(
            clippy::expect_used,
            reason = "infallible: the branch above checked the path is absolute, the only failure this constructor has"
        )]
        MarkedPathBuf::<Absolute>::new(p).expect("checked absolute above")
    } else {
        #[expect(
            clippy::expect_used,
            reason = "infallible: joining onto an absolute parent keeps the result absolute — PathError has no other variant"
        )]
        cwd.join(p).expect("absolute parent joins a bare candidate")
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
    use marked_path::MarkedPathAccess;

    #[rstest::rstest]
    fn resolve_path_relative() {
        // Given a relative path and a CWD.
        let dir = jinn_testutil::tempdir();
        let cwd = &dir.path;

        // When resolving the path against that CWD.
        let resolved = resolve_path("foo/bar.txt", cwd);

        // Then it's joined against CWD.
        assert_eq!(
            resolved,
            dir.path
                .join("foo/bar.txt")
                .expect("temp root joins a bare name")
        );
    }

    #[rstest::rstest]
    fn resolve_path_absolute() {
        // Given an absolute path and a CWD. The path is built from a temp
        // root, so it is absolute on every platform — a leading slash would
        // not be (it is drive-relative on Windows).
        let dir = jinn_testutil::tempdir();
        let cwd = &dir.path;

        // When resolving it against that CWD.
        let input = dir.path.join("etc").expect("a").join("hosts").expect("b");
        let resolved = resolve_path(&input.to_string_lossy(), cwd);

        // Then it comes back unchanged. The rule is the platform's own: a
        // path that is absolute there is returned as-is, and one that is not
        // is joined onto the CWD — so the expectation mirrors that rule
        // rather than assuming one platform's spelling.
        let expected = if input.as_path().is_absolute() {
            input
        } else {
            cwd.join(input)
                .expect("absolute parent joins a bare candidate")
        };
        assert_eq!(resolved, expected);
    }
}
