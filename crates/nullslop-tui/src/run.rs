//! Terminal setup, event loop, and teardown.
//!
//! Sets up the terminal (raw mode + alternate screen), runs the
//! main event loop, and restores the terminal on exit. Also manages
//! the background event thread lifecycle, stopping it before terminal
//! suspension and restarting it afterward.

use std::io::{self, Stdout};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use error_stack::{Report, ResultExt as _};
use ratatui::{Terminal, backend::CrosstermBackend};
use wherror::Error;

use crate::TuiApp;
use crate::app::scope_for_focus;

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

    // Enable Kitty keyboard protocol so crossterm can distinguish
    // modified special keys (e.g. Shift+Enter, Ctrl+Enter).
    // Terminals that don't support it silently ignore the sequence.
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .change_context(TuiRunError)
    .attach("failed to push keyboard enhancement flags")?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .change_context(TuiRunError)
        .attach("failed to create terminal")?;

    // Start the event poll thread (independent of tokio runtime).
    app.event_thread = Some(app.events.start_event_thread());

    let result = run_main_loop(&mut terminal, &mut app);

    // Clean up event thread.
    if let Some(mut guard) = app.event_thread.take() {
        guard.stop();
    }

    // Shut down actor host — coordinated shutdown.
    nullslop_domain::coordinated_shutdown(
        app.actor_host.backend(),
        &app.core.state,
        &app.services.handle,
        nullslop_domain::SHUTDOWN_TIMEOUT,
    );

    // Restore terminal.
    if let Err(e) = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags) {
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

/// Runs the main TUI event loop — receives events, processes state, and renders frames.
fn run_main_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> Result<(), Report<TuiRunError>> {
    loop {
        let event = app
            .events
            .recv()
            .change_context(TuiRunError)
            .attach("event channel closed")?;
        app.handle_msg(event);

        while let Some(event) = app.events.try_recv() {
            app.handle_msg(event);
        }

        // Check should_quit from shared state (async forwarding task handles messages).
        let state_read = app.core.state.read();
        let should_quit = state_read.frontend.should_quit;
        let scope = scope_for_focus(state_read.frontend.scope_stack.current());
        drop(state_read);
        app.which_key.set_scope(scope);

        // Sync tab manager active tab from AppState.active_tab.
        // Sync is no longer needed — tabs have been removed.

        // Check for pending suspend after event batch processing.
        if let Some(action) = app.suspend.take_action() {
            handle_suspend_action(terminal, app, action)?;
        }

        terminal
            .draw(|frame| {
                app.render(frame);
            })
            .change_context(TuiRunError)
            .attach("failed to draw frame")?;

        if should_quit {
            break;
        }
    }

    Ok(())
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

    let result_content = crate::terminal::suspend_and_run(terminal, || match action {
        crate::suspend::SuspendAction::Edit {
            initial_content,
            on_result,
        } => {
            let edited = dialoguer::Editor::new()
                .edit(&initial_content)
                .ok()
                .flatten();

            let changed = edited.filter(|c| c != &initial_content);
            on_result(changed)
        }
    })
    .change_context(TuiRunError)
    .attach("failed to suspend terminal for editor")?;

    // Restart the event poll thread with a fresh crossterm state.
    app.event_thread = Some(app.events.start_event_thread());

    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .change_context(TuiRunError)
        .attach("failed to redraw after suspend")?;

    // Handle the suspend result directly — set input_buffer on AppState.
    if let Some(content) = result_content {
        app.core
            .state
            .write()
            .active_chat_input_mut()
            .replace_all(content);
    }

    Ok(())
}
