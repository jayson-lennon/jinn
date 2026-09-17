//! Realtime screen task — the per-session pump owner.
//!
//! One task per session owns the session's output receiver exclusively: it
//! batch-drains raw pty output on a ~50 ms cadence, answers terminal
//! capability queries, feeds the shared emulator, and republishes the mirror
//! (bus event + frontend write) on every visible change. This is what keeps
//! the overlay and sidebar live while a program runs on its own — no tool
//! call needs to be in flight.
//!
//! Ask-time settles never touch the receiver: they watch the screen-version
//! counter ([`ScreenHandle::version`]) and the pump-closed flag, so a settle
//! and this task cannot race for chunks.
//!
//! The shared emulator sits behind a `parking_lot::Mutex` (no poisoning);
//! guards are never held across an await. Lock order is shared-terminal →
//! writer (query replies); no path takes them in the opposite order.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::{Mutex, MutexGuard};

use crate::emulator::Emulator;
use crate::pty_session::OutputRx;
use crate::query_responder::respond_to_queries;
use jinn_domain::common::services::bus_service::BusService;
use jinn_term_msg::event::TermScreenUpdated;

/// Parse cadence of the screen task: ~20 fps — smooth for htop-style
/// animation, cheap for quiet programs.
pub const SCREEN_TICK: Duration = Duration::from_millis(50);

/// The emulator plus its screen-version counter, shared between the screen
/// task (writer) and the coordinator actor (reader/resizer).
pub struct SharedTerminal {
    emulator: Emulator,
    version_tx: tokio::sync::watch::Sender<u64>,
}

impl SharedTerminal {
    /// The shared emulator (reads from the actor, parsing in the task).
    #[must_use]
    pub(crate) fn emulator(&self) -> &Emulator {
        &self.emulator
    }

    /// The shared emulator, mutably (transcript sync from the actor).
    pub(crate) fn emulator_mut(&mut self) -> &mut Emulator {
        &mut self.emulator
    }
    /// Pairs an emulator with its version counter.
    #[must_use]
    pub fn new(emulator: Emulator, version_tx: tokio::sync::watch::Sender<u64>) -> Self {
        Self {
            emulator,
            version_tx,
        }
    }

    /// Bumps the screen-version counter (a settle wake-up signal).
    fn bump_version(&mut self) {
        self.version_tx.send_modify(|version| {
            *version += 1;
        });
    }
}

/// Cloneable handle to a session's shared screen state.
#[derive(Clone)]
pub struct ScreenHandle {
    shared: Arc<Mutex<SharedTerminal>>,
    /// The pty writer, shared with the coordinator so query replies can be
    /// written from here. Separate lock; see the module docs for ordering.
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    version: tokio::sync::watch::Receiver<u64>,
    pump_closed: Arc<AtomicBool>,
}

impl ScreenHandle {
    /// Pairs the shared emulator state with a fresh writer slot and the
    /// version receiver.
    #[must_use]
    pub fn new(shared: SharedTerminal, version: tokio::sync::watch::Receiver<u64>) -> Self {
        Self {
            shared: Arc::new(Mutex::new(shared)),
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            version,
            pump_closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Installs the real pty writer (called once at spawn, before the child
    /// can emit output that triggers a query reply).
    pub fn install_writer(&self, writer: Box<dyn std::io::Write + Send>) {
        *self.writer.lock() = writer;
    }

    /// Locks the shared emulator.
    pub fn lock(&self) -> MutexGuard<'_, SharedTerminal> {
        self.shared.lock()
    }

    /// A receiver watching the screen-version counter (settle wake-ups).
    #[must_use]
    pub fn version(&self) -> tokio::sync::watch::Receiver<u64> {
        self.version.clone()
    }

    /// Whether the output pump has closed (the program exited).
    #[must_use]
    pub fn pump_closed(&self) -> bool {
        self.pump_closed.load(Ordering::SeqCst)
    }

    /// Writes bytes to the pty (query replies from the screen task).
    pub(crate) fn write_reply(&self, bytes: &[u8]) {
        if let Err(err) = self.writer.lock().write_all(bytes) {
            tracing::debug!(%err, "screen task query reply failed");
        }
    }

    /// Writes bytes to the pty through the shared writer slot (agent input
    /// path). Returns the io error for the caller to wrap.
    pub(crate) fn write_all_via(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.lock().write_all(bytes)
    }

    /// The emulator grid size as `(rows, cols)`.
    #[must_use]
    pub fn emulator_size(&self) -> (u16, u16) {
        self.lock().emulator.size()
    }

    /// Resizes the shared emulator grid (after the pty resize).
    pub fn set_emulator_size(&self, rows: u16, cols: u16) {
        self.lock().emulator.set_size(rows, cols);
    }

    /// Marks the pump closed and clears the session's live flag.
    fn finish(&self, wiring: &ScreenWiring) {
        self.pump_closed.store(true, Ordering::SeqCst);
        wiring.set_live(false);
    }
}

/// Everything the screen task needs to publish mirrors on its own.
#[derive(Clone)]
pub struct ScreenWiring {
    /// Bus for `TermScreenUpdated` events.
    pub bus: BusService,
    /// Shared application state (mirror writes).
    pub state: jinn_domain::common::state::State,
    /// The owning chat session (event, mirror + live-flag key).
    pub chat: jinn_core_types::SessionId,
}

impl ScreenWiring {
    fn set_live(&self, live: bool) {
        self.with_tabs(|ops| {
            ops.set_live(&self.chat, live);
        });
    }

    /// Publishes one screen snapshot: bus event + frontend mirror.
    pub async fn publish_screen(
        &self,
        screen: String,
        cells: crate::emulator::ScreenCells,
        cursor: (u16, u16),
        cursor_hidden: bool,
    ) {
        self.bus
            .publish(TermScreenUpdated {
                chat_session_id: self.chat.clone(),
                screen: screen.clone(),
                cells: cells.clone(),
                cursor,
                cursor_hidden,
            })
            .await;
        self.with_tabs(|ops| {
            ops.apply_screen(&self.chat, screen, cells, cursor, cursor_hidden);
        });
    }
}

/// Spawns the realtime screen task for a session; returns its abort handle.
///
/// The task exits when the pump closes (pty EOF), the channel drops, or the
/// handle is aborted (session killed/replaced).
pub(crate) fn spawn_screen_task(
    mut rx: OutputRx,
    handle: ScreenHandle,
    wiring: ScreenWiring,
) -> tokio::task::AbortHandle {
    tokio::spawn(async move {
        // Live from the first tick (see `mark_live` for why the task owns
        // this transition).
        wiring.set_live(true);
        // Seed the change tracker with the current screen so pre-existing
        // content is not republished as a delta.
        let mut previous_screen = handle.lock().emulator.plain_text();
        loop {
            // Wait for the first chunk of a burst (or idle-tick out).
            let first = match tokio::time::timeout(SCREEN_TICK, rx.recv()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,      // pump closed: the program exited.
                Err(_tick) => continue, // idle: nothing buffered.
            };
            let mut batch = vec![first];
            while let Ok(chunk) = rx.try_recv() {
                batch.push(chunk);
            }
            for chunk in &batch {
                let mut guard = handle.lock();
                // Queries are answered from the pre-parse chunk with the
                // pre-parse cursor, mirroring a real terminal's order.
                let replies = respond_to_queries(chunk, guard.emulator.cursor_position());
                if !replies.is_empty() {
                    handle.write_reply(&replies);
                }
                guard.emulator.feed(chunk);
            }
            let (screen, cells) = {
                let guard = handle.lock();
                (guard.emulator.plain_text(), guard.emulator.cells())
            };
            if screen != previous_screen {
                previous_screen = screen.clone();
                let (cursor, cursor_hidden) = {
                    let guard = handle.lock();
                    (
                        guard.emulator().cursor_position(),
                        guard.emulator().cursor_hidden(),
                    )
                };
                handle.lock().bump_version();
                wiring
                    .publish_screen(screen, cells, cursor, cursor_hidden)
                    .await;
            }
        }
        handle.finish(&wiring);
    })
    .abort_handle()
}

impl ScreenWiring {
    /// Updates the `term/tabs` cell (slice-owned terminal mirrors).
    fn with_tabs<R>(&self, f: impl FnOnce(&mut jinn_term_msg::TerminalTabState) -> R) -> R {
        let slices = self
            .state
            .read()
            .frontend
            .slices()
            .cloned()
            .expect("term cell missing: slices not attached");
        let cell = slices
            .reader::<jinn_term_msg::TerminalTabState>(&jinn_term_msg::term_tabs_slot())
            .expect("term/tabs cell missing: slice not registered");
        let mut tabs = cell.read().clone();
        let out = f(&mut tabs);
        cell.update(|t| *t = tabs.clone());
        out
    }
}
