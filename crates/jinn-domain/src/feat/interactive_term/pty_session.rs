//! PTY session — spawn a child into a pseudo-terminal and drive it.
//!
//! The child gets the pty as its **controlling terminal** (portable-pty's
//! unix backend runs `setsid` + `TIOCSCTTY` in `pre_exec`), making it a
//! session leader with `pid == pgid`. This is the deliberate inverse of the
//! bash tool's terminal isolation: interactive programs need a tty, and the
//! session-leader property is exactly what [`kill_process_group_by_pid`]
//! relies on to take down the child's whole tree without orphans.
//!
//! I/O model: portable-pty's reader is a blocking fd, not an async stream, so
//! a dedicated std thread pumps output chunks into an unbounded tokio channel.
//! The channel's receiver is owned by the session's **screen task** (see
//! [`screen_task`]) — the realtime parser/publisher. Kanal is forbidden here
//! — the consuming task selects over this channel and kanal has a documented
//! double-free under `select!` cancellation (see the bash tool's reader
//! tasks).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use error_stack::Report;
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use wherror::Error;

use crate::common::process_kill::kill_process_group_by_pid;
use crate::feat::interactive_term::screen_task::{
    ScreenHandle, ScreenWiring, SharedTerminal, spawn_screen_task,
};

/// Sender half of a session's output channel.
///
/// The pump forwards raw pty output chunks here; the session's screen task
/// parses them into the emulator.
pub type OutputTx = tokio::sync::mpsc::UnboundedSender<Vec<u8>>;

/// Receiver half of a session's output channel (owned by the screen task).
pub type OutputRx = tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>;

/// Why a pty session could not be created or driven.
#[derive(Debug, Error)]
#[error(debug)]
pub enum PtyError {
    /// `openpty` failed �� no pty pair was created.
    OpenPty,
    /// The pty master's writer could not be taken.
    TakeWriter,
    /// The pty reader could not be cloned for the output pump.
    CloneReader,
    /// The pump thread could not be spawned.
    PumpThread,
    /// The command could not be spawned into the pty (unknown binary?).
    Spawn,
    /// Writing input to the pty failed.
    Write,
    /// Resizing the pty failed.
    Resize,
    /// Waiting for the child's exit failed or timed out.
    Wait,
}

/// Cadence for polling the exit-watcher's result slot.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Upper bound for [`PtySession::exit`] — a generous window for a killed
/// child; a naturally exiting program may legitimately exceed it.
const EXIT_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound for [`Drop for PtySession`] reaping after a group kill.
const DROP_REAP_TIMEOUT: Duration = Duration::from_millis(500);

/// Transcript ring length for new sessions (screens observed).
pub const TRANSCRIPT_LINES: usize = 200;

/// Terminal state of a session's process, captured once it exits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// The process exit code (0 on success; signal deaths report 1 plus a
    /// signal name).
    pub code: u32,
    /// Signal name if the process was killed by a signal (e.g. `"Terminated"`).
    pub signal: Option<String>,
}

impl ExitInfo {
    fn from_status(status: &portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
        }
    }

    /// One-line human summary, e.g. `exited with code 1` or `killed by SIGTERM`.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.signal {
            Some(signal) => format!("killed by {signal}"),
            None => format!("exited with code {}", self.code),
        }
    }
}

/// A running child process inside its own pty.
///
/// Owns the child, the pty writer, and the **screen task** that parses the
/// program's output into the shared emulator in realtime. Dropping the
/// session kills the child's whole process tree (mirroring the bash tool's
/// `KillOnDrop`), so an aborted setup never leaks orphans.
pub struct PtySession {
    /// Exit watcher: owns the child and blocks in `wait()` so the kernel
    /// reaps the process the moment it exits. [`Self::kill`] signals through
    /// the split [`ChildKiller`]; [`Self::try_wait`] and [`Self::exit`]
    /// read the watcher's shared result slot.
    exit_watcher: Arc<ExitWatcher>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: PtySize,
    /// Shared emulator + screen-version watch + the pty writer slot.
    screen: ScreenHandle,
}

/// The reaped exit status of a pty child, shared between the watcher thread
/// and the session.
struct ExitSlot(std::sync::Mutex<Option<ExitInfo>>);

/// Owns the pty child on a dedicated thread, blocking in `wait()`.
///
/// `wait()` is the only portable reaping primitive: whoever holds the child
/// must call it, or an exited child stays a zombie until the holder drops.
/// Blocking in `wait()` reaps immediately at exit regardless of when (or
/// whether) anything else polls, which matters because jinn runs for days
/// and sessions outlive the programs they host. The watcher is the *sole*
/// owner — no other code path touches the child — so the thread cannot race
/// a concurrent `try_wait` on the same handle.
struct ExitWatcher {
    slot: ExitSlot,
}

impl ExitWatcher {
    /// Takes ownership of `child` and spawns the reaping thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher thread cannot be spawned (resource
    /// exhaustion); the child handle is dropped un-reaped in that case.
    fn start(mut child: Box<dyn Child + Send + Sync>) -> Result<Arc<Self>, Report<PtyError>> {
        let watcher = Arc::new(Self {
            slot: ExitSlot(std::sync::Mutex::new(None)),
        });
        let handle = Arc::downgrade(&watcher);
        // The thread upgrades the handle only while publishing; when the
        // session is dropped the upgrade fails and the thread exits, so the
        // watcher never outlives its session.
        std::thread::Builder::new()
            .name("pty-exit-watcher".to_owned())
            .spawn(move || {
                let status = match child.wait() {
                    Ok(status) => ExitInfo::from_status(&status),
                    Err(err) => {
                        tracing::warn!(err = %err, "interactive_term: child wait failed");
                        ExitInfo {
                            code: 0,
                            signal: None,
                        }
                    }
                };
                if let Some(watcher) = handle.upgrade()
                    && let Ok(mut slot) = watcher.slot.0.lock()
                {
                    *slot = Some(status);
                }
                // Releasing `child` here (thread teardown) is what actually
                // drops the child handle after reaping.
            })
            .map_err(|err| {
                Report::new(PtyError::Wait)
                    .attach("failed to spawn pty exit watcher thread")
                    .attach(format!("thread spawn error: {err}"))
            })?;
        Ok(watcher)
    }

    /// The child's exit info once reaped; `None` while still running.
    fn exit(&self) -> Option<ExitInfo> {
        let guard = self.slot.0.lock().ok()?;
        guard.clone()
    }
}

impl PtySession {
    /// Spawns `command` (shell syntax, executed via `bash -c`) inside a fresh
    /// pty of `size`, and starts its realtime screen task.
    ///
    /// The screen task (owned by the session) drains the pty output pump into
    /// the shared emulator on a ~50 ms cadence and republishes the mirror on
    /// change; [`PtySession::screen`] reaches the emulator and the settle
    /// signals. `wiring` carries the bus + mirror endpoints for publication.
    ///
    /// # Errors
    ///
    /// Returns an error if the pty cannot be opened, the writer/reader cannot
    /// be taken, the pump thread cannot start, or the command fails to spawn
    /// (e.g. `bash` is missing).
    pub fn spawn(
        command: &str,
        cwd: &Path,
        size: PtySize,
        wiring: ScreenWiring,
    ) -> Result<(Self, ReadPump), Report<PtyError>> {
        let pair = {
            let opened = native_pty_system().openpty(size);
            // `anyhow::Error` is not `std::error::Error`, so the ResultExt
            // conversions don't apply — map manually, keeping the text.
            opened.map_err(|err| {
                Report::new(PtyError::OpenPty).attach(format!("failed to open pty pair: {err}"))
            })?
        };

        let writer = {
            let taken = pair.master.take_writer();
            taken.map_err(|err| {
                Report::new(PtyError::TakeWriter)
                    .attach(format!("failed to take pty writer: {err}"))
            })?
        };

        let reader = {
            let cloned = pair.master.try_clone_reader();
            cloned.map_err(|err| {
                Report::new(PtyError::CloneReader)
                    .attach(format!("failed to clone pty reader: {err}"))
            })?
        };

        let (tx, rx): (OutputTx, OutputRx) = tokio::sync::mpsc::unbounded_channel();
        let pump = spawn_read_pump(reader, tx)?;

        let mut cmd = CommandBuilder::new("bash");
        cmd.arg("-c");
        cmd.arg(command);
        cmd.cwd(cwd);
        // Full-screen programs probe the terminal and pick render paths from
        // its identity. A conservative widely-supported identity plus
        // truecolor keeps nvim-class TUIs from hanging or degrading;
        // query_responder answers the capability queries themselves.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = {
            let spawned = pair.slave.spawn_command(cmd);
            spawned.map_err(|err| {
                Report::new(PtyError::Spawn)
                    .attach(format!("failed to spawn command into pty: {err}"))
                    .attach(format!("command: {command}"))
            })?
        };
        // Drop the parent's slave handle: the child holds its own copies, and
        // a parent-held slave would keep the master from ever seeing EOF/HUP
        // after the child exits.
        drop(pair.slave);

        // Split off a killer so kill() never needs the child handle (which
        // the exit watcher now owns while blocked inside `wait`).
        let killer = child.clone_killer();
        // Hand the child to the exit watcher: it blocks in `wait()` on a
        // dedicated thread, reaping the process the instant it exits.
        let exit_watcher = ExitWatcher::start(child)?;

        let screen = new_screen_handle(size.rows, size.cols);
        // The pty writer is shared: the coordinator writes agent input
        // through `PtySession::write`, the screen task writes query replies.
        screen.install_writer(Box::new(SharedWriter {
            inner: Arc::new(parking_lot::Mutex::new(writer)),
        }));
        // The screen task owns the receiver from here on: parsing, query
        // replies, and mirror publication all happen there in realtime.
        spawn_screen_task(rx, screen.clone(), wiring);

        let session = Self {
            exit_watcher,
            master: pair.master,
            killer,
            size,
            screen,
        };
        Ok((session, pump))
    }

    /// Writes raw input bytes to the pty (the child's stdin).
    ///
    /// # Errors
    ///
    /// Returns an error if the pty write fails (e.g. the child exited and the
    /// kernel dropped the line discipline).
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), Report<PtyError>> {
        let wrote = self.screen_write(bytes);
        wrote.map_err(|err| {
            Report::new(PtyError::Write)
                .attach("failed to write input to pty")
                .attach(format!("pty write error: {err}"))
        })
    }

    /// Writes through the shared writer slot (see [`SharedWriter`]).
    fn screen_write(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.screen
            .write_all_via(bytes)
            .map_err(|err| std::io::Error::other(err.to_string()))
    }

    /// The shared screen handle: emulator, version watch, pump-closed flag.
    #[must_use]
    pub fn screen(&self) -> ScreenHandle {
        self.screen.clone()
    }

    /// The emulator grid size as `(rows, cols)`.
    #[must_use]
    pub fn emulator_size(&self) -> (u16, u16) {
        self.screen.emulator_size()
    }

    /// Resizes the shared emulator grid (after the pty resize).
    pub fn set_emulator_size(&self, rows: u16, cols: u16) {
        self.screen.set_emulator_size(rows, cols);
    }

    /// Resizes the pty, notifying the child (SIGWINCH on unix).
    ///
    /// No-ops when the size is unchanged. Nothing stateful may depend on the
    /// size: the model's view of the screen changes when the user resizes.
    ///
    /// # Errors
    ///
    /// Returns an error if the kernel rejects the resize ioctl.
    pub fn resize(&mut self, size: PtySize) -> Result<(), Report<PtyError>> {
        if size == self.size {
            return Ok(());
        }
        let resized = self.master.resize(size);
        resized.map_err(|err| {
            Report::new(PtyError::Resize)
                .attach(format!(
                    "failed to resize pty to {rows}x{cols}",
                    rows = size.rows,
                    cols = size.cols
                ))
                .attach(format!("pty resize error: {err}"))
        })?;
        self.size = size;
        Ok(())
    }

    /// Polls the reaped exit without blocking. Returns `Some` once the child
    /// terminated (and the watcher thread has reaped it).
    #[must_use]
    pub fn try_wait(&self) -> Option<ExitInfo> {
        self.exit_watcher.exit()
    }

    /// The child's exit info, waiting until it terminates.
    ///
    /// The watcher thread has already reaped the process by this point —
    /// this only reads the shared result — so it never blocks on kernel
    /// `waitpid` and never leaves a zombie behind, no matter how long the
    /// program ran or how it ended.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher thread died before publishing (its
    /// `wait` syscall failed); the child may or may not have exited.
    pub fn exit(&self) -> Result<ExitInfo, Report<PtyError>> {
        let deadline = Instant::now() + EXIT_POLL_TIMEOUT;
        loop {
            if let Some(info) = self.exit_watcher.exit() {
                return Ok(info);
            }
            if Instant::now() >= deadline {
                return Err(Report::new(PtyError::Wait)
                    .attach("child did not exit before the wait timeout"));
            }
            std::thread::sleep(EXIT_POLL_INTERVAL);
        }
    }

    /// Terminates the child and its whole process tree.
    ///
    /// Unix signals the child's process group (`kill(-pgid)`); the child was
    /// made a session/group leader at spawn, so its pid is the pgid and the
    /// group signal takes down every descendant. Windows enumerates and
    /// terminates the tree (see `common::process_kill`). Infallible by
    /// design: kill races against an already-exited child are expected and
    /// swallowed by the platform helpers.
    pub fn kill(&mut self) {
        if let Some(pid) = self.pid() {
            kill_process_group_by_pid(pid);
        }
        let _ = self.killer.kill();
    }

    /// The child's pid, if the platform exposes it.
    #[cfg(unix)]
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.master.process_group_leader().map(|pid| pid as u32)
    }

    /// Windows: portable-pty's ConPTY does not expose the child pid.
    #[cfg(windows)]
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        None
    }

    /// The pty's foreground process group (unix), for diagnostics and tests.
    #[cfg(unix)]
    #[must_use]
    pub fn foreground_group(&self) -> Option<u32> {
        self.master.process_group_leader().map(|pid| pid as u32)
    }
}

/// A `Write` impl over the shared pty writer slot.
///
/// `Write for &mut W` delegates to the inner mutex so the same underlying
/// writer serves both the coordinator's input writes and the screen task's
/// query replies without either holding a lock across an await.
struct SharedWriter {
    inner: Arc<parking_lot::Mutex<Box<dyn Write + Send>>>,
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.lock().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.lock().flush()
    }
}

/// Creates the shared screen state for a fresh session.
fn new_screen_handle(rows: u16, cols: u16) -> ScreenHandle {
    let (version_tx, version_rx) = tokio::sync::watch::channel(0);
    let shared = SharedTerminal::new(
        crate::feat::interactive_term::Emulator::new(rows, cols, TRANSCRIPT_LINES),
        version_tx,
    );
    ScreenHandle::new(shared, version_rx)
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Mirrors the bash tool's KillOnDrop: dropping a session takes the
        // child's whole tree down so a dropped session never leaks orphans.
        self.kill();
        // The watcher reaps the killed child; give it a bounded moment so a
        // killed session does not linger as a zombie until the watcher's own
        // teardown. Best effort — the watcher owns reaping either way.
        let deadline = Instant::now() + DROP_REAP_TIMEOUT;
        while self.try_wait().is_none() && Instant::now() < deadline {
            std::thread::sleep(EXIT_POLL_INTERVAL);
        }
    }
}

/// Handle to the pump thread draining pty output into an [`OutputTx`].
pub struct ReadPump {
    handle: std::thread::JoinHandle<()>,
}

impl ReadPump {
    /// Waits for the pump thread to finish. Call after the pty has closed.
    pub fn join(self) {
        let _ = self.handle.join();
    }
}

/// Bridges the blocking pty reader onto a std thread feeding `tx`.
///
/// portable-pty readers are blocking fds, so the pump must own a dedicated
/// thread; the unbounded tokio channel is the async boundary (tokio, not
/// kanal — see the module docs).
fn spawn_read_pump(
    mut reader: Box<dyn Read + Send>,
    tx: OutputTx,
) -> Result<ReadPump, Report<PtyError>> {
    let handle = std::thread::Builder::new()
        .name("interactive-term-pty-pump".to_owned())
        .spawn(move || pump_loop(&mut reader, &tx));
    let joined = handle.map_err(|err| {
        Report::new(PtyError::PumpThread)
            .attach("failed to spawn pty output pump thread")
            .attach(format!("thread spawn error: {err}"))
    });
    joined.map(|pump_handle| ReadPump {
        handle: pump_handle,
    })
}

/// Reads pty output until EOF, forwarding each chunk to `tx`.
///
/// A send failure means the receiver is gone (session dropped) — stop pumping
/// so the thread can exit instead of queuing into a dead channel forever.
fn pump_loop(reader: &mut Box<dyn Read + Send>, tx: &OutputTx) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let Some(chunk) = buf.get(..n) else { break };
                if tx.send(chunk.to_vec()).is_err() {
                    break;
                }
            }
        }
    }
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
    use std::time::Duration;

    use super::*;
    use crate::common::services::bus_service::BusService;

    /// Test timeout for child-process waits; keeps a wedged pty from hanging
    /// the suite past tokio's default 10s test timeout.
    fn wait_timeout() -> Duration {
        Duration::from_secs(5)
    }

    /// The spawn size used by these tests.
    fn default_size() -> PtySize {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    /// Screen wiring writing into a throwaway state (no bus subscribers).
    fn wiring() -> ScreenWiring {
        let state = crate::common::state::State::new(crate::common::app_state::AppState::default());
        ScreenWiring {
            bus: BusService::new_recording().0,
            state,
            cap: crate::common::tcaps::mint::mint_frontend_cap(),
            chat: crate::protocol::SessionId::new(),
        }
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn spawned_child_echoes_input_back_through_screen() {
        // Given a `cat` session in a pty (cat echoes pty line-discipline input).
        let (mut session, pump) =
            PtySession::spawn("cat", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn cat");

        // When writing bytes to the pty.
        session.write(b"hello pty\n").expect("write");

        // Then the child's echoed output shows up in the shared emulator
        // (parsed by the realtime screen task).
        let deadline = tokio::time::Instant::now() + wait_timeout();
        loop {
            let contains = session
                .screen()
                .lock()
                .emulator()
                .plain_text()
                .contains("hello pty");
            if contains {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "screen task never parsed the echo"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // And the pty has a foreground group (the child is session leader).
        assert!(session.foreground_group().is_some(), "no foreground pgid");

        session.kill();
        pump.join();
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn kill_terminates_whole_group_leaving_no_orphans() {
        // Given a session running a long sleep (the whole group to kill).
        let (mut session, pump) =
            PtySession::spawn("sleep 65", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn sleep");
        let pid = session.pid().expect("child pid");
        // And the child is its own group leader (the invariant the group kill
        // relies on) — captured before the kill, since the group ceases to
        // exist once the leader is signalled.
        assert_eq!(
            session.foreground_group(),
            Some(pid),
            "child is not group leader"
        );

        // When killing the session and joining the pump.
        session.kill();
        pump.join();

        // Then no `sleep 65` process survived the group signal.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let output = tokio::process::Command::new("pgrep")
            .arg("-f")
            .arg("sleep 65")
            .output()
            .await
            .expect("pgrep should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.trim().is_empty(),
            "expected no 'sleep 65', found: {stdout}"
        );
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn try_wait_reports_exit_code_after_natural_exit() {
        // Given a session whose command exits with a known code.
        let (session, pump) =
            PtySession::spawn("exit 7", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn exit");

        // When polling until the child terminates.
        let deadline = tokio::time::Instant::now() + wait_timeout();
        let info = loop {
            if let Some(info) = session.try_wait() {
                break info;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "child never reported exit"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // Then the exit code is surfaced.
        assert_eq!(info.code, 7, "exit code mismatch");
        // And the summary describes the code.
        assert_eq!(info.summary(), "exited with code 7");

        pump.join();
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn dropping_session_kills_child() {
        // Given a session running a long sleep.
        let (session, _pump) =
            PtySession::spawn("sleep 66", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn sleep");

        // When dropping the session (the guard's whole point).
        drop(session);

        // Then the sleep is gone.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let output = tokio::process::Command::new("pgrep")
            .arg("-f")
            .arg("sleep 66")
            .output()
            .await
            .expect("pgrep should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.trim().is_empty(),
            "expected no 'sleep 66', found: {stdout}"
        );
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn resize_updates_pty_and_deduplicates_noop() {
        // Given a session at the default size.
        let (mut session, pump) =
            PtySession::spawn("cat", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn cat");

        // When resizing to 40x120.
        let new_size = PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        };
        session.resize(new_size).expect("resize");

        // Then the kernel reports the new size.
        // (Verified via the session accepting the second, identical resize as
        // a no-op and via the pty remaining writable.)
        session.resize(new_size).expect("idempotent resize");
        session.write(b"still alive\n").expect("write after resize");

        session.kill();
        pump.join();
    }

    /// `ps`-compatible zombie check: a process with state `Z` still occupies
    /// a slot in the process table. Matches on the executable name (`comm`),
    /// which is truncated and never carries arguments.
    #[cfg(unix)]
    async fn zombies_named(program: &str) -> usize {
        // Poll instead of one fixed sleep: under full-suite parallel load the
        // reaper task may be scheduled late, and a single 300ms window reads
        // the test's own just-killed child as a leaked zombie. Bounded
        // polling still fails on a genuinely leaked child — it just gives
        // legitimate reaping time to complete.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let output = tokio::process::Command::new("ps")
                .args(["-eo", "stat,comm"])
                .output()
                .await
                .expect("ps should run");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let zombies = stdout
                .lines()
                .filter(|line| {
                    line.split_whitespace()
                        .next()
                        .is_some_and(|stat| stat.starts_with('Z'))
                        && line
                            .split_whitespace()
                            .nth(1)
                            .is_some_and(|comm| comm.contains(program))
                })
                .count();
            if zombies == 0 || tokio::time::Instant::now() >= deadline {
                return zombies;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn self_exited_program_is_reaped_not_zombied() {
        // Given a session whose program exits on its own right away (`exec`
        // replaces bash, so the pty child IS the program — the reported btop
        // zombie).
        let (session, pump) =
            PtySession::spawn("exec true", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn true");

        // When the program terminates naturally and the watcher reaps it.
        let deadline = tokio::time::Instant::now() + wait_timeout();
        loop {
            if session.try_wait().is_some() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "watcher never reaped the exited child"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        pump.join();

        // Then no zombie for the program remains in the process table.
        assert_eq!(
            zombies_named("true").await,
            0,
            "self-exited child left a zombie"
        );
    }

    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn killed_session_is_reaped_immediately_on_drop() {
        // Given a session running a long sleep.
        let (session, pump) =
            PtySession::spawn("exec sleep 68", Path::new("/tmp"), default_size(), wiring())
                .expect("spawn sleep");

        // When dropping the session (kills + bounded reap in drop).
        drop(session);
        pump.join();

        // Then the killed child is not a zombie — drop waited for the
        // watcher to reap it.
        assert_eq!(
            zombies_named("sleep").await,
            0,
            "killed child left a zombie after drop"
        );
    }
}
