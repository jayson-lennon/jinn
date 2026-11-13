//! The one way a session becomes active.
//!
//! Activation lives in one function because three callers need it — the
//! sessions list, the session picker, and subagent entry — and they were
//! already drifting apart. Each decided for itself whether a measurement was
//! due, and the subagent path decided it never was, so a large in-memory child
//! laid its history out inline on the frame it was opened. One entry point
//! means the three cannot drift again.

use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::IntentResult;
use jinn_session_store_msg::SessionLoadRequested;

/// Switches to `target_id`, measuring it if it needs measuring.
///
/// Returns the message that carries the activation. The caller passes `extra`
/// — its own scope work, its own signals — and gets it back with the activation
/// message folded in, so a caller adds what it needs without this function
/// having to know what callers do.
///
/// The width is read *before* the switch and carried on the command. After the
/// switch the target's own `content_width` is the never-rendered zero, and
/// measuring there produces counts no frame can use, which the completion actor
/// then discards as stale — a silent failure that looks like a performance
/// regression rather than a bug.
///
/// The load guard is armed before the switch so the very next frame sees it,
/// which is what puts the loading indication up for the one frame between here
/// and the switch taking effect.
pub fn activate_session(
    state: &mut AppState,
    target_id: SessionId,
    extra: IntentResult,
) -> IntentResult {
    // The width the next frame will render at is the one the session on screen
    // last used, not the incoming session's: it has never rendered, so its own
    // width is stale.
    let content_width = state.session.active_session().content_width();
    let needs_measurement = match state.frontend.line_cache_cell() {
        Some(cell) => cell.update(|cache| {
            !super::history::is_session_measured(cache, state, &target_id, content_width)
        }),
        // No cell means the catalog never ran, so nothing was ever cached and
        // the session is unmeasured.
        None => true,
    };

    if needs_measurement {
        state.session.begin_load(target_id.clone());
    }
    state.session.set_active(target_id.clone());

    // Drop the other sessions' decode/protocol state: image caches are
    // keyed by session, and the render pass's per-frame
    // `evict_other_sessions` catches anything that slips (a switch the
    // frame misses), but doing it at the switch keeps memory bounded
    // without waiting for the next frame.
    if let Some(cell) = state.frontend.graphics_cell() {
        cell.update(|graphics| graphics.evict_other_sessions(&target_id));
    }

    if needs_measurement {
        extra.with_message(SessionLoadRequested {
            session_id: target_id,
            content_width: Some(content_width),
        })
    } else {
        // A measured session needs no message at all. The store actor is what
        // decides whether a request means "read it" or "it is already here";
        // a caller that knows it is measured is saving the actor a round trip,
        // not pre-empting its decision.
        extra
    }
}
