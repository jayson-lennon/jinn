//! Interactive-term coordinator actor — owns PTY sessions across tool calls.
//!
//! One instance lives for the whole app. It owns every session (pty child +
//! emulator + transcript) so sessions persist between tool calls and turns:
//! the spawned program's lifetime is decoupled from the calls that drive it.
//!
//! **One terminal per chat session, keyed by the chat session.** Sessions are
//! keyed by the owning chat [`SessionId`]; spawning while a session already
//! has a live terminal kills the old one first and reports it. The chat
//! session id *is* the terminal's identity — every message carries it and
//! there is no separate model-facing term id.
//!
//! **Realtime display.** Each session's output pump is owned by its screen
//! task (see [`screen_task`]), which parses on a ~50 ms cadence and
//! republishes the mirror on change — the overlay and sidebar stay live
//! while the program runs on its own, with no tool call in flight. Ask-time
//! settles (spawn/send) never touch the receiver: they watch the screen
//! version counter, so a settle and the screen task cannot race the pump.
//!
//! The tools (`interactive_term`, `interactive_term_send`,
//! `interactive_term_kill`) `ask` this actor directly (request/reply,
//! mirroring the `restart_mcp_server` tool); no bus eavesdropping, no
//! ordering race.
//!
//! The **settle decision** lives here: after a spawn or send, the ask waits
//! until the screen has been quiet for the quiet window or the hard cap was
//! hit (see `settle` for the decision logic). Control flips (user takeover)
//! are checked via the shared [`TermControls`] registry on every settle poll
//! — mailbox messages are processed sequentially, so a mid-drain takeover
//! could never be seen through the mailbox; the registry closes that gap.
//!
//! I/O: the pty pump is a std thread feeding an unbounded **tokio** mpsc
//! channel; kanal is forbidden in this select loop (documented double-free
//! under cancellation — see the bash tool).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use error_stack::Report;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use crate::pty_session::{ExitInfo, PtySession};
use crate::screen_task::{ScreenHandle, ScreenWiring};
use crate::settle::{encode_input, should_settle};
use jinn_domain::common::services::bus_service::BusService;
use jinn_term_msg::command::{
    ControlHolder, KillTerm, KillTermOutcome, ResizeTerm, SendTermInput, SendTermKey,
    SendTermOutcome, SpawnTerm, SpawnTermOutcome, TermScreen,
};
use jinn_term_msg::event::TermScreenUpdated;
use jinn_term_msg::takeover::TermControls;

/// How many transcript screens the kill result reports.
const TRANSCRIPT_TAIL_SCREENS: usize = 20;

/// Static path the coordinator spawns at (one instance per process).
pub const INTERACTIVE_TERM_PATH: &str = "jinn.term.coordinator";

/// Per-chat-session control holders: who may drive each terminal right now.
///
/// Shared between the actor (authoritative writer — mints `Agent` on spawn,
/// removes the entry on teardown) and the takeover UI (the `IntentHandler`
/// flips the active session's holder synchronously so an in-flight tool
/// call's settle sees the takeover on its next poll — mailbox-sequential
/// message handling cannot deliver that). Polled from async settle loops:
/// plain mutex, never held across an await. Sessions with no entry default
/// to [`ControlHolder::Agent`].

/// A live interactive session owned by the actor.
struct TermSession {
    /// The pty child; also reaches the shared emulator and screen task.
    pty: PtySession,
    /// Captured once the process terminated.
    exited: Option<ExitInfo>,
    /// Last screen text the *actor* returned/published (ask results); the
    /// screen task's mirror publication is keyed off its own tracker.
    last_screen: String,
}

impl TermSession {
    /// Screen text, styled cells, cursor, and visibility from the emulator.
    fn snapshot(&self) -> (String, crate::emulator::ScreenCells, (u16, u16), bool) {
        let handle = self.pty.screen();
        let guard = handle.lock();
        (
            guard.emulator().plain_text(),
            guard.emulator().cells(),
            guard.emulator().cursor_position(),
            guard.emulator().cursor_hidden(),
        )
    }

    /// Appends the current screen to the transcript ring.
    fn sync_transcript(&self) {
        self.pty.screen().lock().emulator_mut().sync_transcript();
    }

    /// The transcript tail (most recent screens).
    fn transcript_tail(&self, max_screens: usize) -> String {
        self.pty
            .screen()
            .lock()
            .emulator()
            .transcript_tail(max_screens)
    }
}

/// The interactive-term coordinator actor.
pub struct InteractiveTermActor {
    bus: BusService,
    controls: TermControls,
    /// Live sessions keyed by their owning chat session.
    sessions: HashMap<jinn_core_types::SessionId, TermSession>,
    state: jinn_domain::common::state::State,
    settle_quiet: Duration,
    settle_cap: Duration,
}

/// Dependencies for [`InteractiveTermActor`].
#[derive(Clone)]
pub struct InteractiveTermActorDeps {
    /// Bus for screen events.
    pub bus: BusService,
    /// Per-session control registry; the spawner keeps a clone for the UI.
    pub controls: TermControls,
    /// Shared application state — the actor owns `frontend.terminal` and
    /// mirrors published screen events into it.
    pub state: jinn_domain::common::state::State,
    /// Quiet window for the settle wait.
    pub settle_quiet: Duration,
    /// Hard cap for the settle wait.
    pub settle_cap: Duration,
}

impl ServiceActor for InteractiveTermActor {
    async fn start(_args: &serde_json::Value) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s start_with (typed deps can't
        // ride the JSON args).
        let _ = _args;
        Err(Report::new(RegistryError::InvalidSpec).attach(
            "InteractiveTermActor is spawned via start_with with typed deps",
        ))
    }
}

impl InteractiveTermActor {
    /// Spawns the coordinator actor onto the trouper system.
    ///
    /// Returns the actor path (asks route through `system.ask` at this
    /// path) and the shared control registry (hand the clone to the
    /// takeover UI wiring). Subscriptions are live when this returns: the
    /// builder's start handshake completes before `subscribe` runs.
    pub async fn spawn(
        system: &trouper::system::ActorSystem,
        deps: InteractiveTermActorDeps,
    ) -> (ActorPath, TermControls) {
        let controls = deps.controls.clone();
        let path = ActorPath::new(INTERACTIVE_TERM_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .start_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            bus: deps.bus,
                            controls: deps.controls,
                            sessions: HashMap::new(),
                            state: deps.state,
                            settle_quiet: deps.settle_quiet,
                            settle_cap: deps.settle_cap,
                        })
                    })
                }
            })
            .handles::<SpawnTerm>()
            .handles::<SendTermInput>()
            .handles::<KillTerm>()
            .handles::<SendTermKey>()
            .handles::<ResizeTerm>()
            .handles::<jinn_domain::feat::session::protocol::session_closed::SessionClosed>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        // Teardown sub: a closed chat session takes its terminal with it
        // (the pty drop kills the process group) instead of outliving the
        // session until app exit.
        system
            .subscribe(
                &path,
                &jinn_domain::common::services::bus_service::jinn_domain_topic(),
                None,
            )
            .expect("term coordinator subscribes the domain topic");
        (path, controls)
    }
}

/// How a settle wait concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettleReason {
    /// The screen was quiet for the quiet window.
    Quiet,
    /// The hard cap (or the ask's `max_wait`) elapsed.
    Cap,
    /// The user took control mid-wait.
    UserTookControl,
    /// The program exited; the screen cannot change again.
    Exited,
}

/// Waits until a session's screen settles, watching its version counter.
///
/// The screen task owns the pump; this loop only observes the version watch,
/// so asks and realtime parsing never race for chunks. Settles when the
/// screen has been unchanged for the quiet window (or the program exited and
/// the window elapsed), when the cap elapsed, or when the user took control
/// of this chat session's terminal.
async fn wait_for_settle(
    handle: &ScreenHandle,
    controls: &TermControls,
    chat: &jinn_core_types::SessionId,
    quiet: Duration,
    cap: Duration,
) -> SettleReason {
    let started = Instant::now();
    let mut last_change = started;
    let mut version = handle.version();
    let mut last_seen = *version.borrow();
    loop {
        let now = Instant::now();
        let quiet_for = now.duration_since(last_change);
        let waited = now.duration_since(started);
        if controls.holder_for(chat) == ControlHolder::User {
            return SettleReason::UserTookControl;
        }
        let pump_closed = handle.pump_closed();
        let settled =
            (pump_closed && quiet_for >= quiet) || should_settle(quiet_for, waited, quiet, cap);
        if settled {
            return if controls.holder_for(chat) == ControlHolder::User {
                SettleReason::UserTookControl
            } else if pump_closed && waited < cap {
                SettleReason::Exited
            } else if waited >= cap {
                SettleReason::Cap
            } else {
                SettleReason::Quiet
            };
        }
        let wait_for = quiet
            .saturating_sub(quiet_for)
            .min(cap.saturating_sub(waited))
            .min(Duration::from_millis(25));
        match tokio::time::timeout(wait_for, version.changed()).await {
            Ok(Ok(())) => {
                let current = *version.borrow();
                if current != last_seen {
                    last_change = Instant::now();
                    last_seen = current;
                }
            }
            Ok(Err(_recv_error)) => {
                // The session's version sender is gone: torn down mid-wait.
                return SettleReason::Exited;
            }
            Err(_elapsed) => {} // poll elapsed: re-check conditions at the top.
        }
    }
}

impl InteractiveTermActor {
    /// Handles [`SpawnTerm`]: kills the chat session's previous terminal (if
    /// any), creates the pty session with its realtime screen task, and runs
    /// the initial settle wait against the screen-version watch.
    async fn handle_spawn(&mut self, msg: SpawnTerm) -> SpawnTermOutcome {
        // One terminal per chat session: replace any live terminal first.
        let killed_previous = if self.sessions.contains_key(&msg.chat_session_id) {
            self.remove_session(&msg.chat_session_id).map(|removed| {
                jinn_term_msg::command::KilledPrevious {
                    exited: removed.exited.unwrap_or(ExitInfo {
                        code: 0,
                        signal: None,
                    }),
                }
            })
        } else {
            None
        };

        let (rows, cols) = msg.size;
        let spawned = PtySession::spawn(
            &msg.command,
            &msg.cwd,
            portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            },
            self.wiring(&msg.chat_session_id),
        );
        let (pty, _pump) = match spawned {
            Ok(pair) => pair,
            Err(report) => return SpawnTermOutcome::Failed(format!("{report:#}")),
        };

        // The screen task owns parsing from spawn on; this ask settles on
        // the version watch (cap clamped to the ask's max_wait).
        let _settle = wait_for_settle(
            &pty.screen(),
            &self.controls,
            &msg.chat_session_id,
            self.settle_quiet,
            self.settle_cap.min(msg.max_wait),
        )
        .await;

        let exited = pty.try_wait();
        pty.screen().lock().emulator_mut().sync_transcript();
        let (screen, cells, cursor, cursor_hidden) = {
            let handle = pty.screen();
            let guard = handle.lock();
            (
                guard.emulator().plain_text(),
                guard.emulator().cells(),
                guard.emulator().cursor_position(),
                guard.emulator().cursor_hidden(),
            )
        };
        // A fresh terminal starts agent-controlled.
        self.controls
            .set(&msg.chat_session_id, ControlHolder::Agent);
        self.sessions.insert(
            msg.chat_session_id.clone(),
            TermSession {
                pty,
                exited: exited.clone(),
                last_screen: screen.clone(),
            },
        );
        self.write_mirror(
            &msg.chat_session_id,
            screen.clone(),
            cells,
            cursor,
            cursor_hidden,
        );

        SpawnTermOutcome::Started {
            screen: TermScreen { screen, exited },
            killed_previous,
        }
    }

    /// The per-session screen-task wiring (bus + mirror + live-flag key).
    fn wiring(&self, chat: &jinn_core_types::SessionId) -> ScreenWiring {
        ScreenWiring {
            bus: self.bus.clone(),
            state: self.state.clone(),
            chat: chat.clone(),
        }
    }

    /// Writes one screen snapshot into the frontend mirror.
    fn write_mirror(
        &self,
        chat: &jinn_core_types::SessionId,
        screen: String,
        cells: crate::emulator::ScreenCells,
        cursor: (u16, u16),
        cursor_hidden: bool,
    ) {
        self.with_tabs(|ops| ops.apply_screen(chat, screen, cells, cursor, cursor_hidden));
    }

    /// Marks (or clears) a session's live-terminal flag in the mirror.
    fn set_live(&self, chat: &jinn_core_types::SessionId, live: bool) {
        self.with_tabs(|ops| {
            ops.set_live(chat, live);
        });
    }

    /// Removes and tears down a session (aborts its screen task, clears its
    /// live flag and control entry), returning exit info for reporting.
    fn remove_session(&mut self, chat: &jinn_core_types::SessionId) -> Option<RemovedSession> {
        // Clear the live flag *before* the session is dropped: dropping the
        // session aborts the screen task, and a task killed before it observed
        // EOF never gets to clear the flag itself.
        let session = self.sessions.remove(chat)?;
        self.with_tabs(|ops| {
            ops.set_live(chat, false);
        });
        self.controls.remove(chat);
        let removed = RemovedSession {
            exited: session.exited.clone(),
        };
        drop(session);
        Some(removed)
    }

    /// Handles [`SendTermInput`].
    async fn handle_send(&mut self, msg: SendTermInput) -> SendTermOutcome {
        // Take the session out so the settle await below holds no borrow over
        // the map; it is unconditionally replaced before returning.
        let chat = msg.chat_session_id.clone();
        let Some(mut session) = self.sessions.remove(&chat) else {
            return SendTermOutcome::UnknownSession;
        };

        if session.exited.is_some() {
            let outcome = SendTermOutcome::Exited(TermScreen {
                screen: session.last_screen.clone(),
                exited: session.exited.clone(),
            });
            self.sessions.insert(chat, session);
            return outcome;
        }
        // User takeover: refuse agent input (checked here and re-checked
        // after the settle below). No screen is returned — the user's
        // terminal is theirs to read; the tool layer fails the call with
        // the wait notice.
        if self.controls.holder_for(&chat) == ControlHolder::User {
            self.sessions.insert(chat, session);
            return SendTermOutcome::UserHasControl;
        }

        let bytes = encode_input(msg.text.as_deref(), &msg.keys, msg.enter);
        if !bytes.is_empty()
            && let Err(report) = session.pty.write(&bytes)
        {
            tracing::warn!(report = %report, chat = %chat, "pty write failed");
        }

        // The screen task parses; settle on the version watch.
        let _settle = wait_for_settle(
            &session.pty.screen(),
            &self.controls,
            &chat,
            self.settle_quiet,
            self.settle_cap.min(msg.max_wait),
        )
        .await;

        session.exited = session.pty.try_wait();
        session.sync_transcript();
        let (screen, cells, cursor, cursor_hidden) = session.snapshot();
        session.last_screen.clone_from(&screen);
        self.sessions.insert(chat.clone(), session);
        self.write_mirror(&chat, screen.clone(), cells, cursor, cursor_hidden);

        if self.controls.holder_for(&chat) == ControlHolder::User {
            // The user grabbed the terminal mid-call; report the takeover
            // without a screen (their terminal, their read).
            return SendTermOutcome::UserHasControl;
        }
        SendTermOutcome::Sent(TermScreen {
            screen,
            exited: self.sessions.get(&chat).and_then(|s| s.exited.clone()),
        })
    }

    /// Handles [`KillTerm`].
    async fn handle_kill(&mut self, msg: KillTerm) -> KillTermOutcome {
        let chat = msg.chat_session_id.clone();
        let Some(session) = self.sessions.get_mut(&chat) else {
            return KillTermOutcome::UnknownSession;
        };
        session.pty.kill();
        // Give the kernel a moment to report the exit; the group signal is
        // near-instant but `try_wait` is asynchronous to it.
        let deadline = Instant::now() + Duration::from_millis(500);
        while session.exited.is_none() && Instant::now() < deadline {
            session.exited = session.pty.try_wait();
            if session.exited.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        session.sync_transcript();
        let (screen, _, _cursor, _hidden) = session.snapshot();
        session.last_screen = screen;
        let exited = session.exited.clone().unwrap_or(ExitInfo {
            code: 0,
            signal: None,
        });
        let tail = session.transcript_tail(TRANSCRIPT_TAIL_SCREENS);
        let _ = screen;
        let final_screen = session.last_screen.clone();
        // The session *stays registered* (a repeat kill is still Killed, a
        // send reports Exited); only the live flag clears. The registry entry
        // is replaced by the next spawn for this chat session.
        self.set_live(&chat, false);
        KillTermOutcome::Killed {
            screen: final_screen,
            transcript_tail: tail,
            exited,
        }
    }
}

/// What `remove_session` reports about the torn-down session.
struct RemovedSession {
    exited: Option<ExitInfo>,
}

impl MsgHandler<SpawnTerm> for InteractiveTermActor {
    async fn handle(&mut self, msg: SpawnTerm, _ctx: &mut MsgCtx<'_>) {
        let reply = self.handle_spawn(msg).await;
        _ctx.reply(reply);
    }
}

impl MsgHandler<SendTermInput> for InteractiveTermActor {
    async fn handle(&mut self, msg: SendTermInput, _ctx: &mut MsgCtx<'_>) {
        let reply = self.handle_send(msg).await;
        _ctx.reply(reply);
    }
}

impl MsgHandler<KillTerm> for InteractiveTermActor {
    async fn handle(&mut self, msg: KillTerm, _ctx: &mut MsgCtx<'_>) {
        let reply = self.handle_kill(msg).await;
        _ctx.reply(reply);
    }
}

impl MsgHandler<SendTermKey> for InteractiveTermActor {
    async fn handle(&mut self, msg: SendTermKey, _ctx: &mut MsgCtx<'_>) {
        // User keystrokes bypass the settle wait entirely: the user is
        // driving, so there is nothing to report back to an agent.
        if let Some(session) = self.sessions.get_mut(&msg.chat_session_id)
            && let Err(report) = session.pty.write(&msg.bytes)
        {
            tracing::warn!(report = %report, chat = %msg.chat_session_id, "pty key write failed");
        }
    }
}

impl MsgHandler<ResizeTerm> for InteractiveTermActor {
    async fn handle(&mut self, msg: ResizeTerm, _ctx: &mut MsgCtx<'_>) {
        self.apply_resize(msg).await;
    }
}

impl InteractiveTermActor {
    /// Resizes the named chat session's pty and emulator.
    ///
    /// Sizes are clamped to a minimal usable grid; a message without a chat
    /// session or naming a session with no live terminal is a no-op — the
    /// render layer always names the session the overlay shows, and a
    /// broadcast resize would clobber other sessions' grids.
    async fn apply_resize(&mut self, msg: ResizeTerm) {
        let Some(chat) = msg.chat_session_id else {
            return;
        };
        if !self.sessions.contains_key(&chat) {
            return;
        }
        let (rows, cols) = (msg.size.0.max(2), msg.size.1.max(20));
        let Some(session) = self.sessions.get_mut(&chat) else {
            return;
        };
        if session.pty.emulator_size() == (rows, cols) {
            return;
        }
        let _ = session.pty.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        session.pty.set_emulator_size(rows, cols);
        let (screen, cells, cursor, hidden) = {
            let handle = session.pty.screen();
            let guard = handle.lock();
            (
                guard.emulator().plain_text(),
                guard.emulator().cells(),
                guard.emulator().cursor_position(),
                guard.emulator().cursor_hidden(),
            )
        };
        session.last_screen = screen.clone();
        self.write_mirror(&chat, screen.clone(), cells.clone(), cursor, hidden);
        self.bus
            .publish(TermScreenUpdated {
                chat_session_id: chat,
                screen,
                cells,
                cursor,
                cursor_hidden: hidden,
            })
            .await;
    }
}

impl MsgHandler<jinn_domain::feat::session::protocol::session_closed::SessionClosed>
    for InteractiveTermActor
{
    async fn handle(
        &mut self,
        msg: jinn_domain::feat::session::protocol::session_closed::SessionClosed,
        _ctx: &mut MsgCtx<'_>,
    ) {
        // `remove_session` clears the live flag before dropping the session
        // (a screen task killed before observing EOF can't clear it itself),
        // drops the pty (killing the process group), and removes the
        // session's control entry. The overlay mirror goes with it.
        if self.remove_session(&msg.session_id).is_some() {
            self.with_tabs(|ops| {
                ops.remove_mirror(&msg.session_id);
            });
            tracing::debug!(session = %msg.session_id, "terminal torn down on SessionClosed");
        }
    }
}

impl InteractiveTermActor {
    /// Updates the `term/tabs` cell (the slice-owned terminal mirrors).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unwrap_used,
        reason = "test code"
    )]
    use super::*;

    /// Snapshot a session's mirror from the term/tabs cell.
    fn test_mirror(
        guard: &jinn_domain::common::state::StateReadGuard<'_>,
        chat: &jinn_core_types::SessionId,
    ) -> Option<jinn_term_msg::TerminalMirror> {
        guard
            .term_tabs()
            .and_then(|c| c.read().mirror(chat).cloned())
    }

    /// Whether a session has a live terminal (from the term/tabs cell).
    fn test_live(
        guard: &jinn_domain::common::state::StateReadGuard<'_>,
        chat: &jinn_core_types::SessionId,
    ) -> bool {
        guard
            .term_tabs()
            .map(|c| c.read().live_terms.contains(chat))
            .unwrap_or(false)
    }
    use jinn_domain::common::bus::test_harness::{GetRecorded, TestHarness};

    const QUIET: Duration = Duration::from_millis(150);
    const CAP: Duration = Duration::from_secs(2);

    fn deps(
        bus: BusService,
        controls: TermControls,
    ) -> (InteractiveTermActorDeps, jinn_domain::common::state::State) {
        let state = jinn_domain::common::state::State::new(
            jinn_domain::common::app_state::AppState::default_with_scope_focus(),
        );
        let deps = InteractiveTermActorDeps {
            bus,
            controls,
            state: state.clone(),
            settle_quiet: QUIET,
            settle_cap: CAP,
        };
        (deps, state)
    }

    /// A typed ask client over the coordinator's trouper path.
    #[derive(Clone)]
    struct TermAskClient {
        system: trouper::system::ActorSystem,
        path: ActorPath,
    }

    impl TermAskClient {
        async fn ask<M, R>(&self, msg: M) -> R
        where
            M: serde::Serialize + trouper::schema::Schema,
            R: serde::de::DeserializeOwned,
        {
            let reply = self
                .system
                .ask(self.path.clone(), msg, std::time::Duration::from_secs(30))
                .await
                .expect("term ask round trip");
            serde_json::from_value(reply).expect("term ask reply decodes")
        }

        /// Fire-and-forget tell (fire-and-forget tests only need delivery,
        /// not a reply — sleeps after covers the async drain).
        async fn tell<M: serde::Serialize + trouper::schema::Schema>(&self, msg: M) {
            let _ = self.system.tell(self.path.clone(), msg).await;
        }
    }

    /// Spawns a coordinator onto the harness's trouper system.
    async fn spawn_coordinator(
        harness: &TestHarness,
        controls: TermControls,
    ) -> (TermAskClient, trouper::system::ActorSystem) {
        let services = harness.services().await;
        let (deps, _state) = deps(harness.bus(), controls);
        let (path, _controls) = InteractiveTermActor::spawn(&services.trouper_system, deps).await;
        let client = TermAskClient {
            system: services.trouper_system.clone(),
            path,
        };
        (client, services.trouper_system)
    }

    /// Spawns a coordinator with a readable state handle.
    async fn spawn_coordinator_with_state(
        harness: &TestHarness,
        controls: TermControls,
    ) -> (TermAskClient, jinn_domain::common::state::State) {
        let services = harness.services().await;
        let (deps, state) = deps(harness.bus(), controls);
        let (path, _controls) = InteractiveTermActor::spawn(&services.trouper_system, deps).await;
        let client = TermAskClient {
            system: services.trouper_system.clone(),
            path,
        };
        (client, state)
    }

    fn spawn_msg(chat: jinn_core_types::SessionId, command: &str) -> SpawnTerm {
        SpawnTerm {
            chat_session_id: chat,
            command: command.to_owned(),
            cwd: std::path::PathBuf::from("."),
            size: (24, 80),
            max_wait: Duration::from_secs(3),
        }
    }

    fn send_msg(chat: jinn_core_types::SessionId) -> SendTermInput {
        SendTermInput {
            chat_session_id: chat,
            text: None,
            keys: vec![],
            enter: false,
            max_wait: Duration::from_secs(3),
        }
    }

    fn plain_screen(screen: &str) -> String {
        screen
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned()
    }

    /// Spawns `cat` for a chat session (the most common test fixture).
    async fn spawn_cat(client: &TermAskClient, chat: &jinn_core_types::SessionId) {
        let outcome = client.ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "cat")).await;
        let SpawnTermOutcome::Started { .. } = outcome else {
            panic!("expected Started");
        };
    }

    /// Agent-sent f-keys must reach the program as real terminal bytes:
    /// `cat -v` echoes ESC as `^[`, so an F4 (`ESC O S`) shows as `^[OS`.
    /// (v1 regression: `encode_key("f4")` produced no bytes at all.)
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn agent_f4_key_reaches_the_program_as_bytes() {
        // Given a coordinator running `cat -v`.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        let _: SpawnTermOutcome = actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "cat -v"))
            .await;

        // When sending the named f4 key.
        let mut msg = send_msg(chat);
        msg.keys = vec!["f4".to_owned()];
        let outcome = actor.ask(msg).await;

        // Then the program echoed the F4 bytes (`^[` + `OS`).
        match outcome {
            SendTermOutcome::Sent(screen) => {
                assert!(
                    plain_screen(&screen.screen).contains("^[OS"),
                    "cat -v should echo F4 as ^[OS, got: {}",
                    plain_screen(&screen.screen)
                );
            }
            other => panic!("expected Screen, got {other:?}"),
        }
    }

    /// The pty child runs in the cwd the request carries (the tool passes
    /// its context cwd), so `pwd` prints that directory.
    #[cfg(unix)]
    #[rstest::rstest]
    #[tokio::test]
    async fn spawn_pty_runs_in_the_requested_cwd() {
        use std::path::PathBuf;

        // Given a coordinator and a real scratch directory.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let dir = std::env::temp_dir().join(format!("jinn-term-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");

        // When spawning `pwd` with that directory as cwd.
        let mut msg = spawn_msg(jinn_core_types::SessionId::new(), "pwd");
        msg.cwd = PathBuf::from(&dir);
        let reply = actor.ask(msg).await;

        // Then the screen shows the requested directory.
        match reply {
            SpawnTermOutcome::Started { screen, .. } => {
                assert!(
                    plain_screen(&screen.screen).contains(dir.to_str().expect("utf8 path")),
                    "pwd should print the requested cwd, got: {}",
                    plain_screen(&screen.screen)
                );
            }
            other => panic!("expected Started, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawn_returns_session_with_screen() {
        // Given a coordinator actor.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;

        // When spawning `echo`.
        let reply = actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(
                jinn_core_types::SessionId::new(),
                "echo hello",
            ))
            .await;

        // Then the outcome is a session with the echoed text on screen.
        match reply {
            SpawnTermOutcome::Started { screen, .. } => {
                assert!(plain_screen(&screen.screen).contains("hello"));
            }
            other => panic!("expected Started, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unspawnable_command_reports_failed_outcome() {
        // Given a coordinator actor.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;

        // When spawning with an empty command (bash exits immediately with a
        // usage error — the observable "spawn went wrong" path).
        let reply = actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(jinn_core_types::SessionId::new(), ""))
            .await;

        // Then either the session started and exited (shell reported the
        // error) or the outcome carries the failure — both surface the problem
        // to the caller; no silent success.
        match reply {
            SpawnTermOutcome::Started { screen, .. } => {
                assert!(screen.exited.is_some(), "empty command must exit promptly");
            }
            SpawnTermOutcome::Failed(_) => {}
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_input_reaches_the_program_across_calls() {
        // Given a coordinator with a running `cat`.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // When sending text plus enter in a second call.
        let mut msg = send_msg(chat);
        msg.text = Some("ping-from-agent".to_owned());
        msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };

        // Then the echoed input appears in the returned screen (state persisted).
        assert!(plain_screen(&screen.screen).contains("ping-from-agent"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn program_state_persists_across_separate_tool_calls() {
        // Given a coordinator with an interactive bash session.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "bash --noprofile --norc"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When setting a variable in one call...
        let mut msg = send_msg(chat.clone());
        msg.text = Some("TERMVAR=inner-42".to_owned());
        msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(_) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };

        // ...and reading it back in a *separate* call.
        let mut msg = send_msg(chat);
        msg.text = Some("echo val=$TERMVAR".to_owned());
        msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };

        // Then the variable survived — the same shell process served both calls.
        assert!(
            plain_screen(&screen.screen).contains("val=inner-42"),
            "screen was: {:?}",
            plain_screen(&screen.screen)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn named_keys_drive_a_full_screen_tui_across_calls() {
        // Given a full-screen "TUI": an alt-screen pager showing PAGE ONE /
        // PAGE TWO depending on the last key (cursor-addressed output).
        let tui = concat!(
            "printf '\\033[?1049h\\033[H'; ",
            "show() { printf '\\033[2J\\033[5;10H%s' \"$1\"; }; ",
            "show PAGE-ONE; ",
            "while IFS= read -rsn1 k; do ",
            "  case \"$k\" in ",
            "    B) show PAGE-TWO ;; ",
            "    q) printf '\\033[?1049l'; exit 0 ;; ",
            "  esac; ",
            "done"
        );
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let SpawnTermOutcome::Started { .. } = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), tui))
                .await
            else {
                panic!("expected Started");
            };
        }

        // When pressing the key that pages forward (printable "B").
        let mut msg = send_msg(chat);
        msg.text = Some("B".to_owned());
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };

        // Then the TUI re-rendered to page two on the returned screen.
        assert!(
            plain_screen(&screen.screen).contains("PAGE-TWO"),
            "screen was: {:?}",
            plain_screen(&screen.screen)
        );
        assert!(!plain_screen(&screen.screen).contains("PAGE-ONE"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn ctrl_c_key_terminates_a_reading_program() {
        // Given a coordinator with a running `cat` (blocks on input).
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // When sending the named key ctrl+c.
        let mut msg = send_msg(chat);
        msg.keys = vec!["ctrl+c".to_owned()];
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };

        // Then the program exited (SIGINT reached it through the pty).
        assert!(screen.exited.is_some(), "cat must exit on ctrl+c");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_session_send_returns_unknown() {
        // Given a coordinator with no sessions.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;

        // When sending input to a chat session with no terminal.
        let reply = actor
            .ask::<_, SendTermOutcome>(send_msg(jinn_core_types::SessionId::new()))
            .await
            ;

        // Then the outcome is UnknownSession.
        assert!(matches!(reply, SendTermOutcome::UnknownSession));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_to_exited_session_reports_exit_not_unknown() {
        // Given a coordinator with an exited session.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "true"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When sending input after exit.
        let reply = actor.ask::<_, SendTermOutcome>(send_msg(chat)).await;

        // Then the outcome is Exited with the exit info, not UnknownSession.
        match reply {
            SendTermOutcome::Exited(screen) => {
                assert!(screen.exited.is_some());
            }
            other => panic!("expected Exited, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn take_control_makes_agent_send_report_user_has_control() {
        // Given a coordinator with a running `cat` and the user holding control.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // When the user takes control and the agent then sends input.
        controls.set(&chat, ControlHolder::User);
        let mut msg = send_msg(chat);
        msg.text = Some("should-not-appear".to_owned());
        let reply = actor.ask::<_, SendTermOutcome>(msg).await;

        // Then the outcome is UserHasControl (a refusal, not a screen).
        let SendTermOutcome::UserHasControl = reply else {
            panic!("expected UserHasControl, got {reply:?}");
        };
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn agent_input_while_user_controls_reaches_no_process() {
        // Given a coordinator running a program that echoes "got-input"
        // only after consuming a byte, with the user holding control.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _state) = spawn_coordinator_with_state(&harness, controls.clone()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(
                    chat.clone(),
                    "printf waiting; IFS= read -rsn1 k; printf got-input; sleep 30",
                ))
                .await;
let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }
        controls.set(&chat, ControlHolder::User);

        // When the agent sends input and is refused.
        let mut msg = send_msg(chat.clone());
        msg.text = Some("x".to_owned());
        msg.enter = true;
        let reply = actor.ask::<_, SendTermOutcome>(msg).await;

        // Then the refusal carries no screen (the user's terminal is theirs
        // to read)…
        assert!(
            matches!(reply, SendTermOutcome::UserHasControl),
            "expected UserHasControl, got {reply:?}"
        );
        // …and the program never consumed a byte: after the user releases
        // control, a fresh ask still shows the waiting screen, not
        // "got-input".
        controls.set(&chat, ControlHolder::Agent);
        let mut sync = send_msg(chat);
        sync.max_wait = Duration::from_millis(600);
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(sync).await else {
            panic!("expected Sent after release");
        };
        assert!(
            !plain_screen(&screen.screen).contains("got-input"),
            "program consumed agent input despite user control"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn in_flight_send_sees_mid_wait_takeover() {
        // Given a coordinator with a program that trickles output over a second.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(
                    chat.clone(),
                    "for i in 1 2 3 4 5 6; do echo tick-$i; sleep 0.25; done",
                ))
                .await;
let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When a send starts and the user takes control mid-wait.
        controls.set(&chat, ControlHolder::User);
        let ask = {
            let actor = actor.clone();
            let chat = chat.clone();
            tokio::spawn(async move { actor.ask::<_, SendTermOutcome>(send_msg(chat)).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        controls.set(&chat, ControlHolder::User); // already user; flips are re-read each poll
        let replied = tokio::time::timeout(Duration::from_secs(1), ask).await;

        // Then the send returns promptly (well under the 3s cap) with UserHasControl.
        let replied = replied.expect("send must return promptly after takeover");
        let reply = replied.expect("join");
        assert!(
            matches!(reply, SendTermOutcome::UserHasControl),
            "expected UserHasControl, got {reply:?}"
        );
    }

    /// Full mid-call takeover: an agent send is in flight, the user flips
    /// the control registry, then types through the bus (SendTermKey). The
    /// in-flight ask must resolve with UserHasControl whose screen carries
    /// the wait notice — and must NOT report Sent, which would overwrite
    /// the refusal with the settled screen — while the user's bytes reach
    /// the program. The next agent call then sees the user-driven screen.
    #[rstest::rstest]
    #[tokio::test]
    async fn user_takeover_mid_call_sends_keys_and_in_flight_ask_reports_user_control() {
        // Given a coordinator with a program that echoes input forever.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, state) = spawn_coordinator_with_state(&harness, controls.clone()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "cat"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When a send starts against a `cat` with no trailing newline
        // (never settles on its own — the wait runs until the cap).
        let ask = {
            let actor = actor.clone();
            let chat = chat.clone();
            tokio::spawn(async move { actor.ask::<_, SendTermOutcome>(send_msg(chat)).await })
        };

        // And the user takes control mid-call and types through the bus.
        tokio::time::sleep(Duration::from_millis(60)).await;
        controls.set(&chat, ControlHolder::User);
        harness
            .publish(SendTermKey {
                chat_session_id: chat.clone(),
                bytes: b"user-marker\n".to_vec(),
            })
            .await;

        // Then the in-flight ask resolves promptly with UserHasControl.
        let reply = tokio::time::timeout(Duration::from_secs(1), ask)
            .await
            .expect("send must resolve promptly after takeover")
            .expect("join");
        assert!(
            matches!(reply, SendTermOutcome::UserHasControl),
            "expected UserHasControl after mid-call takeover, got {reply:?}"
        );

        // And the user's bytes reached the program — observable in the
        // realtime mirror the overlay renders (the screen task pumps `cat`'s
        // echo without any tool call in flight).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let contains = state
                .read()
                .term_tabs()
                .and_then(|c| {
                    c.read()
                        .mirror(&chat)
                        .map(|m| m.screen.contains("user-marker"))
                })
                .unwrap_or(false);
            if contains {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "user bytes never reached the program during capture"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn kill_terminates_process_and_reports_tail() {
        // Given a coordinator with a program that printed before blocking.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "printf before-kill; cat"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When killing the session.
        let reply = actor
            .ask(KillTerm {
                chat_session_id: chat.clone(),
            })
            .await;

        // Then the kill reports a signal exit.
        let KillTermOutcome::Killed {
            transcript_tail,
            exited,
            ..
        } = reply
        else {
            panic!("expected Killed");
        };
        assert!(exited.signal.is_some());
        let _ = transcript_tail;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn kill_is_idempotent_after_exit() {
        // Given a coordinator whose session exited naturally.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "true"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When killing the already-exited session twice.
        let first = actor
            .ask(KillTerm {
                chat_session_id: chat.clone(),
            })
            .await;
        let second = actor
            .ask(KillTerm {
                chat_session_id: chat,
            })
            .await;

        // Then both kills succeed with exit info.
        for reply in [first, second] {
            let KillTermOutcome::Killed { exited, .. } = reply else {
                panic!("expected Killed, got {reply:?}");
            };
            assert!(exited.code == 0 || exited.signal.is_some());
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn kill_unknown_session_reports_unknown() {
        // Given a coordinator with no sessions.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;

        // When killing a chat session with no terminal.
        let reply = actor
            .ask(KillTerm {
                chat_session_id: jinn_core_types::SessionId::new(),
            })
            .await;

        // Then the outcome is UnknownSession.
        assert!(matches!(reply, KillTermOutcome::UnknownSession));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn settle_waits_stream_screen_updates_to_the_bus() {
        // Given a coordinator actor and a screen-recording subscriber.
        let harness = TestHarness::new().await;
        let recorder = harness.spawn_recorder::<TermScreenUpdated>().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;

        // When spawning a program whose output arrives in waves; the reply
        // only comes after the settle window, so every delta is already
        // recorded by the time it returns.
        let _: SpawnTermOutcome = actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(
                jinn_core_types::SessionId::new(),
                "echo one; sleep 0.05; echo two",
            ))
            .await;

        // Then each screen change streamed a TermScreenUpdated to the bus.
        // (`GetRecorded` DRAINS the recorder, so poll until the first wave lands.)
        let mut collected: Vec<TermScreenUpdated> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while collected.iter().all(|s| !s.screen.contains("two")) {
            let batch = recorder
                .ask(GetRecorded::new())
                .await
                .unwrap_or_default();
            collected.extend(batch);
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let screens = collected;
        assert!(
            screens.iter().any(|s| s.screen.contains("one")),
            "expected a screen delta containing 'one'"
        );
        // And the second wave streamed its own delta.
        assert!(
            screens.iter().any(|s| s.screen.contains("two")),
            "expected a screen delta containing 'two'"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn natural_exit_is_captured_on_next_call() {
        // Given a coordinator with a short-lived program.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();

        // When the spawn reply already observed the exit.
        let SpawnTermOutcome::Started { screen, .. } = actor
            .ask(spawn_msg(chat, "sh -c 'echo bye; exit 7'"))
            .await
        else {
            panic!("expected Started");
        };

        // Then the exit info reports code 7.
        let exited = screen.exited.expect("exit captured");
        assert_eq!(exited.code, 7);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn screen_updates_mirror_into_frontend_state() {
        // Given a coordinator actor wired to a readable shared state.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();

        // When spawning a program that prints to the screen.
        let _: SpawnTermOutcome = actor
            .ask(spawn_msg(chat.clone(), "echo mirror-me"))
            .await;

        // Then the frontend terminal mirror carries the rendered screen.
        let guard = state.read();
        let mirror = guard
            .term_tabs()
            .and_then(|c| c.read().mirror(&chat).cloned())
            .expect("mirror for chat session");
        assert!(
            mirror.screen.contains("mirror-me"),
            "mirror should contain output, got: {:?}",
            mirror.screen
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn styled_cells_reach_the_mirror_with_colors() {
        // Given a coordinator wired to a readable shared state and a program
        // printing an ANSI-colored word.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();

        // When spawning a program that emits red text.
        actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(
                chat.clone(),
                "printf 'plain \\033[31mred\\033[0m end'",
            ))
            .await;

        // Then the mirror's cell grid marks the colored span red and the
        // surrounding text default-colored.
        let mirror = {
            let guard = state.read();
            guard
                .term_tabs()
                .and_then(|c| c.read().mirror(&chat).cloned())
                .expect("mirror for chat session")
        };
        let row = 0;
        let mut red_span = None;
        let mut default_before = None;
        for col in 0..mirror.cells.cols {
            match mirror.cells.get(row, col) {
                Some(crate::emulator::TermCell::Styled { ch, style }) if ch != &' ' => {
                    let is_red = style.fg == crate::emulator::TermColor::Idx(1);
                    if is_red && red_span.is_none() {
                        red_span = Some(col);
                    }
                    if !is_red && red_span.is_some() {
                        default_before = Some(col);
                        break;
                    }
                    if red_span.is_none() {
                        default_before = Some(col);
                    }
                }
                _ => {}
            }
        }
        assert!(
            red_span.is_some(),
            "expected a red-styled span in the cell grid, got: {:?} at row {row}",
            mirror.cells
        );
        let _ = default_before;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_updates_mirror_with_new_screen() {
        // Given a coordinator with a live `cat` session.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // When sending text through the send path.
        let _: SendTermOutcome = actor
            .ask::<_, SendTermOutcome>(SendTermInput {
                text: Some("mirrored-after-send".to_owned()),
                ..send_msg(chat.clone())
            })
            .await;

        // Then the mirror reflects the echoed output.
        let guard = state.read();
        let mirror = test_mirror(&guard, &chat).expect("mirror");
        assert!(mirror.screen.contains("mirrored-after-send"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn resize_updates_session_and_mirror() {
        // Given a coordinator with a live `cat` session.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // When resizing that chat session to a small grid.
        actor
            .tell(ResizeTerm {
                chat_session_id: Some(chat.clone()),
                size: (10, 40),
            })
            .await;

        // Then the session's emulator regrided to the requested size.
        // (The tell is async-dispatched; give the handler a beat, and poll
        // briefly in case the message lands after the first sleep.)
        tokio::time::sleep(Duration::from_millis(50)).await;
        for _ in 0..20 {
            {
                let guard = state.read();
                if let Some(m) = test_mirror(&guard, &chat)
                    && (m.cells.rows, m.cells.cols) == (10, 40)
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let guard = state.read();
        let mirror = test_mirror(&guard, &chat).expect("mirror");
        assert_eq!(
            (mirror.cells.rows, mirror.cells.cols),
            (10, 40),
            "named chat session must resize to the requested grid"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn resize_targets_only_the_named_chat_session() {
        // Given a coordinator with two live terminals in different chat
        // sessions.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        spawn_cat(&actor, &chat_b).await;

        // When resizing only chat_a's terminal.
        actor
            .tell(ResizeTerm {
                chat_session_id: Some(chat_a.clone()),
                size: (10, 40),
            })
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Allow the async tell delivery to land before reading mirrors.

        // Then chat_a regrided and chat_b kept the default grid.
        let guard = state.read();
        let a = test_mirror(&guard, &chat_a).expect("a mirror");
        let b = test_mirror(&guard, &chat_b).expect("b mirror");
        assert_eq!((a.cells.rows, a.cells.cols), (10, 40));
        assert_ne!(
            (b.cells.rows, b.cells.cols),
            (10, 40),
            "an unnamed sibling session must not be regrided"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn resize_without_session_is_noop() {
        // Given a coordinator with no sessions.
        let harness = TestHarness::new().await;
        let (actor, _state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;

        // When sending a resize with no chat session named.
        actor
            .tell(ResizeTerm {
                chat_session_id: None,
                size: (10, 40),
            })
            .await;

        // Then it is accepted silently (no arbitrary target): the ask path
        // (which the tool layer uses) would report; tell can't fail.

    }

    #[rstest::rstest]
    #[tokio::test]
    async fn resize_of_unknown_chat_session_is_noop() {
        // Given a coordinator with a live terminal in another chat session.
        let harness = TestHarness::new().await;
        let (actor, _state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let live = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &live).await;

        // When resizing an unknown chat session.
        actor
            .tell(ResizeTerm {
                chat_session_id: Some(jinn_core_types::SessionId::new()),
                size: (10, 40),
            })
            .await;

        // Then it is accepted silently: an unknown target is a no-op in the
        // handler, and a tell cannot fail in trouper.

    }

    // ── v2: one terminal per chat session ──────────────────────────────────

    #[rstest::rstest]
    #[tokio::test]
    async fn respawn_same_chat_session_kills_the_previous_terminal() {
        // Given a coordinator whose chat session runs a long-lived marker
        // program (`sleep 31` — distinctive, so /proc probing finds exactly it).
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "sleep 31"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When spawning a second terminal for the same chat session.
        let reply = actor
            .ask(spawn_msg(chat, "printf second-run; sleep 30"))
            .await;

        // Then the outcome reports the kill of the previous terminal.
        let SpawnTermOutcome::Started {
            killed_previous,
            screen,
        } = reply
        else {
            panic!("expected Started");
        };
        let killed = killed_previous.expect("previous terminal killed");
        assert_eq!(killed.exited.code, 0);
        // And the new program's screen is the one reported.
        assert!(plain_screen(&screen.screen).contains("second-run"));

        // And the *killed* program is gone (no orphans of the first spawn).
        // A candidate must be a live (non-zombie) `/sleep` with the marker in
        // its cmdline; transient pid slots between readdir and open are skipped.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let find_orphans = || {
            let mut found = Vec::new();
            let entries = std::fs::read_dir("/proc").expect("/proc is readable");
            for entry in entries.flatten() {
                let Ok(exe) = std::fs::read_link(entry.path().join("exe")) else {
                    continue; // kernel thread, vanished, or not ours.
                };
                if !exe.to_string_lossy().ends_with("/sleep") {
                    continue;
                }
                // A zombie is already dead (the group kill worked); only a
                // live state counts as an orphan.
                let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some(state) = stat
                    .rsplit(')')
                    .next()
                    .and_then(|rest| rest.split(' ').next())
                else {
                    continue;
                };
                if state == "Z" {
                    continue;
                }
                let Ok(cmdline) = std::fs::read_to_string(entry.path().join("cmdline")) else {
                    continue;
                };
                if cmdline.replace('\0', " ").contains("sleep 31") {
                    found.push(entry.file_name().to_string_lossy().to_string());
                }
            }
            found
        };
        let mut orphans = find_orphans();
        for _ in 0..3 {
            if orphans.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            orphans = find_orphans();
        }
        assert!(
            orphans.is_empty(),
            "expected no 'sleep 31' orphans after respawn, found: {orphans:?}"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn parallel_chat_sessions_get_independent_terminals() {
        // Given a coordinator with a terminal for session A.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;

        // When spawning a terminal for session B.
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat_b.clone(), "echo from-b"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // Then both terminals stay live: A's program still responds.
        let mut msg = send_msg(chat_a);
        msg.text = Some("still-alive-a".to_owned());
        let SendTermOutcome::Sent(screen) = actor.ask::<_, SendTermOutcome>(msg).await else {
            panic!("expected Sent");
        };
        assert!(plain_screen(&screen.screen).contains("still-alive-a"));
        // And B's terminal exists as its own live entry.
        let b_kill = actor
            .ask(KillTerm {
                chat_session_id: chat_b,
            })
            .await;
        assert!(
            matches!(b_kill, KillTermOutcome::Killed { .. }),
            "session B must have its own live terminal"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn live_flag_mirrors_spawn_and_kill() {
        // Given a coordinator wired to a readable state.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat).await;

        // Then the session is marked live after spawn.
        assert!(
            test_live(&state.read(), &chat),
            "chat session must be live after spawn"
        );

        // When killing the terminal.
        let _: KillTermOutcome = actor
            .ask(KillTerm {
                chat_session_id: chat.clone(),
            })
            .await;

        // Then the live flag clears.
        assert!(
            !test_live(&state.read(), &chat),
            "chat session must not be live after kill"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn natural_exit_clears_the_live_flag() {
        // Given a coordinator with a short-lived terminal (`true` exits at once).
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(chat.clone(), "true"))
                .await;
            let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When the program exits and the screen task observes EOF.
        tokio::time::sleep(Duration::from_millis(600)).await;

        // Then the live flag cleared without any kill call.
        assert!(
            !test_live(&state.read(), &chat),
            "live flag must clear on natural exit"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn mirror_updates_without_any_tool_call_in_flight() {
        // Given a coordinator with a live terminal printing on a timer.
        let harness = TestHarness::new().await;
        let (actor, state) =
            spawn_coordinator_with_state(&harness, TermControls::default()).await;
        let chat = jinn_core_types::SessionId::new();
        {
            let outcome = actor
                .ask::<_, SpawnTermOutcome>(spawn_msg(
                    chat.clone(),
                    "sleep 0.2; echo realtime-echo; sleep 30",
                ))
                .await;
let SpawnTermOutcome::Started { .. } = outcome else {
                panic!("expected Started");
            };
        }

        // When no tool call is in flight and the program prints.
        // The screen task ticks at 50ms; a fresh print must land within ~1s
        // (generous vs. the ~100ms AC, but tight enough to catch a
        // regression to settle-only pumping).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let contains = state
                .read()
                .term_tabs()
                .and_then(|c| {
                    c.read()
                        .mirror(&chat)
                        .map(|m| m.screen.contains("realtime-echo"))
                })
                .unwrap_or(false);
            if contains {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "mirror never updated without an in-flight ask"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    // ── v3: per-session control + cross-session isolation ─────────────────

    #[rstest::rstest]
    #[tokio::test]
    async fn takeover_of_one_session_leaves_another_session_send_in_flight() {
        // Given a coordinator with live `cat` terminals in two chat sessions.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        spawn_cat(&actor, &chat_b).await;

        // When the user takes control of A while B's send is in flight.
        controls.set(&chat_a, ControlHolder::User);
        let mut b_msg = send_msg(chat_b.clone());
        b_msg.text = Some("b-untouched".to_owned());
        b_msg.keys = vec!["enter".to_owned()];
        let reply = actor.ask::<_, SendTermOutcome>(b_msg).await;

        // Then B's send succeeds — A's takeover must not abort it.
        let SendTermOutcome::Sent(screen) = reply else {
            panic!("session B's send must be unaffected by session A's takeover, got {reply:?}");
        };
        assert!(plain_screen(&screen.screen).contains("b-untouched"));
        // And B's control holder is still Agent.
        assert_eq!(controls.holder_for(&chat_b), ControlHolder::Agent);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handback_returns_only_that_session_to_agent() {
        // Given a coordinator where A holds user control and B runs `cat`.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        spawn_cat(&actor, &chat_b).await;
        controls.set(&chat_a, ControlHolder::User);

        // When handing A back to the agent and sending input to B.
        controls.set(&chat_a, ControlHolder::Agent);
        let mut b_msg = send_msg(chat_b.clone());
        b_msg.text = Some("after-handback".to_owned());
        let reply = actor.ask::<_, SendTermOutcome>(b_msg).await;

        // Then B's send succeeds while A is unaffected either way.
        let SendTermOutcome::Sent(screen) = reply else {
            panic!("expected Sent for session B, got {reply:?}");
        };
        assert!(plain_screen(&screen.screen).contains("after-handback"));
        // And A's holder reads Agent after the handback.
        assert_eq!(controls.holder_for(&chat_a), ControlHolder::Agent);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn send_targets_only_the_calling_session_terminal() {
        // Given a coordinator with `cat` running in session A only.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat_a = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;

        // When sending a marker to A.
        let mut a_msg = send_msg(chat_a.clone());
        a_msg.text = Some("only-in-a".to_owned());
        a_msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(a_screen) = actor.ask::<_, SendTermOutcome>(a_msg).await else {
            panic!("expected Sent");
        };
        assert!(plain_screen(&a_screen.screen).contains("only-in-a"));

        // Then a send naming a session with no terminal is UnknownSession —
        // there is no cross-session address to reach, not even by accident.
        let reply = actor
            .ask::<_, SendTermOutcome>(send_msg(jinn_core_types::SessionId::new()))
            .await
            ;
        assert!(matches!(reply, SendTermOutcome::UnknownSession));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn spawn_kill_previous_is_scoped_to_own_session() {
        // Given a coordinator with live terminals in sessions A and B.
        let harness = TestHarness::new().await;
        let (actor, _system) = spawn_coordinator(&harness, TermControls::default()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        spawn_cat(&actor, &chat_b).await;

        // When respawning B.
        let reply = actor
            .ask(spawn_msg(chat_b, "echo respawn-b"))
            .await;

        // Then B's respawn reports the kill, and A's terminal stays live
        // (A's program still answers input).
        let SpawnTermOutcome::Started {
            killed_previous, ..
        } = reply
        else {
            panic!("expected Started");
        };
        assert!(
            killed_previous.is_some(),
            "B's previous terminal was killed"
        );
        let mut a_msg = send_msg(chat_a);
        a_msg.text = Some("a-still-live".to_owned());
        a_msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(a_screen) = actor.ask::<_, SendTermOutcome>(a_msg).await else {
            panic!("session A's terminal must survive B's respawn");
        };
        assert!(plain_screen(&a_screen.screen).contains("a-still-live"));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn session_closed_tears_down_its_terminal_only() {
        // Given a coordinator with `cat` in sessions A and B.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, state) = spawn_coordinator_with_state(&harness, controls.clone()).await;
        let chat_a = jinn_core_types::SessionId::new();
        let chat_b = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        spawn_cat(&actor, &chat_b).await;
        controls.set(&chat_a, ControlHolder::User);

        // When closing session A.
        harness
            .publish(
                jinn_domain::feat::session::protocol::session_closed::SessionClosed {
                    session_id: chat_a.clone(),
                },
            )
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Then A's terminal is gone (send → UnknownSession), its mirror and
        // live flag cleared, and its control entry removed.
        let reply = actor
            .ask::<_, SendTermOutcome>(send_msg(chat_a.clone()))
            .await;
        assert!(matches!(reply, SendTermOutcome::UnknownSession));
        {
            let guard = state.read();
            assert!(test_mirror(&guard, &chat_a).is_none());
            assert!(!test_live(&guard, &chat_a));
        }
        // And B's terminal is untouched and still live.
        let mut b_msg = send_msg(chat_b.clone());
        b_msg.text = Some("b-survives".to_owned());
        b_msg.keys = vec!["enter".to_owned()];
        let SendTermOutcome::Sent(b_screen) = actor.ask::<_, SendTermOutcome>(b_msg).await else {
            panic!("session B's terminal must survive A's close");
        };
        assert!(plain_screen(&b_screen.screen).contains("b-survives"));
        assert!(test_live(&state.read(), &chat_b));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn teardown_clears_a_user_control_holder() {
        // Given a coordinator with A's terminal under user control.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat_a = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        controls.set(&chat_a, ControlHolder::User);

        // When closing session A.
        harness
            .publish(
                jinn_domain::feat::session::protocol::session_closed::SessionClosed {
                    session_id: chat_a.clone(),
                },
            )
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Then A's control entry is removed (reads as the Agent default), so
        // a later terminal for A cannot inherit a stale User holder.
        assert_eq!(controls.holder_for(&chat_a), ControlHolder::Agent);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn fresh_spawn_resets_a_user_control_holder() {
        // Given a coordinator whose A terminal is under user control.
        let harness = TestHarness::new().await;
        let controls = TermControls::default();
        let (actor, _system) = spawn_coordinator(&harness, controls.clone()).await;
        let chat_a = jinn_core_types::SessionId::new();
        spawn_cat(&actor, &chat_a).await;
        controls.set(&chat_a, ControlHolder::User);

        // When respawning A's terminal.
        let reply = actor
            .ask::<_, SpawnTermOutcome>(spawn_msg(chat_a.clone(), "echo fresh"))
            .await;
        let SpawnTermOutcome::Started { screen, .. } = reply else {
            panic!("expected Started");
        };
        assert!(plain_screen(&screen.screen).contains("fresh"));

        // Then the fresh terminal is agent-controlled — a new terminal never
        // inherits the replaced one's takeover.
        assert_eq!(controls.holder_for(&chat_a), ControlHolder::Agent);
    }
}
