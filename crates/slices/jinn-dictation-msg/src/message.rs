//! Messages the dictation slice exchanges on the bus.

use serde::Deserialize;
use serde::Serialize;

use jinn_slices::BusMessage;

/// A dictation toggle press, published by the alt-i route action.
///
/// The action is synchronous and the actor is not: the action reads the
/// state cell to decide the *hint* (hint writes are synchronous UI), and
/// publishes this command for the *lifecycle work* (arm, disarm, and the
/// busy-state presses that only need an idempotent acknowledgement —
/// the actor folds the command onto its machine so the state stays the
/// single truth).
///
/// One declarant: the dictation actor.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "A dictation toggle press (alt-i) for the dictation actor to fold.")]
pub struct DictationToggle;

impl BusMessage for DictationToggle {}

/// A dictation lifecycle completion: a backend fetch or load landing,
/// or the capture reporting it vanished without a toggle.
///
/// Self-addressed (the watchdog's `StallTick` pattern): the blocking
/// task publishes it, the dictation actor folds it. The kind names the
/// completion — `fetched`, `loaded`, or `capture-lost` — and the detail
/// carries the rendered fault or refusal when there is one.
///
/// One declarant: the dictation actor.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(
    description = "A dictation backend completion or capture report, folded by the dictation actor."
)]
pub struct DictationLifecycleTick {
    /// Which completion landed: `fetched`, `loaded`, or `capture-lost`.
    pub kind: String,
    /// The rendered fault or refusal, when the completion carries one.
    pub detail: Option<String>,
}

impl BusMessage for DictationLifecycleTick {}
