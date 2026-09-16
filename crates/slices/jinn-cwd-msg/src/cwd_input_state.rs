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

/// Expands a leading `~` or `~/` into the user's home directory.
///
/// Returns `None` when home cannot be determined (no `HOME`). Paths without a
/// leading `~` are returned unchanged.
fn expand_tilde(raw: &str) -> Option<PathBuf> {
    if raw == "~" {
        dirs::home_dir()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir().map(|home| home.join(rest))
    } else {
        Some(PathBuf::from(raw))
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
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return CwdResolution::Empty;
    }

    // Tilde expansion falls back to the literal path when home is unavailable.
    let candidate = match expand_tilde(trimmed) {
        Some(p) => p,
        None => PathBuf::from(trimmed),
    };

    // Relative paths resolve against the current session cwd; absolute paths
    // and home-expanded paths pass through unchanged.
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        current_cwd.join(&candidate)
    };

    // Canonicalize resolves `..`, `.`, and symlinks. A failure here means the
    // path doesn't exist - treat as NotADir so the footer shows the error.
    let normalized = normalize_lexically(&absolute);
    let Ok(canonical) = std::fs::canonicalize(&normalized) else {
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
        let dir = tempdir().unwrap();
        assert_eq!(resolve_cwd_input("", dir.path()), CwdResolution::Empty);
    }

    #[rstest]
    fn whitespace_only_input_is_empty() {
        let dir = tempdir().unwrap();
        assert_eq!(
            resolve_cwd_input("   \t  ", dir.path()),
            CwdResolution::Empty
        );
    }

    #[rstest]
    fn nonexistent_path_is_not_a_dir() {
        let dir = tempdir().unwrap();
        let res = resolve_cwd_input("nope/does/not/exist", dir.path());
        assert!(matches!(res, CwdResolution::NotADir(_)), "{res:?}");
    }

    #[rstest]
    fn regular_file_is_not_a_dir() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a_file.txt");
        fs::write(&file, "x").unwrap();
        let res = resolve_cwd_input("a_file.txt", dir.path());
        assert!(matches!(res, CwdResolution::NotADir(_)), "{res:?}");
    }

    #[rstest]
    fn relative_subdir_resolves_against_current_cwd() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        let res = resolve_cwd_input("sub", dir.path());
        assert_eq!(res, CwdResolution::Ok(canonicalize(&sub)));
    }

    #[rstest]
    fn dot_segments_collapse() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sibling").join("sub");
        fs::create_dir_all(&sub).unwrap();
        // from dir, go into sibling/sub then back up one with ..
        let res = resolve_cwd_input("sibling/sub/../.", dir.path());
        assert_eq!(
            res,
            CwdResolution::Ok(canonicalize(&dir.path().join("sibling")))
        );
    }

    #[rstest]
    fn parent_relative_resolves() {
        let root = tempdir().unwrap();
        let parent = root.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();
        // cwd = child, input = ".." resolves back up to parent.
        let res = resolve_cwd_input("..", &child);
        assert_eq!(res, CwdResolution::Ok(canonicalize(&parent)));
    }

    #[rstest]
    fn tilde_expands_to_home() {
        // Point HOME at a temp dir so we control expansion and don't touch the
        // real (possibly read-only) home directory.
        let home = tempdir().unwrap();
        let name = format!("jinn_cwd_test_{}", std::process::id());
        let target = home.path().join(&name);
        fs::create_dir_all(&target).unwrap();
        // SAFETY: single-threaded test; env var mutation is isolated to this test.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        let res = resolve_cwd_input(&format!("~/{name}"), Path::new("/some/unrelated/cwd"));
        assert_eq!(res, CwdResolution::Ok(canonicalize(&target)));
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    #[rstest]
    fn bare_tilde_resolves_to_home() {
        let home = tempdir().unwrap();
        // SAFETY: single-threaded test; env var mutation is isolated to this test.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        let res = resolve_cwd_input("~", Path::new("/some/unrelated/cwd"));
        assert_eq!(res, CwdResolution::Ok(canonicalize(home.path())));
        // SAFETY: see above.
        unsafe {
            std::env::remove_var("HOME");
        }
    }

    #[rstest]
    fn absolute_path_passes_through() {
        let dir = tempdir().unwrap();
        let res = resolve_cwd_input(&dir.path().to_string_lossy(), Path::new("/unrelated"));
        assert_eq!(res, CwdResolution::Ok(canonicalize(dir.path())));
    }

    /// Helper: canonicalize without dragging `std::fs::canonicalize` into every test.
    fn canonicalize(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).expect("test fixture dir must canonicalize")
    }
}

/// Shorten a path for display: replace the home directory prefix with `~`.
///
/// Paths under `$HOME` collapse to `~/…` (or just `~` when the path *is* the home
/// directory); any other path is returned unchanged as a display string. Falls
/// back to the raw path when `dirs::home_dir()` cannot be determined.
///
/// This is a pure display transform — [`resolve_cwd_input`] expands `~` back
/// when resolving user input, so shortened paths round-trip.
#[must_use]
pub fn shorten_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        let display = relative.display().to_string();
        if display.is_empty() {
            return "~".to_owned();
        }
        return format!("~/{display}");
    }
    path.display().to_string()
}
