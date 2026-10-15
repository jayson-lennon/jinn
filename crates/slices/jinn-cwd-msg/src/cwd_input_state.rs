//! CWD input popup vocabulary — the popup's edit state and the pure path
//! resolver shared between the kernel's project-add popup and the cwd slice.
//!
//! The kernel owns the popup-independent flows (the external `<M-c>`/`<M-d>`
//! selector suspend in composition); the cwd slice owns the in-app popup.
//! Both speak these types.

use jinn_slices::LineInput;
use jinn_slices::SlotKey;
use std::path::{Path, PathBuf};

/// The `cwd/state` slot: the in-app cwd popup's single edit state.
#[must_use]
pub fn cwds_slot() -> SlotKey {
    SlotKey::builtin("cwd", "state")
}

/// State for the cwd input popup - editing a directory path.
///
/// The editable text and cursor live in [`LineInput`] (shared with the arg and
/// rename session inputs) under the [`CwdInputState::text`] field; access via
/// `.text.input` / `.text.cursor_pos`.
#[derive(Debug, Clone, Default)]
pub struct CwdInputState {
    /// The editable text + cursor.
    pub text: LineInput,
}

/// The outcome of resolving a cwd input string.
///
/// Distinguishes the three cases the popup needs to render differently and that
/// the confirm handler needs to gate on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdResolution {
    /// Resolved to an existing directory.
    Ok(PathBuf),
    /// Empty or whitespace-only input (nothing to resolve yet).
    Empty,
    /// Path does not exist or is not a directory.
    NotADir(String),
}

/// The directory `~` expands against.
///
/// `jinn-cwd-msg` is a contract crate — it may not depend on any slice
/// implementation crate, so the process's home is read here (the only
/// environment read in the crate) and threaded explicitly into
/// [`resolve_cwd_input_with_home`] by the call sites that know the process
/// context. Tests inject a temp home the same way.
///
/// The read is per call, not cached at startup: this is the resolver a popup
/// confirm dispatches through, and a fresh read matches how the rest of the
/// popup path works (the seeded text was itself just derived from a live
/// `shorten_path` call).
#[must_use]
pub fn tilde_home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// Expands a leading `~` (or `~/`) into `home`.
///
/// Paths without a leading `~` are returned unchanged. Accepts both `~/` and
/// the platform's native separator after the tilde (`~\` on Windows), since
/// the popup seeds itself with the native-separator form
/// [`jinn_common::shorten_path`] produces.
fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = strip_tilde_prefix(raw) {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Strips the `~` marker plus its following separator from `raw`, returning
/// the remainder.
///
/// `is_separator` is platform-aware: on Unix only `/` counts, so `~\foo`
/// stays a literal path; on Windows both `/` and `\` count, so the
/// native-separator form [`jinn_common::shorten_path`] seeds the popup with
/// (`~\code\jinn`) expands too.
fn strip_tilde_prefix(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix('~')?;
    let mut chars = rest.chars();
    match chars.next() {
        Some(sep) if std::path::is_separator(sep) => Some(chars.as_str()),
        _ => None,
    }
}

/// Lexically normalizes `.` and `..` components in `path`.
///
/// This collapses `a/./b` to `a/b` and `a/child/../b` to `a/b` *before* we
/// `canonicalize`, so user-typed relative paths resolve even on filesystems
/// whose `canonicalize` is strict about `..` traversal. `..` that would climb
/// above a root is clamped at the root. Symlinks are still resolved by the
/// subsequent `canonicalize`.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut stack: Vec<std::path::Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {
                // `.` is a no-op - drop it.
            }
            std::path::Component::ParentDir => {
                // Pop the last normal component if there is one; otherwise
                // keep the `..` (clamped at root once absolute).
                match stack.last() {
                    Some(std::path::Component::Normal(_)) => {
                        stack.pop();
                    }
                    _ => stack.push(component),
                }
            }
            _ => stack.push(component),
        }
    }
    stack.iter().map(|c| c.as_os_str()).collect()
}

/// Resolves a raw cwd input string against the current session cwd.
///
/// `raw` is trimmed; empty/whitespace returns [`CwdResolution::Empty`].
/// Otherwise performs `~` expansion, joins relative paths against `current_cwd`,
/// canonicalizes, and verifies the result is an existing directory.
///
/// `~` expands against the process's home directory (via [`tilde_home`]);
/// callers that already hold a home (tests) pass it through
/// [`resolve_cwd_input_with_home`] instead.
///
/// # Errors
///
/// This function is infallible by construction - FS failures (path missing,
/// not a directory, canonicalize error) all collapse to
/// [`CwdResolution::NotADir`], which the render layer renders as a red `x`
/// footer and the confirm handler treats as "stay open, do nothing". This
/// matches the user's mental model: "anything that's not an existing dir is an
/// error I can fix by typing more."
#[must_use]
pub fn resolve_cwd_input(raw: &str, current_cwd: &Path) -> CwdResolution {
    resolve_cwd_input_with_home(raw, current_cwd, &tilde_home())
}

/// Resolves a raw cwd input string against `current_cwd`, expanding `~`
/// against the caller-supplied `home`.
///
/// Same semantics as [`resolve_cwd_input`]; the home is explicit so tests
/// inject a temp home without touching the process environment, and so the
/// expansion never depends on how `dirs` reads the platform's home on any
/// given OS.
///
/// # Errors
///
/// Infallible by construction, like [`resolve_cwd_input`]: failures collapse
/// to [`CwdResolution::NotADir`].
#[must_use]
pub fn resolve_cwd_input_with_home(raw: &str, current_cwd: &Path, home: &Path) -> CwdResolution {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CwdResolution::Empty;
    }

    // Tilde expansion falls back to the literal path when the input does not
    // start with `~` plus a separator (or a bare `~`).
    let candidate = expand_tilde(trimmed, home);

    // Relative paths resolve against the current session cwd; absolute paths
    // and home-expanded paths pass through unchanged.
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        current_cwd.join(&candidate)
    };

    // Canonicalize resolves `..`, `.`, and symlinks, into jinn's plain path
    // form (no Windows verbatim `\\?\` prefix) so the result is comparable,
    // storable, and displayable. A failure here means the path doesn't exist
    // - treat as NotADir so the footer shows the error.
    let normalized = normalize_lexically(&absolute);
    let Ok(canonical) = jinn_common::canonicalize_display(&normalized) else {
        return CwdResolution::NotADir(absolute.display().to_string());
    };

    if canonical.is_dir() {
        CwdResolution::Ok(canonical)
    } else {
        CwdResolution::NotADir(canonical.display().to_string())
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
    use rstest::rstest;
    use std::fs;
    use tempfile::tempdir;

    #[rstest]
    fn empty_input_is_empty() {
        // Given a temp dir to resolve against.
        let dir = tempdir().unwrap();

        // When resolving an empty input.
        // Then the resolution is Empty.
        assert_eq!(resolve_cwd_input("", dir.path()), CwdResolution::Empty);
    }

    #[rstest]
    fn whitespace_only_input_is_empty() {
        // Given a temp dir to resolve against.
        let dir = tempdir().unwrap();

        // When resolving an input made only of whitespace.
        // Then the resolution is Empty.
        assert_eq!(
            resolve_cwd_input("   \t  ", dir.path()),
            CwdResolution::Empty
        );
    }

    #[rstest]
    fn nonexistent_path_is_not_a_dir() {
        // Given a temp dir as the cwd.
        let dir = tempdir().unwrap();

        // When resolving a path that does not exist.
        let res = resolve_cwd_input("nope/does/not/exist", dir.path());

        // Then the resolution is NotADir.
        assert!(matches!(res, CwdResolution::NotADir(_)), "{res:?}");
    }

    #[rstest]
    fn regular_file_is_not_a_dir() {
        // Given a temp dir holding a regular file.
        let dir = tempdir().unwrap();
        let file = dir.path().join("a_file.txt");
        fs::write(&file, "x").unwrap();

        // When resolving the file name as cwd input.
        let res = resolve_cwd_input("a_file.txt", dir.path());

        // Then the resolution is NotADir.
        assert!(matches!(res, CwdResolution::NotADir(_)), "{res:?}");
    }

    #[rstest]
    fn relative_subdir_resolves_against_current_cwd() {
        // Given a temp dir containing a `sub` directory.
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();

        // When resolving `sub` relative to the temp dir.
        let res = resolve_cwd_input("sub", dir.path());

        // Then it resolves to the canonical sub directory.
        assert_eq!(res, CwdResolution::Ok(canonicalize(&sub)));
    }

    #[rstest]
    fn dot_segments_collapse() {
        // Given a temp dir holding sibling/sub.
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sibling").join("sub");
        fs::create_dir_all(&sub).unwrap();

        // When resolving `sibling/sub/../.` from the temp dir.
        let res = resolve_cwd_input("sibling/sub/../.", dir.path());

        // Then the dot segments collapse to `sibling`.
        assert_eq!(
            res,
            CwdResolution::Ok(canonicalize(&dir.path().join("sibling")))
        );
    }

    #[rstest]
    fn parent_relative_resolves() {
        // Given a temp dir with parent/child, using child as the cwd.
        let root = tempdir().unwrap();
        let parent = root.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();

        // When resolving `..` from the child directory.
        let res = resolve_cwd_input("..", &child);

        // Then it resolves to the parent directory.
        assert_eq!(res, CwdResolution::Ok(canonicalize(&parent)));
    }

    #[rstest]
    fn tilde_expands_to_home() {
        // Given a temp home containing the target directory.
        let home = tempdir().unwrap();
        let name = format!("jinn_cwd_test_{}", std::process::id());
        let target = home.path().join(&name);
        fs::create_dir_all(&target).unwrap();

        // When resolving `~/<name>` from an unrelated cwd against that home.
        let res = resolve_cwd_input_with_home(
            &format!("~/{name}"),
            Path::new("/some/unrelated/cwd"),
            home.path(),
        );

        // Then it expands to the directory under the temp home — never
        // touching the real home.
        assert_eq!(res, CwdResolution::Ok(canonicalize(&target)));
    }

    #[rstest]
    fn bare_tilde_resolves_to_home() {
        // Given a temp home.
        let home = tempdir().unwrap();

        // When resolving a bare `~` from an unrelated cwd against that home.
        let res = resolve_cwd_input_with_home("~", Path::new("/some/unrelated/cwd"), home.path());

        // Then it resolves to the temp home itself.
        assert_eq!(res, CwdResolution::Ok(canonicalize(home.path())));
    }

    #[rstest]
    fn native_separator_tilde_input_resolves() {
        // Given a temp home containing the target directory.
        let home = tempdir().unwrap();
        let name = format!("jinn_cwd_test_{}", std::process::id());
        let target = home.path().join(&name);
        fs::create_dir_all(&target).unwrap();

        // When resolving the input the popup seeds — the native-separator
        // form after `~` — from an unrelated cwd. `Path::join` emits the
        // platform's own separator, so this is `~\name` on Windows (the
        // reseeded native form) and `~/name` on Unix (the plain tilde
        // form); both must resolve to the directory under the temp home.
        let input = Path::new("~").join(&name);
        let res = resolve_cwd_input_with_home(
            &input.to_string_lossy(),
            Path::new("/some/unrelated/cwd"),
            home.path(),
        );

        // Then it expands to the directory under the temp home.
        assert_eq!(res, CwdResolution::Ok(canonicalize(&target)));
    }

    #[rstest]
    fn absolute_path_passes_through() {
        // Given a temp dir to resolve against.
        let dir = tempdir().unwrap();

        // When resolving that absolute path from an unrelated cwd.
        let res = resolve_cwd_input(&dir.path().to_string_lossy(), Path::new("/unrelated"));

        // Then it passes through unchanged, canonicalized.
        assert_eq!(res, CwdResolution::Ok(canonicalize(dir.path())));
    }

    /// Helper: canonicalize without dragging the shared canonicalizer into
    /// every test. Uses the same plain-form helper the resolver uses, so
    /// expectations match the resolver's non-verbatim output on Windows.
    fn canonicalize(p: &Path) -> PathBuf {
        jinn_common::canonicalize_display(p).expect("test fixture dir must canonicalize")
    }
}
