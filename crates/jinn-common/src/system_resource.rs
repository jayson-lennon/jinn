//! System resource loader - loads internal system files from the user directory.
//!
//! System resources are files like `_handoff.md` and `_static.md` that live in
//! the prompts directory but are prefixed with `_` to mark them as internal.
//! They are not prompt templates (no TOML frontmatter) and should not appear
//! in the user-facing autocomplete picker.
//!
//! Two load contracts:
//!
//! - [`load_system_resource`] **hard-fails** when the file is absent — for
//!   resources a turn cannot proceed without (the handoff instructions).
//! - [`try_load_system_resource`] **soft-fails**: absence or an unreadable
//!   file is a tracing warning and `None` — for optional enhancements the
//!   user silences by deleting (the static prompt section).

use error_stack::{Report, ResultExt};
use marked_path::{Absolute, MarkedPath, MarkedPathAccess};

/// Error type for system resource loading failures.
#[derive(Debug, wherror::Error)]
#[error("system resource load error")]
pub struct SystemResourceError;

/// Load a system resource file by name from the user directory.
///
/// Reads `user_dir/<name>`: internal system resources live beside the prompt
/// templates they serve.
///
/// # Errors
///
/// Returns an error if the file does not exist or if reading fails.
/// The caller should attach contextual information via `.attach()`.
pub fn load_system_resource(
    name: &str,
    user_dir: &MarkedPath<Absolute>,
) -> Result<String, Report<SystemResourceError>> {
    let path = resolve_resource_path(name, user_dir)?;

    if !path.is_file() {
        return Err(Report::new(SystemResourceError)
            .attach(format!("system resource '{name}' not found"))
            .attach(format!("  searched: {}", path.to_string_lossy())));
    }

    std::fs::read_to_string(&path)
        .change_context(SystemResourceError)
        .attach(format!(
            "failed to read system resource: {}",
            path.to_string_lossy()
        ))
}

/// Load a system resource file by name from the user directory, tolerating
/// absence.
///
/// Identical lookup to [`load_system_resource`], but a missing file — or one
/// that cannot be read — is a tracing warning and `None` rather than an
/// error. For optional system resources: the caller renders nothing and the
/// turn proceeds. Blank-after-trim content is returned as-is; callers that
/// treat blank as absent trim themselves.
///
/// A path that cannot be built (the join failed) also degrades to `None`
/// with the same warning; this loader has no error path.
#[must_use]
pub fn try_load_system_resource(name: &str, user_dir: &MarkedPath<Absolute>) -> Option<String> {
    let path = match resolve_resource_path(name, user_dir) {
        Ok(path) => path,
        Err(report) => {
            tracing::warn!(%report, "cannot resolve system resource path; treating as absent");
            return None;
        }
    };

    if !path.is_file() {
        return None;
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(error) => {
            tracing::warn!(
                path = %path.to_string_lossy(),
                %error,
                "system resource exists but could not be read; treating as absent"
            );
            None
        }
    }
}

/// Joins the resource name onto the user directory.
///
/// Shared by both load contracts; the only fallible step either one has.
fn resolve_resource_path(
    name: &str,
    user_dir: &MarkedPath<Absolute>,
) -> Result<marked_path::MarkedPathBuf<Absolute>, Report<SystemResourceError>> {
    user_dir
        .join(name)
        .change_context(SystemResourceError)
        .attach("unable to build resource path")
        .attach_with(|| format!("path: {}", user_dir.as_path().join(name).to_string_lossy()))
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

    use super::*;

    #[rstest::rstest]
    fn load_returns_content_from_user_dir() {
        // Given a user dir containing the resource.
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::write(
            user_dir
                .join("_test_resource.md")
                .expect("typed parent joins a bare name"),
            "user prompt",
        )
        .expect("write");

        // When loading the system resource.
        let result = load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then the user file content is returned.
        assert_eq!(result.expect("load"), "user prompt");
    }

    #[rstest::rstest]
    fn try_load_returns_content_from_user_dir() {
        // Given a user dir containing the resource.
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::write(
            user_dir
                .join("_test_resource.md")
                .expect("typed parent joins a bare name"),
            "static body",
        )
        .expect("write");

        // When loading the resource tolerantly.
        let result = try_load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then the file content is returned.
        assert_eq!(result, Some("static body".to_owned()));
    }

    #[rstest::rstest]
    fn try_load_returns_none_when_file_missing() {
        // Given a user dir without the resource.
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");

        // When loading the resource tolerantly.
        let result = try_load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then nothing is returned and there is no error path.
        assert_eq!(result, None);
    }

    #[rstest::rstest]
    fn try_load_returns_none_for_dir_shaped_resource() {
        // Given a directory where the resource would be.
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(
            user_dir
                .join("_test_resource.md")
                .expect("typed parent joins a bare name"),
        )
        .expect("create dir");

        // When loading the resource tolerantly.
        let result = try_load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then nothing is returned — a directory is not a readable resource.
        assert_eq!(result, None);
    }

    #[rstest::rstest]
    fn try_load_returns_none_on_read_failure() {
        // Given a user dir with an unreadable resource (permission denied).
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&user_dir).expect("create user dir");

        let file_path = user_dir
            .join("_test_resource.md")
            .expect("typed parent joins a bare name");
        std::fs::write(&file_path, "user prompt").expect("write");

        // Make file unreadable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }

        // When loading the resource tolerantly.
        // On Windows the assertion below is compiled out, so the binding
        // would otherwise be unused there.
        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows skips the unreadable-file assertion"
            )
        )]
        let result = try_load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then nothing is returned (on Unix, permission denied prevents
        // reading) and no panic or error escapes.
        #[cfg(unix)]
        assert_eq!(result, None, "expected None when file is unreadable");

        // Restore permissions so temp dir cleanup can succeed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644))
                .expect("restore perms");
        }
    }

    #[rstest::rstest]
    fn load_returns_err_when_file_missing() {
        // Given a user dir without the resource.
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");

        // When loading the system resource.
        let result = load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then an error is returned.
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err:#?}");
        assert!(
            msg.contains("not found"),
            "error should mention 'not found'"
        );
    }

    #[rstest::rstest]
    fn load_returns_err_on_read_failure() {
        // Given a user dir with an unreadable resource (permission denied).
        let root = jinn_testutil::tempdir();
        let user_dir = root
            .path
            .join("config/jinn/prompts")
            .expect("temp root joins a bare name");
        std::fs::create_dir_all(&user_dir).expect("create user dir");

        let file_path = user_dir
            .join("_test_resource.md")
            .expect("typed parent joins a bare name");
        std::fs::write(&file_path, "user prompt").expect("write");

        // Make file unreadable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }

        // When loading the system resource.
        // On Windows the assertion below is compiled out, so the binding
        // would otherwise be unused there.
        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows skips the unreadable-file assertion"
            )
        )]
        let result = load_system_resource("_test_resource.md", &user_dir.as_marked_path());

        // Then an error is returned (on Unix, permission denied prevents reading).
        #[cfg(unix)]
        assert!(result.is_err(), "expected error when file is unreadable");

        // Restore permissions so temp dir cleanup can succeed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o644))
                .expect("restore perms");
        }
    }
}
