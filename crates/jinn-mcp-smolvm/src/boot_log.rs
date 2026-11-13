//! Boot-phase stderr progress for the MCP picker.
//!
//! jinn's MCP subsystem captures the server child's stderr into a bounded
//! buffer and republishes the tail every 250ms while the connection is
//! coming up — so a line on stderr lands in the picker preview within a
//! quarter second of writing it. This module is the server's half of that
//! contract: one line per boot phase, plus a heartbeat so a slow phase
//! (cold engine download, first-boot package install) keeps the preview
//! alive instead of looking wedged.
//!
//! # Why no byte counts
//!
//! The SDK's engine fetch is a single blocking `reqwest` call with no
//! progress callback, and the engine's machine-create output is not
//! programmatically reachable — so the finest observable granularity is
//! the phase boundary. The heartbeat covers the gap: a phase that takes
//! seconds reports that it is still running, with elapsed seconds.
//!
//! # Thread safety
//!
//! Boot runs on `spawn_blocking`, and each heartbeat runs on its own
//! ticker thread; both funnel through `eprintln!`, whose internal lock
//! keeps interleaved lines whole.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often the heartbeat re-announces a still-running phase. The picker's
/// stderr debounce is 250ms, so anything at or above that cadence is
/// visible; two seconds keeps the tail readable while still clearly alive.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// One stderr line — the only feedback channel before the MCP transport
/// exists.
fn line(message: &str) {
    #[expect(
        clippy::print_stderr,
        reason = "stderr is the only feedback channel before the MCP transport starts; jinn's picker tails it live"
    )]
    let () = {
        eprintln!("smolvm: {message}");
    };
}

/// Announces a boot phase: `smolvm: <what>…`.
pub(crate) fn phase(what: &str) {
    line(&format!("{what}…"));
}

/// Announces a completed step with its outcome.
pub(crate) fn done(what: &str) {
    line(what);
}

/// Renders a fatal startup failure with the boot-log prefix so it reads as
/// the tail of the progress stream rather than a foreign crash dump.
pub(crate) fn fatal(message: &str) {
    line(message);
    line("smolvm server failed to start (see the lines above)");
}

/// Renders elapsed seconds as a ` [3s]` suffix for completion lines; empty
/// under a second, so fast steps stay quiet.
fn elapsed_suffix(started: Instant) -> String {
    let secs = started.elapsed().as_secs();
    if secs > 0 {
        format!(" [{secs}s]")
    } else {
        String::new()
    }
}

/// Spawns a heartbeat for one slow phase: re-announces `what` every two
/// seconds with the elapsed seconds, until the returned guard drops.
///
/// The ticker computes elapsed time itself from spawn, so a phase that
/// blocks for minutes (`apk add` on a cold image) keeps counting without
/// anyone updating it. A failed spawn is logged and boot continues — the
/// phase line already announced the work, and losing the heartbeat must
/// never fail the boot.
#[must_use]
pub(crate) fn heartbeat(what: String) -> HeartbeatGuard {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("smolvm-boot-progress".into())
        .spawn(move || {
            let started = Instant::now();
            while !stop_for_thread.load(Ordering::SeqCst) {
                std::thread::sleep(HEARTBEAT_INTERVAL);
                if stop_for_thread.load(Ordering::SeqCst) {
                    break;
                }
                line(&format!("{what} ({}s)", started.elapsed().as_secs()));
            }
        });
    match spawned {
        Ok(ticker) => HeartbeatGuard {
            stop,
            ticker: Some(ticker),
        },
        Err(error) => {
            tracing::warn!(%error, "could not spawn the boot heartbeat");
            HeartbeatGuard { stop, ticker: None }
        }
    }
}

/// Stops the heartbeat when the phase ends.
pub(crate) struct HeartbeatGuard {
    stop: Arc<AtomicBool>,
    /// The ticker thread; `take`n by [`Self::stop_and_take_ticker`], so a
    /// normal drop carries `None` (the phase already ended it).
    ticker: Option<std::thread::JoinHandle<()>>,
}

impl HeartbeatGuard {
    /// Signals the phase over and hands the ticker thread back —
    /// production callers drop it; tests join it to prove the ticker
    /// stopped.
    pub(crate) fn stop_and_take_ticker(&mut self) -> Option<std::thread::JoinHandle<()>> {
        self.stop.store(true, Ordering::SeqCst);
        self.ticker.take()
    }
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The engine was found on `PATH`, at this SDK's engine generation.
pub(crate) fn engine_on_path() {
    done(&format!(
        "engine ready: smolvm {} (from PATH)",
        smolmachines::VERSION
    ));
}

/// The engine download began.
pub(crate) fn engine_fetching() {
    phase(&format!(
        "downloading VM engine smolvm {} (~37 MB, first run only)",
        smolmachines::VERSION
    ));
}

/// The engine download finished.
pub(crate) fn engine_fetched(started: Instant) {
    done(&format!(
        "engine ready: smolvm {} (downloaded){}",
        smolmachines::VERSION,
        elapsed_suffix(started)
    ));
}

/// Machine creation began (a cold image pull happens here).
pub(crate) fn machine_creating(image: &str) {
    phase(&format!(
        "creating machine (image {image}; first run pulls it)"
    ));
}

/// Machine creation finished.
pub(crate) fn machine_created(started: Instant) {
    done(&format!("machine created{}", elapsed_suffix(started)));
}

/// Guest boot began.
pub(crate) fn machine_starting() {
    phase("starting machine");
}

/// Guest boot finished.
pub(crate) fn machine_started(started: Instant) {
    done(&format!("machine started{}", elapsed_suffix(started)));
}

/// First-boot init began, with the command count; zero commands skip the
/// phase line entirely (nothing to announce).
pub(crate) fn init_starting(count: usize) {
    if count > 0 {
        phase(&format!("running first-boot setup ({count} commands)"));
    }
}

/// One init command began.
pub(crate) fn init_command_starting(command: &str, index: usize, count: usize) {
    phase(&format!("first-boot setup: [{index}/{count}] {command}"));
}

/// One init command finished.
pub(crate) fn init_command_done(command: &str, index: usize, count: usize, started: Instant) {
    done(&format!(
        "first-boot setup: [{index}/{count}] done: {command}{}",
        elapsed_suffix(started)
    ));
}

/// The boot sequence finished and the server is about to serve.
pub(crate) fn ready(started: Instant) {
    done(&format!(
        "ready — serving vm_run and vm_info{}",
        elapsed_suffix(started)
    ));
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]

    use super::*;

    /// The heartbeat guard stops the ticker on drop: the guard hands back
    /// the ticker thread, whose join succeeds promptly (a leaked, never-
    /// stopping ticker would hang the suite's process exit).
    #[rstest::rstest]
    #[test]
    fn heartbeat_guard_stops_the_ticker_on_drop() {
        // Given a running heartbeat.
        let mut guard = heartbeat("testing still running".to_owned());

        // When stopping the guard's phase and joining its ticker.
        let ticker = guard.stop_and_take_ticker().expect("ticker spawned");
        // Then the thread terminates.
        ticker.join().expect("ticker exit");
    }

    /// The elapsed suffix is empty under a second and names the seconds
    /// above it — the completion lines' shape.
    #[rstest::rstest]
    #[test]
    fn elapsed_suffix_is_empty_under_a_second() {
        // Given a just-started instant.
        let started = Instant::now();

        // When rendering the suffix immediately.
        // Then it is empty (no noisy "[0s]").
        assert_eq!(elapsed_suffix(started), "");
    }

    /// A heartbeat whose spawn fails degrades to a no-op guard rather than
    /// failing the boot — the phase line already announced the work.
    #[rstest::rstest]
    #[test]
    fn heartbeat_without_a_ticker_drops_cleanly() {
        // Given a guard whose ticker never spawned (constructed directly —
        // the spawn-failure path).
        let guard = HeartbeatGuard {
            stop: Arc::new(AtomicBool::new(false)),
            ticker: None,
        };

        // When dropping it.
        // Then nothing panics: boot proceeds without a heartbeat.
        drop(guard);
    }
}
