//! Tests for the manual rerun action.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_attendant_msg::AttendantBehavior;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::state::State;
use jinn_session_state::ChatSessionState;

use crate::rerun::{rerun, rerun_blocked_reason};

/// State holding one composed attendant (prep mode off) with the given
/// behavior, plus the parent it reports to.
fn state_with_attendant(behavior: AttendantBehavior) -> (State, jinn_core_types::SessionId) {
    let state = State::new(AppState::default());
    let id = {
        let mut guard = state.write();
        let parent = ChatSessionState::new();
        let mut attendant = ChatSessionState::new_attendant(&parent, true);
        attendant.set_attendant_behavior(behavior);
        // A fresh attendant is composing; these tests are about running
        // one, so composition ends here.
        attendant.set_attendant_is_prepping(false);
        let id = attendant.session_id().clone();
        guard.session.insert(attendant);
        guard.session.insert(parent);
        id
    };
    (state, id)
}

/// The parent id an attendant built by [`state_with_attendant`] reports to.
///
/// Read off the attendant's own link rather than by scanning the map: the
/// map holds both sessions and a wrong guess here fails as a confusing
/// assertion about the prompt text rather than about this helper.
fn parent_id_of(state: &State, attendant: &jinn_core_types::SessionId) -> String {
    let guard = state.read();
    guard
        .session
        .get(attendant)
        .and_then(|s| s.parent_session().clone())
        .map_or_else(String::new, |id| id.to_string())
}

#[rstest::rstest]
#[test]
fn rerun_on_a_reset_attendant_dispatches_the_seeded_run() {
    // Given a reset attendant with a prior report and a template.
    let (state, id) = state_with_attendant(AttendantBehavior::Reset);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.append_attendant_report("prior finding".to_owned());
        session.set_seed_template("verify: <prior report>".to_owned());
    }

    // When the attendant is re-run.
    let (cancel, dispatch, _reset) = rerun(&state, &id).expect("rerun allowed");

    // Then no cancel is needed (the session was idle) and the dispatch
    // carries the report through the template.
    assert!(cancel.is_none());
    let dispatch = dispatch.expect("reset mode dispatches");
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } = &dispatch.entry.kind
    else {
        panic!("seed must be a user entry");
    };
    assert!(
        display.starts_with("verify: prior finding"),
        "the seeded prompt leads with the user's template: {display:?}"
    );
    assert!(
        display.contains(&parent_id_of(&state, &id)),
        "the seeded prompt names the parent session: {display:?}"
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_busy_attendant_cancels_its_own_turn() {
    // Given a reset attendant whose turn is mid-flight.
    let (state, id) = state_with_attendant(AttendantBehavior::Reset);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.begin_streaming();
    }

    // When the attendant is re-run.
    let (cancel, dispatch, _reset) = rerun(&state, &id).expect("rerun allowed");

    // Then the in-flight turn is cancelled and a new run dispatches.
    assert_eq!(cancel.expect("busy session cancels").session_id, id);
    assert!(dispatch.is_some());
}

#[rstest::rstest]
#[test]
fn reset_run_excludes_non_pinned_entries_before_dispatching() {
    // Given a reset attendant with one pinned and two unpinned entries.
    let (state, id) = state_with_attendant(AttendantBehavior::Reset);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.push_entry(jinn_kernel::protocol::ChatEntry::user(
            "pinned instructions",
        ));
        let pinned_id = session.history()[0].id.clone();
        session.pin_entry(&pinned_id, jinn_core_types::PinPosition::Relative);
        session.push_entry(jinn_kernel::protocol::ChatEntry::assistant("an answer"));
        session.push_entry(jinn_kernel::protocol::ChatEntry::user("a follow-up"));
        session.set_seed_template("verify: <prior report>".to_owned());
        session.append_attendant_report("prior finding".to_owned());
    }

    // When the attendant is re-run.
    let (_, dispatch, _reset) = rerun(&state, &id).expect("rerun allowed");

    // Then the run still dispatches.
    assert!(dispatch.is_some(), "a reset run must still dispatch");
    // And every non-pinned entry is force-excluded from context, so the
    // model sees the pins alone.
    let guard = state.read();
    let session = guard.session.get(&id).expect("attendant");
    let history = session.history();
    assert_ne!(
        history[0].context_override(),
        jinn_core_types::ContextOverride::ForcedExclude,
        "the pinned entry must survive the reset"
    );
    assert_eq!(
        history[1].context_override(),
        jinn_core_types::ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[2].context_override(),
        jinn_core_types::ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[case(AttendantBehavior::Reset)]
#[case(AttendantBehavior::Preserve)]
fn an_attendants_run_is_persistable_in_every_dispatchable_mode(#[case] mode: AttendantBehavior) {
    // Given an attendant in the given mode.
    let (state, id) = state_with_attendant(mode);

    // When it is re-run.
    let (_, dispatch, _) = rerun(&state, &id).expect("rerun allowed");

    // Then the session is persistable, so the turn the run starts reaches
    // disk. An attendant that forgets its run on restart cannot be re-run
    // again, which is the whole feature.
    //
    // Persistability comes from the constructor rather than from the run, so
    // this is the property a mode switch has to preserve: editing the
    // prep mode in the properties popup must not drop the session out of
    // storage.
    let guard = state.read();
    assert!(
        guard.session.get(&id).expect("attendant").is_persistable(),
        "an attendant must stay persistable in {mode:?}"
    );
    // And the run was dispatched with the seeded prompt.
    assert!(dispatch.is_some(), "{mode:?} must dispatch a seeded run");
}

#[rstest::rstest]
fn reset_exclusions_survive_a_restart() {
    // Given a reset attendant that has been re-run once, so its context
    // was excluded in memory only.
    let (state, id) = state_with_attendant(AttendantBehavior::Reset);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.push_entry(jinn_kernel::protocol::ChatEntry::user("an answer"));
    }
    let (_, _, reset) = rerun(&state, &id).expect("rerun allowed");
    assert!(!reset.is_empty(), "the first reset has work to persist");

    // When the session is written to the store and read back into a fresh
    // shell, the way the load path rebuilds one.
    let guard = state.read();
    let session = guard.session.get(&id).expect("attendant");
    let written = session.capture_snapshot();
    let mut restored = jinn_session_state::ChatSessionState::new();
    restored.restore_history(written.entries);

    // Then the exclusion is still in force after the restart.
    let entry = restored.history()[0].clone();
    assert_eq!(
        entry.context_override(),
        jinn_core_types::ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_busy_attendant_cancels_before_it_dispatches_the_seed() {
    // Given a reset attendant whose turn is mid-flight.
    let (state, id) = state_with_attendant(AttendantBehavior::Reset);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.begin_streaming();
    }

    // When the attendant is re-run.
    let (cancel, dispatch, _reset) = rerun(&state, &id).expect("rerun allowed");

    // Then the cancel and the seed are both produced, and the cancel is
    // named before the dispatch.
    //
    // The phase settles when the session actor applies the cancel command
    // — nothing writes a phase synchronously any more — so what `R`
    // guarantees here is message order: the enqueue handler queues a user
    // message that arrives while the session is still `Sending`/
    // `Streaming`, and only the cancel applied ahead of it in the bus
    // order makes the seed dispatch instead of queue behind a dead turn.
    // The settle itself is the session actor's, covered by the cancel
    // tests there.
    assert_eq!(cancel.expect("busy session cancels").session_id, id);
    assert!(
        dispatch.is_some(),
        "the seeded run dispatches after the cancel"
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_preserve_attendant_seeds_through_the_template() {
    // Given a preserve-mode attendant with a prior report and a template.
    //
    // `R` is the user saying "ask again", so it goes through the template
    // and inserts the seeded message in every mode. Preserve mode governs
    // what an unattended trigger fire does — carry the context as-is — not
    // what a manual re-run does.
    let (state, id) = state_with_attendant(AttendantBehavior::Preserve);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("attendant");
        session.append_attendant_report("prior finding".to_owned());
        session.set_seed_template("verify: <prior report>".to_owned());
    }

    // When the attendant is re-run.
    let (_, dispatch, _) = rerun(&state, &id).expect("rerun allowed");

    // Then the seeded run dispatches with the report substituted.
    let dispatch = dispatch.expect("R must seed in every non-seed mode");
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } = &dispatch.entry.kind
    else {
        panic!("seed must be a user entry");
    };
    assert!(
        display.starts_with("verify: prior finding"),
        "the seeded prompt leads with the user's template: {display:?}"
    );
    assert!(
        display.contains(&parent_id_of(&state, &id)),
        "the seeded prompt names the parent session: {display:?}"
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_composing_attendant_is_refused() {
    // Given an attendant still being composed.
    let (state, id) = {
        let (state, id) = state_with_attendant(AttendantBehavior::Reset);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("attendant")
            .set_attendant_is_prepping(true);
        (state, id)
    };

    // When the attendant is re-run.
    let outcome = rerun(&state, &id);

    // Then nothing dispatches, and the reason names the state by the name
    // the panel shows it under.
    assert!(outcome.is_none());
    assert_eq!(
        rerun_blocked_reason(&state, &id),
        Some("attendant is in prep mode")
    );
}

#[rstest::rstest]
#[test]
fn rerun_on_a_non_attendant_session_is_refused() {
    // Given a plain user session.
    let state = State::new(AppState::default());
    let id = state.read().session.active_session_id().clone();

    // When it is re-run as an attendant.
    let outcome = rerun(&state, &id);

    // Then nothing dispatches, and the reason names the kind.
    assert!(outcome.is_none());
    assert_eq!(rerun_blocked_reason(&state, &id), Some("not an attendant"));
}
