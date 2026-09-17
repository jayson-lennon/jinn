//! IntentHandler arms for the terminal overlay (control capture and view actions).
//!
//! Five intents:
//! - [`Intent::TerminalTakeControl`] — control-toggle key in TerminalView:
//!   flips the active session's control holder to *user* (synchronously, so
//!   an in-flight tool call's drain sees the takeover on its next iteration —
//!   mailbox ordering cannot deliver that) and pushes [`FocusScope::TerminalControl`].
//! - [`Intent::TerminalSendKey`] — catch-all routing in TerminalControl: the
//!   encoded bytes go straight to the pty via [`IntentHandlerCommand::SendTermKey`].
//! - [`Intent::TerminalHandback`] — control-toggle key in TerminalControl:
//!   releases control back to the agent and pops to TerminalView. Sends
//!   nothing to the model — the status hint advertises `I` for that.
//! - [`Intent::TerminalYank`] — `y` in TerminalView: copies the visible
//!   screen to the clipboard via the TUI signal.
//! - [`Intent::TerminalPushScreen`] — `I` in TerminalView: yanks the screen
//!   *and* pushes its text to the model using the same session-phase routing
//!   as chat submit (busy → steering buffer drained at next dispatch; idle →
//!   dispatched immediately by the queue actor).
//!
//! Per the style guide, the IntentHandler is the exempt frontend mutator: it
//! may write the shared control registry even though the coordinator actor
//! also mints/removes entries — that's the documented optimistic-write +
//! authoritative-write pattern.

use crate::common::app_state::{AppState, FocusScope};
use crate::feat::interactive_term::protocol::command::ControlHolder;
use jinn_term_msg::takeover::TermControls;

/// The shared control registry installed by actor wiring. Set once at
/// startup; before that, takeover intents no-op (the overlay renders an
/// empty screen).
pub static TERM_CONTROLS: std::sync::OnceLock<TermControls> = std::sync::OnceLock::new();

/// Returns the shared control registry, if installed.
fn controls() -> Option<&'static TermControls> {
    TERM_CONTROLS.get()
}

/// The status hint shown after exiting capture mode: releasing control sends
/// nothing, so the hint advertises the explicit push key.
const HANDLED_HINT: &str =
    "terminal control released — press I to send the current screen to the agent";

/// Handles [`Intent::TerminalTakeControl`].
///
/// No status hint here: taking control is its own visible mode (the
/// scope switch is the announcement); the hint fires on handback.
pub fn handle_take_control(state: &mut AppState) -> crate::protocol::intent::IntentResult {
    if let Some(registry) = controls() {
        registry.set(state.session.active_session_id(), ControlHolder::User);
    }
    state.frontend.scope_push(FocusScope::TerminalControl);
    crate::protocol::intent::IntentResult::empty()
}

/// Handles [`Intent::TerminalSendKey`].
pub fn handle_send_key(
    state: &mut AppState,
    bytes: Vec<u8>,
    _label: String,
) -> crate::protocol::intent::IntentResult {
    // No-op unless the user actually holds control.
    if state.frontend.scope() != FocusScope::TerminalControl {
        return crate::protocol::intent::IntentResult::empty();
    }
    // The overlay targets the active chat session's terminal.
    crate::protocol::intent::IntentResult::empty().with_message(
        crate::feat::interactive_term::protocol::command::SendTermKey {
            chat_session_id: state.session.active_session_id().clone(),
            bytes,
        },
    )
}

/// Handles [`Intent::TerminalHandback`].
///
/// Pure state transition: releases control to the agent (shared registry),
/// pops to TerminalView, and sets a status hint advertising `I`. Sends
/// nothing to the model — pushing the screen is an explicit `I`.
pub fn handle_handback(
    state: &mut AppState,
    slices: &crate::common::slices::Slices,
) -> crate::protocol::intent::IntentResult {
    if state.frontend.scope() != FocusScope::TerminalControl {
        return crate::protocol::intent::IntentResult::empty();
    }
    if let Some(registry) = controls() {
        registry.set(state.session.active_session_id(), ControlHolder::Agent);
    }
    state.frontend.scope_pop();
    crate::feat::ui::status_hint::set_hint(state, slices, Some(HANDLED_HINT.to_owned()));
    crate::protocol::intent::IntentResult::empty()
}

/// Builds the model-facing message text for [`Intent::TerminalPushScreen`].
///
/// Public because the wording is part of the feature's contract with the
/// agent: it arrives as the user's own message, so it speaks in first
/// person — "Here is the current terminal screen" (never "The user …",
/// which would be confusing, and never "handed back", which would imply a
/// release event, or the user-control notice, which belongs solely to
/// refusal paths).
#[must_use]
pub fn push_screen_text(screen: &str) -> String {
    format!("Here is the current terminal screen:\n\n```\n{screen}\n```")
}

/// The visible screen of the active session's terminal, if any.
fn active_screen(state: &AppState) -> Option<String> {
    let chat = state.session.active_session_id();
    state
        .term_tabs()
        .and_then(|cell| cell.read().mirror(chat).map(|m| m.screen.clone()))
}

/// Handles [`Intent::TerminalYank`].
///
/// Copies the visible screen to the clipboard (via the TUI yank signal) and
/// reports the copied size in a status hint. No mirror → no-op with a hint.
pub fn handle_yank(
    state: &mut AppState,
    slices: &crate::common::slices::Slices,
) -> crate::protocol::intent::IntentResult {
    if state.frontend.scope() != FocusScope::TerminalView {
        return crate::protocol::intent::IntentResult::empty();
    }
    let Some(screen) = active_screen(state) else {
        crate::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some("no live terminal to yank — ask the agent to run `interactive_term`".to_owned()),
        );
        return crate::protocol::intent::IntentResult::empty();
    };
    let lines = screen.lines().count();
    state
        .frontend
        .update_scope(|s| s.signals.yank_text = Some(screen));
    crate::feat::ui::status_hint::set_hint(
        state,
        slices,
        Some(format!("yanked {lines} terminal lines to the clipboard")),
    );
    crate::protocol::intent::IntentResult::empty()
}

/// Handles [`Intent::TerminalPushScreen`].
///
/// Yanks (see [`handle_yank`]) and pushes the screen text to the model:
/// busy → `SubmitSteeringMessage` (drained at the next dispatch-resume);
/// idle → `EnqueueUserMessage` (dispatched immediately). No mirror → no-op
/// with a hint.
pub fn handle_push_screen(
    state: &mut AppState,
    slices: &crate::common::slices::Slices,
) -> crate::protocol::intent::IntentResult {
    if state.frontend.scope() != FocusScope::TerminalView {
        return crate::protocol::intent::IntentResult::empty();
    }
    let Some(screen) = active_screen(state) else {
        crate::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some("no live terminal to share — ask the agent to run `interactive_term`".to_owned()),
        );
        return crate::protocol::intent::IntentResult::empty();
    };
    let lines = screen.lines().count();
    state
        .frontend
        .update_scope(|s| s.signals.yank_text = Some(screen.clone()));
    crate::feat::ui::status_hint::set_hint(
        state,
        slices,
        Some(format!(
            "yanked {lines} terminal lines and sent the screen to the agent"
        )),
    );

    let text = push_screen_text(&screen);
    let session_id = state.session.active_session_id().clone();
    if state.active_session().phase() == crate::feat::session::phase_machine::PhaseKind::Idle {
        crate::protocol::intent::IntentResult::empty().with_message(
            crate::feat::chat_input::protocol::command::EnqueueUserMessage {
                session_id,
                entry: crate::protocol::ChatEntry::user(text),
            },
        )
    } else {
        crate::protocol::intent::IntentResult::empty().with_message(
            crate::feat::chat_input::protocol::command::SubmitSteeringMessage { session_id, text },
        )
    }
}
