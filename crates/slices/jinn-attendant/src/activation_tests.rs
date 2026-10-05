//! Tests for the pure run-preparation helpers.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_attendant_msg::{
    NO_PARENT_SESSION_TEXT, NO_PRIOR_REPORT_TEXT, PRIOR_REPORT_PLACEHOLDER, default_seed_template,
};
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::{ContextOverride, PinPosition};
use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::state::State;
use jinn_session_msg::SessionOrigin;
use jinn_session_state::ChatSessionState;

use crate::activation::{prepare_trigger_run, render_seed_text, reset_context};

/// A stand-in parent id, so the appended line is visible in exact-match
/// assertions and obvious in the failure output when it changes.
const PARENT_ID: &str = "parent-1";

#[rstest::rstest]
#[case(
    "check: <prior report>",
    "the build was green",
    "check: the build was green\n\nThe parent session's id is parent-1"
)]
#[case(
    "fixed prompt",
    "the build was green",
    "fixed prompt\n\nthe build was green\n\nThe parent session's id is parent-1"
)]
fn seed_rendering_substitutes_or_appends(
    #[case] template: &str,
    #[case] prior: &str,
    #[case] expected: &str,
) {
    // Given a seed template and whatever the prior run reported.

    // When the run's seed text is rendered.
    let rendered = render_seed_text(template, Some(prior), PARENT_ID);

    // Then the placeholder is substituted, or the report is appended when
    // the template has none.
    assert_eq!(rendered.as_deref(), Some(expected));
}

#[rstest::rstest]
fn a_first_run_substitutes_the_no_report_sentence() {
    // Given a template still carrying its placeholder, on the first run.

    // When the seed text is rendered with no prior report.
    let rendered = render_seed_text("check: <prior report>", None, PARENT_ID).expect("seeds");

    // Then the placeholder becomes a plain first-run sentence. Shipping the
    // raw token would put `<prior report>` in front of the model as if it
    // were the user having asked about a report that does not exist.
    assert_eq!(
        rendered,
        format!("check: {NO_PRIOR_REPORT_TEXT}\n\nThe parent session's id is {PARENT_ID}")
    );
}

#[rstest::rstest]
fn no_placeholder_survives_into_a_first_run_prompt() {
    // Given the default template, on the first run.

    // When the seed text is rendered.
    let rendered = render_seed_text(
        &jinn_attendant_msg::default_seed_template(),
        None,
        PARENT_ID,
    )
    .expect("seeds");

    // Then the raw token is gone — a prompt is user-visible, and the token
    // is a template instruction, not something the model should be handed.
    assert!(
        !rendered.contains(PRIOR_REPORT_PLACEHOLDER),
        "the token must not reach the model: {rendered}"
    );
    assert!(rendered.contains(NO_PRIOR_REPORT_TEXT));
}

#[rstest::rstest]
fn a_placeholder_free_template_passes_through_unchanged() {
    // Given a template the user wrote with no placeholder in it.

    // When it is rendered on a run with no prior report.
    let rendered = render_seed_text("count the files in src/", None, PARENT_ID);

    // Then it is passed through verbatim. A template that does not ask for
    // the report must not be given one — the substitution is a plain string
    // replace, and there is simply no token to replace.
    assert_eq!(
        rendered.as_deref(),
        Some("count the files in src/\n\nThe parent session's id is parent-1")
    );
}

#[rstest::rstest]
#[test]
fn empty_template_injects_nothing() {
    // Given an attendant whose user cleared the seed template.

    // When the seed text is rendered, with and without a prior report.
    let without_prior = render_seed_text("", None, PARENT_ID);
    let with_prior = render_seed_text("", Some("a finding"), PARENT_ID);

    // Then neither produces a seed — an empty template means no injection.
    assert_eq!(without_prior, None);
    assert_eq!(with_prior, None);
}

#[rstest::rstest]
#[test]
fn default_template_carries_the_placeholder() {
    // Given the shipped default template.

    // When it is rendered against a prior report.
    let rendered = render_seed_text(&default_seed_template(), Some("finding"), PARENT_ID);

    // Then the placeholder was substituted — the default is usable as-is.
    assert!(rendered.is_some_and(|text| !text.contains(PRIOR_REPORT_PLACEHOLDER)));
}

#[rstest::rstest]
fn the_default_template_prescribes_no_particular_work() {
    // Given the shipped default template.

    // When its wording is read.
    let template = default_seed_template();

    // Then it only describes the situation. The attendant feature is not
    // code-specific — a default that tells the model to check code against
    // the current code misleads every attendant that is not inspecting code,
    // and the user has to notice and rewrite it.
    assert!(
        !template.to_lowercase().contains("code"),
        "the default template must not presume a code-review task: {template}"
    );
    assert!(!template.contains("Confirm or refute"));
}

/// An attendant of `parent` that has finished composing.
///
/// `new_attendant` alone leaves it in prep mode — the state `N` creates it
/// in — so a test about a run has to say that composition ended. Doing it
/// here keeps that one line out of eighteen fixtures.
fn composed_attendant_of(parent: &ChatSessionState) -> ChatSessionState {
    let mut attendant = ChatSessionState::new_attendant(parent, true);
    attendant.set_attendant_is_prepping(false);
    attendant
}

/// Builds a session with one pinned and two unpinned entries, returning
/// (session, pinned_id, unpinned_ids).
fn session_with_pins() -> (
    ChatSessionState,
    jinn_core_types::ChatEntryId,
    Vec<jinn_core_types::ChatEntryId>,
) {
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("pinned instructions"));
    let pinned_id = session.history()[0].id.clone();
    session.pin_entry(&pinned_id, PinPosition::Relative);
    session.push_entry(ChatEntry::assistant("an answer"));
    session.push_entry(ChatEntry::user("a follow-up"));
    let unpinned: Vec<_> = session.history()[1..]
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    (session, pinned_id, unpinned)
}

#[rstest::rstest]
#[test]
fn reset_context_excludes_every_non_pinned_entry() {
    // Given a session with one pinned and two unpinned entries.
    let (mut session, pinned_id, unpinned) = session_with_pins();

    // When the context is reset.
    let changed = reset_context(&mut session);

    // Then only the unpinned entries were excluded — the model will see
    // exactly the pins.
    assert_eq!(changed.len(), 2);
    assert!(changed.contains(&unpinned[0]));
    assert!(changed.contains(&unpinned[1]));
    let history = session.history();
    assert_ne!(
        history[0].context_override(),
        ContextOverride::ForcedExclude,
        "the pinned entry must survive the reset"
    );
    assert_eq!(
        history[1].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(history[0].id, pinned_id);
}

#[rstest::rstest]
#[test]
fn reset_context_is_idempotent() {
    // Given a session whose context has already been reset once.
    let (mut session, _pinned, _unpinned) = session_with_pins();
    let _changed = reset_context(&mut session);

    // When the context is reset again.
    let changed = reset_context(&mut session);

    // Then nothing changes — already-excluded entries are no-ops.
    assert!(changed.is_empty());
}

#[rstest::rstest]
#[test]
fn preserve_behavior_dispatches_the_template_without_resetting_context() {
    // Given a preserve-behavior attendant with a prior report.
    let parent = ChatSessionState::new();
    let mut session = composed_attendant_of(&parent);
    session.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Preserve);
    session.append_attendant_report("a finding".to_owned());
    session.set_seed_template("carry on".to_owned());

    // When the run's seed entry is prepared.
    let (seed, reset) = prepare_trigger_run(&mut session);

    // Then the template is dispatched. The earlier design returned nothing
    // here, which left every parent-completed preserve attendant inert.
    let seed = seed.expect("preserve must still dispatch the template");
    assert!(
        seed.text().starts_with("carry on"),
        "the run leads with the user's template: {:?}",
        seed.text()
    );
    // And nothing was force-excluded — preserving context is the whole behavior.
    assert!(reset.is_empty(), "preserve must not force-exclude anything");
}

#[rstest::rstest]
#[test]
fn reset_run_seeds_through_the_template_with_the_prior_report() {
    // Given a reset attendant that reported once.
    let parent = ChatSessionState::new();
    let mut session = composed_attendant_of(&parent);
    session.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
    session.append_attendant_report("the tests were actually passing".to_owned());

    // When the run's seed entry is prepared.
    let (seed, _reset) = prepare_trigger_run(&mut session);
    let seed = seed.expect("reset mode with a prior report seeds");

    // Then the seed entry carries the report through the template.
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } = &seed.kind else {
        panic!("seed must be a user entry");
    };
    assert!(display.contains("the tests were actually passing"));
    assert!(!display.contains(PRIOR_REPORT_PLACEHOLDER));
}

#[rstest::rstest]
#[test]
fn attendant_created_from_parent_links_and_defaults() {
    // Given a parent with environment values.
    let mut parent = ChatSessionState::new();
    parent.set_project(Some(std::path::PathBuf::from("/tmp/p")));

    // When an attendant is created from it.
    let attendant = ChatSessionState::new_attendant(&parent, true);

    // Then the attendant links the parent and starts in prep mode: `N`
    // hands the user an attendant they have not finished writing.
    assert_eq!(attendant.origin(), SessionOrigin::Attendant);
    assert!(attendant.attendant_is_prepping());
    assert_eq!(attendant.project(), Some(std::path::Path::new("/tmp/p")));
    assert!(attendant.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn prep_mode_makes_the_trigger_inert_end_to_end() {
    // Given a parent with a ParentCompleted attendant still in prep mode,
    // and the trigger actor live on the bus.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let canceled = harness
        .spawn_recorder::<jinn_inference_msg::CancelTurn>()
        .await;
    let state = State::new(AppState::default());
    // The default state pre-seeds an active session, so the fixture parent
    // must be identified by id at fixture time — a `find` over the map
    // would race the HashMap's per-process iteration order and sometimes
    // publish the completion against the wrong (default) session.
    let parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        // Built by `new_attendant` and left composing: the user has not
        // finished writing it, so its trigger is written down and inert.
        let mut attendant = {
            let read = s.session.get(&id).expect("parent").clone();
            ChatSessionState::new_attendant(&read, true)
        };
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_seed_template("re-check".to_owned());
        s.session.insert(attendant);
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Then nothing dispatches and nothing cancels — prep mode is inert, and
    // the trigger that is configured on it is not enough to override it.
    assert!(dispatched.is_empty(), "prep mode must not dispatch");
    assert!(canceled.is_empty(), "prep mode must not cancel");
}

#[rstest::rstest]
#[tokio::test]
async fn succeeded_parent_turn_fires_its_triggered_attendant() {
    // Given a parent with a reset-activated ParentCompleted attendant.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    // The default state pre-seeds an active session, so the fixture parent
    // must be identified by id at fixture time — a `find` over the map
    // would race the HashMap's per-process iteration order and sometimes
    // publish the completion against the wrong (default) session.
    let parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        let mut attendant = {
            let read = s.session.get(&id).expect("parent").clone();
            composed_attendant_of(&read)
        };
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_seed_template("verify: <prior report>".to_owned());
        attendant.append_attendant_report("prior finding".to_owned());
        s.session.insert(attendant);
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    let dispatches = jinn_testutil::bus_harness::await_recorded::<
        jinn_chat_input_msg::EnqueueUserMessage,
    >(&dispatched, 1, std::time::Duration::from_secs(10))
    .await;

    // Then exactly one dispatch went to the attendant, seeded through the
    // template with the prior report.
    assert_eq!(dispatches.len(), 1);
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } =
        &dispatches[0].entry.kind
    else {
        panic!("seed must be a user entry");
    };
    assert!(
        display.starts_with("verify: prior finding"),
        "the seeded prompt leads with the user's template: {display:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn errored_and_canceled_turns_fire_nothing() {
    // Given a parent with a reset-activated ParentCompleted attendant.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    // Same fixture rule: parent id captured at fixture time (the default
    // state pre-seeds an active session).
    let parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        let mut attendant = {
            let read = s.session.get(&id).expect("parent").clone();
            composed_attendant_of(&read)
        };
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        s.session.insert(attendant);
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When an errored completion is published, then a cancelled one.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id.clone(),
            outcome: jinn_session_msg::TurnOutcome::Error,
        })
        .await;
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Canceled,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Then nothing dispatches — only success is worth verifying against.
    assert!(
        dispatched.is_empty(),
        "an errored or cancelled turn must not fire attendants"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_attendant_created_after_the_parent_completed_never_fires_retroactively() {
    // Given a parent whose turn already completed successfully — with no
    // attendant existing at that moment — and an attendant added afterward
    // with a ParentCompleted trigger.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    let _parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's completion is published (nothing was listening for
    // this attendant — it does not exist yet), and only *then* is the
    // attendant created with a fired trigger.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    {
        let mut s = state.write();
        let parent_read = s
            .session
            .iter()
            .next()
            .map(|(_, p)| p.clone())
            .expect("parent");
        let mut attendant = composed_attendant_of(&parent_read);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        s.session.insert(attendant);
    }

    // Then no dispatch ever happens: the trigger query only runs on a live
    // `TurnCompleted`, never against the session map's current shape.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let dispatches = dispatched.drain();
    assert!(
        dispatches.is_empty(),
        "an attendant created after the fact must not fire, got {dispatches:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn manual_rerun_runs_a_late_attendant_even_though_its_trigger_never_fired() {
    // Given an attendant created after its parent completed.
    let state = State::new(AppState::default());
    let (attendant_id, parent_id) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        let mut attendant = composed_attendant_of(&parent);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.append_attendant_report("late finding".to_owned());
        attendant.set_seed_template("verify: <prior report>".to_owned());
        let id = attendant.session_id().clone();
        s.session.insert(attendant);
        (id, parent_id)
    };

    // When the user re-runs it with `R`.
    let (cancel, dispatch, _reset) =
        crate::rerun::rerun(&state, &attendant_id).expect("rerun allowed");

    // Then the run dispatches the seeded turn regardless of the trigger's
    // history — manual re-run has no trigger condition.
    assert!(cancel.is_none());
    let dispatch = dispatch.expect("reset mode dispatches");
    let jinn_core_types::chat_entry::ChatEntryKind::User { display, .. } = &dispatch.entry.kind
    else {
        panic!("seed must be a user entry");
    };
    assert!(
        display.starts_with("verify: late finding"),
        "the seeded prompt leads with the user's template: {display:?}"
    );
    assert!(
        display.contains(&parent_id.to_string()),
        "the seeded prompt names the parent session: {display:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn manual_trigger_attendant_does_not_fire_on_parent_completion() {
    // Given a parent with a reset-activated but Manual-trigger attendant.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    // Same fixture rule: parent id captured at fixture time (the default
    // state pre-seeds an active session).
    let parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        let mut attendant = {
            let read = s.session.get(&id).expect("parent").clone();
            composed_attendant_of(&read)
        };
        // Trigger stays Manual.
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        s.session.insert(attendant);
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Then nothing dispatches — the attendant only runs when asked.
    assert!(dispatched.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn every_sibling_attendant_fires_when_the_parent_turn_succeeds() {
    // Given a parent watched by three sibling attendants, all dispatchable
    // and all on the parent-completed trigger.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    let parent_id = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let id = parent.session_id().clone();
        s.session.insert(parent);
        for index in 0..3 {
            let mut attendant = {
                let read = s.session.get(&id).expect("parent").clone();
                composed_attendant_of(&read)
            };
            attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
            attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
            attendant.set_seed_template(format!("check-{index}: <prior report>"));
            attendant.append_attendant_report("prior finding".to_owned());
            s.session.insert(attendant);
        }
        id
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id.clone(),
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    let dispatches = jinn_testutil::bus_harness::await_recorded::<
        jinn_chat_input_msg::EnqueueUserMessage,
    >(&dispatched, 3, std::time::Duration::from_secs(10))
    .await;

    // Then every sibling fired. One firing per parent completion is the
    // whole contract: a parent with three watchers runs all three, or the
    // user is silently left waiting on the two that never start.
    assert_eq!(dispatches.len(), 3);
    // And each sibling got its own seeded prompt, not a repeat of one.
    let parent_label = parent_id.to_string();
    let mut seeded: Vec<String> = dispatches.iter().map(|d| d.entry.text().clone()).collect();
    seeded.sort();
    assert_eq!(
        seeded,
        (0..3)
            .map(|i| {
                format!("check-{i}: prior finding\n\nThe parent session's id is {parent_label}")
            })
            .collect::<Vec<String>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_attendant_nested_under_another_attendant_does_not_fire_from_the_parents_completion() {
    // Given a root session with two dispatchable parent-completed
    // attendants, and a third attendant nested under the first one.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    // Each attendant is built from an already-cloned parent, so no read
    // guard is ever taken while the write guard below is held.
    let arm = |parent: &ChatSessionState| {
        let mut attendant = composed_attendant_of(parent);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_seed_template("check".to_owned());
        attendant.append_attendant_report("prior finding".to_owned());
        attendant
    };
    let (root_id, nested_id) = {
        let mut s = state.write();
        let root = ChatSessionState::new();
        let root_id = root.session_id().clone();
        s.session.insert(root.clone());
        let first = arm(&root);
        let nested = arm(&first);
        s.session.insert(first);
        s.session.insert(arm(&root));
        let nested_id = nested.session_id().clone();
        s.session.insert(nested);
        (root_id, nested_id)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the root's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: root_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Then the nested attendant fires too. "Everything under this session"
    // is the contract; it is not one hop.
    let seen: Vec<jinn_core_types::SessionId> = dispatched
        .drain()
        .iter()
        .map(|d| d.session_id.clone())
        .collect();
    assert!(
        seen.contains(&nested_id),
        "an attendant nested under another attendant must fire from the root's completion; dispatched to {seen:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_cyclic_parent_link_does_not_hang_the_trigger_walk() {
    // Given two attendants that each claim the other as their parent, both
    // dispatchable and parent-completed.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let dispatched = harness
        .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
        .await;
    let state = State::new(AppState::default());
    let (root_id, first_id) = {
        let mut s = state.write();
        let root = ChatSessionState::new();
        let root_id = root.session_id().clone();
        s.session.insert(root.clone());
        let arm = |parent: &ChatSessionState| {
            let mut attendant = composed_attendant_of(parent);
            attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
            attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
            attendant.set_seed_template("check".to_owned());
            attendant.append_attendant_report("prior finding".to_owned());
            attendant
        };
        let first = arm(&root);
        let first_id = first.session_id().clone();
        s.session.insert(first);
        // A second attendant under the root, then re-point the first's
        // parent at it so the chain loops.
        let second = arm(&root);
        let second_id = second.session_id().clone();
        s.session.insert(second);
        if let Some(a) = s.session.get_mut(&first_id) {
            a.set_parent_session(second_id);
        }
        (root_id, first_id)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the root's turn completes.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: root_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Then the walk terminated and each attendant fired exactly once. A
    // cycle is a corrupt tree, not a reason to stop serving the rest.
    let seen: Vec<jinn_core_types::SessionId> = dispatched
        .drain()
        .iter()
        .map(|d| d.session_id.clone())
        .collect();
    assert_eq!(
        seen.iter().filter(|id| **id == first_id).count(),
        1,
        "a cyclic parent link must not fire an attendant twice; dispatched to {seen:?}"
    );
}

/// Builds state plus a live session-turn actor, so an `EnqueueUserMessage`
/// actually lands in a session's history rather than stopping at the bus.
///
/// Without the session actor a test can only count published dispatches,
/// which is exactly the assertion that missed this bug: a dispatch can be
/// published and still never reach its own session.
async fn harness_with_session_actor() -> (jinn_testutil::bus_harness::TestHarness, State) {
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let state = State::new(AppState::default());
    let deps = ActorDeps {
        services: {
            let mut services =
                jinn_kernel::common::services::test_services::TestServices::builder()
                    .paths(jinn_kernel::common::app_paths::AppPaths::new_in(
                        std::path::Path::new(""),
                    ))
                    .build();
            services.bus = harness.bus();
            services.trouper_system = harness.system().clone();
            services
        },
    };
    let system = deps.services.trouper_system.clone();
    drop(jinn_context_assembly::service::ensure_spawned(&system));
    jinn_session_turn::activate(
        &system,
        jinn_session_turn::session_actor::SessionPersistenceActorDeps {
            deps,
            state: state.clone(),
            counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
            token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
            image_converter: jinn_llm_support::image_convert::ImageConverterService::system(),
        },
    );
    (harness, state)
}

#[rstest::rstest]
#[tokio::test]
async fn every_sibling_attendant_lands_its_own_seeded_entry() {
    // Given a parent watched by five dispatchable parent-completed
    // attendants, with the session-turn actor live on the same state.
    let (harness, state) = harness_with_session_actor().await;
    let landed = harness
        .spawn_recorder::<jinn_chat_input_msg::ChatEntrySubmitted>()
        .await;
    let (parent_id, sibling_ids) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        s.session.insert(parent.clone());
        let mut ids = Vec::new();
        for index in 0..5 {
            let mut attendant = composed_attendant_of(&parent);
            attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
            attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
            attendant.set_seed_template(format!("check-{index}: <prior report>"));
            attendant.append_attendant_report("prior finding".to_owned());
            ids.push(attendant.session_id().clone());
            s.session.insert(attendant);
        }
        (parent_id, ids)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;

    let submissions = jinn_testutil::bus_harness::await_recorded::<
        jinn_chat_input_msg::ChatEntrySubmitted,
    >(&landed, 5, std::time::Duration::from_secs(15))
    .await;

    // Then all five seeded entries landed, one per session. A published
    // dispatch is not a landed entry, and the difference is invisible
    // everywhere except here: the attendant simply sits idle.
    let mut got: Vec<String> = submissions
        .iter()
        .map(|s| s.session_id.to_string())
        .collect();
    got.sort();
    let mut want: Vec<String> = sibling_ids
        .iter()
        .map(jinn_core_types::SessionId::to_string)
        .collect();
    want.sort();
    assert_eq!(
        got, want,
        "every attendant must land its own seeded entry, not just the first"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_attendant_built_the_way_the_ui_builds_one_still_fires() {
    // Given two attendants constructed exactly as `N` constructs them —
    // `new_attendant` and nothing else — with the user having since flipped
    // the *first* out of seed, but not the second.
    let (harness, state) = harness_with_session_actor().await;
    let landed = harness
        .spawn_recorder::<jinn_chat_input_msg::ChatEntrySubmitted>()
        .await;
    let (parent_id, first_id, second_id) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        s.session.insert(parent.clone());
        let mut first = composed_attendant_of(&parent);
        first.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        // The user configured this one and left seed.
        first.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        first.set_seed_template("first: <prior report>".to_owned());
        first.append_attendant_report("prior".to_owned());
        let first_id = first.session_id().clone();
        s.session.insert(first);
        let mut second = ChatSessionState::new_attendant(&parent, true);
        second.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        second.set_seed_template("second: <prior report>".to_owned());
        let second_id = second.session_id().clone();
        s.session.insert(second);
        (parent_id, first_id, second_id)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    let submissions = jinn_testutil::bus_harness::await_recorded::<
        jinn_chat_input_msg::ChatEntrySubmitted,
    >(&landed, 1, std::time::Duration::from_secs(10))
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Then the configured attendant fired, and the still-seeded one is
    // reported as *skipped by mode* rather than silently swallowed.
    let fired: Vec<String> = submissions
        .iter()
        .map(|x| x.session_id.to_string())
        .collect();
    assert_eq!(
        fired,
        vec![first_id.to_string()],
        "the configured attendant must fire; a seed-mode attendant is inert by design"
    );
    // And the seed-mode attendant's inertness is visible, not invisible: it
    // is reported so the user can see why its peer ran and it did not.
    let guard = state.read();
    let second_texts: Vec<String> = guard
        .session
        .get(&second_id)
        .expect("second exists")
        .history()
        .iter()
        .map(|e| e.text().clone())
        .collect();
    assert!(
        second_texts.is_empty(),
        "a seed-mode attendant must not dispatch: {second_texts:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_trigger_does_not_cancel_an_attendant_that_is_already_running() {
    // Given a Streaming parent-completed attendant.
    let harness = jinn_testutil::bus_harness::TestHarness::new().await;
    let state = State::new(AppState::default());
    let (attendant_id, parent_id) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        s.session.insert(parent.clone());
        let mut attendant = composed_attendant_of(&parent);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_seed_template("seeded: <prior report>".to_owned());
        attendant.append_attendant_report("prior".to_owned());
        let id = attendant.session_id().clone();
        s.session.insert(attendant);
        {
            let session = s.session.get_mut(&id).expect("just inserted");
            session.push_entry(ChatEntry::user("in flight"));
            session.begin_streaming();
        }
        (id, parent_id)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes and the trigger fires the busy
    // attendant.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Then the running turn is untouched. A trigger answers a parent's
    // completion; it is not a user asking for this work to be abandoned, so
    // it has no business cancelling a turn already in flight.
    let guard = state.read();
    let session = guard.session.get(&attendant_id).expect("exists");
    assert_eq!(
        session.phase(),
        jinn_session_msg::PhaseKind::Streaming,
        "a trigger must not cancel an attendant that is already running"
    );
    // And the running turn is not recorded as cancelled.
    let texts: Vec<String> = session.history().iter().map(|e| e.text().clone()).collect();
    assert!(
        !texts.iter().any(|t| t == "Cancelled"),
        "a superseded turn must not leave a Cancelled entry behind: {texts:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_trigger_on_a_busy_attendant_queues_its_seeded_turn_for_after_the_current_one() {
    // Given a Streaming parent-completed attendant, with the session-turn
    // actor live so the enqueue is handled the way it is in production.
    let (harness, state) = harness_with_session_actor().await;
    let (attendant_id, parent_id) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        s.session.insert(parent.clone());
        let mut attendant = composed_attendant_of(&parent);
        attendant.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
        attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
        attendant.set_seed_template("seeded: <prior report>".to_owned());
        attendant.append_attendant_report("prior".to_owned());
        let id = attendant.session_id().clone();
        s.session.insert(attendant);
        {
            let session = s.session.get_mut(&id).expect("just inserted");
            session.push_entry(ChatEntry::user("in flight"));
            session.begin_streaming();
        }
        (id, parent_id)
    };
    let _actor = crate::trigger_actor::AttendantTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::AttendantTriggerActorDeps {
            services: harness.services().await,
            state: state.clone(),
        },
    );

    // When the parent's turn completes.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id.clone(),
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Then the seeded turn is held in the session's queue, not in the input
    // box and not lost. The enqueue handler queues anything arriving while
    // a session is Sending/Streaming, and that is the correct outcome: the
    // attendant is mid-answer, so its new question runs next.
    let guard = state.read();
    let session = guard.session.get(&attendant_id).expect("exists");
    let queued: Vec<String> = session
        .message_queue()
        .items()
        .iter()
        .map(|item| match item {
            jinn_turn_dispatch_msg::QueueItem::UserMessage(entry) => entry.text().clone(),
            jinn_turn_dispatch_msg::QueueItem::ToolContinuation => "tool-continuation".to_owned(),
        })
        .collect();
    assert_eq!(
        queued,
        vec![format!(
            "seeded: prior\n\nThe parent session's id is {parent_id}"
        )],
        "the seeded turn must wait in the queue behind the running one"
    );
    // And nothing put it in the input box.
    let draft = session.with_input(|i| i.text().to_owned(), String::new);
    assert!(
        draft.is_empty(),
        "a trigger must never put its seeded turn in the input box: {draft:?}"
    );
}

#[rstest::rstest]
fn the_seed_prompt_names_the_parent_session_so_the_attendant_can_search_it() {
    // Given a composed attendant whose user template says nothing about its
    // parent.
    let parent = ChatSessionState::new();
    let mut attendant = composed_attendant_of(&parent);
    attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
    attendant.set_seed_template("Summarise the work.".to_owned());

    // When the seed prompt is built.
    let (entry, _) = prepare_trigger_run(&mut attendant);
    let entry = entry.expect("a seed entry");

    // Then the prompt carries the parent's session id. The attendant runs
    // in its own session and has no way to reach the parent's transcript
    // without it — a session-search tool needs an id to search by.
    let parent_id = parent.session_id().to_string();
    assert!(
        entry.text().contains(&parent_id),
        "the seed prompt must name the parent session {}; got {:?}",
        parent_id,
        entry.text()
    );
}

#[rstest::rstest]
fn the_parent_session_id_is_appended_rather_than_substituted_into_the_users_template() {
    // Given a composed attendant with a user template carrying its own text.
    let parent = ChatSessionState::new();
    let mut attendant = composed_attendant_of(&parent);
    attendant.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);
    attendant.set_seed_template("Summarise the work.".to_owned());
    attendant.append_attendant_report("prior finding".to_owned());

    // When the seed prompt is built.
    let (entry, _) = prepare_trigger_run(&mut attendant);
    let entry = entry.expect("a seed entry");
    let text = entry.text();

    // Then the user's own words are intact and the id is appended, so
    // nothing the user typed is reordered or rewritten.
    assert!(
        text.starts_with("Summarise the work."),
        "the user's template must lead the prompt: {text:?}"
    );
    assert!(
        text.contains(&parent.session_id().to_string()),
        "the parent id must still be present: {text:?}"
    );
}

#[rstest::rstest]
fn a_seed_prompt_names_a_missing_parent_as_unavailable() {
    // Given a template and no parent id to report.
    let template = "Summarise the work.";

    // When the prompt is rendered for a session that has no parent link.
    let rendered = render_seed_text(template, None, NO_PARENT_SESSION_TEXT);

    // Then the parent line still reads as a finished sentence. A prompt
    // trailing off after "is" is one the model tries to interpret; the word
    // says plainly that there is no id here to search by.
    assert!(
        rendered
            .expect("a rendered prompt")
            .ends_with("The parent session's id is unavailable"),
        "the parent line must still read as a sentence"
    );
}
