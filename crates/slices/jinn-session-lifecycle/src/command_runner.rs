//! Lifecycle command runner - executes setup and teardown commands.
//!
//! Two entry points:
//! - [`run_setup_command`] - expects stdout output (last line becomes the session CWD)
//! - [`run_teardown_command`] - only checks exit code, output is irrelevant

use std::path::PathBuf;

use error_stack::Report;
use wherror::Error;

/// Canonicalizes through the shared verbatim-prefix-stripping helper, so the
/// session CWD is a plain path (`C:\…` on Windows) comparable to every other
/// path jinn builds. Identity on Unix.
fn strip_verbatim(result: std::io::Result<PathBuf>) -> std::io::Result<PathBuf> {
    result.map(jinn_common::strip_verbatim_prefix)
}

// Note: kill_process_tree(&mut Child) is NOT imported here — the cancel path
// uses the PID-based kill_process_group_by_pid (called from the actor's cancel
// handler), and this module no longer holds the Child past spawn.

/// Errors that can occur when running a lifecycle command.
#[derive(Debug, Error)]
pub enum LifecycleCommandError {
    /// The command exited with a non-zero status.
    #[error("command failed with exit code {exit_code:?}")]
    CommandFailed {
        /// The process exit code.
        exit_code: Option<i32>,
        /// Captured stdout output.
        stdout: String,
        /// Captured stderr output.
        stderr: String,
    },

    /// The path returned by the command could not be resolved on the filesystem.
    #[error("path does not exist or cannot be resolved: {path}")]
    InvalidPath {
        /// The path that was returned by the command.
        path: PathBuf,
    },
    /// The path returned by the command is not a directory.
    #[error("path is not a directory: {path}")]
    NotADirectory {
        /// The path that was returned by the command.
        path: PathBuf,
    },
    /// Failed to spawn the command.
    #[error("failed to execute command")]
    ExecutionFailed,
}

/// Shared shell invocation logic used by both setup and teardown runners.
async fn run_command(
    command: &str,
    shell: &str,
) -> Result<(std::process::Output, String, String), Report<LifecycleCommandError>> {
    use error_stack::ResultExt as _;

    let output = tokio::process::Command::new(shell)
        .arg("-c")
        .arg(command)
        .output()
        .await
        .change_context(LifecycleCommandError::ExecutionFailed)
        .attach("failed to spawn lifecycle command")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(Report::new(LifecycleCommandError::CommandFailed {
            exit_code: output.status.code(),
            stdout,
            stderr,
        }));
    }

    Ok((output, stdout, stderr))
}

/// Runs a setup command and returns the resulting directory path, if any.
///
/// Spawns the command via the provided shell.
/// Captures stdout and stderr. On success, returns the last non-empty line of
/// stdout (trimmed), canonicalized and verified as an existing directory.
///
/// A command that exits 0 but prints no usable stdout line is treated as a
/// successful side-effect-only setup and returns `Ok(None)`; the caller keeps
/// the default CWD.
///
/// # Errors
///
/// Returns [`LifecycleCommandError::CommandFailed`] if the process exits non-zero.
/// Returns [`LifecycleCommandError::InvalidPath`] if the path cannot be resolved.
/// Returns [`LifecycleCommandError::NotADirectory`] if the path is not a directory.
/// Returns [`LifecycleCommandError::ExecutionFailed`] if the process cannot be spawned.
pub async fn run_setup_command(
    command: &str,
    shell: &str,
) -> Result<Option<PathBuf>, Report<LifecycleCommandError>> {
    let (_output, stdout, _stderr) = run_command(command, shell).await?;

    let last_line = stdout.lines().map(str::trim).rfind(|line| !line.is_empty());

    let canonical = match last_line {
        Some(last_line) => {
            let raw_path = PathBuf::from(last_line);

            let canonical =
                strip_verbatim(tokio::fs::canonicalize(&raw_path).await).map_err(|e| {
                    Report::new(e)
                        .change_context(LifecycleCommandError::InvalidPath {
                            path: raw_path.clone(),
                        })
                        .attach("setup command output is not a valid path")
                })?;

            if !canonical.is_dir() {
                return Err(Report::new(LifecycleCommandError::NotADirectory {
                    path: canonical,
                }));
            }

            canonical
        }
        None => return Ok(None),
    };

    Ok(Some(canonical))
}

/// Runs a teardown command. Only checks the exit code - output is ignored.
///
/// Spawns the command via the provided shell.
///
/// # Errors
///
/// Returns [`LifecycleCommandError::CommandFailed`] if the process exits non-zero.
/// Returns [`LifecycleCommandError::ExecutionFailed`] if the process cannot be spawned.
pub async fn run_teardown_command(
    command: &str,
    shell: &str,
) -> Result<(), Report<LifecycleCommandError>> {
    run_command(command, shell).await?;
    Ok(())
}

/// Handle used to cancel a running lifecycle shell process from outside its
/// reader task.
///
/// Carries:
/// - `pid`: the process-group leader's id. On Unix the child is spawned in
///   its own session (setsid), so its pid _is_ the process-group id. The cancel
///   path signals the whole group via `kill_process_group_by_pid(pid)`, which
///   reaches the leader _and_ any backgrounded descendants (grandchildren)
///   in a single syscall — without touching the reader task's owned `Child`.
/// - `abort_handle`: the abort handle of the **inner reader task**. Aborting
///   it makes the caller's wrapper task (in `session_lifecycle_actor/handlers/setup.rs`)
///   observe a `JoinError` and take its `Err(_)` arm, which publishes the
///   "cancelled" finish event. The finish handler then owns all cleanup
///   (busy, cwd, chat entry, phase).
///
/// This is lock-free and await-free: the cancel handler can run on a tokio
/// worker thread without panicking.
#[derive(Debug)]
pub struct LifecycleCancelHandle {
    /// Process-group id of the running lifecycle command (also its pid).
    pub pid: u32,
    /// Abort handle for the inner reader task.
    pub abort_handle: tokio::task::AbortHandle,
}

/// Result type returned by [`spawn_setup_command`].
pub type SpawnSetupResult = Result<
    (
        LifecycleCancelHandle,
        tokio::task::JoinHandle<Result<Option<PathBuf>, Report<LifecycleCommandError>>>,
    ),
    Report<LifecycleCommandError>,
>;

/// Result type returned by [`spawn_teardown_command`].
pub type SpawnTeardownResult = Result<
    (
        LifecycleCancelHandle,
        tokio::task::JoinHandle<Result<(), Report<LifecycleCommandError>>>,
    ),
    Report<LifecycleCommandError>,
>;

/// Spawns a setup command and returns a cancel handle and a joinable reader task.
///
/// The shell process is spawned with `cwd` as its working directory so the
/// script's relative paths resolve against the session CWD, not jinn's process
/// working directory.
///
/// The [`LifecycleCancelHandle`] can be used to cancel the command from outside
/// this task (see its docs). The reader task awaits exit, reads stdout/stderr,
/// and produces the canonicalized CWD path.
///
/// # Panics
///
/// Panics if the spawned child has no PID immediately after spawn (should never happen for a process group child).
///
/// # Errors
/// Returns an error under any of these circumstances:
/// - spawning command fails
/// - joining fails
/// - the returned path cannot be canonicalized
#[expect(clippy::expect_used, reason = "infallible")]
pub fn spawn_setup_command(command: &str, shell: &str, cwd: &std::path::Path) -> SpawnSetupResult {
    use error_stack::ResultExt as _;

    let mut child = {
        let mut std_cmd = std::process::Command::new(shell);
        std_cmd
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Terminal isolation: setsid (pid == pgid, no controlling tty) on
        // Unix / CREATE_NO_WINDOW on Windows. Replaces the former
        // `process_group(0)` — combining both would fail setsid with EPERM.
        // The kill path (`kill_process_group_by_pid`) still works: the
        // child's pid remains its process-group id.
        jinn_common::process_isolation::isolate(&mut std_cmd);
        let mut cmd = tokio::process::Command::from(std_cmd);

        cmd.spawn()
            .change_context(LifecycleCommandError::ExecutionFailed)
            .attach("failed to spawn lifecycle command")?
    };

    // Capture the process-group id BEFORE moving the child into the reader task.
    // On Unix the child was spawned in its own session via setsid, so its
    // pid is also its process-group id. The cancel path signals the whole
    // `kill_process_group_by_pid(pid)`, which needs no Child handle.
    let pid = child.id().expect("child has a pid immediately after spawn");

    // Take pipes before moving the child.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    // Clone the cwd for the reader task so it can resolve relative paths
    // emitted by the script. The script's process CWD is gone by the time
    // we canonicalize, so without this a relative `./foo` resolves against
    // jinn's process dir instead of the session dir.
    let cwd = cwd.to_path_buf();
    let handle = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        // Read stdout.
        let stdout_bytes = if let Some(mut pipe) = stdout_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        };

        // Read stderr.
        let stderr_bytes = if let Some(mut pipe) = stderr_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        };

        // Wait for exit. The reader owns the Child by value (no lock). When the
        // cancel path SIGKILLs the process group, wait() resolves with a
        // signal-killed status naturally.
        let status = child
            .wait()
            .await
            .change_context(LifecycleCommandError::ExecutionFailed)
            .attach("failed to wait for lifecycle command")?;

        let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
        let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

        if !status.success() {
            return Err(Report::new(LifecycleCommandError::CommandFailed {
                exit_code: status.code(),
                stdout,
                stderr,
            }));
        }

        // A successful command may print no CWD (side-effect-only setups,
        // e.g. `sleep 3` to warm a cache). Treat that as success-without-cwd;
        // the caller advances lifecycle and keeps the default CWD.
        let last_line = stdout.lines().map(str::trim).rfind(|line| !line.is_empty());

        let canonical = match last_line {
            Some(last_line) => {
                let raw_path = PathBuf::from(last_line);

                // The script ran with `cwd` as its working directory, but by
                // the time we canonicalize that process is gone. Resolve
                // relative paths against `cwd` so `./foo` doesn't silently
                // resolve against jinn's process dir. Absolute paths pass through.
                let resolved_path = if raw_path.is_absolute() {
                    raw_path.clone()
                } else {
                    cwd.join(&raw_path)
                };

                let canonical = strip_verbatim(tokio::fs::canonicalize(&resolved_path).await)
                    .map_err(|e| {
                        Report::new(e)
                            .change_context(LifecycleCommandError::InvalidPath {
                                path: resolved_path.clone(),
                            })
                            .attach("setup command output is not a valid path")
                    })?;

                if !canonical.is_dir() {
                    return Err(Report::new(LifecycleCommandError::NotADirectory {
                        path: canonical,
                    }));
                }

                canonical
            }
            None => return Ok(None),
        };

        Ok(Some(canonical))
    });

    let abort_handle = handle.abort_handle();
    Ok((LifecycleCancelHandle { pid, abort_handle }, handle))
}

/// Spawns a teardown command and returns a shared child handle and a joinable task.
///
/// The shell process is spawned with `cwd` as its working directory so the
/// script's relative paths resolve against the session CWD, not jinn's process
/// working directory.
///
/// Same pattern as [`spawn_setup_command`] but only checks the exit code.
///
/// # Panics
///
/// Panics if the spawned child has no PID immediately after spawn (should never happen for a process group child).
///
/// # Errors
///
/// Returns an error if the shell command fails to spawn or canonicalize the working directory.
#[expect(clippy::expect_used, reason = "infallible")]
pub fn spawn_teardown_command(
    command: &str,
    shell: &str,
    cwd: &std::path::Path,
) -> SpawnTeardownResult {
    use error_stack::ResultExt as _;

    let mut child = {
        let mut std_cmd = std::process::Command::new(shell);
        std_cmd
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Same terminal-isolation rationale as `spawn_setup_command` —
        // see that function for the Unix/Windows split and the setsid
        // invariant the PID-based cancel path relies on.
        jinn_common::process_isolation::isolate(&mut std_cmd);
        let mut cmd = tokio::process::Command::from(std_cmd);

        cmd.spawn()
            .change_context(LifecycleCommandError::ExecutionFailed)
            .attach("failed to spawn lifecycle command")?
    };

    // Capture the process-group id BEFORE moving the child into the reader task
    // (see spawn_setup_command for the rationale).
    let pid = child.id().expect("child has a pid immediately after spawn");

    // Take pipes before moving the child.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let handle = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        // Read stdout.
        let stdout_bytes = if let Some(mut pipe) = stdout_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        };

        // Read stderr.
        let stderr_bytes = if let Some(mut pipe) = stderr_pipe {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        } else {
            Vec::new()
        };

        // Wait for exit (reader owns the Child by value; no lock). When the
        // cancel path SIGKILLs the process group, wait() resolves naturally.
        let status = child
            .wait()
            .await
            .change_context(LifecycleCommandError::ExecutionFailed)
            .attach("failed to wait for lifecycle command")?;

        if !status.success() {
            let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
            return Err(Report::new(LifecycleCommandError::CommandFailed {
                exit_code: status.code(),
                stdout,
                stderr,
            }));
        }

        Ok(())
    });

    let abort_handle = handle.abort_handle();
    Ok((LifecycleCancelHandle { pid, abort_handle }, handle))
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
    #![allow(
        clippy::print_stderr,
        reason = "the skip path must be visible, not silent"
    )]
    use super::*;
    use jinn_common::fs_paths::canonicalize_display;

    /// Resolves the shell these tests run their scripts through — the same
    /// shared resolver production uses (`jinn_common::shell`).
    ///
    /// The bare `bash` fallback is probed once (it only resolves if a POSIX
    /// bash is actually on `PATH`); an absolute resolved path was already
    /// existence-checked by the resolver. When nothing usable resolves, the
    /// caller prints a skip reason and returns — a visible skip, never a
    /// silent pass.
    fn shell_or_skip() -> Option<String> {
        let shell = jinn_common::shell::resolve_shell();
        if shell != "bash" {
            return Some(shell);
        }
        let probe = std::process::Command::new(&shell)
            .arg("-c")
            .arg("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match probe {
            Ok(_) => Some(shell),
            Err(err) => {
                eprintln!(
                    "SKIP: no usable POSIX shell resolved for command_runner tests \
                     (bare 'bash' probe failed: {err})"
                );
                None
            }
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_returns_canonicalized_directory_path() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a command that echoes a directory path.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().to_string_lossy().to_string();
        let expected = canonicalize_display(dir.path()).expect("canonicalize");

        // When running the setup command.
        let result = run_setup_command(&format!("echo '{path}'"), &shell).await;

        // Then the result is the canonicalized directory path.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_canonicalizes_last_non_empty_line() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a command that outputs multiple lines.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().to_string_lossy().to_string();
        let expected = canonicalize_display(dir.path()).expect("canonicalize");

        // When running the setup command with leading output.
        let result =
            run_setup_command(&format!("echo 'setting up...'; echo '{path}'"), &shell).await;

        // Then the result is the canonicalized last non-empty line.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_trims_whitespace_before_canonicalizing() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a command that echoes with trailing whitespace.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().to_string_lossy().to_string();
        let expected = canonicalize_display(dir.path()).expect("canonicalize");

        // When running the setup command.
        let result = run_setup_command(&format!("echo '{path}  '"), &shell).await;

        // Then the result is trimmed and canonicalized.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_returns_error_on_nonzero_exit() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a command that exits with code 1.
        // When running it as the setup command.
        let result = run_setup_command("exit 1", &shell).await;

        // Then the result is a CommandFailed error.
        assert!(result.is_err());
        let report = result.unwrap_err();
        let err = report
            .downcast_ref::<LifecycleCommandError>()
            .expect("downcast");
        match err {
            LifecycleCommandError::CommandFailed { exit_code, .. } => {
                assert_eq!(*exit_code, Some(1));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_captures_stdout_and_stderr_on_failure() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a command that writes to both stdout and stderr before failing.
        // When running it as the setup command.
        let result = run_setup_command(
            "echo 'stdout message'; echo 'stderr message' >&2; exit 1",
            &shell,
        )
        .await;

        // Then the error contains both stdout and stderr output.
        assert!(result.is_err());
        let report = result.unwrap_err();
        let err = report
            .downcast_ref::<LifecycleCommandError>()
            .expect("downcast");
        match err {
            LifecycleCommandError::CommandFailed { stdout, stderr, .. } => {
                assert!(stdout.contains("stdout message"));
                assert!(stderr.contains("stderr message"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_returns_none_on_empty_stdout() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that succeeds with no output (side-effect-only).
        // When running it.
        let result = run_setup_command("true", &shell).await;

        // Then the result is Ok(None): success, but no CWD path to apply.
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_canonicalizes_relative_path_to_absolute() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a real subdirectory of the current working directory.
        let dir = tempfile::tempdir_in(".").expect("temp dir in cwd");
        let dir_name = dir
            .path()
            .file_name()
            .expect("dir name")
            .to_string_lossy()
            .to_string();
        let expected = canonicalize_display(dir.path()).expect("canonicalize");

        // When running the setup command with a relative path.
        let result = run_setup_command(&format!("echo './{dir_name}'"), &shell).await;

        // Then the result is the canonicalized absolute path.
        assert!(
            result.is_ok(),
            "expected Ok, got Err: {:?}",
            result.unwrap_err()
        );
        assert_eq!(result.unwrap(), Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_returns_error_when_path_does_not_exist() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that outputs a non-existent path.
        // When running it.
        let result = run_setup_command("echo /nonexistent/path/xyzzy", &shell).await;

        // Then the result is an InvalidPath error.
        assert!(result.is_err());
        let report = result.unwrap_err();
        let err = report
            .downcast_ref::<LifecycleCommandError>()
            .expect("downcast");
        match err {
            LifecycleCommandError::InvalidPath { path } => {
                assert_eq!(path, &PathBuf::from("/nonexistent/path/xyzzy"));
            }
            other => panic!("expected InvalidPath, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_returns_error_when_path_is_not_a_directory() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that outputs a file path (not a directory).
        let dir = tempfile::tempdir().expect("temp dir");
        let file_path = dir.path().join("some-file.txt");
        std::fs::write(&file_path, b"contents").expect("write file");

        // When running the setup command.
        let result = run_setup_command(&format!("echo '{}'", file_path.display()), &shell).await;

        // Then the result is a NotADirectory error.
        assert!(result.is_err());
        let report = result.unwrap_err();
        let err = report
            .downcast_ref::<LifecycleCommandError>()
            .expect("downcast");
        match err {
            LifecycleCommandError::NotADirectory { path } => {
                assert_eq!(
                    path,
                    &canonicalize_display(&file_path).expect("canonicalize")
                );
            }
            other => panic!("expected NotADirectory, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_canonicalizes_dot_to_actual_cwd() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that outputs ".".
        let expected = canonicalize_display(std::path::Path::new(".")).expect("canonicalize cwd");

        // When running the setup command.
        let result = run_setup_command("echo .", &shell).await;

        // Then the result is the canonicalized current working directory.
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_succeeds_with_empty_stdout() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a teardown command that removes a marker file (no stdout).
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("teardown-marker");
        std::fs::write(&marker, b"present").expect("write marker");
        assert!(marker.exists(), "marker should exist before teardown");

        // When running the teardown command.
        let result = run_teardown_command(&format!("rm -f '{}'", marker.display()), &shell).await;

        // Then the command succeeds (teardown doesn't require stdout).
        assert!(result.is_ok(), "teardown should succeed without stdout");
        // And the marker file is gone.
        assert!(!marker.exists(), "teardown should have removed the marker");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_returns_failure_on_nonzero_exit() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a teardown command that fails.
        // When running it.
        let result = run_teardown_command("exit 42", &shell).await;

        // Then we get a CommandFailed error.
        assert!(result.is_err());
        let report = result.unwrap_err();
        let err = report.downcast_ref::<LifecycleCommandError>();
        assert!(
            matches!(err, Some(LifecycleCommandError::CommandFailed { exit_code, .. }) if *exit_code == Some(42)),
            "expected CommandFailed with exit code 42"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_commands_run_sequentially() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given two marker files.
        let dir = tempfile::tempdir().expect("temp dir");
        let marker_a = dir.path().join("a");
        let marker_b = dir.path().join("b");
        std::fs::write(&marker_a, b"a").expect("write a");
        std::fs::write(&marker_b, b"b").expect("write b");

        // When running two teardown commands sequentially (as shutdown does).
        let cmd_a = format!("rm -f '{}'", marker_a.display());
        let cmd_b = format!("rm -f '{}'", marker_b.display());
        let _ = run_teardown_command(&cmd_a, &shell).await;
        let _ = run_teardown_command(&cmd_b, &shell).await;

        // Then both files are removed.
        assert!(!marker_a.exists(), "first teardown should remove marker a");
        assert!(!marker_b.exists(), "second teardown should remove marker b");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_succeeds_with_any_stdout() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a teardown command that produces stdout but exits 0.
        // When running it.
        let result = run_teardown_command("echo 'cleaning up...'", &shell).await;

        // Then the command succeeds (stdout is ignored).
        assert!(result.is_ok());
    }

    //
    // These observe the *spawned process's actual working directory* —
    // the gap that let the bug ship. They use spawn_setup_command /
    // spawn_teardown_command directly (the functions with the cwd param),
    // not the run_* convenience wrappers.

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_script_runs_in_session_cwd() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a tempdir used as the session CWD.
        // We observe the spawned shell's *own* working dir (printed by the
        // script itself), so the assertion is independent of where the jinn
        // test process or its reader task runs.
        let dir = tempfile::tempdir().expect("temp dir");
        let expected = canonicalize_display(dir.path()).expect("canonicalize dir");

        // When spawning a setup that echoes its own working directory.
        // git-bash reports `pwd` in MSYS mount form (/c/...), which is not
        // derivable from the session's Windows path; `pwd -W` prints the
        // forward-slash Windows form, which canonicalizes back to it.
        #[cfg(windows)]
        let script = "pwd -W";
        #[cfg(not(windows))]
        let script = "echo $PWD";
        let (_handle, join_handle) =
            spawn_setup_command(script, &shell, dir.path()).expect("spawn");
        let result = join_handle.await.expect("join").expect("setup ok");

        // Then the resolved canonical path equals the session CWD, proving the
        // spawned shell ran *in* it. If the bug regressed (no .current_dir),
        // the printed dir would be jinn's process dir and this would fail.
        assert_eq!(result, Some(expected));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_side_effect_script_writes_file_in_session_cwd() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a tempdir as the session CWD.
        let dir = tempfile::tempdir().expect("temp dir");
        let out_file = dir.path().join("out.txt");
        assert!(!out_file.exists(), "out.txt should not exist before setup");

        // When spawning a setup that writes a relative file and prints nothing
        // (side-effect-only, no stdout CWD).
        let (_handle, join_handle) =
            spawn_setup_command("touch out.txt", &shell, dir.path()).expect("spawn");
        let result = join_handle.await.expect("join").expect("setup ok");

        // Then out.txt exists *in the session CWD* (not jinn's process dir), and
        // the command returns Ok(None) since it printed no path.
        assert!(
            out_file.exists(),
            "out.txt should be created in session CWD"
        );
        assert_eq!(result, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn setup_canonicalizes_relative_output_against_session_cwd() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a tempdir (NOT in the process cwd) with a child subdir.
        // Using tempdir() rather than tempdir_in(".") guarantees the session
        // cwd differs from jinn's process cwd, so a relative echo can only
        // resolve if canonicalize uses the session cwd as the base.
        let dir = tempfile::tempdir().expect("temp dir");
        let subdir = dir.path().join("workspace");
        std::fs::create_dir_all(&subdir).expect("create subdir");
        let expected = canonicalize_display(&subdir).expect("canonicalize subdir");

        // When the setup script echoes a RELATIVE path and the spawn cwd is
        // the session cwd.
        let (_handle, join_handle) =
            spawn_setup_command("echo ./workspace", &shell, dir.path()).expect("spawn");
        let result = join_handle.await.expect("join").expect("setup ok");

        // Then the resolved canonical path is the session cwd's child, not a
        // path resolved against jinn's process cwd.
        assert_eq!(
            result,
            Some(expected),
            "relative output must canonicalize against the session cwd"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_script_runs_in_session_cwd() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a tempdir as the session CWD.
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("cleanup.log");
        assert!(
            !marker.exists(),
            "cleanup.log should not exist before teardown"
        );

        // When spawning a teardown that writes a relative file.
        let (_handle, join_handle) =
            spawn_teardown_command("printf x > cleanup.log", &shell, dir.path()).expect("spawn");
        let result = join_handle.await.expect("join");

        // Then cleanup.log exists *in the session CWD*, and the teardown succeeded.
        assert!(
            marker.exists(),
            "cleanup.log should be created in session CWD"
        );
        assert!(result.is_ok(), "teardown should succeed");
    }
    #[rstest::rstest]
    #[tokio::test]
    async fn aborting_reader_task_yields_cancelled_or_failed_outcome() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that sleeps indefinitely.
        let (handle, join_handle) =
            spawn_setup_command("sleep 30", &shell, std::path::Path::new(".")).expect("spawn");

        // When killing the process group then aborting the inner reader task.
        jinn_kernel::common::process_kill::kill_process_group_by_pid(handle.pid);
        handle.abort_handle.abort();

        // Then the join resolves to a failure outcome — either the inner task
        // observed the kill and returned CommandFailed (reaped before abort),
        // or the abort won and produced a JoinError. Either way: not success.
        let outcome = join_handle.await;
        let failed = !matches!(outcome, Ok(Ok(_)));
        assert!(failed, "cancel must produce a non-success outcome");
    }

    #[rstest::rstest]
    #[test]
    fn cancel_handle_kills_and_aborts_without_panic() {
        let Some(shell) = shell_or_skip() else {
            return;
        };
        // Given a setup command that sleeps, spawned on a multi-thread runtime
        // so the cancel runs on a WORKER thread (the original bug: blocking_lock
        // on a tokio worker thread panicked with SIGABRT).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread runtime");

        // When running the cancel sequence from inside a spawned task (i.e., on a
        // tokio worker thread). The spawn itself happens in runtime context so the
        // internally-spawned reader task can register with the reactor. If the cancel
        // panicked like the old code, the join below would surface it as a panic payload.
        let handle_cl = rt.handle().clone();
        rt.block_on(async move {
            let (handle, _join_handle) =
                spawn_setup_command("sleep 30", &shell, std::path::Path::new(".")).expect("spawn");

            // Cancel from a runtime worker thread, exactly as the session actor
            // does. The whole point of this test is that this does NOT panic.
            let cancel = handle_cl.spawn(async move {
                jinn_kernel::common::process_kill::kill_process_group_by_pid(handle.pid);
                handle.abort_handle.abort();
            });
            // Then the cancel task joins cleanly — no panic payload, so the
            // old `blocking_lock` SIGABRT on a worker thread cannot recur.
            cancel.await.expect("cancel task must not panic");
        });
        rt.shutdown_background();
    }
}
