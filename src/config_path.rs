//! Which `jinn.toml` a run reads and writes.
//!
//! `jinn.toml` normally lives at the platform's per-user location
//! (`~/.config/jinn/jinn.toml` on Linux). A run may instead point
//! `--config` at a different file, which redirects *both* reads and
//! writes for that run so the layer stays a single coherent source of
//! truth.
//!
//! The two cases differ deliberately:
//!
//! - The default path is a first-install affordance — nobody typed it, so
//!   a missing file is seeded from the embedded template.
//! - An explicit `--config` path is a user reference. If it does not
//!   exist, that is almost certainly a typo, and silently starting from
//!   defaults is a failure worth surfacing instead. Nothing is created.
//!
//! [`resolve_config_path`] only *decides*; it never writes. Seeding is a
//! separate step, which keeps this function pure and its tests free of
//! filesystem mutation beyond setup.
//!
//! Like the other CLI resolvers, this module is the seam where the raw
//! `--config` value is re-typed; the error carries the user's own text so
//! the usage message can name it.
#![expect(
    clippy::disallowed_types,
    reason = "argv boundary: the raw --config flag value is re-typed here; the error carries the user's raw text for the usage message"
)]

use std::path::{Path, PathBuf};

use error_stack::Report;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};
use wherror::Error;

/// The `--config` flag names a file that does not exist.
///
/// The path is carried as data so the message can name it — a user who
/// passed `--config /typo.toml` needs to see *that* file, not the
/// generic `jinn.toml` a default-path failure would report.
#[derive(Debug, Error)]
#[error(debug)]
pub struct ConfigPathError {
    /// The `--config` path that could not be used.
    pub path: PathBuf,
}

/// Where the run's `jinn.toml` lives, and what still needs doing to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPathResolution {
    /// The document to read and write for the whole run.
    ///
    /// Always absolute: the default location is absolute by construction,
    /// and a `--config` override is resolved against the current directory
    /// at this boundary so every consumer downstream reads one shape.
    pub path: MarkedPathBuf<Absolute>,
    /// True only when `path` is the default location and did not exist.
    ///
    /// An override never sets this: the user supplies their own file, and
    /// a missing one is an error rather than something to create.
    pub seed_template: bool,
}

/// Chooses the `jinn.toml` for a run.
///
/// `override_path` is the `--config` value, if the user gave one.
/// `default_path` is the platform's per-user location.
///
/// # Errors
///
/// Returns [`ConfigPathError`] when `override_path` is given but does not
/// exist. The default path never errors — a missing one resolves to a
/// resolution with `seed_template: true`.
pub fn resolve_config_path(
    override_path: Option<&Path>,
    default_path: &MarkedPathBuf<Absolute>,
) -> Result<ConfigPathResolution, Report<ConfigPathError>> {
    match override_path {
        Some(path) => resolve_override(path),
        None => Ok(resolve_default(default_path)),
    }
}

/// Resolves an explicit `--config` path, which must already exist.
///
/// A relative override is resolved against the current directory here —
/// the CLI boundary — so the resolution is absolute. The error carries the
/// user's original spelling, not the resolved form.
fn resolve_override(path: &Path) -> Result<ConfigPathResolution, Report<ConfigPathError>> {
    if path.exists() {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            let cwd = std::env::current_dir()
                .expect("current directory is readable for a relative --config");
            cwd.join(path)
        };
        let typed =
            MarkedPathBuf::<Absolute>::new(absolute).expect("resolved --config path is absolute");
        return Ok(ConfigPathResolution {
            path: typed,
            seed_template: false,
        });
    }
    Err(Report::new(ConfigPathError {
        path: path.to_path_buf(),
    })
    .attach(format!("config file not found: {}", path.display()))
    .attach("create it first, or omit --config to use the default location"))
}

/// Chooses the target for a `jinn config init` invocation.
///
/// An override is honored here even when the file does not exist — writing
/// the file is the entire point of the command, so requiring it to be
/// present first would make `jinn --config <path> config init` impossible.
/// That is also why this cannot reuse [`resolve_config_path`], whose
/// existence check is correct for a run that intends to *read* the file.
///
/// Infallible: there is no override to reject, and the default path is
/// always a legitimate thing to create.
#[must_use]
pub fn config_init_target(
    override_path: Option<&Path>,
    default_path: &MarkedPathBuf<Absolute>,
) -> MarkedPathBuf<Absolute> {
    match override_path {
        Some(path) => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                let cwd = std::env::current_dir()
                    .expect("current directory is readable for a relative --config");
                cwd.join(path)
            };
            MarkedPathBuf::<Absolute>::new(absolute).expect("resolved --config path is absolute")
        }
        None => default_path.clone(),
    }
}

/// Resolves the platform default, flagging a missing file for seeding.
fn resolve_default(default_path: &MarkedPathBuf<Absolute>) -> ConfigPathResolution {
    ConfigPathResolution {
        path: default_path.clone(),
        seed_template: !default_path.as_path().exists(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default location for a test: a `jinn.toml` that does not exist.
    fn absent_default() -> (jinn_testutil::JinnTempDir, MarkedPathBuf<Absolute>) {
        let root = jinn_testutil::tempdir();
        let path = root
            .path
            .join("jinn.toml")
            .expect("temp root joins a bare name");
        (root, path)
    }

    #[rstest::rstest]
    #[test]
    fn absent_default_file_is_flagged_for_seeding() {
        // Given a default path that does not exist.
        let (_root, default) = absent_default();

        // When resolving.
        let result = resolve_config_path(None, &default).expect("resolves");

        // Then the default path is chosen.
        assert_eq!(result.path, default);
        // And it is flagged for template seeding.
        assert!(result.seed_template);
    }

    #[rstest::rstest]
    #[test]
    fn present_default_file_is_not_flagged_for_seeding() {
        // Given a default path that already exists.
        let (_root, default) = absent_default();
        std::fs::write(&default, "# existing\n").expect("write");

        // When resolving.
        let result = resolve_config_path(None, &default).expect("resolves");

        // Then the default path is chosen and nothing needs seeding.
        assert_eq!(result.path, default);
        assert!(!result.seed_template);
    }

    #[rstest::rstest]
    #[test]
    fn existing_override_is_chosen() {
        // Given a default path and a separate override file that exists.
        let (_root, default) = absent_default();
        let override_root = jinn_testutil::tempdir();
        let override_path = override_root
            .path
            .join("alt.toml")
            .expect("temp root joins a bare name");
        std::fs::write(&override_path, "# alt\n").expect("write");

        // When resolving.
        let result =
            resolve_config_path(Some(override_path.as_path()), &default).expect("resolves");

        // Then the override path is chosen.
        assert_eq!(result.path, override_path);
        // And it is not flagged for seeding — the user supplied the file.
        assert!(!result.seed_template);
    }

    #[rstest::rstest]
    #[test]
    fn missing_override_is_rejected() {
        // Given an override path that does not exist.
        let (_root, default) = absent_default();
        let override_root = jinn_testutil::tempdir();
        let override_path = override_root
            .path
            .join("missing.toml")
            .expect("temp root joins a bare name");

        // When resolving.
        let result = resolve_config_path(Some(override_path.as_path()), &default);

        // Then resolution fails.
        let report = result.expect_err("missing override must fail");
        // And the error names the offending path.
        assert_eq!(report.current_context().path, override_path.as_path());
        // And nothing was created on disk.
        assert!(!override_path.exists());
    }

    #[rstest::rstest]
    #[test]
    fn missing_override_ignores_a_missing_default() {
        // Given a missing override alongside a default that also does not exist.
        let (_root, default) = absent_default();
        let override_root = jinn_testutil::tempdir();
        let override_path = override_root
            .path
            .join("missing.toml")
            .expect("temp root joins a bare name");

        // When resolving.
        let result = resolve_config_path(Some(override_path.as_path()), &default);

        // Then it fails on the override rather than seeding the default.
        assert!(result.is_err());
        // And the default was not seeded.
        assert!(!default.exists());
    }
}
