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
use jinn_common::app_paths::AppPaths;
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

/// Canonical default `stream_rules.toml` embedded at compile time.
///
/// The shipped command guards, in the same `[[stream_rules.entry]]` shape
/// `jinn.toml` uses. `jinn install` writes this file once, next to
/// `jinn.toml`, and never touches an existing one; `jinn config init
/// --force` is the supported path for overwriting it with a newer shipped
/// set. At launch the runtime reads the embedded copy directly — a missing
/// user file is the default state, not an error.
pub const STREAM_RULES_TEMPLATE: &str = include_str!("stream_rules.toml");

/// The filename of the user-ownable stream-rules file, relative to the
/// directory `jinn.toml` lives in.
pub const STREAM_RULES_FILE_NAME: &str = "stream_rules.toml";

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
/// Derives from [`AppPaths`] (`~/.config/jinn/jinn.toml`), so the
/// `JINN_PATHS_ROOT` override is honored here too.
#[must_use]
pub fn preferences_path() -> PathBuf {
    AppPaths::default().preferences_path()
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

/// Outcome of a stream-rules seeding attempt.
///
/// Three-variant, because the two callers mean different things by "the
/// file is already there": `jinn install` treats it as a hard skip (the
/// file is never read or modified), while `jinn config init` without
/// `--force` treats it as a soft skip worth reporting and carrying on
/// from — the command's own target is `jinn.toml`, which may still have
/// been created successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamRulesSeedOutcome {
    /// Template was written to a previously-missing path.
    Created(PathBuf),
    /// The file already existed and was never read or modified this run.
    ///
    /// Only [`init_stream_rules_for_install`] returns this; it is the
    /// exactly-once contract made visible.
    Untouched(PathBuf),
    /// The existing file was overwritten (caller passed `force: true`).
    ///
    /// Only [`init_stream_rules_to`] returns this; it is the supported
    /// path for pulling in an updated shipped set.
    Overwritten(PathBuf),
}

impl StreamRulesSeedOutcome {
    /// The file path this outcome refers to.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            StreamRulesSeedOutcome::Created(path)
            | StreamRulesSeedOutcome::Untouched(path)
            | StreamRulesSeedOutcome::Overwritten(path) => path,
        }
    }
}

/// Writes [`STREAM_RULES_TEMPLATE`] to `path` exactly once, for
/// `jinn install`.
///
/// A missing file is created with the embedded template. An existing file
/// is **never read or modified** — there is no force flag on this path, by
/// the same contract as `jinn.toml` under `install`: the user's edits win,
/// and pulling in a newer shipped set is what `jinn config init --force`
/// is for.
///
/// # Errors
///
/// Returns [`Report<InitDefaultConfigError>`] if directory creation or
/// file writing fails.
pub fn init_stream_rules_for_install<P>(
    path: P,
) -> Result<StreamRulesSeedOutcome, Report<InitDefaultConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if path.exists() {
        return Ok(StreamRulesSeedOutcome::Untouched(path.to_path_buf()));
    }

    write_stream_rules_template(path)?;
    Ok(StreamRulesSeedOutcome::Created(path.to_path_buf()))
}

/// Writes [`STREAM_RULES_TEMPLATE`] to `path`, for `jinn config init`.
///
/// - Missing file → created, [`StreamRulesSeedOutcome::Created`].
/// - Existing file, `force: false` → left alone,
///   [`StreamRulesSeedOutcome::Untouched`] — a fresh `config init` next to
///   an already-installed rules file must not clobber it.
/// - Existing file, `force: true` → overwritten with the embedded template,
///   [`StreamRulesSeedOutcome::Overwritten`].
///
/// # Errors
///
/// Returns [`Report<InitDefaultConfigError>`] if directory creation or
/// file writing fails.
pub fn init_stream_rules_to<P>(
    path: P,
    force: bool,
) -> Result<StreamRulesSeedOutcome, Report<InitDefaultConfigError>>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();

    if path.exists() {
        if !force {
            return Ok(StreamRulesSeedOutcome::Untouched(path.to_path_buf()));
        }
    } else {
        write_stream_rules_template(path)?;
        return Ok(StreamRulesSeedOutcome::Created(path.to_path_buf()));
    }

    write_stream_rules_template(path)?;
    Ok(StreamRulesSeedOutcome::Overwritten(path.to_path_buf()))
}

/// Writes the embedded stream-rules template to `path`, creating parents.
fn write_stream_rules_template(path: &Path) -> Result<(), Report<InitDefaultConfigError>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .change_context(InitDefaultConfigError)
            .attach("failed to create stream rules directory")?;
    }

    std::fs::write(path, STREAM_RULES_TEMPLATE)
        .change_context(InitDefaultConfigError)
        .attach("failed to write stream rules template")
}
