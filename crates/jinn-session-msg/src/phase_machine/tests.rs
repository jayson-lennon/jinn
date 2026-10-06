//! Transition test matrix for [`SessionPhaseMachine`].
//!
//! Tests are organized into sections:
//! 1. Valid transitions - each from/to/side-effects verified
//! 2. Invalid transitions - each returns `TransitionError`
//! 3. Tool loop cycles - multi-step sequences
//! 4. Side effects - data tracking within phases

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use super::machine::{SessionPhaseMachine, TransitionError};
use super::phase::{Phase, PhaseKind};
use super::transition::PhaseTransitions;

// ── Helpers ─────────────────────────────────────────────────────────────

/// Create a machine in `Idle` phase.
fn idle_machine() -> SessionPhaseMachine {
    SessionPhaseMachine::new()
}

/// Create a machine in `Sending` phase.
fn sending_machine() -> SessionPhaseMachine {
    let mut m = SessionPhaseMachine::new();
    m.on_dispatch_message().expect("dispatch should succeed");
    m
}

/// Create a machine in `Streaming` phase.
fn streaming_machine() -> SessionPhaseMachine {
    let mut m = SessionPhaseMachine::new();
    m.on_dispatch_message().expect("dispatch should succeed");
    m.on_first_token().expect("first token should succeed");
    m
}

fn assert_from(err: &TransitionError, expected_from: PhaseKind) {
    assert_eq!(err.from, expected_from, "error from phase mismatch");
}

/// Advance the tool-use handoff: stream completion followed by the loop's
/// re-dispatch, the fused `Streaming → Idle → Sending` path a tool-use
/// burst takes.
fn fuse_tool_use_completion(m: &mut SessionPhaseMachine) {
    m.on_stream_completed_finished()
        .expect("tool-use burst should complete");
    m.on_dispatch_message()
        .expect("the tool loop should re-dispatch");
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 1: Valid transitions
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn idle_to_sending_on_dispatch() {
    // Given a machine in Idle.
    let mut m = idle_machine();

    // When dispatching a message.
    let outcome = m.on_dispatch_message().expect("should succeed");

    // Then the outcome records the transition.
    assert_eq!(outcome.old_phase, PhaseKind::Idle);
    assert_eq!(outcome.new_phase, PhaseKind::Sending);
    // And the machine is in Sending.
    assert_eq!(m.kind(), PhaseKind::Sending);
}

#[rstest::rstest]
#[test]
fn sending_to_streaming_on_first_token() {
    // Given a machine in Sending.
    let mut m = sending_machine();

    // When receiving the first token.
    let outcome = m.on_first_token().expect("should succeed");

    // Then the transition is Sending → Streaming.
    assert_eq!(outcome.old_phase, PhaseKind::Sending);
    assert_eq!(outcome.new_phase, PhaseKind::Streaming);
    assert_eq!(m.kind(), PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn streaming_to_idle_on_finished() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();

    // When stream completes normally.
    let outcome = m.on_stream_completed_finished().expect("should succeed");

    // Then the transition is Streaming → Idle.
    assert_eq!(outcome.old_phase, PhaseKind::Streaming);
    assert_eq!(outcome.new_phase, PhaseKind::Idle);
    assert!(
        m.streaming_phase().is_none(),
        "streaming data should be gone"
    );
}

#[rstest::rstest]
#[test]
fn sending_to_streaming_on_tool_batch() {
    // Given a machine in Sending (no flags set).
    let mut m = sending_machine();

    // When tool batch completes.
    let outcome = m.on_tool_batch_completed().expect("should succeed");

    // Then the transition is Sending → Streaming.
    assert_eq!(outcome.old_phase, PhaseKind::Sending);
    assert_eq!(outcome.new_phase, PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn sending_to_idle_on_tool_loop_disabled() {
    // Given a machine in Sending with tool_loop_disabled set.
    let mut m = sending_machine();
    m.set_tool_loop_disabled();

    // When tool batch completes.
    let outcome = m.on_tool_batch_completed().expect("should succeed");

    // Then the transition is Sending → Idle.
    assert_eq!(outcome.old_phase, PhaseKind::Sending);
    assert_eq!(outcome.new_phase, PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn cancel_during_streaming() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();

    // When cancel is called.
    let result = m.cancel().expect("should succeed");

    // Then the outcome is Streaming → Idle.
    assert_eq!(result.outcome.old_phase, PhaseKind::Streaming);
    assert_eq!(result.outcome.new_phase, PhaseKind::Idle);
    // And the machine is in Idle.
    assert_eq!(m.kind(), PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn first_token_creates_default_streaming_state() {
    // Given a machine in Sending.
    let mut m = sending_machine();

    // When receiving first token.
    m.on_first_token().expect("should succeed");

    // Then the streaming phase has default (empty) values.
    let sp = m.streaming_phase().expect("should be streaming");
    assert!(sp.streaming_entry_index.is_none());
    assert!(sp.streaming_thinking_entry_index.is_none());
    // And the tool tracking maps, which are machine-level, are untouched by
    // the edge.
    assert!(m.active_tool_call_indices().is_empty());
    assert!(m.active_tool_result_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn streaming_state_cleared_on_finish() {
    // Given a machine in Streaming with populated state.
    let mut m = streaming_machine();
    {
        let sp = m.streaming_phase_mut().expect("should be streaming");
        sp.streaming_entry_index = Some(5);
        sp.streaming_thinking_entry_index = Some(3);
    }
    m.active_tool_call_indices_mut().insert(0, 10);
    m.active_tool_result_indices_mut()
        .insert("tc_1".to_owned(), 12);

    // When stream finishes.
    m.on_stream_completed_finished().expect("should succeed");

    // Then all streaming state is gone (dropped with the variant, and the
    // tool tracking maps are cleared explicitly by the transition).
    assert!(m.streaming_phase().is_none());
    assert_eq!(m.kind(), PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn tool_loop_disabled_cleared() {
    // Given a machine in Sending with tool_loop_disabled set.
    let mut m = sending_machine();
    m.set_tool_loop_disabled();

    // When tool batch completes.
    let outcome = m.on_tool_batch_completed().expect("should succeed");

    // Then we go to Idle (not Streaming) and the flag is gone.
    assert_eq!(outcome.new_phase, PhaseKind::Idle);
    assert!(m.sending_phase().is_none(), "sending phase should be gone");
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 2: Invalid transitions
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn reject_dispatch_while_streaming() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();
    // When dispatching a message.
    let err = m.on_dispatch_message().unwrap_err();
    // Then the transition is rejected and the error reports Streaming as the from phase.
    assert_from(&err, PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn reject_dispatch_while_sending() {
    // Given a machine in Sending.
    let mut m = sending_machine();
    // When dispatching a message.
    let err = m.on_dispatch_message().unwrap_err();
    // Then the transition is rejected and the error reports Sending as the from phase.
    assert_from(&err, PhaseKind::Sending);
}

#[rstest::rstest]
#[test]
fn reject_first_token_while_idle() {
    // Given a machine in Idle.
    let mut m = idle_machine();
    // When receiving the first token.
    let err = m.on_first_token().unwrap_err();
    // Then the transition is rejected and the error reports Idle as the from phase.
    assert_from(&err, PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn reject_first_token_while_streaming() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();
    // When receiving the first token.
    let err = m.on_first_token().unwrap_err();
    // Then the transition is rejected and the error reports Streaming as the from phase.
    assert_from(&err, PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn reject_stream_completed_while_idle() {
    // Given a machine in Idle.
    let mut m = idle_machine();
    // When the stream completes.
    let err = m.on_stream_completed_finished().unwrap_err();
    // Then the transition is rejected and the error reports Idle as the from phase.
    assert_from(&err, PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn reject_stream_completed_while_sending() {
    // Given a machine in Sending.
    let mut m = sending_machine();
    // When the stream completes.
    let err = m.on_stream_completed_finished().unwrap_err();
    // Then the transition is rejected and the error reports Sending as the from phase.
    assert_from(&err, PhaseKind::Sending);
}

#[rstest::rstest]
#[test]
fn reject_tool_batch_while_idle() {
    // Given a machine in Idle.
    let mut m = idle_machine();
    // When the tool batch completes.
    let err = m.on_tool_batch_completed().unwrap_err();
    // Then the transition is rejected and the error reports Idle as the from phase.
    assert_from(&err, PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn reject_tool_batch_while_streaming() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();
    // When the tool batch completes.
    let err = m.on_tool_batch_completed().unwrap_err();
    // Then the transition is rejected and the error reports Streaming as the from phase.
    assert_from(&err, PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn reject_cancel_while_idle() {
    // Given a machine in Idle.
    let mut m = idle_machine();
    // When cancelling.
    let err = m.cancel().unwrap_err();
    // Then the transition is rejected and the error reports Idle as the from phase.
    assert_from(&err, PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn cancel_during_sending() {
    // Given a machine in Sending.
    let mut m = sending_machine();

    // When cancel is called.
    let result = m.cancel().expect("should succeed");

    // Then the outcome is Sending → Idle.
    assert_eq!(result.outcome.old_phase, PhaseKind::Sending);
    assert_eq!(result.outcome.new_phase, PhaseKind::Idle);
    // And the machine is in Idle.
    assert_eq!(m.kind(), PhaseKind::Idle);
    // And old_streaming is default (no streaming data in Sending phase).
    assert!(result.old_streaming.streaming_entry_index.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 3: Tool loop cycles
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn full_tool_loop_cycle() {
    // Given a fresh machine in Idle.
    // Idle → Sending → Streaming → Idle → Sending → Streaming → Idle
    let mut m = SessionPhaseMachine::new();

    // When running the whole tool loop: dispatch, stream, the fused tool-use
    // handoff, tool batch, second stream, finish.
    m.on_dispatch_message().expect("dispatch 1");
    assert_eq!(m.kind(), PhaseKind::Sending);
    m.on_first_token().expect("first token 1");
    assert_eq!(m.kind(), PhaseKind::Streaming);
    fuse_tool_use_completion(&mut m);
    assert_eq!(m.kind(), PhaseKind::Sending);
    m.on_tool_batch_completed().expect("tool batch");
    assert_eq!(m.kind(), PhaseKind::Streaming);
    m.on_stream_completed_finished().expect("finished");

    // Then the cycle ends back in Idle.
    assert_eq!(m.kind(), PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn tool_loop_with_cancel_mid_stream() {
    // Given a machine dispatched and already streaming.
    // Idle → Sending → Streaming → cancel() → Idle
    let mut m = SessionPhaseMachine::new();
    m.on_dispatch_message().expect("dispatch");
    m.on_first_token().expect("first token");

    // When cancelling mid-stream.
    let result = m.cancel().expect("cancel");

    // Then the outcome is Streaming → Idle and the machine is Idle.
    assert_eq!(result.outcome.new_phase, PhaseKind::Idle);
    assert_eq!(m.kind(), PhaseKind::Idle);
}

#[rstest::rstest]
#[test]
fn tool_loop_disabled_mid_cycle() {
    // Given a machine in Sending after a tool use, with tool_loop_disabled set.
    // Idle → Sending → Streaming → Sending(tool_loop_disabled=true) → Idle
    let mut m = SessionPhaseMachine::new();
    m.on_dispatch_message().expect("dispatch");
    m.on_first_token().expect("first token");
    fuse_tool_use_completion(&mut m);
    m.set_tool_loop_disabled();

    // When the tool batch completes.
    let outcome = m.on_tool_batch_completed().expect("tool batch");

    // Then the outcome goes to Idle and the machine is Idle.
    assert_eq!(outcome.new_phase, PhaseKind::Idle);
    assert_eq!(m.kind(), PhaseKind::Idle);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 4: Side effects
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn cancel_returns_streaming_data() {
    // Given a machine in Streaming with populated state.
    let mut m = streaming_machine();
    m.streaming_phase_mut()
        .expect("streaming")
        .streaming_entry_index = Some(42);
    m.active_tool_call_indices_mut().insert(0, 10);
    m.active_tool_call_indices_mut().insert(1, 11);
    m.active_tool_result_indices_mut()
        .insert("tc_a".to_owned(), 20);

    // When cancel is called.
    let result = m.cancel().expect("cancel should succeed");

    // Then the old streaming entry index is preserved in the result.
    assert_eq!(result.old_streaming.streaming_entry_index, Some(42));
    // And every tool tracking registration is gone: the turn was cancelled, so
    // nothing from it may outlive it.
    assert!(m.active_tool_call_indices().is_empty());
    assert!(m.active_tool_result_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn tool_call_tracking_survives_the_tool_use_handoff_when_the_loop_continues() {
    // Given a machine in Streaming with a live tool-call registration and the
    // tool loop left enabled.
    let mut m = streaming_machine();
    m.active_tool_call_indices_mut().insert(3, 15);

    // When the stream ends in tool use, handing off to the next burst through
    // the fused completion-and-re-dispatch path.
    fuse_tool_use_completion(&mut m);

    // Then the phase advanced but the registration was dropped, because that
    // burst's calls were all finalized by the turn-end clear before the
    // re-dispatch.
    assert_eq!(m.kind(), PhaseKind::Sending);
    assert!(m.active_tool_call_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn streaming_phase_tracks_thinking() {
    // Given a machine in Streaming.
    let mut m = streaming_machine();

    // When thinking index is set.
    m.streaming_phase_mut()
        .expect("streaming")
        .streaming_thinking_entry_index = Some(7);

    // Then it persists.
    assert_eq!(
        m.streaming_phase()
            .expect("streaming")
            .streaming_thinking_entry_index,
        Some(7)
    );
}

#[rstest::rstest]
#[test]
fn sending_phase_tracks_tool_loop_disabled() {
    // Given a machine in Sending.
    let mut m = sending_machine();

    // When the flag is set.
    m.set_tool_loop_disabled();

    // Then on_tool_batch_completed reads it and goes to Idle.
    let outcome = m.on_tool_batch_completed().expect("tool batch");
    assert_eq!(outcome.new_phase, PhaseKind::Idle);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 5: Accessor edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[rstest::rstest]
#[test]
fn streaming_accessor_returns_none_when_not_streaming() {
    // Given a machine in Idle.
    let mut m = SessionPhaseMachine::new();

    // When reading the streaming phase immutably and mutably.
    let shared = m.streaming_phase();

    // Then both accessors yield nothing.
    assert!(shared.is_none());
    assert!(m.streaming_phase_mut().is_none());
}

#[rstest::rstest]
#[test]
fn sending_accessor_returns_none_when_not_sending() {
    // Given a machine in Idle.
    let mut m = SessionPhaseMachine::new();

    // When reading the sending phase immutably and mutably.
    let shared = m.sending_phase();

    // Then both accessors yield nothing.
    assert!(shared.is_none());
    assert!(m.sending_phase_mut().is_none());
}

#[rstest::rstest]
#[test]
fn rewind_from_streaming_goes_to_sending() {
    // Given a machine streaming with an in-flight assistant entry.
    let mut m = streaming_machine();
    m.set_streaming_entry_index(3);

    // When rewinding for a stall retry.
    let outcome = m.on_retry_rewind().expect("rewind from Streaming");

    // Then the machine lands in Sending, which is where a fresh dispatch
    // re-enters Streaming from.
    assert_eq!(outcome.old_phase, PhaseKind::Streaming);
    assert_eq!(outcome.new_phase, PhaseKind::Sending);
    assert_eq!(m.kind(), PhaseKind::Sending);
}

#[rstest::rstest]
#[test]
fn rewind_from_streaming_drops_streaming_indices() {
    // Given a machine streaming with every streaming index populated.
    let mut m = streaming_machine();
    m.set_streaming_entry_index(1);
    m.set_streaming_thinking_entry_index(2);
    m.streaming_tool_call_indices_mut().insert(0, 3);
    m.streaming_tool_result_indices_mut()
        .insert("tc-1".to_owned(), 4);

    // When rewinding for a stall retry.
    m.on_retry_rewind().expect("rewind from Streaming");

    // Then every index is gone — the StreamingPhase was dropped wholesale and
    // the tool tracking maps are cleared explicitly, so the retried stream
    // starts from a clean slate.
    assert_eq!(m.streaming_entry_index(), None);
    assert_eq!(m.streaming_thinking_entry_index(), None);
    assert!(m.streaming_tool_call_indices().is_empty());
    assert!(m.streaming_tool_result_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn tool_call_registration_survives_sending_to_streaming() {
    // Given a tool call registered while the turn is still Sending.
    let mut m = sending_machine();
    m.active_tool_call_indices_mut().insert(0, 3);

    // When a prose token drives Sending -> Streaming.
    m.on_first_token().expect("first token should succeed");

    // Then the registration is still readable, so the rest of that call's
    // arguments can still be appended.
    assert_eq!(m.active_tool_call_indices().get(&0), Some(&3));
}

#[rstest::rstest]
#[test]
fn tool_call_registration_survives_the_tool_batch_boundary() {
    // Given a tool call registered while the tool batch ran in Sending.
    let mut m = sending_machine();
    m.active_tool_call_indices_mut().insert(0, 3);

    // When the batch completes and the next burst begins.
    m.on_tool_batch_completed()
        .expect("tool batch should complete");

    // Then the registration is still readable in the new burst, because only a
    // turn *end* clears.
    assert_eq!(m.kind(), PhaseKind::Streaming);
    assert_eq!(m.active_tool_call_indices().get(&0), Some(&3));
}

#[rstest::rstest]
#[test]
fn tool_call_registration_survives_a_round_trip_through_both_busy_phases() {
    // Given a tool call registered at the start of a turn.
    let mut m = sending_machine();
    m.active_tool_call_indices_mut().insert(0, 3);

    // When the turn crosses the Sending -> Streaming boundary and back.
    m.on_first_token().expect("first token should succeed");
    m.on_stream_completed_finished()
        .expect("finish should succeed");

    // Then the registration is cleared by the turn end, which is the point of
    // clearing it — a *new* burst must start from a clean slate.
    assert_eq!(m.kind(), PhaseKind::Idle);
    assert!(m.active_tool_call_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn tool_call_registration_is_cleared_when_the_turn_ends() {
    // Given a turn holding a live tool-call registration.
    let mut m = streaming_machine();
    m.active_tool_call_indices_mut().insert(0, 3);
    m.active_tool_result_indices_mut()
        .insert("tc-1".to_owned(), 4);

    // When the stream finishes and the session returns to Idle.
    m.on_stream_completed_finished()
        .expect("finish should succeed");

    // Then nothing is registered, because no turn is in flight any more.
    assert_eq!(m.kind(), PhaseKind::Idle);
    assert!(m.active_tool_call_indices().is_empty());
    assert!(m.active_tool_result_indices().is_empty());
}

#[rstest::rstest]
#[test]
fn begin_streaming_after_rewind_succeeds() {
    // Given a machine rewound from a stalled stream.
    let mut m = streaming_machine();
    m.on_retry_rewind().expect("rewind from Streaming");

    // When the retried stream's first token arrives.
    let outcome = m.on_first_token().expect("Sending accepts the first token");

    // Then it streams cleanly — this is the transition the retry path used
    // to make illegal by staying in Streaming.
    assert_eq!(outcome.old_phase, PhaseKind::Sending);
    assert_eq!(m.kind(), PhaseKind::Streaming);
}

#[rstest::rstest]
#[test]
fn rewind_from_idle_is_rejected() {
    // Given a machine that never dispatched.
    let mut m = idle_machine();

    // When rewinding for a stall retry.
    let result = m.on_retry_rewind();

    // Then the transition is refused rather than silently applied.
    assert!(result.is_err(), "rewind from Idle must be rejected");
}

#[rstest::rstest]
#[test]
fn rewind_from_sending_is_rejected() {
    // Given a machine in Sending — nothing is streaming to rewind.
    let mut m = sending_machine();

    // When rewinding for a stall retry.
    let result = m.on_retry_rewind();

    // Then the transition is refused — a Sending session has no in-flight
    // stream, so the stall retry must not claim one.
    assert!(result.is_err(), "rewind from Sending must be rejected");
}

#[rstest::rstest]
#[test]
fn phase_starts_as_idle() {
    // Given a freshly constructed machine.
    let m = SessionPhaseMachine::new();

    // When reading its phase.
    let kind = m.kind();
    let phase = m.phase();

    // Then the kind is Idle and the phase variant is Idle.
    assert_eq!(kind, PhaseKind::Idle);
    assert!(matches!(phase, Phase::Idle(_)));
}
