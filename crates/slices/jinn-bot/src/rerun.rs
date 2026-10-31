//! Re-running a bot on the user's request.
//!
//! The `R` key in the sessions section calls into this from the frontend.
//! The manual path is deliberately its own sequence, not the trigger's with
//! a flag: `R` is the user saying "ask again", so it seeds through the
//! template in every behavior, and it stops the bot's descendants
//! along with it. A trigger cannot do either — it does not know which
//! descendant should be cancelled, and it must not inject a message the user
//! did not ask for.
//!
//! The one configuration `R` refuses is prep mode: a bot still being
//! composed has half-written pins, and running it would dispatch a prompt
//! built from instructions the user has not finished. The block names the
//! reason so the sidebar can put it on screen — a key that silently does
//! nothing is the complaint this replaces.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntryId, SessionId};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::common::state::State;
use jinn_slices::PhaseView;

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

/// Runs a bot now, without any trigger condition.
///
/// The sequence: cancel the in-flight turn, reset context if the mode says
/// so, seed through the template, dispatch. Returns `None` when the run
/// cannot start — see [`rerun_blocked_reason`] for which reason applies.
///
/// Descendant cancels are *not* produced here: the subtree walk needs the
/// whole application state, which the caller holds, and the sidebar's `R`
/// handler publishes them alongside this outcome.
pub fn rerun(state: &State, bot_id: &SessionId) -> Option<RerunOutcome> {
    if rerun_blocked_reason(state, bot_id).is_some() {
        return None;
    }
    let mut guard = state.write();
    rerun_in_state(&mut guard, bot_id)
}

/// The keybind path: rerun against already-held mutable state.
///
/// The sidebar resolves the highlighted row and holds `&mut AppState`; the
/// trigger actor holds the shared [`State`]. Both run the same sequence, so
/// both delegate here with whatever access they have.
pub fn rerun_in_state(
    state: &mut jinn_kernel::AppState,
    bot_id: &SessionId,
) -> Option<RerunOutcome> {
    if rerun_blocked_reason_in(state, bot_id).is_some() {
        return None;
    }
    let session = state.session.get_mut(bot_id)?;
    // Superseding a busy bot: cancel the in-flight turn as a command
    // so the seeded entry below *dispatches* rather than queueing — the
    // enqueue handler queues anything arriving while a session is
    // Sending/Streaming. The command ends the previous turn through the
    // session actor; no phase is written synchronously here.
    // `Esc` drains the cancelled partial into the input box through its own
    // path; `R` must not, because the run about to start would carry the old
    // turn's leftovers.
    let cancel = (session.phase() != PhaseView::Idle).then(|| {
        session.finalize_entries_for_cancel(jiff::Timestamp::now());
        CancelTurn {
            session_id: bot_id.clone(),
            // Not ESC-ESC semantics: the run about to start must carry the
            // old turn's leftovers, so nothing drains into the input box.
            drain_buffers_to_input: false,
        }
    });
    let (entry, reset) = activation::prepare_manual_run(session);
    let dispatch = entry.map(|entry| EnqueueUserMessage {
        session_id: bot_id.clone(),
        entry,
    });
    Some((cancel, dispatch, reset))
}

/// Why a rerun cannot start, for the status-bar hint.
///
/// `None` means the rerun is allowed.
#[must_use]
pub fn rerun_blocked_reason(state: &State, bot_id: &SessionId) -> Option<&'static str> {
    let guard = state.read();
    rerun_blocked_reason_in(&guard, bot_id)
}

/// The keybind path of [`rerun_blocked_reason`], over held state.
#[must_use]
pub fn rerun_blocked_reason_in(
    state: &jinn_kernel::AppState,
    bot_id: &SessionId,
) -> Option<&'static str> {
    let session = state.session.get(bot_id)?;
    if !session.is_bot() {
        Some("not a bot")
    } else if session.bot_is_prepping() {
        Some("bot is in prep mode")
    } else {
        None
    }
}
