//! IntentHandler arms for the terminal overlay (view toggle).
//!
//! [`Intent::ToggleTerminalOverlay`] opens the overlay for a session with a
//! live terminal (view mode) or closes it when already open. The overlay
//! targets the **active** chat session when no explicit id is given (global
//! `<M-t>`) or the **selected** session from the sidebar (sidebar toggle key).
//! Takeover, handback, and key forwarding live in
//! [`super::takeover_intent`] — those semantics are unchanged; they operate
//! on whichever session the overlay is showing (the active one).
//!
//! Toggle targets without a live terminal no-op: the overlay is a *window*
//! onto a running program, never a spawner.

use crate::common::app_state::{AppState, FocusScope};
use crate::protocol::intent::IntentResult;
use jinn_slices::SidebarSectionId;

/// Resolves the selected session when the Sessions sidebar section is
/// focused, for callers that act on the sidebar's selection (the sidebar
/// toggle key). `None` when no selection or the wrong section is focused.
#[must_use]
pub fn selected_sessions_sidebar_target(state: &AppState) -> Option<crate::protocol::SessionId> {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(SidebarSectionId::Sessions)
    ) {
        return None;
    }
    let index = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)?;
    let sessions = crate::feat::session::sessions_list::state::sorted_open_sessions(state);
    sessions.get(index).map(|entry| entry.id.clone())
}

/// Handles [`Intent::ToggleTerminalOverlay`].
pub fn handle_toggle_overlay(
    state: &mut AppState,
    slices: &crate::common::slices::Slices,
    session_id: Option<&crate::protocol::SessionId>,
) -> IntentResult {
    // Already open (view or control): any toggle closes it.
    if matches!(
        state.frontend.scope(),
        FocusScope::TerminalView | FocusScope::TerminalControl
    ) {
        state.frontend.scope_pop();
        return IntentResult::empty();
    }
    // Resolve the target: explicit (sidebar selection) or active session.
    let target = session_id.map_or_else(
        || state.session.active_session_id().clone(),
        std::clone::Clone::clone,
    );
    // A session without a live terminal has nothing to show; the overlay is
    // never a spawn trigger. A status hint explains the inert press.
    if !state.frontend.terminal.live_terms.contains(&target) {
        crate::feat::ui::status_hint::set_hint(
            state,
            slices,
            Some(
                "that session has no live terminal — ask the agent to run `interactive_term`"
                    .to_owned(),
            ),
        );
        return IntentResult::empty();
    }
    // If a different popup holds the top of the stack, it is replaced: the
    // overlay mounts on the base scope (Esc semantics for the buried popup).
    state.frontend.scope_clear_overlays();
    state.frontend.scope_push(FocusScope::TerminalView);
    IntentResult::empty()
}
