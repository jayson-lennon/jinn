//! `jinn.toml` bootstrap: where it lives, and the template it is seeded from.
//!
//! `jinn.toml` has no aggregate schema struct. Each section is declared in
//! `crate::schemas` by a `Configurable` or `ConfigList` impl and read
//! through the configuration layer (`jinn_config::ConfigLayer`). This
//! module owns the two things a section cannot provide: the canonical
//! on-disk path, and the comment-rich template a fresh install is seeded
//! from.
//!
//! The template is documentation, not authority — it is not consulted at
//! read time. It is written as *bytes*, never serialized from a struct,
//! because serializing would strip every comment it ships with.

use std::path::{Path, PathBuf};

use error_stack::{Report, ResultExt as _};
use jinn_common::app_info::{APP_NAME, PREFS_FILE_NAME};
use wherror::Error;

// Every section type lives in `crate::schemas` — one module per section,
// wherever the feature that runs it lives — and the common ones are
// re-exported here so a consumer has one import home for `jinn.toml`
// shapes.
pub use crate::schemas::{
    AutoPruneConfig, ChatLogConfig, CompactionConfig, CwdSelectorConfig, DiscordConfig,
    InteractiveTermPrefs, LegacyCommandPolicyRule, McpServerConfig, McpServersConfig,
    MinimapConfig, ProjectConfig, RequestRetryConfig, SessionLifecycle, SkillsConfig,
    StallWatchdogConfig, ToolCallWatchdogConfig, ToolsConfig, TransportKind, WebSearchConfig,
};

/// Canonical default `jinn.toml` embedded at compile time.
///
/// Used both to auto-create the file on first run and to back the
/// `jinn config init` subcommand.
pub const DEFAULT_CONFIG: &str = include_str!("default_jinn.toml");

/// Errors that can occur while seeding `jinn.toml` on disk.
#[derive(Debug, Error)]
pub enum UserPreferencesError {
    /// Filesystem I/O failure.
    #[error("user preferences I/O error")]
    Io,
    /// TOML parsing or structural error.
    #[error("user preferences parse error")]
    Parse,
}

/// Returns the path to the user's `jinn.toml`.
///
/// Uses `dirs::config_dir()` → `~/.config/jinn/jinn.toml`.
#[must_use]
pub fn preferences_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join(PREFS_FILE_NAME)
}

/// Writes the canonical default template to `path`, creating parents.
///
/// # Errors
///
/// Returns [`UserPreferencesError::Io`] if directory creation or file
/// writing fails.
pub fn create_default_preferences_to<P>(path: P) -> Result<(), Report<UserPreferencesError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(UserPreferencesError::Io)
            .attach("failed to create preferences directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(UserPreferencesError::Io)
        .attach("failed to write default user preferences")
}

/// Error returned by [`init_default_config_to`].
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct InitDefaultConfigError;

/// Outcome of [`init_default_config_to`].
#[derive(Debug)]
pub enum InitOutcome {
    /// Template was written to a previously-missing path.
    Created,
    /// Existing file was overwritten (caller passed `force: true`).
    Overwritten,
}

/// Writes [`DEFAULT_CONFIG`] to `path`.
///
/// - If `path` does not exist: writes the template, returns [`InitOutcome::Created`].
/// - If `path` exists and `force` is false: returns `Err(InitDefaultConfigError)`.
/// - If `path` exists and `force` is true: overwrites, returns [`InitOutcome::Overwritten`].
///
/// Creates parent directories as needed.
///
/// # Errors
///
/// Returns [`Report<InitDefaultConfigError>`] if the file already exists and
/// `force` is false, or if directory creation / file writing fails.
pub fn init_default_config_to<P>(
    path: P,
    force: bool,
) -> Result<InitOutcome, Report<InitDefaultConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    let existed = path.exists();

    if existed && !force {
        return Err(Report::new(InitDefaultConfigError))
            .attach("path already exists; pass --force to overwrite")
            .attach(format!("path: {}", path.display()));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(InitDefaultConfigError)
            .attach("failed to create preferences directory")?;
    }

    std::fs::write(path, DEFAULT_CONFIG)
        .change_context(InitDefaultConfigError)
        .attach("failed to write default user preferences")?;

    if existed {
        Ok(InitOutcome::Overwritten)
    } else {
        Ok(InitOutcome::Created)
    }
}
