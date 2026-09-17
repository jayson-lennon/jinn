//! Interactive terminal sessions — drive TUI programs from the agent.
//!
//! The term slice owns the whole PTY actor family: spawn, emulation,
//! settle detection, realtime screen mirroring, and the coordinator
//! that keeps one terminal per chat session. The kernel's tools speak
//! to the coordinator through the `TermHandle` trait; the TUI reads
//! the `term/tabs` cell and renders the slice's overlay views.

pub mod emulator;
pub mod interactive_term_actor;
pub mod pty_session;
pub mod query_responder;
pub mod screen_task;
pub mod settle;

/// Activates the term slice: registers the `term/tabs` cell, mints the
/// shared control registry, and stores the `TermHandle` implementation.
///
/// Returns nothing; the caller (composition) spawns the coordinator
/// actor itself so the boot-order invariants stay in one place.
pub fn activate(services: &jinn_domain::common::services::Services) {
    // The control registry is minted exactly once, before any intent row
    // or tool can read it.
    let _ = jinn_term_msg::TERM_CONTROLS.set(jinn_term_msg::TermControls::default());

    // The cell is registered by composition today (actor_wiring) so the
    // spawn order matches the tools-registry cell; re-registering is a
    // no-op error we ignore deliberately.
    let _ = services.slices.register(
        jinn_term_msg::term_tabs_slot(),
        jinn_term_msg::TerminalTabState::default(),
    );

    tracing::debug!("term slice activated");
}
