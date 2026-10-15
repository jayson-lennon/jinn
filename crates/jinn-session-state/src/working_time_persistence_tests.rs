//! Tests for working-time persistence on the session snapshot.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    reason = "test code"
)]

use jiff::{SignedDuration, Timestamp};
use jinn_core_types::WorkingInterval;

use crate::snapshot::SessionSnapshotMetadata;

fn at(offset_secs: i64) -> Timestamp {
    Timestamp::from_second(1_000_000_000 + offset_secs).expect("valid offset")
}

/// Metadata from a real session capture, so the test exercises the same
/// construction production does.
fn metadata() -> SessionSnapshotMetadata {
    crate::ChatSessionState::new().capture_snapshot().metadata
}

#[rstest::rstest]
fn metadata_carries_no_intervals_by_default() {
    // Given metadata built without working time.
    let metadata = metadata();

    // Then it records none.
    assert_eq!(
        metadata.working_intervals,
        [] as [jinn_core_types::WorkingInterval; 0]
    );
}

#[rstest::rstest]
fn metadata_written_before_working_time_existed_still_loads() {
    // Given metadata carrying no working-time key, as a snapshot persisted
    // before this feature would.
    let mut value = serde_json::to_value(metadata()).expect("serialize");
    value
        .as_object_mut()
        .expect("object")
        .remove("working_intervals");

    // When deserializing.
    let metadata: SessionSnapshotMetadata =
        serde_json::from_value(value).expect("pre-working-time metadata");

    // Then the load succeeds with none recorded, rather than failing.
    assert_eq!(
        metadata.working_intervals,
        [] as [jinn_core_types::WorkingInterval; 0]
    );
}

#[rstest::rstest]
fn working_intervals_survive_a_json_roundtrip() {
    // Given metadata carrying a session's recorded working intervals.
    let mut metadata = metadata();
    metadata.working_intervals = vec![
        WorkingInterval::closed(at(0), at(10)),
        WorkingInterval::closed(at(20), at(25)),
    ];

    // When serializing and deserializing.
    let json = serde_json::to_string(&metadata).expect("serialize");
    let round: SessionSnapshotMetadata = serde_json::from_str(&json).expect("deserialize");

    // Then every interval survives, so working time is not lost on restart.
    assert_eq!(round.working_intervals, metadata.working_intervals);
}

#[rstest::rstest]
fn an_open_interval_survives_a_json_roundtrip() {
    // Given metadata carrying an interval left open by a killed session.
    let mut metadata = metadata();
    metadata.working_intervals = vec![WorkingInterval::open(at(0))];

    // When serializing and deserializing.
    let json = serde_json::to_string(&metadata).expect("serialize");
    let round: SessionSnapshotMetadata = serde_json::from_str(&json).expect("deserialize");

    // Then it is still open, so the load path can close it and bill only up to
    // the moment the work stopped.
    assert!(
        round
            .working_intervals
            .first()
            .expect("interval survived")
            .is_open()
    );
}

#[rstest::rstest]
fn a_snapshot_captured_from_a_session_carries_no_intervals() {
    // Given a live session that has worked.
    let mut session = crate::ChatSessionState::new();
    session.begin_streaming();

    // When capturing a snapshot from it.
    let snapshot = session.capture_snapshot();

    // Then the snapshot carries none: working time lives in the work-time
    // cell, and the core must not become a second copy with a second writer.
    assert_eq!(
        snapshot.metadata.working_intervals,
        [] as [jinn_core_types::WorkingInterval; 0]
    );
}

#[rstest::rstest]
fn a_fork_does_not_inherit_the_parents_working_time() {
    // Given a source snapshot that recorded ten seconds of work.
    let mut source = crate::ChatSessionState::new().capture_snapshot();
    source.metadata.working_intervals = vec![WorkingInterval::closed(at(0), at(10))];

    // When forking it.
    let child = source.forked_from(jinn_core_types::SessionId::new(), 0);

    // Then the child starts with none of its own, rather than being billed for
    // the parent's history.
    assert_eq!(
        child.metadata.working_intervals,
        [] as [jinn_core_types::WorkingInterval; 0]
    );
}

#[rstest::rstest]
fn restored_intervals_total_the_recorded_span() {
    // Given metadata carrying two closed intervals.
    let mut metadata = metadata();
    metadata.working_intervals = vec![
        WorkingInterval::closed(at(0), at(10)),
        WorkingInterval::closed(at(20), at(25)),
    ];

    // When totalling them at a later moment.
    let total = jinn_core_types::total(&metadata.working_intervals, at(100));

    // Then the total is the 15 seconds recorded.
    assert_eq!(total, SignedDuration::from_secs(15));
}
