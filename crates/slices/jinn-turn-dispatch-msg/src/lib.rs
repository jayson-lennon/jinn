//! Turn-dispatch crossing contracts.
//!
//! The EXPORT surface of the turn-dispatch slice: the commands that tell
//! the slice to drain and dispatch a session's turn. Kernel publishers
//! (the session actor's enqueue paths and stall-retry handler) depend on
//! this crate; the slice's queue actor consumes the command over the
//! `jinn.turn-dispatch` trouper topic.
//!
//! The queue vocabulary (`QueueItem`, `TurnQueue`) has its production
//! consumer in this slice's queue actor, and `ChatEntry` lives in
//! `jinn-core-types`, so neither type needs a msg→kernel edge. They are
//! in-memory session payload types, not wire messages — no `BusMessage`
//! impls.
//!
//! The crate stays otherwise narrow: streaming-side vocabulary
//! (`SendToLlmProvider`, `CancelTurn`, `StreamToken`, `StreamCompleted`)
//! stays in the kernel.

pub mod queue_item;
pub mod turn_queue;

pub use queue_item::QueueItem;
pub use turn_queue::TurnQueue;

use jinn_core_types::SessionId;
use serde::{Deserialize, Serialize};

/// Ask the turn-dispatch slice to dispatch the session's prepared turn.
///
/// Published by the session actor wherever a turn becomes dispatchable
/// outside the queue actor's own `Idle`-transition trigger: the
/// idle-direct user send (the enqueue path has already expanded,
/// image-resolved, vision-gated, and pushed the entry), the resume-turn
/// tail, and the stall-retry re-dispatch. All three have verified their
/// eligibility and completed their session-side preparation before
/// publishing — busy-ignore for resumes and guard-refusal for stall
/// retries are decided in the session actor, so the slice dispatches
/// unconditionally on receipt: drain steering, assemble, resolve the
/// model, and publish `SendToLlmProvider`.
///
/// Queued items (busy-session sends) are deliberately *not* drained by
/// this command — they wait for the next `SessionPhaseChanged → Idle`
/// transition, which the queue actor handles on its own subscription.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Ask the turn-dispatch slice to dispatch the session's prepared turn.")]
pub struct DispatchTurn {
    /// The session whose prepared turn should be dispatched.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for DispatchTurn {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn deserializes_from_json_payload() {
        // Given a JSON payload shaped like a bridge relay's deserialized
        // body.
        let payload = serde_json::json!({
            "session_id": "01933dc5-2b14-7e21-8f52-3d1d8f4e7f9a"
        });

        // When deserializing into the command.
        let msg: DispatchTurn = serde_json::from_value(payload).unwrap();

        // Then the session id round-trips.
        assert_eq!(
            msg.session_id.to_string(),
            "01933dc5-2b14-7e21-8f52-3d1d8f4e7f9a"
        );
    }

    #[rstest::rstest]
    #[test]
    fn schema_def_carries_name_kind_and_field() {
        // Given the command's schema definition.
        let schema = <DispatchTurn as trouper::schema::Schema>::schema_def();

        // When inspecting its name, kind, and fields.
        // Then it is a command named DispatchTurn with the session_id
        // field.
        assert_eq!(schema.name, "DispatchTurn");
        assert!(matches!(schema.kind, trouper::schema::SchemaKind::Command));
        assert_eq!(schema.fields.len(), 1);
        let field = schema.fields.first().expect("exactly one field");
        assert_eq!(field.name, "session_id");
    }
}
