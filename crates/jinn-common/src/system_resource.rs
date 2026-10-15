//! System resource loader - loads internal system files from user and system directories.
//!
//! System resources are files like `_compaction.md` that live in the prompts
//! directory but are prefixed with `_` to mark them as internal. They are not
//! prompt templates (no TOML frontmatter) and should not appear in the user-facing
//! autocomplete picker.
//!
//! Resolution order: user override → system default. Hard failure if neither exists.

use std::path::Path;

use error_stack::{Report, ResultExt as _};

/// Error type for system resource loading failures.
#[derive(Debug, wherror::Error)]
#[error("system resource load error")]
pub struct SystemResourceError;

/// Load a system resource file by name from user and system directories.
///
/// Resolution order:
/// 1. `user_dir/<name>` - user override
/// 2. `system_dir/<name>` - system default
///
/// # Errors
///
/// Returns an error if neither file exists or if reading fails.
/// The caller should attach contextual information via `.attach()`.
pub fn load_system_resource(
    name: &str,
    user_dir: &Path,
    system_dir: &Path,
) -> Result<String, Report<SystemResourceError>> {
    let user_path = user_dir.join(name);
    let system_path = system_dir.join(name);

    // User override takes priority.
    if user_path.is_file() {
        return std::fs::read_to_string(&user_path)
            .change_context(SystemResourceError)
            .attach(format!(
                "failed to read user system resource: {}",
                user_path.display()
            ));
    }

    // System default.
    if system_path.is_file() {
        return std::fs::read_to_string(&system_path)
            .change_context(SystemResourceError)
            .attach(format!(
                "failed to read system resource: {}",
                system_path.display()
            ));
    }

    Err(Report::new(SystemResourceError)
        .attach(format!("system resource '{name}' not found"))
        .attach(format!("  searched user:   {}", user_path.display()))
        .attach(format!("  searched system: {}", system_path.display())))
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
        // Given a user dir with the resource and an empty system dir.
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/prompts");
        let system_dir = root.path().join("share/prompts");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::write(user_dir.join("_compaction.md"), "user prompt").expect("write");

        // When loading the system resource.
        let result = load_system_resource("_compaction.md", &user_dir, &system_dir);

        // Then the user file content is returned.
        assert_eq!(result.expect("load"), "user prompt");
    }

    #[rstest::rstest]
    fn load_falls_back_to_system_dir() {
        // Given an empty user dir and a system dir with the resource.
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/prompts");
        let system_dir = root.path().join("share/prompts");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::create_dir_all(&system_dir).expect("create system dir");
        std::fs::write(system_dir.join("_compaction.md"), "system prompt").expect("write");

        // When loading the system resource.
        let result = load_system_resource("_compaction.md", &user_dir, &system_dir);

        // Then the system file content is returned.
        assert_eq!(result.expect("load"), "system prompt");
    }

    #[rstest::rstest]
    fn load_prefers_user_over_system() {
        // Given both dirs have the resource with different content.
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/prompts");
        let system_dir = root.path().join("share/prompts");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::create_dir_all(&system_dir).expect("create system dir");
        std::fs::write(user_dir.join("_compaction.md"), "user override").expect("write");
        std::fs::write(system_dir.join("_compaction.md"), "system default").expect("write");

        // When loading the system resource.
        let result = load_system_resource("_compaction.md", &user_dir, &system_dir);

        // Then the user file content wins.
        assert_eq!(result.expect("load"), "user override");
    }

    #[rstest::rstest]
    fn load_returns_err_when_neither_exists() {
        // Given empty user and system dirs.
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/prompts");
        let system_dir = root.path().join("share/prompts");

        // When loading the system resource.
        let result = load_system_resource("_compaction.md", &user_dir, &system_dir);

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
        // Given a system dir with an unreadable file (permission denied).
        let root = tempfile::TempDir::new().expect("temp dir");
        let user_dir = root.path().join("config/jinn/prompts");
        let system_dir = root.path().join("share/prompts");
        std::fs::create_dir_all(&user_dir).expect("create user dir");
        std::fs::create_dir_all(&system_dir).expect("create system dir");

        let file_path = system_dir.join("_compaction.md");
        std::fs::write(&file_path, "system prompt").expect("write");

        // Make file unreadable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000))
                .expect("chmod");
        }

        // When loading the system resource.
        #[cfg_attr(unix, expect(unused_variables, reason = "Unix asserts below"))]
        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows skips the unreadable-file assertion"
            )
        )]
        let result = load_system_resource("_compaction.md", &user_dir, &system_dir);

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
