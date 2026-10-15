//! Theme file discovery and loading.
//!
//! Scans user and system themes directories for `*.toml` files, parses them,
//! and resolves missing fields from the default theme. User themes override
//! system themes of the same name.

use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt as _};
use wherror::Error;

use crate::default_theme;
use crate::theme::{Theme, ThemeFile};

/// Errors that can occur during theme loading and parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThemeError {
    /// Theme file not found.
    #[error("theme not found")]
    NotFound,
    /// TOML parsing error.
    #[error("theme parse error")]
    Parse,
    /// Invalid color value.
    #[error("invalid theme color")]
    InvalidColor,
    /// I/O error reading theme file.
    #[error("theme I/O error")]
    Io,
}

/// Discovers all theme TOML files in the given themes directory.
///
/// Returns a list of `(name, path)` pairs, where `name` is the filename
/// without the `.toml` extension. Returns an empty list if the directory
/// doesn't exist.
///
/// # Errors
///
/// Returns [`ThemeError::Io`] if the directory cannot be read.
pub fn discover_themes(themes_dir: &Path) -> Result<Vec<(String, PathBuf)>, Report<ThemeError>> {
    if !themes_dir.exists() {
        return Ok(Vec::new());
    }

    let mut themes = Vec::new();
    let entries = std::fs::read_dir(themes_dir)
        .change_context(ThemeError::Io)
        .attach("failed to read themes directory")?;

    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "toml")
            && let Some(name) = path.file_stem().and_then(|s| s.to_str())
        {
            themes.push((name.to_owned(), path));
        }
    }

    themes.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(themes)
}

/// Loads a theme by name, searching user dir first then system dir.
///
/// Looks for `<dir>/<name>.toml` in the user themes directory first, then
/// falls back to the system themes directory. If not found in either,
/// returns [`ThemeError::NotFound`].
///
/// # Errors
///
/// - [`ThemeError::NotFound`] - no TOML file with that name exists in either dir.
/// - [`ThemeError::Parse`] - TOML is malformed or contains invalid colors.
/// - [`ThemeError::Io`] - file cannot be read.
pub fn load_theme(
    name: &str,
    user_dir: &Path,
    system_dir: &Path,
) -> Result<Theme, Report<ThemeError>> {
    let user_path = user_dir.join(format!("{name}.toml"));
    if user_path.exists() {
        return load_theme_from_file(&user_path);
    }
    let system_path = system_dir.join(format!("{name}.toml"));
    if system_path.exists() {
        return load_theme_from_file(&system_path);
    }
    Err(Report::new(ThemeError::NotFound).attach(format!("theme file not found: {name}")))
}

/// Loads a theme by name from a single directory.
///
/// Convenience wrapper for callers that only have one directory
/// (e.g., tests). Looks for `<dir>/<name>.toml`.
///
/// # Errors
///
/// Same as [`load_theme`].
pub fn load_theme_from_dir(name: &str, dir: &Path) -> Result<Theme, Report<ThemeError>> {
    let path = dir.join(format!("{name}.toml"));
    if !path.exists() {
        return Err(Report::new(ThemeError::NotFound)
            .attach(format!("theme file not found: {}", path.display())));
    }
    load_theme_from_file(&path)
}

/// Loads a theme from a specific file path.
///
/// Missing fields in the TOML are filled from the default theme.
///
/// # Errors
///
/// - [`ThemeError::Parse`] - TOML is malformed or contains invalid colors.
/// - [`ThemeError::Io`] - file cannot be read.
pub fn load_theme_from_file(path: &Path) -> Result<Theme, Report<ThemeError>> {
    let content = std::fs::read_to_string(path)
        .change_context(ThemeError::Io)
        .attach(format!("failed to read theme file: {}", path.display()))?;

    let file: ThemeFile = toml::from_str(&content)
        .change_context(ThemeError::Parse)
        .attach(format!("failed to parse theme file: {}", path.display()))?;

    Ok(file.resolve())
}

/// Resolves a theme name to a `Theme`, searching user dir then system dir.
///
/// - If `name` is `None` or `"default"`: tries loading `default.toml` from
///   user dir first, then system dir. If not found in either, falls back to
///   the embedded default theme.
/// - Otherwise, loads the named theme from user dir, then system dir.
///
/// # Errors
///
/// Propagates errors from [`load_theme`] for non-default theme names.
/// For the default theme, only returns errors for parse/IO failures
/// (not-found silently falls back to embedded).
pub fn resolve_theme(
    name: Option<&str>,
    user_dir: &Path,
    system_dir: &Path,
) -> Result<Theme, Report<ThemeError>> {
    match name {
        None | Some("default") => {
            // Try user dir, then system dir - allows user customization.
            match load_theme("default", user_dir, system_dir) {
                Ok(theme) => Ok(theme),
                Err(err) if err.downcast_ref::<ThemeError>() == Some(&ThemeError::NotFound) => {
                    // File not found anywhere - use embedded default.
                    Ok(default_theme())
                }
                Err(err) => Err(err),
            }
        }
        Some(name) => load_theme(name, user_dir, system_dir),
    }
}

/// Resolves a theme from a single directory.
///
/// Convenience wrapper for callers that only have one directory (e.g., tests).
///
/// # Errors
///
/// Returns an error if the theme file cannot be read or parsed.
pub fn resolve_theme_from_dir(
    name: Option<&str>,
    themes_dir: &Path,
) -> Result<Theme, Report<ThemeError>> {
    match name {
        None | Some("default") => match load_theme_from_dir("default", themes_dir) {
            Ok(theme) => Ok(theme),
            Err(err) if err.downcast_ref::<ThemeError>() == Some(&ThemeError::NotFound) => {
                Ok(default_theme())
            }
            Err(err) => Err(err),
        },
        Some(name) => load_theme_from_dir(name, themes_dir),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use tempfile::TempDir;

    use super::*;

    #[rstest::rstest]
    fn discover_themes_returns_empty_for_missing_dir() {
        // Given a themes directory that doesn't exist.
        let dir = PathBuf::from("/nonexistent/path/themes");

        // When discovering themes.
        let result = discover_themes(&dir);

        // Then it returns Ok with an empty list.
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [] as [(std::string::String, std::path::PathBuf); 0]
        );
    }

    #[rstest::rstest]
    fn load_theme_returns_not_found_for_missing_file() {
        // Given a themes directory with no such theme file.
        let dir = TempDir::new().expect("temp dir");

        // When loading a theme name that doesn't exist.
        let result = load_theme_from_dir("nonexistent_theme_xyz", dir.path());

        // Then it returns NotFound error.
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<ThemeError>()
                .is_some_and(|e| matches!(e, ThemeError::NotFound)),
            "expected NotFound error"
        );
    }

    #[rstest::rstest]
    fn load_theme_from_file_parses_valid_toml() {
        // Given a valid theme TOML file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("test.toml");
        std::fs::write(&path, "focus_accent = \"red\"\nprimary_text = \"#FFFFFF\"").expect("write");

        // When loading.
        let theme = load_theme_from_file(&path).expect("load");

        // Then the specified fields are set.
        assert_eq!(theme.focus_accent, ratatui::style::Color::Red);
        assert_eq!(
            theme.primary_text,
            ratatui::style::Color::Rgb(255, 255, 255)
        );
        // And unspecified fields fall back to default.
        assert_eq!(theme.muted_text, default_theme().muted_text);
    }

    #[rstest::rstest]
    fn load_theme_from_file_rejects_invalid_toml() {
        // Given an invalid TOML file.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml {{{{").expect("write");

        // When loading.
        let result = load_theme_from_file(&path);

        // Then it returns a Parse error.
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<ThemeError>()
                .is_some_and(|e| matches!(e, ThemeError::Parse)),
            "expected Parse error"
        );
    }

    #[rstest::rstest]
    fn load_theme_from_file_rejects_invalid_color() {
        // Given a TOML file with an invalid color value.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("bad-color.toml");
        std::fs::write(&path, "focus_accent = \"not_a_color\"").expect("write");

        // When loading.
        let result = load_theme_from_file(&path);

        // Then it returns a Parse error.
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .downcast_ref::<ThemeError>()
                .is_some_and(|e| matches!(e, ThemeError::Parse)),
            "expected Parse error"
        );
    }

    #[rstest::rstest]
    fn resolve_theme_none_returns_default_when_no_file() {
        // Given an empty themes directory.
        let dir = TempDir::new().expect("temp dir");

        // When resolving with None.
        let theme = resolve_theme_from_dir(None, dir.path()).expect("resolve");

        // Then it returns the embedded default theme.
        assert_eq!(theme.focus_accent, default_theme().focus_accent);
    }

    #[rstest::rstest]
    fn resolve_theme_default_string_returns_default_when_no_file() {
        // Given an empty themes directory.
        let dir = TempDir::new().expect("temp dir");

        // When resolving with "default".
        let theme = resolve_theme_from_dir(Some("default"), dir.path()).expect("resolve");

        // Then it returns the embedded default theme.
        assert_eq!(theme.focus_accent, default_theme().focus_accent);
    }

    #[rstest::rstest]
    fn resolve_theme_default_loads_from_filesystem() {
        // Given a themes directory with a custom default.toml.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("default.toml");
        std::fs::write(&path, "focus_accent = \"red\"").expect("write");

        // When resolving with "default".
        let theme = resolve_theme_from_dir(Some("default"), dir.path()).expect("resolve");

        // Then it loads from the filesystem (focus_accent overridden).
        assert_eq!(theme.focus_accent, ratatui::style::Color::Red);
        // And other fields fall back to embedded default.
        assert_eq!(theme.muted_text, default_theme().muted_text);
    }

    #[rstest::rstest]
    fn discover_themes_finds_toml_files() {
        // Given a temp directory with theme files.
        let dir = TempDir::new().expect("temp dir");
        std::fs::write(dir.path().join("ocean.toml"), "focus_accent = \"blue\"").expect("write");
        std::fs::write(dir.path().join("forest.toml"), "focus_accent = \"green\"").expect("write");
        std::fs::write(dir.path().join("readme.txt"), "not a theme").expect("write");

        // When discovering themes.
        let themes = discover_themes(dir.path()).expect("discover");

        // Then only .toml files are found, sorted by name.
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].0, "forest");
        assert_eq!(themes[1].0, "ocean");
    }

    #[rstest::rstest]
    fn load_theme_searches_user_then_system() {
        // Given a user dir without the theme and a system dir with it.
        let user_dir = TempDir::new().expect("temp dir");
        let system_dir = TempDir::new().expect("temp dir");
        std::fs::write(
            system_dir.path().join("ocean.toml"),
            "focus_accent = \"blue\"",
        )
        .expect("write");

        // When loading from both dirs.
        let theme = load_theme("ocean", user_dir.path(), system_dir.path()).expect("load");

        // Then the system theme is loaded.
        assert_eq!(theme.focus_accent, ratatui::style::Color::Blue);
    }

    #[rstest::rstest]
    fn load_theme_user_overrides_system() {
        // Given user dir with "ocean" (blue) and system dir with "ocean" (red).
        let user_dir = TempDir::new().expect("temp dir");
        let system_dir = TempDir::new().expect("temp dir");
        std::fs::write(
            user_dir.path().join("ocean.toml"),
            "focus_accent = \"blue\"",
        )
        .expect("write");
        std::fs::write(
            system_dir.path().join("ocean.toml"),
            "focus_accent = \"red\"",
        )
        .expect("write");

        // When loading from both dirs.
        let theme = load_theme("ocean", user_dir.path(), system_dir.path()).expect("load");

        // Then the user version wins.
        assert_eq!(theme.focus_accent, ratatui::style::Color::Blue);
    }

    #[rstest::rstest]
    fn resolve_theme_searches_both_dirs() {
        // Given user dir with "ocean" and system dir with "forest".
        let user_dir = TempDir::new().expect("temp dir");
        let system_dir = TempDir::new().expect("temp dir");
        std::fs::write(
            user_dir.path().join("ocean.toml"),
            "focus_accent = \"blue\"",
        )
        .expect("write");
        std::fs::write(
            system_dir.path().join("forest.toml"),
            "focus_accent = \"green\"",
        )
        .expect("write");

        // When resolving "forest" (only in system).
        let theme =
            resolve_theme(Some("forest"), user_dir.path(), system_dir.path()).expect("resolve");

        // Then the system theme is found.
        assert_eq!(theme.focus_accent, ratatui::style::Color::Green);
    }
}
