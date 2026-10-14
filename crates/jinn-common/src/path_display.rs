//! Path display transforms shared across slices.
//!
//! Several slices render a path into a single-line label, and all of them want
//! the same treatment: collapse the home directory to `~`. The transform is pure
//! and has no slice-specific meaning, so it lives here rather than in whichever
//! slice happened to need it first.

use std::path::{Path, PathBuf};

/// Shorten a path for display: replace the home directory prefix with `~`.
///
/// Paths under `$HOME` collapse to `~/…` (or just `~` when the path *is* the home
/// directory); any other path is returned unchanged as a display string. Falls
/// back to the raw path when `dirs::home_dir()` cannot be determined.
///
/// The collapsed form is rebuilt through `PathBuf` so every separator is the
/// platform's own (`~/code/jinn` on Unix, `~\code\jinn` on Windows) — never a
/// `~` glued onto a differently-separated remainder.
///
/// This is a pure display transform. The cwd slice's `resolve_cwd_input` expands
/// `~` back when resolving user input (both `~/` and the native-separator
/// form), so shortened paths round-trip.
#[must_use]
pub fn shorten_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        // Join component-wise: the `~` marker and every remaining component
        // are appended with the platform separator, normalizing any mixed
        // separators carried in the remainder.
        let label = relative
            .components()
            .fold(PathBuf::from("~"), |mut acc, component| {
                acc.push(component.as_os_str());
                acc
            });
        return label.display().to_string();
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::shorten_path;
    use std::path::{Path, PathBuf};

    #[rstest::rstest]
    fn path_outside_home_is_returned_unchanged() {
        // Given a path outside the home directory.
        let path = Path::new("/opt/jinn/config.toml");

        // When shortening it.
        let display = shorten_path(path);

        // Then the full path is shown.
        assert_eq!(display, "/opt/jinn/config.toml");
    }

    #[rstest::rstest]
    fn home_relative_path_collapses_to_tilde() {
        // Given a home directory.
        let Some(home) = dirs::home_dir() else {
            return;
        };

        // And a path beneath it.
        let path = home.join("projects").join("jinn");

        // When shortening it.
        let display = shorten_path(&path);

        // Then the home prefix is replaced with a tilde and the remainder
        // keeps the platform's native separators (PathBuf-built, never a
        // `~` glued onto a differently-separated string).
        let expected = PathBuf::from("~")
            .join("projects")
            .join("jinn")
            .display()
            .to_string();
        assert_eq!(display, expected);
    }

    #[rstest::rstest]
    fn home_itself_shortens_to_bare_tilde() {
        // Given the home directory itself.
        let Some(home) = dirs::home_dir() else {
            return;
        };

        // When shortening it.
        let display = shorten_path(&home);

        // Then only a tilde remains.
        assert_eq!(display, "~");
    }

    #[rstest::rstest]
    fn mixed_separators_in_remainder_are_normalized_to_native() {
        // Given a home-anchored path whose remainder mixes separators (as
        // when a label fragment was string-built before this transform).
        let Some(home) = dirs::home_dir() else {
            return;
        };

        // And a path beneath it carrying a forward slash after the first
        // native component.
        let path = home.join("code").join("sub/dir");

        // When shortening it.
        let display = shorten_path(&path);

        // Then every separator after `~` is native — the fold rebuilds the
        // remainder component-wise.
        let expected = PathBuf::from("~")
            .join("code")
            .join("sub")
            .join("dir")
            .display()
            .to_string();
        assert_eq!(display, expected);
    }
}
