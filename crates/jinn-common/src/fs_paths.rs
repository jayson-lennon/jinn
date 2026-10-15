//! Filesystem-path canonicalization into jinn's plain path form.
//!
//! On Windows, [`std::fs::canonicalize`] returns *verbatim* paths —
//! `\\?\C:\users\jeff\…` for drive paths, `\\?\UNC\server\share\…` for
//! network shares. Those forms are what Win32 APIs want, but they are not
//! what anything else wants: they never compare equal to user-built paths,
//! they must not be persisted, and they are confusing to display.
//!
//! [`canonicalize_display`] is the one shared wrapper every production site
//! uses when canonicalizing a path for storage, comparison, or display. The
//! underlying transform, [`strip_verbatim_prefix`], is also public so async
//! callers (e.g. `tokio::fs::canonicalize`) can apply the identical
//! normalization instead of hand-rolling prefix surgery at call sites.

use std::io;
use std::path::{Path, PathBuf};

/// The classic Win32 path limit, in UTF-16 code units, including the
/// terminating NUL. Plain (non-verbatim) paths of 260 or more units cannot be
/// addressed by non-verbatim-aware APIs, so stripping the prefix there would
/// produce a path nothing can reopen.
#[cfg(windows)]
const MAX_CLASSIC_PATH: usize = 260;

/// The verbatim prefix Windows canonicalization produces.
#[cfg(windows)]
const VERBATIM_PREFIX: &str = r"\\?\";

/// Marker following the verbatim prefix for network-share paths.
#[cfg(windows)]
const UNC_MARKER: &str = r"UNC\";

/// Canonicalizes `path` into jinn's plain path form.
///
/// Equivalent to [`std::fs::canonicalize`] on non-Windows platforms. On
/// Windows, the verbatim prefix the std canonicalization produces is stripped
/// first (see [`strip_verbatim_prefix`]), so the result is directly comparable
/// to user-built paths (`C:\…`, `\\server\share\…`) and safe to store or
/// display.
///
/// # Errors
///
/// Propagates the [`io::Error`] from [`std::fs::canonicalize`] unchanged —
/// for example [`io::ErrorKind::NotFound`] when `path` does not exist. The
/// Windows prefix stripping never fails; when it cannot produce a usable
/// plain form the verbatim form is returned instead.
pub fn canonicalize_display(path: &Path) -> io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    Ok(strip_verbatim_prefix(canonical))
}

/// Strips the Windows `\\?\` verbatim prefix from a canonicalized path.
///
/// Transforms the two verbatim forms [`std::fs::canonicalize`] produces on
/// Windows into their classic equivalents:
///
/// - `\\?\C:\users\jeff` → `C:\users\jeff` (drive form)
/// - `\\?\UNC\server\share\file` → `\\server\share\file` (network form)
///
/// Three kinds of input are returned **unchanged**, because no safe plain
/// form exists:
///
/// - Paths whose plain form would be at or beyond the classic `MAX_PATH`
///   limit — only the verbatim prefix lets APIs address those, so stripping
///   would produce a path nothing can reopen.
/// - Verbatim *device* paths (`\\?\COM1`, `\\?\PIPE\…`): there is no drive or
///   share to expose, and stripping would fabricate a bogus relative path.
/// - Non-UTF-8-representable paths, which cannot be rebuilt safely here.
///
/// On non-Windows platforms this is the identity function, so callers can
/// apply it unconditionally to the result of any canonicalize (including
/// `tokio::fs::canonicalize`).
#[must_use]
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        strip_verbatim_windows(path)
    }
    #[cfg(not(windows))]
    {
        path
    }
}

/// Windows implementation of [`strip_verbatim_prefix`].
#[cfg(windows)]
fn strip_verbatim_windows(path: PathBuf) -> PathBuf {
    // Not representable as UTF-8: rebuilding the path would be guesswork, and
    // the verbatim form is accepted everywhere a plain path is.
    let Some(text) = path.to_str() else {
        return path;
    };
    let Some(rest) = text.strip_prefix(VERBATIM_PREFIX) else {
        return path;
    };

    let plain = match rest.strip_prefix(UNC_MARKER) {
        Some(share) => format!(r"\\{share}"),
        None if is_drive_path(rest) => rest.to_owned(),
        None => return path,
    };

    // A plain path at or beyond the classic limit is un-addressable without
    // the prefix; keep the verbatim form rather than emitting a broken path.
    if plain.chars().map(char::len_utf16).sum::<usize>() >= MAX_CLASSIC_PATH {
        return path;
    }
    PathBuf::from(plain)
}

/// Whether `rest` (a verbatim path minus its `\\?\` prefix) names a drive
/// root or something under one — `C:\`, `C:\users\…`. Anything else after the
/// prefix is a device path.
#[cfg(windows)]
fn is_drive_path(rest: &str) -> bool {
    matches!(
        rest.as_bytes(),
        [drive, b':', _sep @ (b'\\' | b'/'), ..] if drive.is_ascii_alphabetic()
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::canonicalize_display;

    #[rstest::rstest]
    #[test]
    fn canonicalize_display_errors_for_nonexistent_path() {
        // Given an existing temp directory and a path inside it that does not exist.
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = dir.path().join("does-not-exist.txt");

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
        let dir = tempfile::tempdir().expect("create temp dir");
        let file = dir.path().join("marker.txt");
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
        use std::path::PathBuf;

        use super::super::{canonicalize_display, strip_verbatim_prefix};

        #[rstest::rstest]
        #[test]
        fn canonicalize_display_returns_a_plain_path() {
            // Given a real temp directory.
            let dir = tempfile::tempdir().expect("create temp dir");

            // When canonicalizing it through the helper.
            let out = canonicalize_display(dir.path()).expect("canonicalize temp dir");

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
        fn unc_verbatim_form_strips_to_network_path() {
            // Given a verbatim network-share path.
            let verbatim = PathBuf::from(r"\\?\UNC\server\share\file.txt");

            // When stripping the prefix.
            let out = strip_verbatim_prefix(verbatim);

            // Then it becomes the classic network form.
            assert_eq!(out, PathBuf::from(r"\\server\share\file.txt"));
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
        #[case(256, true)]
        #[case(257, false)]
        fn plain_form_at_the_classic_limit_boundary_decides_stripping(
            #[case] tail_len: usize,
            #[case] stripped: bool,
        ) {
            // Given a verbatim drive path whose plain form is 259 chars (259 = 3 + 256)
            // or 260 chars (260 = 3 + 257) — straddling the classic MAX_PATH limit.
            let verbatim = PathBuf::from(format!(r"\\?\C:\{}", "a".repeat(tail_len)));

            // When stripping the prefix.
            let out = strip_verbatim_prefix(verbatim.clone());

            // Then only the form that fits the classic limit is stripped.
            assert_eq!(out != verbatim, stripped);
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::super::canonicalize_display;

        #[rstest::rstest]
        #[test]
        fn canonicalize_display_is_a_passthrough_of_std_canonicalize() {
            // Given a real temp directory.
            let dir = tempfile::tempdir().expect("create temp dir");

            // When canonicalizing it through the helper.
            let out = canonicalize_display(dir.path()).expect("canonicalize temp dir");

            // Then the result is identical to std's canonicalization.
            let direct = std::fs::canonicalize(dir.path()).expect("canonicalize directly");
            assert_eq!(out, direct);
        }
    }
}
