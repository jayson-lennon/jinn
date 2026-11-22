//! Cross-platform process-tree termination.
//!
//! Provides two entry points:
//!
//! - [`kill_process_tree`] — terminates a spawned child _and its entire
//!   descendant tree_. Used by the bash tool's `KillOnDrop` guard. Takes the
//!   `&mut Child` directly.
//! - [`kill_process_group_by_pid`] — terminates the process group rooted at a
//!   bare PID. Used by the session-lifecycle cancel path, which does **not**
//!   own the `Child` (it lives inside the reader task) and must not block or
//!   lock. Safe to call from a tokio runtime worker thread.
//!
//! Each platform uses its most appropriate mechanism:
//!
//! - **Unix** — `libc::kill(-pgid, SIGKILL)`. Race-free and atomic: the kernel
//!   delivers the signal to every member of the child's process group in one
//!   syscall. Descendants spawned before the call are guaranteed to die.
//! - **Windows** — [`kill_tree`] enumeration via `CreateToolhelp32Snapshot`.
//!   See the Windows branch below for the documented TOCTOU trade-off.

/// Terminates `child` and its entire process tree.
///
/// Infallible by design: this is invoked from `Drop` impls and synchronous
/// cancel handlers where propagating an error is neither possible nor useful.
/// Failures (e.g. the process already exited) are logged at `warn` level and
/// otherwise swallowed. The helper performs **no async operations** and must
/// not depend on a tokio runtime context.
pub fn kill_process_tree(child: &mut tokio::process::Child) {
    // Read the PID once; if the child already exited there is nothing to do.
    let Some(pid) = child.id() else {
        return;
    };

    #[cfg(unix)]
    {
        kill_process_tree_unix(pid, child);
    }

    #[cfg(windows)]
    {
        kill_process_tree_windows(pid);
    }
}

/// Unix implementation: atomic process-group kill.
///
/// The child was spawned with `process_group(0)`, placing it in its own
/// process group whose pgid equals the child's pid. Signalling the negative
/// pgid instructs the kernel to deliver SIGKILL to the entire group,
/// atomically killing all descendants.
///
#[cfg(unix)]
fn kill_process_group_unix(pid: u32) {
    let group_id = pid as libc::pid_t;
    if group_id > 0 {
        // SAFETY: `libc::kill` is safe when the PID argument is valid. The group id
        // is a real OS PID captured at spawn. The negative argument targets the
        // process group rather than a single process.
        let _ = unsafe { libc::kill(-group_id, libc::SIGKILL) };
    }
}

/// Unix implementation of [`kill_process_tree`]: terminates the direct child
/// via its handle first (belt-and-suspenders), then signals the whole group.
#[cfg(unix)]
fn kill_process_tree_unix(pid: u32, child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    kill_process_group_unix(pid);
}

/// Windows implementation: enumerative tree kill via [`kill_tree`].
///
/// # TOCTOU Race — Accepted Trade-Off
///
/// `kill_tree` builds a point-in-time snapshot of the process tree via
/// `CreateToolhelp32Snapshot`, walks parent/child relationships, and calls
/// `TerminateProcess` on each descendant. Unlike the Unix branch (a single
/// atomic group signal), this is an enumerative walk: a grandchild that
/// spawns _between_ the snapshot and the per-process `TerminateProcess`
/// loop will NOT be signalled and can survive as an orphan.
///
/// This race is accepted for this use case (an AI coding agent's bash tool
/// and session lifecycle commands) because the window is small and the
/// consequence — one orphaned background process — is mild. The atomic,
/// race-free alternative (Windows Job Objects with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) was considered and rejected as
/// Option C: it would require `windows-sys` FFI plus a
/// `CREATE_SUSPENDED` + `AssignProcessToJobObject` + `ResumeThread` dance
/// that `tokio::Command::spawn()` does not expose natively.
#[cfg(windows)]
fn kill_process_group_windows(pid: u32) {
    let config = kill_tree::Config {
        signal: "SIGKILL".to_owned(),
        ..Default::default()
    };
    if let Err(e) = kill_tree::blocking::kill_tree_with_config(pid, &config) {
        tracing::warn!(err = %e, pid, "process_kill: windows kill_tree failed");
    }
}

#[cfg(windows)]
fn kill_process_tree_windows(pid: u32) {
    kill_process_group_windows(pid);
}

/// Terminates the process group rooted at `pid` without holding a
/// `tokio::process::Child`.
///
/// This is the cancel-path entry point for the session-lifecycle reader
/// task: the `Child` lives inside the reader task, so the cancel handler
/// cannot take it and must signal by PID instead. The child was spawned
/// with `process_group(0)`, so its pid _is_ the process-group id (Unix) or
/// the tree root (Windows).
///
/// Infallible by design: this is invoked from a synchronous cancel handler
/// on a runtime worker thread where propagating an error is neither possible
/// nor useful. Failures (the process already exited → `ESRCH` on Unix, a
/// missing tree on Windows) are logged at `warn` level and otherwise
/// swallowed. Performs **no async operations** and touches no `Child`, so it
/// is safe to call from a tokio runtime worker thread.
pub fn kill_process_group_by_pid(pid: u32) {
    #[cfg(unix)]
    {
        kill_process_group_unix(pid);
    }
    #[cfg(windows)]
    {
        kill_process_group_windows(pid);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        reason = "test code"
    )]
    #[cfg(unix)]
    use super::*;

    /// Calling [`kill_process_group_by_pid`] on an already-exited process
    /// group must not panic: on Unix the `kill(-pgid)` returns `ESRCH` (no such
    /// process), which we swallow with `let _ =`. This is the PID-based
    /// counterpart of [`kill_process_tree_handles_missing_pid`] and is the
    /// documented safety property the lifecycle cancel path relies on:
    /// the cancel handler may run after the process has already exited.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_process_group_by_pid_is_infallible_on_dead_pid() {
        // Given a process group whose leader has already exited.
        let child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn should succeed");
        let pid = child.id().expect("child has a pid before being awaited");
        // Reap the leader so the group no longer exists.
        let mut child = child;
        let _ = child.wait().await;

        // When killing the (already-dead) process group by PID.
        // Then it returns without panicking (ESRCH swallowed).
        kill_process_group_by_pid(pid);
    }

    /// Ported regression: a grandchild backgrounded by the group leader must
    /// be signalled by `kill_process_group_by_pid`, because the Unix path
    /// uses `kill(-pgid)` (group signal), not a single-process `kill(pid)`,
    /// and the child was spawned with `process_group(0)` so its pid _is_ the
    /// pgid. A naive single-process kill would orphan the grandchild.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_process_group_by_pid_terminates_grandchildren() {
        // Given a shell child that backgrounds a long-running `sleep` (the
        // grandchild) and itself exits, so only the grandchild would survive
        // a naive single-process kill.
        let child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 65 &")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn should succeed");
        let pid = child.id().expect("child has a pid before it exits");
        // The pgid the group signal will address: process_group(0) made the
        // child its own leader, so the pid is the pgid. Captured before the
        // kill, since the group ceases to exist once the leader is signalled.
        let pgid = pid as i32;
        // The shell exits once it has launched the grandchild. The wait is
        // on the grandchild's *existence* within the spawn's own group — the
        // group signal can only reach it once it is running, and a fixed
        // sleep would only guess at when that happens. The group scope keeps
        // an unrelated host process running the same command line out of the
        // oracle.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(5), || {
            !jinn_testutil::platform::find_orphans_in_group("sleep 65", "sleep", pgid).is_empty()
        })
        .await;

        // When killing the process group by PID (the cancel-path entry point).
        // Mirror the real call context — a synchronous handler on a runtime
        // worker thread — via spawn_blocking.
        tokio::task::spawn_blocking(move || kill_process_group_by_pid(pid))
            .await
            .expect("kill task should complete");

        // Then no 'sleep 65' grandchild survived the group signal. The wait
        // is on the grandchild's *absence* within that group after the
        // signal — the kernel reaps it asynchronously, and a fixed sleep
        // would only guess at how long that takes.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(5), || {
            jinn_testutil::platform::find_orphans_in_group("sleep 65", "sleep", pgid).is_empty()
        })
        .await;
    }

    /// Calling the helper on an already-exited child must not panic. On Unix the
    /// process-group `kill(-pgid)` returns `ESRCH` (no such process) which is
    /// swallowed by `let _ =`; on Windows `kill_tree` surfaces a
    /// `MaybeAlready_terminated`-style error that is logged and swallowed.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_process_tree_handles_missing_pid() {
        // Given a child that has already exited (`true` returns immediately).
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn should succeed");
        // Await full exit and reap.
        let _ = child.wait().await;

        // When killing the (already-dead) process tree.
        // Then it returns without panicking.
        kill_process_tree(&mut child);
    }
}
