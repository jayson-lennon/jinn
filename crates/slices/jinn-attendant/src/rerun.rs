//! Re-running an attendant on the user's request.
//!
//! The `R` key in the sessions section calls into this from the frontend.
//! The manual path is deliberately its own sequence, not the trigger's with
//! a flag: `R` is the user saying "ask again", so it seeds through the
//! template in every behavior, and it stops the attendant's descendants
//! along with it. A trigger cannot do either — it does not know which
//! descendant should be cancelled, and it must not inject a message the user
//! did not ask for.
//!
//! The one configuration `R` refuses is prep mode: an attendant still being
//! composed has half-written pins, and running it would dispatch a prompt
//! built from instructions the user has not finished. The block names the
//! reason so the sidebar can put it on screen — a key that silently does
//! nothing is the complaint this replaces.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntryId, SessionId};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::state::State;
use jinn_session_msg::PhaseKind;

use crate::activation;

/// The outcome of a successful rerun: cancel the in-flight turn (if it was
/// busy), then dispatch the seeded run.
///
/// The third element is the ids whose context a `Reset` excluded — non-empty
/// only when the reset actually changed something, which is the caller's cue
/// that the exclusions need writing to disk.
pub type RerunOutcome = (
    Option<CancelTurn>,
    Option<EnqueueUserMessage>,
    Vec<ChatEntryId>,
);

/// Runs an attendant now, without any trigger condition.
///
/// The sequence: cancel the in-flight turn, reset context if the mode says
/// so, seed through the template, dispatch. Returns `None` when the run
/// cannot start — see [`rerun_blocked_reason`] for which reason applies.
///
/// Descendant cancels are *not* produced here: the subtree walk needs the
/// whole application state, which the caller holds, and the sidebar's `R`
/// handler publishes them alongside this outcome.
pub fn rerun(state: &State, attendant_id: &SessionId) -> Option<RerunOutcome> {
    if rerun_blocked_reason(state, attendant_id).is_some() {
        return None;
    }
    let mut guard = state.write();
    rerun_in_state(&mut guard, attendant_id)
}

/// The keybind path: rerun against already-held mutable state.
///
/// The sidebar resolves the highlighted row and holds `&mut AppState`; the
/// trigger actor holds the shared [`State`]. Both run the same sequence, so
/// both delegate here with whatever access they have.
pub fn rerun_in_state(
    state: &mut jinn_kernel::AppState,
    attendant_id: &SessionId,
) -> Option<RerunOutcome> {
    if rerun_blocked_reason_in(state, attendant_id).is_some() {
        return None;
    }
    let session = state.session.get_mut(attendant_id)?;
    // Superseding a busy attendant: cancel the in-flight turn as a command
    // so the seeded entry below *dispatches* rather than queueing — the
    // enqueue handler queues anything arriving while a session is
    // Sending/Streaming. The command ends the previous turn through the
    // session actor; no phase is written synchronously here.
    // `Esc` drains the cancelled partial into the input box through its own
    // path; `R` must not, because the run about to start would carry the old
    // turn's leftovers.
    let cancel = (session.phase() != PhaseKind::Idle).then(|| {
        session.finalize_entries_for_cancel(jiff::Timestamp::now());
        CancelTurn {
            session_id: attendant_id.clone(),
        }
    });
    let (entry, reset) = activation::prepare_manual_run(session);
    let dispatch = entry.map(|entry| EnqueueUserMessage {
        session_id: attendant_id.clone(),
        entry,
    });
    Some((cancel, dispatch, reset))
}

/// Why a rerun cannot start, for the status-bar hint.
///
/// `None` means the rerun is allowed.
#[must_use]
pub fn rerun_blocked_reason(state: &State, attendant_id: &SessionId) -> Option<&'static str> {
    let guard = state.read();
    rerun_blocked_reason_in(&guard, attendant_id)
}

/// The keybind path of [`rerun_blocked_reason`], over held state.
#[must_use]
pub fn rerun_blocked_reason_in(
    state: &jinn_kernel::AppState,
    attendant_id: &SessionId,
) -> Option<&'static str> {
    let session = state.session.get(attendant_id)?;
    if !session.is_attendant() {
        Some("not an attendant")
    } else if session.attendant_is_prepping() {
        Some("attendant is in prep mode")
    } else {
        None
    }
}
