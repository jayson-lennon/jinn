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

use crossterm::event::Event;
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
    #[expect(clippy::expect_used, reason = "thread spawn failure is fatal")]
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

/// Concatenates a leading paste chunk with any immediately-following paste
/// chunks from `pending`, returning the full coalesced paste text and the
/// remaining (non-paste) tail.
///
/// `pending` are events already read from crossterm that follow the initial
/// paste chunk. Iteration stops at the first non-`Paste` event, which becomes
/// the head of the returned remaining slice so the caller can re-emit it in
/// order. Back-to-back paste chunks merge into a single string; any boundary
/// (a key, mouse, resize, etc.) terminates the run.
///
/// Pure over an already-read event slice so the merge contract is unit-testable
/// without a live terminal.
fn coalesce_paste(initial: String, pending: &[Event]) -> (String, &[Event]) {
    // Reserve the combined byte length up front so the result allocates once
    // instead of growing incrementally — matters for arbitrarily large pastes.
    let paste_byte_len: usize = pending
        .iter()
        .take_while(|e| matches!(e, Event::Paste(_)))
        .filter_map(|e| match e {
            Event::Paste(chunk) => Some(chunk.len()),
            _ => None,
        })
        .sum();

    let mut full = initial;
    full.reserve(paste_byte_len);

    let mut remaining = pending;
    while let Some((first, rest)) = remaining.split_first() {
        match first {
            Event::Paste(chunk) => {
                full.push_str(chunk);
                remaining = rest;
            }
            _ => break,
        }
    }

    (full, remaining)
}

/// Non-blocking drain of every crossterm event that is ready *right now*.
///
/// Returns once `poll(Duration::ZERO)` reports no event, on a read/poll error,
/// or when `stop` is set. Used to gather follow-on paste chunks (and any
/// interleaved non-paste event) into one batch so a paste action can be
/// coalesced before it crosses the message channel.
fn drain_ready_events(stop: &AtomicBool) -> Vec<Event> {
    let mut events = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match crossterm::event::poll(Duration::ZERO) {
            Ok(true) => match crossterm::event::read() {
                Ok(evt) => events.push(evt),
                Err(e) => {
                    tracing::error!(err = ?e, "crossterm event read error during paste drain");
                    break;
                }
            },
            Ok(false) => break,
            Err(e) => {
                tracing::error!(err = ?e, "crossterm event poll error during paste drain");
                break;
            }
        }
    }
    events
}

/// Forwards a crossterm event read by the poll loop, coalescing pastes.
///
/// A `Paste` event triggers a non-blocking drain of any immediately-following
/// chunks; the merged text is emitted as a single [`Msg::Input(Event::Paste(_))`].
/// Any trailing non-paste event consumed by the drain is re-emitted in order.
/// Non-paste events forward unchanged.
fn forward_read_event(sender: &MsgSender, evt: Event, stop: &AtomicBool) {
    let Event::Paste(initial) = evt else {
        sender.send(Msg::Input(evt));
        return;
    };

    let drained = drain_ready_events(stop);
    let (full, remaining) = coalesce_paste(initial, &drained);

    sender.send(Msg::Input(Event::Paste(full)));

    for trailing in remaining {
        sender.send(Msg::Input(trailing.clone()));
    }
}

/// Runs the event poll loop on a dedicated OS thread.
///
/// Uses synchronous `crossterm::event::poll` / `read` instead of the async
/// `EventStream`, so this thread never competes with tokio worker threads.
#[expect(
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
                // Event available - read and forward, coalescing paste chunks.
                match crossterm::event::read() {
                    Ok(evt) => forward_read_event(sender, evt, &stop),
                    Err(e) => {
                        tracing::error!(err = ?e, "crossterm event read error");
                    }
                }
            }
            Ok(false) => {
                // Timeout - no event, loop back to check tick/stop.
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

    /// Builds an `Event::Paste` carrying `text`.
    fn paste(text: &str) -> Event {
        Event::Paste(text.to_owned())
    }

    /// A non-paste event used as a boundary in merge tests.
    fn key_event() -> Event {
        Event::Key(crossterm::event::KeyEvent::new_with_kind_and_state(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Press,
            crossterm::event::KeyEventState::NONE,
        ))
    }

    #[rstest::rstest]
    fn coalesce_two_paste_chunks_merges_into_one() {
        // Given an initial paste chunk and a following paste chunk.
        let pending = [paste(" world")];

        // When coalescing.
        let (full, remaining) = coalesce_paste("hello".to_owned(), &pending);

        // Then the two chunks are concatenated.
        assert_eq!(full, "hello world");
        // And no non-paste event remains.
        assert_eq!(remaining, []);
    }

    #[rstest::rstest]
    fn coalesce_many_paste_chunks_preserve_order() {
        // Given an initial chunk plus four more, in order.
        let pending = [paste("b"), paste("c"), paste("d"), paste("e")];

        // When coalescing.
        let (full, remaining) = coalesce_paste("a".to_owned(), &pending);

        // Then all chunks merge in their original order.
        assert_eq!(full, "abcde");
        assert_eq!(remaining, []);
    }

    #[rstest::rstest]
    fn coalesce_stops_at_first_non_paste_event() {
        // Given a paste chunk, a key event, and another paste chunk.
        let key = key_event();
        let pending = [paste("tail"), key.clone(), paste("after")];

        // When coalescing.
        let (full, remaining) = coalesce_paste("head".to_owned(), &pending);

        // Then only the leading paste run merges.
        assert_eq!(full, "headtail");
        // And the key event is the head of the remaining tail (the
        // trailing paste stays after it, unmerged).
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0], key);
    }

    #[rstest::rstest]
    fn coalesce_empty_chunk_is_harmless() {
        // Given an initial empty paste chunk followed by a non-empty one.
        let pending = [paste(""), paste("data")];

        // When coalescing.
        let (full, remaining) = coalesce_paste(String::new(), &pending);

        // Then the empty chunks contribute nothing and the data merges in.
        assert_eq!(full, "data");
        assert_eq!(remaining, []);
    }

    #[rstest::rstest]
    fn coalesce_single_paste_with_no_pending_returns_full() {
        // Given an initial paste with no following events.
        // When coalescing.
        let (full, remaining) = coalesce_paste("only".to_owned(), &[]);

        // Then the initial text is returned unchanged with no remainder.
        assert_eq!(full, "only");
        assert_eq!(remaining, []);
    }

    #[rstest::rstest]
    fn coalesce_large_paste_is_byte_equal_to_chunk_sum() {
        // Given many paste chunks whose sizes vary.
        let chunk_sizes = [0, 1, 64, 1_024, 16_384, 1];
        let pending: Vec<Event> = chunk_sizes.iter().map(|&n| paste(&"x".repeat(n))).collect();
        let expected: String = chunk_sizes.iter().map(|&n| "x".repeat(n)).collect();

        // When coalescing.
        let (full, remaining) = coalesce_paste(String::new(), &pending);

        // Then the result is byte-for-byte equal to the concatenation.
        assert_eq!(full.len(), expected.len());
        assert_eq!(full, expected);
        assert_eq!(remaining, []);
    }
}
