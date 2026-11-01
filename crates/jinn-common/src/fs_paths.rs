//! Filesystem-path canonicalization into jinn's plain path form.
//!
//! On Windows, [`std::fs::canonicalize`] returns *verbatim* paths —
//! `\\?\C:\users\jeff\…` for drive paths, `\\?\UNC\server\share\…` for
//! network shares. Those forms are what Win32 APIs want, but they are not
//! what anything else wants: they never compare equal to user-built paths,
//! they must not be persisted, and they are confusing to display. The
//! verbatim *disk* form is rewritten to its classic equivalent whenever the
//! classic form is addressable; network shares and over-limit paths keep
//! the verbatim form (there is no classic form that other APIs can open).
//!
//! [`canonicalize_display`] is the one shared wrapper every production site
//! uses when canonicalizing a path for storage, comparison, or display. The
//! underlying transform, [`strip_verbatim_prefix`], is also public so async
//! callers (e.g. `tokio::fs::canonicalize`) can apply the identical
//! normalization instead of hand-rolling prefix surgery at call sites.

use std::io;

use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// Canonicalizes `path` into jinn's plain path form.
///
/// Equivalent to [`std::fs::canonicalize`] on non-Windows platforms. On
/// Windows, the verbatim prefix the std canonicalization produces is stripped
/// first (see [`strip_verbatim_prefix`]), so a drive-path result is directly
/// comparable to a user-built path (`C:\…`) and safe to store or display.
/// Network shares keep the verbatim `\\?\UNC\…` form — the strip never
/// fabricates the classic `\\server\share` spelling.
///
/// # Errors
///
/// Propagates the [`io::Error`] from [`std::fs::canonicalize`] unchanged —
/// for example [`io::ErrorKind::NotFound`] when `path` does not exist. The
/// Windows prefix stripping never fails; when it cannot produce a usable
/// plain form the verbatim form is returned instead.
pub fn canonicalize_display(path: &MarkedPathBuf<Absolute>) -> io::Result<MarkedPathBuf<Absolute>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "this is the sanctioned wrapper: canonicalizes once and strips the verbatim prefix before the value escapes"
    )]
    let canonical = std::fs::canonicalize(path.as_path())?;
    MarkedPathBuf::<Absolute>::new(strip_verbatim_prefix(canonical))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub use marked_path::strip_verbatim_prefix;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    // The raw std-fs round trip is the subject under test: these fixtures
    // compare the helper's output against untyped `std::fs::canonicalize`
    // word-for-word, so the raw calls stay.
    #![expect(
        clippy::disallowed_methods,
        reason = "test fixtures assert the helper against raw std::fs::canonicalize; the raw calls are the test oracle"
    )]
    use super::canonicalize_display;

    #[rstest::rstest]
    #[test]
    fn canonicalize_display_errors_for_nonexistent_path() {
        // Given an existing temp directory and a path inside it that does not exist.
        let dir = jinn_testutil::tempdir();
        let missing = dir
            .path
            .join("does-not-exist.txt")
            .expect("temp root joins a bare name");

        // When canonicalizing the missing path.
        let result = canonicalize_display(&missing);

        // Then the underlying io error propagates unchanged.
        let Err(err) = result else {
            panic!("canonicalizing a nonexistent path must fail");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[rstest::rstest]
    #[test]
    fn canonicalize_display_output_recanonicalizes_to_the_same_file() {
        // Given a real file in a temp directory.
        let dir = jinn_testutil::tempdir();
        let file = dir
            .path
            .join("marker.txt")
            .expect("temp root joins a bare name");
        std::fs::write(&file, b"x").expect("write marker file");

        // When canonicalizing it through the helper.
        let out = canonicalize_display(&file).expect("canonicalize marker file");

        // Then the returned path still resolves to the same file.
        let recanonicalized = std::fs::canonicalize(&out).expect("re-canonicalize helper output");
        let direct = std::fs::canonicalize(&file).expect("canonicalize directly");
        assert_eq!(recanonicalized, direct);
    }

    #[cfg(windows)]
    mod windows {
        // The verbatim-prefix fixtures construct `\\?\` paths on purpose:
        // the raw std forms are the subject under test.
        #![expect(
            clippy::disallowed_types,
            reason = "windows fixtures construct raw verbatim-prefix paths on purpose; the raw std form is the subject under test"
        )]

        use std::path::PathBuf;

        use marked_path::MarkedPathAccess;

        use super::super::{canonicalize_display, strip_verbatim_prefix};

        #[rstest::rstest]
        #[test]
        fn canonicalize_display_returns_a_plain_path() {
            // Given a real temp directory.
            let dir = jinn_testutil::tempdir();

            // When canonicalizing it through the helper.
            let out = canonicalize_display(&dir.path).expect("canonicalize temp dir");

            // Then the result carries no verbatim prefix.
            assert!(
                !out.as_os_str()
                    .to_str()
                    .is_some_and(|text| text.starts_with(r"\\?\"))
            );
        }

        #[rstest::rstest]
        #[test]
        fn drive_verbatim_form_strips_to_plain_drive_path() {
            // Given a verbatim drive path short enough to live without the prefix.
            let verbatim = PathBuf::from(r"\\?\C:\users\jeff");

            // When stripping the prefix.
            let out = strip_verbatim_prefix(verbatim);

            // Then it becomes the classic drive form.
            assert_eq!(out, PathBuf::from(r"C:\users\jeff"));
        }

        #[rstest::rstest]
        #[test]
        fn unc_verbatim_form_is_kept_verbatim() {
            // Given a verbatim network-share path.
            let verbatim = PathBuf::from(r"\\?\UNC\server\share\file.txt");

            // When stripping the prefix.
            let out = strip_verbatim_prefix(verbatim.clone());

            // Then it is kept: the strip simplifies only the verbatim *disk*
            // form (dunce leaves network shares verbatim, so the classic
            // `\\server\share` form is never fabricated).
            assert_eq!(out, verbatim);
        }

        #[rstest::rstest]
        #[test]
        fn device_verbatim_path_is_left_unchanged() {
            // Given a verbatim device path (no drive, no share).
            let device = PathBuf::from(r"\\?\COM1");

            // When stripping the prefix.
            let out = strip_verbatim_prefix(device.clone());

            // Then the verbatim form is kept, not turned into a bogus relative path.
            assert_eq!(out, device);
        }

        #[rstest::rstest]
        #[test]
        fn non_verbatim_path_is_left_unchanged() {
            // Given an already-plain path.
            let plain = PathBuf::from(r"C:\users\jeff");

            // When stripping the prefix.
            let out = strip_verbatim_prefix(plain.clone());

            // Then the path is returned as-is.
            assert_eq!(out, plain);
        }

        #[rstest::rstest]
        #[case(253, true)]
        #[case(254, false)]
        fn plain_form_at_the_classic_limit_boundary_decides_stripping(
            #[case] tail_len: usize,
            #[case] stripped: bool,
        ) {
            // Given a verbatim drive path whose full form is 260 units
            // (260 = 7 + 253) or 261 units (261 = 7 + 254) — straddling the
            // classic MAX_PATH limit as dunce counts it, on the *whole*
            // verbatim path (`\\?\C:\` is 7 units) in both bytes and UTF-16.
            let verbatim = PathBuf::from(format!(r"\\?\C:\{}", "a".repeat(tail_len)));

            // When stripping the prefix.
            let out = strip_verbatim_prefix(verbatim.clone());

            // Then only the form that fits the classic limit is stripped.
            assert_eq!(out != verbatim, stripped);
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::super::{canonicalize_display, strip_verbatim_prefix};
        use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

        #[rstest::rstest]
        #[test]
        fn canonicalize_display_is_a_passthrough_of_std_canonicalize() {
            // Given a real temp directory.
            let dir = jinn_testutil::tempdir();

            // When canonicalizing it through the helper.
            let out = canonicalize_display(&dir.path).expect("canonicalize temp dir");

            // Then the result is identical to std's canonicalization.
            let direct = std::fs::canonicalize(dir.path.as_path()).expect("canonicalize directly");
            let direct = MarkedPathBuf::<Absolute>::new(strip_verbatim_prefix(direct))
                .expect("canonicalized temp dir is absolute");
            assert_eq!(out, direct);
        }
    }
}
