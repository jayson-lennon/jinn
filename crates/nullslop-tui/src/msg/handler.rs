//! Message channel handler and background event thread.
//!
//! [`MsgHandler`] manages the kanal channel, providing synchronous receive
//! for the main loop and a dedicated OS thread that polls crossterm events
//! and periodic ticks.
//!
//! The event thread runs independently of the tokio runtime so that terminal
//! input is never starved by async work on tokio worker threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use derive_more::Debug;
use kanal::Receiver;

use super::{Msg, MsgSender};

/// Manages the message channel for the TUI event loop.
///
/// Use [`Self::start_event_thread`] to spawn the background event thread, and
/// [`Self::drain`] to discard stale messages after stopping it.
#[derive(Debug)]
pub struct MsgHandler {
    /// Sending half of the message channel.
    #[debug(skip)]
    sender: kanal::Sender<Msg>,
    /// Receiving half of the message channel.
    #[debug(skip)]
    receiver: Receiver<Msg>,
}

impl MsgHandler {
    /// Creates a new message handler with an unbounded kanal channel.
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = kanal::unbounded();
        Self { sender, receiver }
    }

    /// Returns a clone of the channel sender.
    pub fn sender(&self) -> MsgSender {
        MsgSender::new(self.sender.clone())
    }

    /// Blocks until the next message is available.
    ///
    /// # Errors
    ///
    /// Returns [`kanal::ReceiveError`] if the channel sender has been dropped.
    pub fn recv(&self) -> Result<Msg, kanal::ReceiveError> {
        self.receiver.recv()
    }

    /// Non-blocking receive. Returns `None` if no message is available.
    pub fn try_recv(&self) -> Option<Msg> {
        self.receiver.try_recv().ok().flatten()
    }

    /// Discards all pending messages from the channel.
    pub fn drain(&self) {
        while self.try_recv().is_some() {}
    }

    /// Spawns a dedicated OS thread that polls crossterm events and periodic ticks.
    ///
    /// The thread runs until the returned [`EventThreadGuard`] is dropped or
    /// [`EventThreadGuard::stop`] is called. This is independent of the tokio
    /// runtime so terminal input is never starved by async work.
    ///
    /// # Panics
    ///
    /// Panics if the OS thread cannot be spawned (e.g. resource exhaustion).
    #[allow(clippy::expect_used, reason = "thread spawn failure is fatal")]
    pub fn start_event_thread(&self) -> EventThreadGuard {
        let sender = self.sender();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let handle = std::thread::Builder::new()
            .name("tui-event-poll".to_owned())
            .spawn(move || {
                run_event_poll(&sender, stop_clone);
            })
            .expect("failed to spawn tui-event-poll thread");

        EventThreadGuard {
            handle: Some(handle),
            stop,
        }
    }
}

impl Default for MsgHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Guard that stops and joins the background event thread on drop.
pub struct EventThreadGuard {
    /// Join handle for the event poll thread.
    handle: Option<std::thread::JoinHandle<()>>,
    /// Shared flag signalling the thread to stop.
    stop: Arc<AtomicBool>,
}

impl EventThreadGuard {
    /// Signals the event thread to stop and waits for it to finish.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for EventThreadGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Polling interval for crossterm events when no event is immediately available.
const POLL_TIMEOUT: Duration = Duration::from_millis(16);

/// Tick interval for periodic render refresh.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Runs the event poll loop on a dedicated OS thread.
///
/// Uses synchronous `crossterm::event::poll` / `read` instead of the async
/// `EventStream`, so this thread never competes with tokio worker threads.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Arc is moved into the thread closure"
)]
fn run_event_poll(sender: &MsgSender, stop: Arc<AtomicBool>) {
    let mut next_tick = Instant::now() + TICK_INTERVAL;

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();

        // Send tick if interval elapsed.
        if now >= next_tick {
            sender.send(Msg::Tick);
            next_tick = now + TICK_INTERVAL;
        }

        // Poll crossterm with a short timeout so we can check `stop` regularly.
        let poll_deadline = next_tick.min(now + POLL_TIMEOUT);
        let poll_duration = poll_deadline.saturating_duration_since(now);

        match crossterm::event::poll(poll_duration) {
            Ok(true) => {
                // Event available — read and forward.
                match crossterm::event::read() {
                    Ok(evt) => sender.send(Msg::Input(evt)),
                    Err(e) => {
                        tracing::error!(err = ?e, "crossterm event read error");
                    }
                }
            }
            Ok(false) => {
                // Timeout — no event, loop back to check tick/stop.
            }
            Err(e) => {
                tracing::error!(err = ?e, "crossterm event poll error");
                // Brief sleep to avoid busy-looping on persistent errors.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]
    use super::*;

    #[rstest::rstest]
    fn msg_handler_send_recv() {
        // Given a MsgHandler.
        let handler = MsgHandler::new();

        // When sending a Tick.
        handler.sender().send(Msg::Tick);

        // Then recv returns Tick.
        let msg = handler.recv().expect("should receive");
        assert!(matches!(msg, Msg::Tick));
    }

    #[rstest::rstest]
    fn msg_handler_try_recv_empty() {
        // Given an empty handler.
        let handler = MsgHandler::new();

        // When try_recv.
        let result = handler.try_recv();

        // Then None.
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn msg_handler_drain() {
        // Given a handler with 3 messages.
        let handler = MsgHandler::new();
        handler.sender().send(Msg::Tick);
        handler.sender().send(Msg::Tick);
        handler.sender().send(Msg::Tick);

        // When draining.
        handler.drain();

        // Then try_recv returns None.
        assert!(handler.try_recv().is_none());
    }
}
