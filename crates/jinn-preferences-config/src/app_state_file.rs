//! Application state file data type and file I/O.
//!
//! Defines [`AppStateFile`] as the schema for `state.toml`,
//! along with loading and saving logic. The file lives at
//! `~/.local/state/jinn/state.toml` and holds runtime state
//! (last model, theme, persona, sidebar width) that changes
//! frequently but is not user-editable configuration.

use std::path::Path;

use error_stack::{Report, ResultExt as _};
use serde::{Deserialize, Serialize};
use wherror::Error;

use jinn_core_types::model_selection::{self, ModelSelection};
/// Errors that can occur during app state file I/O.
#[derive(Debug, Error)]
pub enum AppStateFileError {
    /// Filesystem I/O failure.
    #[error("app state I/O error")]
    Io,
    /// TOML parsing or structural error.
    #[error("app state parse error")]
    Parse,
}

/// Runtime application state persisted in `state.toml`.
///
/// This file stores application state that changes frequently
/// and should survive app restarts — e.g., the last model selected
/// from a picker. Unlike `jinn.toml`, this file is machine-managed
/// and never hand-edited by users.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppStateFile {
    /// The last model or alloy selected from the model picker.
    ///
    /// Accepts both the legacy bare-string format (`last_model = "ollama/llama3"`)
    /// and the new enum format (`last_model = {single = "ollama/llama3"}`).
    /// Serialization always writes the new format.
    #[serde(default, with = "model_selection::last_model_compat")]
    pub last_model: Option<ModelSelection>,
    /// The name of the active theme. `None` uses the built-in theme.
    /// Corresponds to a file in `~/.config/jinn/themes/<name>.toml`.
    #[serde(default)]
    pub theme_name: Option<String>,
    /// The name of the active persona. `None` uses the default (`coding-assistant`).
    /// Corresponds to a file in `~/.config/jinn/personas/<name>.md`.
    #[serde(default)]
    pub persona_name: Option<String>,
    /// Sidebar width in columns. `None` means use the built-in default (30 columns).
    #[serde(default)]
    pub sidebar_width: Option<u16>,
    /// The last-selected reasoning effort, mirrored from the picker.
    ///
    /// Seeds new sessions at creation; each session then owns its own
    /// `profile.reasoning_effort`. `None` means "let the provider decide".
    #[serde(default)]
    pub reasoning_effort: Option<jinn_core_types::ReasoningEffort>,
}

/// Loads app state from a specific path.
///
/// If the path does not exist, returns default state.
/// Does NOT auto-create the file (unlike `jinn.toml`).
///
/// # Errors
///
/// Returns [`AppStateFileError::Parse`] if the TOML is malformed.
/// Returns [`AppStateFileError::Io`] if the file cannot be read.
pub fn load_app_state_from<P>(path: P) -> Result<AppStateFile, Report<AppStateFileError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if !path.exists() {
        return Ok(AppStateFile::default());
    }

    let content = std::fs::read_to_string(path)
        .change_context(AppStateFileError::Io)
        .attach("failed to read app state file")?;

    toml::from_str(&content)
        .change_context(AppStateFileError::Parse)
        .attach("failed to parse app state file")
}

/// Saves app state to a specific path.
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns [`AppStateFileError::Parse`] if serialization fails.
/// Returns [`AppStateFileError::Io`] if writing fails.
pub fn save_app_state_to<P>(state: &AppStateFile, path: P) -> Result<(), Report<AppStateFileError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(AppStateFileError::Io)
            .attach("failed to create app state directory")?;
    }

    let content = toml::to_string_pretty(state)
        .change_context(AppStateFileError::Parse)
        .attach("failed to serialize app state")?;

    std::fs::write(path, content)
        .change_context(AppStateFileError::Io)
        .attach("failed to write app state file")
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
    use tempfile::TempDir;

    use super::*;

    #[rstest::rstest]
    fn default_state_has_all_none() {
        // Given default state.
        let state = AppStateFile::default();

        // Then all fields are None.
        assert!(state.last_model.is_none());
        assert!(state.theme_name.is_none());
        assert!(state.persona_name.is_none());
        assert!(state.sidebar_width.is_none());
        assert!(state.reasoning_effort.is_none());
    }

    #[rstest::rstest]
    fn round_trip_all_fields_set() {
        // Given state with all fields populated.
        let state = AppStateFile {
            last_model: Some(ModelSelection::Single("ollama/llama3".to_owned())),
            theme_name: Some("gruvbox-dark".to_owned()),
            persona_name: Some("coder".to_owned()),
            sidebar_width: Some(40),
            reasoning_effort: Some(jinn_core_types::ReasoningEffort::High),
        };

        // When serializing and deserializing.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state.toml");
        save_app_state_to(&state, &path).expect("save");
        let reloaded = load_app_state_from(&path).expect("load");

        // Then all fields match.
        assert_eq!(reloaded, state);
    }

    #[rstest::rstest]
    fn round_trip_all_fields_none() {
        // Given default state (all fields None).
        let state = AppStateFile::default();

        // When serializing and deserializing.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state.toml");
        save_app_state_to(&state, &path).expect("save");
        let reloaded = load_app_state_from(&path).expect("load");

        // Then all fields are still None.
        assert_eq!(reloaded, state);
    }

    #[rstest::rstest]
    fn load_returns_defaults_when_file_missing() {
        // Given a path that does not exist.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nonexistent.toml");

        // When loading.
        let state = load_app_state_from(&path).expect("load");

        // Then defaults are returned.
        assert_eq!(state, AppStateFile::default());
        // And the file was NOT auto-created.
        assert!(!path.exists());
    }

    #[rstest::rstest]
    fn save_creates_parent_directories() {
        // Given a nested path that doesn't exist.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nested").join("dir").join("state.toml");

        // When saving.
        let state = AppStateFile::default();
        save_app_state_to(&state, &path).expect("save");

        // Then the file exists.
        assert!(path.exists());
    }

    #[rstest::rstest]
    fn load_legacy_bare_string_last_model() {
        // Given a state file with the old bare-string last_model format.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state.toml");
        std::fs::write(
            &path,
            r##"last_model = "ollama/llama3"
theme_name = "gruvbox-dark"
"##,
        )
        .expect("write");

        // When loading.
        let state = load_app_state_from(&path).expect("load");

        // Then last_model is parsed as Single.
        let expected = AppStateFile {
            last_model: Some(ModelSelection::Single("ollama/llama3".to_owned())),
            theme_name: Some("gruvbox-dark".to_owned()),
            persona_name: None,
            sidebar_width: None,
            reasoning_effort: None,
        };
        assert_eq!(state, expected);
    }

    #[rstest::rstest]
    fn legacy_json_without_reasoning_effort_deserializes_to_none() {
        // Given a state file from an older version that lacks reasoning_effort.
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("state.toml");
        std::fs::write(
            &path,
            r##"last_model = "ollama/llama3"
theme_name = "gruvbox-dark"
"##,
        )
        .expect("write");

        // When loading.
        let state = load_app_state_from(&path).expect("load");

        // Then reasoning_effort is None (provider decides).
        assert!(state.reasoning_effort.is_none());
    }
}
