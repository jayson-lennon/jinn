//! Terminal setup, event loop, and teardown.
//!
//! Sets up the terminal (raw mode + alternate screen), runs the
//! main event loop, and restores the terminal on exit. Also manages
//! the background event thread lifecycle, stopping it before terminal
//! suspension and restarting it afterward.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use error_stack::{Report, ResultExt as _};
use jinn_chat_log_view_msg::GraphicsState;
use jinn_preferences_config::schemas::CwdSelectorConfig;
use ratatui::{Terminal, backend::CrosstermBackend};
use wherror::Error;

use crate::TuiApp;
use crate::app::scope_for_focus;
use crate::msg::Msg;
use marked_path::{Absolute, MarkedPathAccess, MarkedPathBuf};

/// Error type for TUI run operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(debug)]
pub struct TuiRunError;

/// Runs the TUI application.
///
/// Sets up the terminal, runs the main event loop, and restores
/// the terminal on exit. The caller must provide a fully-initialized
/// [`TuiApp`] with services already set.
///
/// # Errors
///
/// Returns an error if terminal setup, the event loop, or teardown fails.
pub fn run(mut app: TuiApp) -> Result<(), Report<TuiRunError>> {
    let mut stdout = io::stdout();
    enable_raw_mode()
        .change_context(TuiRunError)
        .attach("failed to enable raw mode")?;
    execute!(stdout, EnterAlternateScreen)
        .change_context(TuiRunError)
        .attach("failed to enter alternate screen")?;

    execute!(stdout, EnableBracketedPaste)
        .change_context(TuiRunError)
        .attach("failed to enable bracketed paste")?;

    let mouse_selection = app.config.mouse_selection;

    // Enable mouse capture so scroll wheel and click events are reported.
    if mouse_selection {
        execute!(stdout, EnableMouseCapture)
            .change_context(TuiRunError)
            .attach("failed to enable mouse capture")?;
    }

    // Enable the Kitty keyboard protocol so crossterm can distinguish
    // modified special keys (e.g. Shift+Enter, Ctrl+Enter). Terminals that
    // don't support it silently ignore the sequence; Windows is a no-op
    // because its console input carries modifier state natively.
    crate::terminal::enable_keyboard_enhancement(&mut stdout)
        .change_context(TuiRunError)
        .attach("failed to push keyboard enhancement flags")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .change_context(TuiRunError)
        .attach("failed to create terminal")?;

    // Query the terminal for graphics capability before the event thread
    // starts: the query reads stdin, and the poll thread would race it.
    // Runs once per launch (a mid-session terminal swap needs a restart to
    // re-detect); the result lands in the chat-log graphics cell, where
    // `None` means every image renders as today's text placeholder.
    install_graphics_picker(&app);

    // Start the event poll thread (independent of tokio runtime).
    app.event_thread = Some(app.events.start_event_thread());

    let result = run_main_loop(&mut terminal, &mut app);

    // Clean up event thread.
    if let Some(mut guard) = app.event_thread.take() {
        guard.stop();
    }

    // Coordinated actor shutdown: signal the root supervisor, then race the
    // shutdown barrier against a 20-second timeout. The trouper sweep
    // fires every supervised child's shutdown watcher, letting each loop
    // drain, run its `on_stop` hook (flush buffers, finalize writes), and
    // tear down — hard-stopping stragglers at its 10s deadline.
    {
        let system = app.services.trouper_system.clone();
        let result = app.services.handle.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(20),
                system.shutdown_graceful(Duration::from_secs(10)),
            )
            .await
        });
        if result.is_err() {
            tracing::warn!("actor shutdown timed out after 20s; proceeding");
        }
    }

    // Restore terminal.
    if let Err(e) = crate::terminal::disable_keyboard_enhancement(terminal.backend_mut()) {
        tracing::error!(err = ?e, "failed to pop keyboard enhancement flags");
    }
    if let Err(e) = execute!(terminal.backend_mut(), DisableBracketedPaste) {
        tracing::error!(err = ?e, "failed to disable bracketed paste");
    }
    if mouse_selection && let Err(e) = execute!(terminal.backend_mut(), DisableMouseCapture) {
        tracing::error!(err = ?e, "failed to disable mouse capture");
    }
    if let Err(e) = disable_raw_mode() {
        tracing::error!(err = ?e, "failed to disable raw mode");
    }
    if let Err(e) = execute!(terminal.backend_mut(), LeaveAlternateScreen) {
        tracing::error!(err = ?e, "failed to leave alternate screen");
    }
    if let Err(e) = terminal.show_cursor() {
        tracing::error!(err = ?e, "failed to show cursor");
    }

    result
}

/// Upper bound on the graphics capability query, wall-clock.
///
/// `Picker::from_query_stdio` has its own internal read timeout, but an
/// unresponsive terminal must never hold startup hostage beyond this
/// window: the query runs on a helper thread and this run races it. On
/// expiry (or any error) the picker stays `None` and images degrade to
/// text placeholders.
#[cfg(not(windows))]
const GRAPHICS_QUERY_BUDGET: Duration = Duration::from_millis(2500);

/// Queries the terminal for graphics capability and installs the resulting
/// picker into the chat-log graphics cell.
///
/// Best-effort by design: an absent cell (test harnesses without the slice
/// activated) or a failed query both leave graphics off, which is today's
/// rendering. Windows is skipped entirely — ConPTY does not pass the
/// graphics escape sequences through, so detection cannot succeed.
fn install_graphics_picker(app: &TuiApp) {
    #[cfg(not(windows))]
    {
        let picker = query_graphics_within_budget().map_or_else(
            || {
                tracing::info!("terminal graphics unavailable; images stay textual");
                None
            },
            Some,
        );
        write_graphics_picker(app, picker);
    }

    #[cfg(windows)]
    {
        // ConPTY does not pass the graphics sequences through, so detection
        // cannot succeed; recording `None` keeps the cell's story explicit.
        write_graphics_picker(app, None);
        tracing::info!("graphics rendering unavailable on Windows; images stay textual");
    }
}

/// Runs the capability query on a helper thread, bounded by
/// [`GRAPHICS_QUERY_BUDGET`].
///
/// The crate's own stdio read has a 2s timeout, but a terminal that stalls
/// mid-answer could still outlast it; this wait bounds the total. `None`
/// on expiry, on a query error, or if the helper panicked.
#[cfg(not(windows))]
fn query_graphics_within_budget() -> Option<ratatui_image::picker::Picker> {
    let query = std::thread::spawn(ratatui_image::picker::Picker::from_query_stdio);
    if !wait_until_finished(&query, GRAPHICS_QUERY_BUDGET) {
        tracing::info!(
            budget_ms = GRAPHICS_QUERY_BUDGET.as_millis() as u64,
            "terminal graphics query timed out"
        );
        return None;
    }
    match query.join() {
        Ok(Ok(picker)) => {
            tracing::info!(
                protocol = ?picker.protocol_type(),
                font_size = ?picker.font_size(),
                "terminal graphics capability detected"
            );
            Some(picker)
        }
        Ok(Err(err)) => {
            tracing::info!(err = ?err, "terminal graphics query failed");
            None
        }
        Err(_) => {
            tracing::info!("terminal graphics query panicked");
            None
        }
    }
}

/// Polls `handle` until it finishes or `budget` elapses; `true` when it
/// finished in time.
#[cfg(not(windows))]
fn wait_until_finished<T>(handle: &std::thread::JoinHandle<T>, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// Writes the picker into the graphics cell, if the cell exists.
fn write_graphics_picker(app: &TuiApp, picker: Option<ratatui_image::picker::Picker>) {
    let Some(cell) = app.core.state.read().frontend.graphics_cell() else {
        tracing::debug!("no graphics cell; chat-log images stay textual");
        return;
    };
    cell.update(|graphics| graphics.set_picker(picker));
}

/// Marks the chat-log graphics state dirty after a suspend.
///
/// Leaving the alternate screen wipes the terminal's graphics state — kitty
/// discards transmitted images when the alt screen exits — so every
/// rendered image must re-transmit on the next frame. Best-effort: a
/// missing cell simply means nothing to mark.
fn mark_graphics_dirty(app: &TuiApp) {
    if let Some(cell) = app.core.state.read().frontend.graphics_cell() {
        cell.update(GraphicsState::mark_dirty);
    }
}

/// Runs the main TUI event loop - receives events, processes state, and renders frames.
fn run_main_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> Result<(), Report<TuiRunError>> {
    let mut last_render: Option<Instant> = None;

    loop {
        // ── Phase 1: Wait for next message ──────────────────────────────
        let event = app
            .events
            .recv()
            .change_context(TuiRunError)
            .attach("event channel closed")?;

        let is_input = matches!(event, Msg::Input(crossterm::event::Event::Key(_)));

        // ── Phase 2: Handle event batch ─────────────────────────────────
        app.handle_msg(event);
        while let Some(event) = app.events.try_recv() {
            app.handle_msg(event);
        }

        // ── Phase 3: State read for quit/scope ──────────────────────────
        let state_read = app.core.state.read();
        let should_quit = state_read.frontend.quit();
        let scope = scope_for_focus(&state_read.frontend.scope());
        drop(state_read);
        app.which_key.set_scope(scope);

        // Check for pending suspend after event batch processing.
        if let Some(action) = app.suspend.take_action() {
            handle_suspend_action(terminal, app, action)?;
        }

        // ── Phase 4: Render ─────────────────────────────────────────────
        let should_render =
            is_input || last_render.is_none_or(|t| t.elapsed() >= Duration::from_millis(33));

        if should_render {
            terminal
                .draw(|frame| {
                    app.render(frame);
                })
                .change_context(TuiRunError)
                .attach("failed to draw frame")?;
            last_render = Some(Instant::now());
        }

        if should_quit {
            break;
        }
    }

    Ok(())
}

/// Result of a suspend/restore cycle.
enum SuspendResult {
    /// The edited content, to be written to the input buffer.
    EditContent(Option<String>),
    /// The selected directory path, to be set as session CWD.
    ChangeCwd(Option<MarkedPathBuf<Absolute>>),
}

/// Executes a suspend/restore cycle for the given action.
///
/// 1. Stops the background event thread
/// 2. Drains stale messages from the channel
/// 3. Suspends the terminal via [`TerminalGuard`](crate::terminal::TerminalGuard)
/// 4. Runs the external editor via `dialoguer::Editor`
/// 5. Invokes the `on_result` closure to produce the new input buffer content
/// 6. Restarts the event thread
/// 7. Redraws the terminal
/// 8. Writes the result directly to the active session's input box via `replace_all`
///
/// The CWD-selector expect cannot fail at runtime: the selector lists paths
/// under the session's search root, which is already typed `Absolute`, so
/// every printed line is absolute; anything else is a selector bug, and a
/// selected non-directory is still rejected below.
#[expect(
    clippy::expect_used,
    reason = "cwd selector prints paths under a typed-absolute search root"
)]
fn handle_suspend_action(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
    action: crate::suspend::SuspendAction,
) -> Result<(), Report<TuiRunError>> {
    // Stop the event thread so crossterm stops polling the terminal.
    if let Some(mut guard) = app.event_thread.take() {
        guard.stop();
    }
    app.events.drain();

    let result = crate::terminal::suspend_and_run(terminal, || match action {
        crate::suspend::SuspendAction::Edit {
            initial_content,
            on_result,
        } => {
            let edited = dialoguer::Editor::new()
                .edit(&initial_content)
                .ok()
                .flatten();

            let changed = edited.filter(|c| c != &initial_content);
            SuspendResult::EditContent(on_result(changed))
        }
        crate::suspend::SuspendAction::ChangeCwd { search_root } => {
            let command_template = app
                .services
                .config
                .read::<CwdSelectorConfig>()
                .command
                .clone();

            // Shell-escape the path for safe substitution.
            let escaped_path = shell_escape(&search_root.as_path().to_string_lossy());
            let rendered = command_template.replace("{path}", &escaped_path);

            let output_result = std::process::Command::new("sh")
                .arg("-c")
                .arg(&rendered)
                .current_dir(&search_root)
                .output();

            let selected = match output_result {
                Ok(output) => {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        stdout
                            .lines()
                            .next()
                            .filter(|line| !line.is_empty())
                            .map(|line| {
                                MarkedPathBuf::<Absolute>::new(line.trim())
                                    .expect("selector line checked absolute below")
                            })
                    } else {
                        // Non-zero exit = user cancelled (ESC in fzf) or error.
                        // Log stderr as debug - user cancelled is not an error.
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if !stderr.is_empty() {
                            tracing::debug!(
                                stderr = %stderr,
                                exit_code = output.status.code().unwrap_or(-1),
                                "CWD selector exited with error"
                            );
                        }
                        None
                    }
                }
                Err(e) => {
                    tracing::error!(err = %e, "failed to run CWD selector command");
                    None
                }
            };
            SuspendResult::ChangeCwd(selected)
        }
    })
    .change_context(TuiRunError)
    .attach("failed to suspend terminal for editor")?;

    // The alternate screen exited and re-entered: the terminal's graphics
    // state is gone, so every image must re-transmit on the next frame.
    mark_graphics_dirty(app);

    // Restart the event poll thread with a fresh crossterm state.
    app.event_thread = Some(app.events.start_event_thread());

    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .change_context(TuiRunError)
        .attach("failed to redraw after suspend")?;

    // Handle the suspend result.
    match result {
        SuspendResult::EditContent(content) => {
            if let Some(content) = content {
                app.core
                    .state
                    .write()
                    .active_session()
                    .update_input(|i| i.replace_all(content));
            }
        }
        SuspendResult::ChangeCwd(path) => {
            if let Some(path) = path {
                let session_id = app.core.state.read().active_session().session_id().clone();
                if apply_selected_cwd(&app.core.bridge, session_id, &path) {
                    tracing::info!(
                        cwd = %app.core.state.read().active_session().cwd(),
                        "session CWD updated"
                    );
                }
            }
        }
    }

    Ok(())
}

/// Shell-escapes a path for safe substitution into a `sh -c` command.
///
/// Wraps in single quotes, escaping any embedded single quotes.
fn shell_escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 2);
    result.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            result.push_str("'\\''");
        } else {
            result.push(ch);
        }
    }
    result.push('\'');
    result
}

/// Validate, canonicalize, and publish [`SetSessionCwd`] for a selected path.
///
/// Used by the `<M-c>`/`<M-d>` suspend-and-`fzf` flow after the user picks a
/// directory. Returns `true` if a `SetSessionCwd` command was published onto
/// `bridge`; `false` if the path was rejected (non-directory or
/// canonicalization failure).
///
/// Routing through the [`Bridge`] is what makes the selection actually
/// reach the session actor.
fn apply_selected_cwd(
    bridge: &jinn_kernel::common::bridge::Bridge,
    session_id: jinn_core_types::SessionId,
    path: &MarkedPathBuf<Absolute>,
) -> bool {
    if !path.is_dir() {
        tracing::warn!(
            path = %path,
            "CWD selector returned non-directory path, ignoring"
        );
        return false;
    }
    let canonical = match jinn_common::fs_paths::canonicalize_display(path) {
        Ok(canonical) => canonical,
        Err(e) => {
            tracing::warn!(
                path = %path,
                err = %e,
                "failed to canonicalize CWD selector result"
            );
            return false;
        }
    };
    let _ = bridge.send(jinn_kernel::Bridge::publish_closure(
        jinn_session_lifecycle_msg::SetSessionCwd {
            session_id,
            cwd: canonical,
        },
    ));
    true
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use jinn_chat_log_view_msg::GraphicsState;
    use jinn_kernel::AppState;
    use jinn_kernel::common::bridge::Bridge;
    use jinn_kernel::protocol::ChatEntryId;
    use jinn_session_lifecycle_msg::SetSessionCwd;
    use jinn_testutil::barrier::Barrier;
    use std::sync::{Arc, Mutex};

    /// A trouper service actor that records decoded `SetSessionCwd`
    /// deliveries into a shared buffer, so bridge-driven publishes are
    /// observable in a unit test.
    ///
    /// It also acknowledges a [`Barrier`], which is what lets a test prove
    /// that a `SetSessionCwd` was *not* published. The recorder is the
    /// only actor on this bus, so it is also the actor that would have
    /// received the message — and because the barrier is published after
    /// the message under test, its acknowledgement proves the recorder
    /// has already drained everything published before it. A test that
    /// slept for a fixed interval and then asserted the buffer was empty
    /// would instead be betting that the sleep outlasted the dispatch,
    /// which fails in the direction that makes the assertion pass for
    /// the wrong reason.
    struct CwdRecorder {
        buffer: Arc<Mutex<Vec<SetSessionCwd>>>,
        acks: Arc<Mutex<Vec<String>>>,
    }

    impl trouper::actor::ServiceActor for CwdRecorder {
        #[expect(
            clippy::unreachable,
            reason = "trouper ServiceActor contract: this actor is spawned via init_with; JSON-arg init is never called"
        )]
        async fn init(
            _args: &trouper::json::Json,
        ) -> Result<Self, error_stack::Report<trouper::registry::RegistryError>> {
            unreachable!("spawned via init_with");
        }
    }

    impl trouper::actor::MsgHandler<SetSessionCwd> for CwdRecorder {
        async fn handle(&mut self, msg: &SetSessionCwd, _ctx: &mut trouper::context::MsgCtx<'_>) {
            self.buffer.lock().unwrap().push(msg.clone());
        }
    }

    impl trouper::actor::MsgHandler<Barrier> for CwdRecorder {
        async fn handle(&mut self, msg: &Barrier, _ctx: &mut trouper::context::MsgCtx<'_>) {
            self.acks.lock().unwrap().push(msg.label.clone());
        }
    }

    /// A single-threaded tokio runtime for driving async bridge delivery.
    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// The handles a spawned cwd recorder exposes: the bus, the bridge that
    /// drains to it, the recorded `SetSessionCwd` deliveries, and the
    /// barrier acknowledgements.
    type CwdRecorderHandles = (
        jinn_kernel::common::services::bus_service::BusService,
        Bridge,
        Arc<Mutex<Vec<SetSessionCwd>>>,
        Arc<Mutex<Vec<String>>>,
    );

    /// Spawns a trouper-backed bus with a subscribed `CwdRecorder` at a
    /// unique path, plus a `Bridge` draining to that bus. Returns everything
    /// the cwd tests need.
    ///
    /// The returned `acks` buffer receives the recorder's barrier
    /// acknowledgements, so a test can wait for the recorder to have
    /// drained everything published before the barrier.
    fn spawn_system_with_recorder(handle: &tokio::runtime::Handle) -> CwdRecorderHandles {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
        let bus =
            jinn_kernel::common::services::bus_service::BusService::new_trouper(system.clone());
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let acks = Arc::new(Mutex::new(Vec::new()));
        let path = trouper::actor::ActorPath::new(format!(
            "test.cwd-recorder.{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        trouper::builder::spawn_service_builder::<CwdRecorder>(&system)
            .at(path.clone())
            .init_with({
                let buffer = buffer.clone();
                let acks = acks.clone();
                move || {
                    let buffer = buffer.clone();
                    let acks = acks.clone();
                    Box::pin(async move { Ok(CwdRecorder { buffer, acks }) })
                }
            })
            .handles::<SetSessionCwd>()
            .handles::<Barrier>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        let bridge = Bridge::with_handle(bus.clone(), handle);
        (bus, bridge, buffer, acks)
    }

    #[rstest::rstest]
    fn shell_escape_simple_path() {
        // Given a temp-rooted path with no shell metacharacters.
        let dir = jinn_testutil::tempdir();
        let base = dir.path.to_string();
        let path = format!("{base}/project");

        // When escaping it for a shell.
        let escaped = shell_escape(&path);

        // Then it is single-quoted verbatim.
        assert_eq!(escaped, format!("'{base}/project'"));
    }

    #[rstest::rstest]
    fn shell_escape_path_with_spaces() {
        // Given a temp-rooted path containing spaces.
        let dir = jinn_testutil::tempdir();
        let base = dir.path.to_string();
        let path = format!("{base}/my project");

        // When escaping it for a shell.
        let escaped = shell_escape(&path);

        // Then the whole path is single-quoted, spaces included.
        assert_eq!(escaped, format!("'{base}/my project'"));
    }

    #[rstest::rstest]
    fn shell_escape_path_with_single_quote() {
        // Given a temp-rooted path containing a single quote.
        let dir = jinn_testutil::tempdir();
        let base = dir.path.to_string();
        let path = format!("{base}/it's/project");

        // When escaping it for a shell.
        let escaped = shell_escape(&path);

        // Then the quote is closed, escaped, and reopened.
        assert_eq!(escaped, format!("'{base}/it'\\''s/project'"));
    }

    #[rstest::rstest]
    fn shell_escape_empty_string() {
        // Given an empty path.
        let path = "";

        // When escaping it for a shell.
        let escaped = shell_escape(path);

        // Then it becomes a pair of empty single quotes.
        assert_eq!(escaped, "''");
    }

    /// Publishes a barrier through `bridge` and waits for the recorder to
    /// acknowledge it.
    ///
    /// The acknowledgement is the proof that everything published before
    /// the barrier has already been dispatched. Only after it arrives is
    /// "no message was published" a real observation rather than a race
    /// with a dispatch still in flight.
    async fn await_recorder_drained(bridge: &Bridge, acks: &Arc<Mutex<Vec<String>>>) {
        bridge
            .send(Bridge::publish_closure(Barrier::new("after-cwd")))
            .expect("bridge send should succeed");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while acks.lock().unwrap().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the cwd recorder never acknowledged the barrier, so the \
                 recorder did not drain the bus; any absence assertion made \
                 now would prove nothing"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[rstest::rstest]
    #[test]
    fn apply_selected_cwd_publishes_set_session_cwd_for_valid_dir() {
        let rt = test_runtime();
        rt.block_on(async {
            // Given a trouper system with a subscribed SetSessionCwd
            // recorder, and a bridge draining to that system.
            let (_system, bridge, buffer, acks) =
                spawn_system_with_recorder(&tokio::runtime::Handle::current());

            let dir = jinn_testutil::tempdir();
            // The expectation travels the same canonicalization path as the
            // production code, so it holds identically on every platform
            // (verbatim prefixes stripped on Windows by both sides).
            let expected =
                jinn_common::fs_paths::canonicalize_display(&dir.path).expect("canonicalize");

            // When applying a real directory path.
            let session_id = jinn_core_types::SessionId::new();
            let published = apply_selected_cwd(&bridge, session_id.clone(), &dir.path);

            // Then exactly one SetSessionCwd is published with the canonical cwd.
            assert!(published, "valid dir should publish");
            await_recorder_drained(&bridge, &acks).await;
            let received = buffer.lock().unwrap();
            assert_eq!(received.len(), 1, "exactly one SetSessionCwd expected");
            assert_eq!(received[0].session_id, session_id);
            assert_eq!(received[0].cwd, expected);
        });
    }

    #[rstest::rstest]
    #[test]
    fn apply_selected_cwd_rejects_non_directory_path() {
        let rt = test_runtime();
        rt.block_on(async {
            // Given a trouper system with a subscribed SetSessionCwd
            // recorder and a bridge draining to it.
            let (_system, bridge, buffer, acks) =
                spawn_system_with_recorder(&tokio::runtime::Handle::current());

            // And a real file (not a directory) inside a temp dir.
            let dir = jinn_testutil::tempdir();
            let file_path = dir
                .path
                .join("not_a_dir.txt")
                .expect("temp root joins a bare name");
            std::fs::write(&file_path, b"contents").expect("write file");

            // When applying a file path.
            let published =
                apply_selected_cwd(&bridge, jinn_core_types::SessionId::new(), &file_path);

            // Then nothing is published and the helper returns false.
            assert!(!published, "file path should not publish");
            await_recorder_drained(&bridge, &acks).await;
            assert!(buffer.lock().unwrap().is_empty(), "no message expected");
        });
    }

    #[rstest::rstest]
    #[test]
    fn apply_selected_cwd_rejects_nonexistent_path() {
        let rt = test_runtime();
        rt.block_on(async {
            // Given a trouper system with a subscribed SetSessionCwd
            // recorder and a bridge draining to it.
            let (_system, bridge, buffer, acks) =
                spawn_system_with_recorder(&tokio::runtime::Handle::current());

            // And a path derived from a test-owned temp dir that does not
            // exist on disk.
            let dir = jinn_testutil::tempdir();
            let missing_path = dir
                .path
                .join("does-not-exist")
                .expect("temp root joins a bare name");

            // When applying a nonexistent path.
            let published =
                apply_selected_cwd(&bridge, jinn_core_types::SessionId::new(), &missing_path);

            // Then nothing is published and the helper returns false.
            assert!(!published, "nonexistent path should not publish");
            await_recorder_drained(&bridge, &acks).await;
            assert!(buffer.lock().unwrap().is_empty(), "no message expected");
        });
    }

    // -------------------------------------------------------------------------
    // Graphics lifecycle: capability query budget, suspend dirty, switch eviction
    // -------------------------------------------------------------------------

    #[rstest::rstest]
    #[cfg(not(windows))]
    fn picker_query_respects_bounded_timeout() {
        // Given a query thread that never finishes.
        let never = std::thread::spawn(std::thread::park);

        // When polling it under a tiny budget.
        let finished = wait_until_finished(&never, Duration::from_millis(20));

        // Then the budget expires first and the query reports unfinished.
        // (The parked thread leaks — a detached never-parked thread, no side
        // effects — and the real query path abandons it the same way.)
        assert!(!finished, "an unfinished query must not report finished");
    }

    #[rstest::rstest]
    fn suspend_restore_marks_protocols_dirty() {
        // Given a state whose graphics cell holds a picker and a dirty-clean state.
        let state = {
            let s = AppState::default_with_scope_focus();
            if let Some(cell) = s.frontend.graphics_cell() {
                let picker = ratatui_image::picker::Picker::halfblocks();
                cell.update(|graphics| graphics.set_picker(Some(picker)));
            }
            s
        };
        let cell = state.frontend.graphics_cell().expect("catalog ran");
        let epoch_before = cell.read().epoch();

        // When the suspend/restore edge marks the graphics state dirty.
        if let Some(cell) = state.frontend.graphics_cell() {
            cell.update(GraphicsState::mark_dirty);
        }

        // Then the next apply_config consumes the dirty mark and bumps the
        // epoch, so every image re-transmits.
        let config = jinn_preferences_config::schemas::GraphicsConfig::default();
        cell.update(|graphics| graphics.apply_config(&config));
        assert!(
            cell.read().epoch() > epoch_before,
            "the dirty mark must bump the epoch on the next apply"
        );
    }

    #[rstest::rstest]
    fn image_cache_evicts_on_session_switch() {
        // Given a graphics cell whose decode cache holds an entry keyed to a
        // session the user has switched away from.
        use jinn_chat_log_view_msg::{GraphicsState, ImageKey};
        let mut graphics = GraphicsState::new();
        let switched_away = jinn_core_types::SessionId::new();
        let entry = ChatEntryId::new();
        let key = ImageKey::new(&switched_away, &entry, 0);
        // A known-undecodable tombstone is a valid cache entry: eviction is
        // about the KEY, not the payload's decodability.
        graphics.insert_decoded(key.clone(), None);
        assert!(
            graphics.peek_decoded(&key).is_some(),
            "warm-up caches the key"
        );

        // When the switch evicts everything not belonging to the new session.
        let now_active = jinn_core_types::SessionId::new();
        graphics.evict_other_sessions(&now_active);

        // Then the switched-away session's decode is gone.
        assert!(
            graphics.peek_decoded(&key).is_none(),
            "the switched-away session's decode must be evicted"
        );
    }
}
