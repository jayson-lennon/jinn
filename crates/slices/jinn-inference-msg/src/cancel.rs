//! Turn cancellation — the one command that ends a turn.
//!
//! [`CancelTurn`] replaced a pair of commands that each performed a different
//! subset of "stop the stream, settle the phase, report the end, drop the
//! queued dispatch". Eleven sites performed those steps separately, and a
//! *partial* cancel left a session wedged in a busy phase with a watchdog
//! still armed.
//!
//! It is a schema broadcast, so both actors that must act on a cancel act on
//! the same message: the inference actor stops the stream, and the session
//! actor applies the cancel through the phase actor, which ends the turn for
//! every component observing it. Nothing else is duplicated.
//!
//! It carries no cause. Whether the dispatch queued behind a cancel dies with
//! it is not a property of the cancel — it falls out of the phase actor's
//! turn record: a cancel kills the live generation, and a queued resume or
//! continuation with no live generation behind it is refused at mint.

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// End a session's turn.
///
/// One command, one settle path. The inference actor tears the provider stream
/// down — cancels pending tool batches, aborts the task — and publishes
/// nothing. The session actor routes the cancel through the phase actor,
/// which settles the phase, kills the generation, and publishes exactly one
/// `StreamCompleted(Canceled)` report via the session actor if the turn had
/// not already ended.
///
/// Two actors publishing a completion for one cancel is what produced the
/// double-settle, so exactly one of them does.
///
/// The kind is an event, deliberately. Two actors declare the schema — the
/// inference actor stops the stream, the session actor reports the turn's
/// end — and a command reaches one route per publish, round-robining
/// between them: one press killed the stream, the next settled the phase,
/// which is where "ESC four times" came from. An event broadcasts to every
/// declarant, so both halves of the cancel happen on the same press.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session's turn was cancelled.")]
pub struct CancelTurn {
    /// The session whose turn should be ended.
    pub session_id: SessionId,
    /// Whether the cancelled turn's buffered work — the steering buffer and
    /// the turn queue — should be drained into the input box as editable
    /// draft text at the settle, instead of re-dispatching after it.
    ///
    /// Off by default. Exactly one publisher sets it: the user-confirmed
    /// ESC-ESC cancel. Every other publisher — the bot's manual re-run, the
    /// watchdogs, the cascade's child stops, the interrupt intent — wants
    /// the buffered work to re-dispatch after the settle, so it keeps the
    /// wire default and the field carries no cause information they would
    /// have to set correctly.
    #[serde(default)]
    pub drain_buffers_to_input: bool,
}

impl jinn_slices::BusMessage for CancelTurn {}
