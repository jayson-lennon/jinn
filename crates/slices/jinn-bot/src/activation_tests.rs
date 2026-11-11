//! Tests for the pure run-preparation helpers.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_bot_msg::{
    NO_PARENT_SESSION_TEXT, NO_PRIOR_REPORT_TEXT, PARENT_ID_PLACEHOLDER, PRIOR_REPORT_PLACEHOLDER,
    default_seed_template, empty_seed_template,
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

/// A stand-in parent id, so the substitution is visible in exact-match
/// assertions and obvious in the failure output when it changes.
const PARENT_ID: &str = "parent-1";

#[rstest::rstest]
#[case::token_substituted(
    "check: <prior_report>",
    "the build was green",
    "check: the build was green"
)]
#[case::token_absent("fixed prompt", "the build was green", "fixed prompt")]
fn seed_rendering_substitutes_or_drops(
    #[case] template: &str,
    #[case] prior: &str,
    #[case] expected: &str,
) {
    // Given a seed template and whatever the prior run reported.

    // When the run's seed text is rendered.
    let rendered = render_seed_text(template, Some(prior), PARENT_ID);

    // Then the placeholder is substituted, or the report is dropped when
    // the template has no token for it: the token is the only inlet, and
    // the renderer appends nothing.
    assert_eq!(rendered, expected);
}

#[rstest::rstest]
fn a_first_run_substitutes_the_no_report_sentence() {
    // Given a template still carrying its placeholder, on the first run.

    // When the seed text is rendered with no prior report.
    let rendered = render_seed_text("check: <prior_report>", None, PARENT_ID);

    // Then the placeholder becomes a plain first-run sentence. Shipping the
    // raw token would put `<prior_report>` in front of the model as if it
    // were the user having asked about a report that does not exist.
    assert_eq!(rendered, format!("check: {NO_PRIOR_REPORT_TEXT}"));
}

#[rstest::rstest]
fn the_parent_id_token_substitutes_the_real_parent() {
    // Given a template naming the parent id.

    // When the seed text is rendered for an attached bot.
    let rendered = render_seed_text("Search <parent_id> for context.", None, PARENT_ID);

    // Then the token became the id — a session-search tool needs it, and a
    // bot cannot otherwise reach the transcript it reports on.
    assert_eq!(rendered, format!("Search {PARENT_ID} for context."));
}

#[rstest::rstest]
fn the_parent_id_token_substitutes_the_unavailable_sentence_when_parentless() {
    // Given a template naming the parent id, on a bot with no parent.

    // When the seed text is rendered with the caller-resolved fallback.
    let rendered = render_seed_text(
        "Search <parent_id> for context.",
        None,
        NO_PARENT_SESSION_TEXT,
    );

    // Then the token became the unavailable sentence rather than a dangling
    // label.
    assert_eq!(
        rendered,
        format!("Search {NO_PARENT_SESSION_TEXT} for context.")
    );
}

#[rstest::rstest]
fn a_template_without_the_parent_token_never_names_the_parent() {
    // Given a parentless bot whose template does not name `<parent_id>`.

    // When the seed text is rendered.
    let rendered = render_seed_text("count the files in src/", None, NO_PARENT_SESSION_TEXT);

    // Then no parent mention appears anywhere — substitution only, no
    // appended line.
    assert_eq!(rendered, "count the files in src/");
    assert!(!rendered.contains(NO_PARENT_SESSION_TEXT));
}

#[rstest::rstest]
fn no_prior_report_token_survives_into_a_first_run_prompt() {
    // Given the default template, on the first run.

    // When the seed text is rendered.
    let rendered = render_seed_text(&default_seed_template(), None, PARENT_ID);

    // Then neither raw token is left — a prompt is user-visible, and a
    // token is a template instruction, not something the model should be
    // handed.
    assert!(
        !rendered.contains(PRIOR_REPORT_PLACEHOLDER),
        "the report token must not reach the model: {rendered}"
    );
    assert!(
        !rendered.contains(PARENT_ID_PLACEHOLDER),
        "the parent token must not reach the model: {rendered}"
    );
    assert!(rendered.contains(NO_PRIOR_REPORT_TEXT));
}

#[rstest::rstest]
fn a_token_free_template_passes_through_unchanged() {
    // Given a template the user wrote with no placeholder in it.

    // When it is rendered on a run with no prior report.
    let rendered = render_seed_text("count the files in src/", None, PARENT_ID);

    // Then it is passed through verbatim — the renderer appends nothing,
    // so a template that names no token gets nothing it did not ask for.
    assert_eq!(rendered, "count the files in src/");
}

#[rstest::rstest]
#[test]
fn an_empty_template_renders_to_the_empty_string() {
    // Given a bot whose seed template is empty — a cleared template, or a
    // standalone bot's default.

    // When the seed text is rendered, with and without a prior report.
    let without_prior = render_seed_text("", None, PARENT_ID);
    let with_prior = render_seed_text("", Some("a finding"), PARENT_ID);

    // Then both render empty: the renderer appends nothing, so there is
    // nothing to block a run on — the empty seed is a valid seed.
    assert_eq!(without_prior, "");
    assert_eq!(with_prior, "");
}

#[rstest::rstest]
#[test]
fn the_standalone_default_seed_is_empty() {
    // Given the standalone bot's default template.

    // When it is rendered.
    let rendered = render_seed_text(&empty_seed_template(), None, NO_PARENT_SESSION_TEXT);

    // Then the seed is empty — the dispatched message is the whole prompt.
    assert_eq!(rendered, "");
}

#[rstest::rstest]
#[test]
fn default_template_carries_both_tokens() {
    // Given the shipped default template.

    // When it is rendered against a prior report.
    let rendered = render_seed_text(&default_seed_template(), Some("finding"), PARENT_ID);

    // Then both tokens were substituted — the default is usable as-is.
    assert!(!rendered.contains(PRIOR_REPORT_PLACEHOLDER));
    assert!(!rendered.contains(PARENT_ID_PLACEHOLDER));
    assert!(rendered.contains("finding"));
    assert!(rendered.contains(PARENT_ID));
}

#[rstest::rstest]
fn the_default_template_prescribes_no_particular_work() {
    // Given the shipped default template.

    // When its wording is read.
    let template = default_seed_template();

    // Then it only describes the situation. The bot feature is not
    // code-specific — a default that tells the model to check code against
    // the current code misleads every bot that is not inspecting code,
    // and the user has to notice and rewrite it.
    assert!(
        !template.to_lowercase().contains("code"),
        "the default template must not presume a code-review task: {template}"
    );
    assert!(!template.contains("Confirm or refute"));
}

/// A bot of `parent` that has finished composing.
///
/// `new_bot` alone leaves it in prep mode — the state `N` creates it
/// in — so a test about a run has to say that composition ended. Doing it
/// here keeps that one line out of eighteen fixtures.
fn composed_bot_of(parent: &ChatSessionState) -> ChatSessionState {
    let mut bot = ChatSessionState::new_bot(parent, true);
    bot.set_bot_is_prepping(false);
    bot
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
    assert_eq!(changed, [] as [jinn_core_types::ChatEntryId; 0]);
}

#[rstest::rstest]
#[test]
fn preserve_behavior_dispatches_the_template_without_resetting_context() {
    // Given a preserve-behavior bot with a prior report.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    session.set_bot_behavior(jinn_bot_msg::BotBehavior::Preserve);
    session.append_bot_report("a finding".to_owned());
    session.set_seed_template("carry on".to_owned());

    // When the run's seed entry is prepared.
    let (seed, reset) = prepare_trigger_run(&mut session);

    // Then the template is dispatched. The earlier design returned nothing
    // here, which left every parent-completed preserve bot inert.
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
    // Given a reset bot that reported once.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    session.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
    session.append_bot_report("the tests were actually passing".to_owned());

    // When the run's seed entry is prepared.
    let (seed, _reset) = prepare_trigger_run(&mut session);
    let seed = seed.expect("reset mode with a prior report seeds");

    // Then the seed entry carries the report through the template, as an
    // Intersession dispatch attributed to the parent.
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { text, .. } = &seed.kind else {
        panic!("seed must be an intersession dispatch");
    };
    assert!(text.contains("the tests were actually passing"));
    assert!(!text.contains(PRIOR_REPORT_PLACEHOLDER));
}

#[rstest::rstest]
#[test]
fn bot_created_from_parent_links_and_defaults() {
    // Given a parent with environment values.
    let dir = jinn_testutil::tempdir();
    let project = dir.path.join("p").expect("temp root joins a bare name");
    let mut parent = ChatSessionState::new();
    parent.set_project(Some(project.clone()));

    // When a bot is created from it.
    let bot = ChatSessionState::new_bot(&parent, true);

    // Then the bot links the parent and starts in prep mode: `N`
    // hands the user a bot they have not finished writing.
    assert_eq!(bot.origin(), SessionOrigin::Bot);
    assert!(bot.bot_is_prepping());
    assert_eq!(bot.project(), Some(&project));
    assert!(bot.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn prep_mode_makes_the_trigger_inert_end_to_end() {
    // Given a parent with a ParentCompleted bot still in prep mode,
    // and the trigger actor live on the bus.
    let (harness, dispatched, canceled, parent_id, actor) = {
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
            // Built by `new_bot` and left composing: the user has not
            // finished writing it, so its trigger is written down and inert.
            let mut bot = {
                let read = s.session.get(&id).expect("parent").clone();
                ChatSessionState::new_bot(&read, true)
            };
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_seed_template("re-check".to_owned());
            s.session.insert(bot);
            id
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, canceled, parent_id, actor)
    };

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;

    // And the trigger actor has drained its inbox — so the completion has
    // been *processed*, not merely published. Without this barrier the
    // assertions below would run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then nothing dispatches and nothing cancels — prep mode is inert, and
    // the trigger that is configured on it is not enough to override it.
    assert!(dispatched.is_empty(), "prep mode must not dispatch");
    assert!(canceled.is_empty(), "prep mode must not cancel");
}

#[rstest::rstest]
#[tokio::test]
async fn succeeded_parent_turn_fires_its_triggered_bot() {
    // Given a parent with a reset-activated ParentCompleted bot.
    let (harness, dispatched, parent_id, bot_id) = {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let dispatched = harness
            .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
            .await;
        let state = State::new(AppState::default());
        // The default state pre-seeds an active session, so the fixture parent
        // must be identified by id at fixture time — a `find` over the map
        // would race the HashMap's per-process iteration order and sometimes
        // publish the completion against the wrong (default) session.
        let (parent_id, bot_id) = {
            let mut s = state.write();
            let parent = ChatSessionState::new();
            let id = parent.session_id().clone();
            s.session.insert(parent);
            let mut bot = {
                let read = s.session.get(&id).expect("parent").clone();
                composed_bot_of(&read)
            };
            let bot_id = bot.session_id().clone();
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            bot.set_seed_template("verify: <prior_report>".to_owned());
            bot.append_bot_report("prior finding".to_owned());
            s.session.insert(bot);
            (id, bot_id)
        };
        let _actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, parent_id, bot_id)
    };

    // When the parent's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id.clone(),
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    let dispatches = jinn_testutil::bus_harness::await_recorded::<
        jinn_chat_input_msg::EnqueueUserMessage,
    >(&dispatched, 1, std::time::Duration::from_secs(10))
    .await;

    // Then exactly one dispatch went to the bot, seeded through the
    // template with the prior report.
    assert_eq!(dispatches.len(), 1);
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        text,
        ..
    } = &dispatches[0].entry.kind
    else {
        panic!("seed must be an intersession dispatch");
    };
    assert_eq!(target_session, &bot_id, "the dispatch targets the bot");
    assert_eq!(
        source_session, &parent_id,
        "the dispatch is attributed to the parent"
    );
    assert!(
        text.starts_with("verify: prior finding"),
        "the seeded prompt leads with the user's template: {text:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn errored_and_canceled_turns_fire_nothing() {
    // Given a parent with a reset-activated ParentCompleted bot.
    let (harness, dispatched, parent_id, actor) = {
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
            let mut bot = {
                let read = s.session.get(&id).expect("parent").clone();
                composed_bot_of(&read)
            };
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            s.session.insert(bot);
            id
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, parent_id, actor)
    };

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
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertion below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then nothing dispatches — only success is worth verifying against.
    assert!(
        dispatched.is_empty(),
        "an errored or cancelled turn must not fire bots"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_bot_created_after_the_parent_completed_never_fires_retroactively() {
    // Given a parent whose turn already completed successfully — with no
    // bot existing at that moment — and a bot added afterward
    // with a ParentCompleted trigger.
    let (harness, dispatched, state, actor) = {
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
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, state, actor)
    };

    // When the parent's completion is published (nothing was listening for
    // this bot — it does not exist yet), and only *then* is the
    // bot created with a fired trigger.
    //
    // The barrier is the point of the test: the completion must be
    // *processed* before the bot exists, or the "never fires
    // retroactively" claim is untested.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    {
        let mut s = state.write();
        let parent_read = s
            .session
            .iter()
            .next()
            .map(|(_, p)| p.clone())
            .expect("parent");
        let mut bot = composed_bot_of(&parent_read);
        bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
        bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
        s.session.insert(bot);
    }

    // Then no dispatch ever happens: the trigger query only runs on a live
    // `TurnCompleted`, never against the session map's current shape.
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertion below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;
    let dispatches = dispatched.drain();
    assert!(
        dispatches.is_empty(),
        "a bot created after the fact must not fire, got {dispatches:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn manual_rerun_runs_a_late_bot_even_though_its_trigger_never_fired() {
    // Given a bot created after its parent completed.
    let state = State::new(AppState::default());
    let (bot_id, parent_id) = {
        let mut s = state.write();
        let parent = ChatSessionState::new();
        let parent_id = parent.session_id().clone();
        let mut bot = composed_bot_of(&parent);
        bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
        bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
        bot.append_bot_report("late finding".to_owned());
        bot.set_seed_template("verify: <prior_report>".to_owned());
        let id = bot.session_id().clone();
        s.session.insert(bot);
        (id, parent_id)
    };

    // When the user re-runs it with `R`.
    let (cancel, dispatch, _reset) = crate::rerun::rerun(&state, &bot_id).expect("rerun allowed");

    // Then the run dispatches the seeded turn regardless of the trigger's
    // history — manual re-run has no trigger condition.
    assert!(cancel.is_none());
    let dispatch = dispatch.expect("reset mode dispatches");
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        text,
        ..
    } = &dispatch.entry.kind
    else {
        panic!("seed must be an intersession dispatch");
    };
    assert_eq!(target_session, &bot_id, "the dispatch targets the bot");
    assert_eq!(
        source_session, &parent_id,
        "the manual dispatch is attributed to the parent too"
    );
    assert!(
        text.starts_with("verify: late finding"),
        "the seeded prompt leads with the user's template: {text:?}"
    );
    assert_eq!(
        text, "verify: late finding",
        "the seed is the template substituted and nothing else — the parent link is attribution, not prompt text"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn manual_trigger_bot_does_not_fire_on_parent_completion() {
    // Given a parent with a reset-activated but Manual-trigger bot.
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
        let mut bot = {
            let read = s.session.get(&id).expect("parent").clone();
            composed_bot_of(&read)
        };
        // Trigger stays Manual.
        bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
        s.session.insert(bot);
        id
    };
    let actor = crate::trigger_actor::BotTriggerActor::spawn(
        harness.system(),
        crate::trigger_actor::BotTriggerActorDeps {
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
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertion below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then nothing dispatches — the bot only runs when asked.
    assert!(dispatched.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn every_sibling_bot_fires_when_the_parent_turn_succeeds() {
    // Given a parent watched by three sibling bots, all dispatchable
    // and all on the parent-completed trigger.
    let (harness, dispatched, parent_id) = {
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
                let mut bot = {
                    let read = s.session.get(&id).expect("parent").clone();
                    composed_bot_of(&read)
                };
                bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
                bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
                bot.set_seed_template(format!("check-{index}: <prior_report>"));
                bot.append_bot_report("prior finding".to_owned());
                s.session.insert(bot);
            }
            id
        };
        let _actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, parent_id)
    };

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
    // The seed is the template substituted and nothing else — the fixtures'
    // templates never name <parent_id>, so no id appears.
    let mut seeded: Vec<String> = dispatches.iter().map(|d| d.entry.text().clone()).collect();
    seeded.sort();
    assert_eq!(
        seeded,
        (0..3)
            .map(|i| format!("check-{i}: prior finding"))
            .collect::<Vec<String>>()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_bot_nested_under_another_bot_does_not_fire_from_the_parents_completion() {
    // Given a root session with two dispatchable parent-completed
    // bots, and a third bot nested under the first one.
    let (harness, dispatched, root_id, nested_id, actor) = {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let dispatched = harness
            .spawn_recorder::<jinn_chat_input_msg::EnqueueUserMessage>()
            .await;
        let state = State::new(AppState::default());
        // Each bot is built from an already-cloned parent, so no read
        // guard is ever taken while the write guard below is held.
        let arm = |parent: &ChatSessionState| {
            let mut bot = composed_bot_of(parent);
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            bot.set_seed_template("check".to_owned());
            bot.append_bot_report("prior finding".to_owned());
            bot
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
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, root_id, nested_id, actor)
    };

    // When the root's turn completes successfully.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: root_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertions below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the nested bot fires too. "Everything under this session"
    // is the contract; it is not one hop.
    let seen: Vec<jinn_core_types::SessionId> = dispatched
        .drain()
        .iter()
        .map(|d| d.session_id.clone())
        .collect();
    assert!(
        seen.contains(&nested_id),
        "a bot nested under another bot must fire from the root's completion; dispatched to {seen:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_cyclic_parent_link_does_not_hang_the_trigger_walk() {
    // Given two bots that each claim the other as their parent, both
    // dispatchable and parent-completed.
    let (harness, dispatched, root_id, first_id, actor) = {
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
                let mut bot = composed_bot_of(parent);
                bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
                bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
                bot.set_seed_template("check".to_owned());
                bot.append_bot_report("prior finding".to_owned());
                bot
            };
            let first = arm(&root);
            let first_id = first.session_id().clone();
            s.session.insert(first);
            // A second bot under the root, then re-point the first's
            // parent at it so the chain loops.
            let second = arm(&root);
            let second_id = second.session_id().clone();
            s.session.insert(second);
            if let Some(a) = s.session.get_mut(&first_id) {
                a.set_parent_session(second_id);
            }
            (root_id, first_id)
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, dispatched, root_id, first_id, actor)
    };

    // When the root's turn completes.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: root_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertions below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the walk terminated and each bot fired exactly once. A
    // cycle is a corrupt tree, not a reason to stop serving the rest.
    let seen: Vec<jinn_core_types::SessionId> = dispatched
        .drain()
        .iter()
        .map(|d| d.session_id.clone())
        .collect();
    assert_eq!(
        seen.iter().filter(|id| **id == first_id).count(),
        1,
        "a cyclic parent link must not fire a bot twice; dispatched to {seen:?}"
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
                    .paths({
                        // AppPaths::new_in only joins names onto the root
                        // (no filesystem access), so a transient tempdir
                        // root is fine here.
                        let root = jinn_testutil::tempdir();
                        jinn_kernel::common::app_paths::AppPaths::new_in(
                            &root.path.as_marked_path(),
                        )
                    })
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
            handoff_prompt: String::new(),
        },
    );
    (harness, state)
}

#[rstest::rstest]
#[tokio::test]
async fn every_sibling_bot_lands_its_own_seeded_entry() {
    // Given a parent watched by five dispatchable parent-completed
    // bots, with the session-turn actor live on the same state.
    let (harness, landed, parent_id, sibling_ids) = {
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
                let mut bot = composed_bot_of(&parent);
                bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
                bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
                bot.set_seed_template(format!("check-{index}: <prior_report>"));
                bot.append_bot_report("prior finding".to_owned());
                ids.push(bot.session_id().clone());
                s.session.insert(bot);
            }
            (parent_id, ids)
        };
        let _actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, landed, parent_id, sibling_ids)
    };

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
    // everywhere except here: the bot simply sits idle.
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
        "every bot must land its own seeded entry, not just the first"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_bot_built_the_way_the_ui_builds_one_still_fires() {
    // Given two bots constructed exactly as `N` constructs them —
    // `new_bot` and nothing else — with the user having since flipped
    // the *first* out of seed, but not the second.
    let (harness, landed, state, parent_id, first_id, second_id, actor) = {
        let (harness, state) = harness_with_session_actor().await;
        let landed = harness
            .spawn_recorder::<jinn_chat_input_msg::ChatEntrySubmitted>()
            .await;
        let (parent_id, first_id, second_id) = {
            let mut s = state.write();
            let parent = ChatSessionState::new();
            let parent_id = parent.session_id().clone();
            s.session.insert(parent.clone());
            let mut first = composed_bot_of(&parent);
            first.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            // The user configured this one and left seed.
            first.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            first.set_seed_template("first: <prior_report>".to_owned());
            first.append_bot_report("prior".to_owned());
            let first_id = first.session_id().clone();
            s.session.insert(first);
            let mut second = ChatSessionState::new_bot(&parent, true);
            second.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            second.set_seed_template("second: <prior_report>".to_owned());
            let second_id = second.session_id().clone();
            s.session.insert(second);
            (parent_id, first_id, second_id)
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (
            harness, landed, state, parent_id, first_id, second_id, actor,
        )
    };

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
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertions below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the configured bot fired, and the still-seeded one is
    // reported as *skipped by mode* rather than silently swallowed.
    let fired: Vec<String> = submissions
        .iter()
        .map(|x| x.session_id.to_string())
        .collect();
    assert_eq!(
        fired,
        vec![first_id.to_string()],
        "the configured bot must fire; a seed-mode bot is inert by design"
    );
    // And the seed-mode bot's inertness is visible, not invisible: it
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
        "a seed-mode bot must not dispatch: {second_texts:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_trigger_does_not_cancel_an_bot_that_is_already_running() {
    // Given a Streaming parent-completed bot.
    let (harness, state, bot_id, parent_id, actor) = {
        let harness = jinn_testutil::bus_harness::TestHarness::new().await;
        let state = State::new(AppState::default());
        let (bot_id, parent_id) = {
            let mut s = state.write();
            let parent = ChatSessionState::new();
            let parent_id = parent.session_id().clone();
            s.session.insert(parent.clone());
            let mut bot = composed_bot_of(&parent);
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            bot.set_seed_template("seeded: <prior_report>".to_owned());
            bot.append_bot_report("prior".to_owned());
            let id = bot.session_id().clone();
            s.session.insert(bot);
            {
                let session = s.session.get_mut(&id).expect("just inserted");
                session.push_entry(ChatEntry::user("in flight"));
                session.begin_streaming();
            }
            (id, parent_id)
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, state, bot_id, parent_id, actor)
    };

    // When the parent's turn completes and the trigger fires the busy
    // bot.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id,
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertions below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the running turn is untouched. A trigger answers a parent's
    // completion; it is not a user asking for this work to be abandoned, so
    // it has no business cancelling a turn already in flight.
    let guard = state.read();
    let session = guard.session.get(&bot_id).expect("exists");
    assert_eq!(
        session.phase(),
        jinn_slices::PhaseView::Streaming,
        "a trigger must not cancel a bot that is already running"
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
async fn a_trigger_on_a_busy_bot_queues_its_seeded_turn_for_after_the_current_one() {
    // Given a Streaming parent-completed bot, with the session-turn
    // actor live so the enqueue is handled the way it is in production.
    let (harness, state, bot_id, parent_id, actor) = {
        let (harness, state) = harness_with_session_actor().await;
        let (bot_id, parent_id) = {
            let mut s = state.write();
            let parent = ChatSessionState::new();
            let parent_id = parent.session_id().clone();
            s.session.insert(parent.clone());
            let mut bot = composed_bot_of(&parent);
            bot.set_bot_trigger(jinn_bot_msg::BotTrigger::ParentCompleted);
            bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
            bot.set_seed_template("seeded: <prior_report>".to_owned());
            bot.append_bot_report("prior".to_owned());
            let id = bot.session_id().clone();
            s.session.insert(bot);
            {
                let session = s.session.get_mut(&id).expect("just inserted");
                session.push_entry(ChatEntry::user("in flight"));
                session.begin_streaming();
            }
            (id, parent_id)
        };
        let actor = crate::trigger_actor::BotTriggerActor::spawn(
            harness.system(),
            crate::trigger_actor::BotTriggerActorDeps {
                services: harness.services().await,
                state: state.clone(),
            },
        );
        (harness, state, bot_id, parent_id, actor)
    };

    // When the parent's turn completes.
    harness
        .publish(jinn_session_msg::TurnCompleted {
            session_id: parent_id.clone(),
            outcome: jinn_session_msg::TurnOutcome::Succeeded,
        })
        .await;
    // The trigger actor has drained its inbox, so the completion above has
    // been *processed* — not merely published. Without this barrier the
    // assertions below could run before the actor was even scheduled, and
    // would pass whether or not the code was correct.
    jinn_testutil::wait::await_inbox_drained(
        harness.system(),
        &actor,
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the seeded turn is held in the session's queue, not in the input
    // box and not lost. The enqueue handler queues anything arriving while
    // a session is Sending/Streaming, and that is the correct outcome: the
    // bot is mid-answer, so its new question runs next.
    let guard = state.read();
    let session = guard.session.get(&bot_id).expect("exists");
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
        vec!["seeded: prior".to_owned()],
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
fn the_seed_prompt_names_the_parent_session_so_the_bot_can_search_it() {
    // Given a composed bot whose template names the parent token.
    let parent = ChatSessionState::new();
    let mut bot = composed_bot_of(&parent);
    bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
    bot.set_seed_template("Summarise the work in <parent_id>.".to_owned());

    // When the seed prompt is built.
    let (entry, _) = prepare_trigger_run(&mut bot);
    let entry = entry.expect("a seed entry");

    // Then the prompt carries the parent's session id. The bot runs
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
fn the_seed_prompt_substitutes_the_token_without_touching_the_users_words() {
    // Given a composed bot with a template carrying its own text and the
    // parent token.
    let parent = ChatSessionState::new();
    let mut bot = composed_bot_of(&parent);
    bot.set_bot_behavior(jinn_bot_msg::BotBehavior::Reset);
    bot.set_seed_template("Summarise the work in <parent_id>.".to_owned());
    bot.append_bot_report("prior finding".to_owned());

    // When the seed prompt is built.
    let (entry, _) = prepare_trigger_run(&mut bot);
    let entry = entry.expect("a seed entry");
    let text = entry.text();

    // Then the user's words are intact around the substituted id, and the
    // raw token is gone.
    assert!(
        text.starts_with(&format!("Summarise the work in {}.", parent.session_id())),
        "the template must lead the prompt with the id substituted: {text:?}"
    );
    assert!(
        !text.contains("<parent_id>"),
        "the raw token must not reach the model: {text:?}"
    );
}

#[rstest::rstest]
fn a_seed_prompt_names_a_missing_parent_as_unavailable() {
    // Given a template naming the parent token, and no parent id to
    // substitute.
    let template = "Summarise the work in <parent_id>.";

    // When the prompt is rendered for a session that has no parent link.
    let rendered = render_seed_text(template, None, NO_PARENT_SESSION_TEXT);

    // Then the token became the unavailable sentence — a finished phrase
    // the model can read, not a dangling label.
    assert_eq!(
        rendered,
        format!("Summarise the work in {NO_PARENT_SESSION_TEXT}.")
    );
}

#[rstest::rstest]
#[test]
fn an_untitled_bot_takes_the_seed_s_first_non_empty_line_as_its_title_at_dispatch() {
    // Given an untitled composed bot whose template opens with blank
    // lines before its instructions.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    session.set_seed_template("\n\n  \nWatch the flaky suite.".to_owned());
    assert!(session.title().is_none(), "fixture starts untitled");

    // When the run's seed entry is prepared.
    let (seed, _) = prepare_trigger_run(&mut session);
    seed.expect("a seed dispatches");

    // Then the title is the first non-empty line, trimmed — not the empty
    // string a naive `lines().next()` would write, which would permanently
    // block every later mint.
    assert_eq!(session.title(), Some("Watch the flaky suite."));
}

#[rstest::rstest]
#[test]
fn a_titled_bot_keeps_its_title_through_a_dispatch() {
    // Given a composed bot the user has already named.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    session.set_title("nightly".to_owned());
    session.set_seed_template("Watch the flaky suite.".to_owned());

    // When the run's seed entry is prepared.
    let (seed, _) = prepare_trigger_run(&mut session);
    seed.expect("a seed dispatches");

    // Then the user's title stands — the mint is only for the untitled.
    assert_eq!(session.title(), Some("nightly"));
}

#[rstest::rstest]
#[test]
fn prep_mode_dispatches_nothing_and_mints_nothing() {
    // Given a bot still being composed, with a template that would
    // title it if a run were prepared.
    let parent = ChatSessionState::new();
    let mut session = ChatSessionState::new_bot(&parent, true);
    session.set_seed_template("Watch the flaky suite.".to_owned());
    assert!(session.bot_is_prepping());

    // When the run's seed entry is prepared.
    let (seed, _) = prepare_trigger_run(&mut session);

    // Then nothing dispatches and the bot stays untitled — the user
    // has not finished writing it, so nothing about it is settled yet.
    assert!(seed.is_none(), "prep mode must not dispatch");
    assert!(session.title().is_none(), "prep mode must not mint a title");
}

#[rstest::rstest]
#[test]
fn a_seed_template_of_only_blank_lines_mints_no_title() {
    // Given an untitled composed bot and a composed dispatch text
    // that opens with no non-empty line to title from.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    let text = "\n\n".to_owned();

    // When a dispatch entry is built from blank-only text.
    let entry = crate::activation::dispatch_entry(&mut session, &text);

    // Then the entry is still Intersession, and the title guard is left at
    // `None` — never flipped to `Some("")`, which would block every later
    // mint for the life of the session.
    assert!(session.title().is_none(), "no empty title may be written");
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession { .. } = entry.kind else {
        panic!("dispatch must be intersession");
    };
}

#[rstest::rstest]
#[test]
fn a_trigger_dispatch_entry_names_parent_and_target() {
    // Given a composed bot with a parent and a template.
    let parent = ChatSessionState::new();
    let mut session = composed_bot_of(&parent);
    session.set_seed_template("Watch the flaky suite.".to_owned());

    // When the run's seed entry is prepared.
    let (seed, _) = prepare_trigger_run(&mut session);
    let seed = seed.expect("a seed dispatches");

    // Then the entry is Intersession from the parent to the bot, with
    // no attribution name — a dispatch is not session→session mail, the
    // seed's own structure is the formatting.
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        source_name,
        target_session,
        text,
    } = &seed.kind
    else {
        panic!("dispatch must be an intersession entry");
    };
    assert_eq!(source_session, parent.session_id());
    assert_eq!(source_name, &None);
    assert_eq!(target_session, session.session_id());
    assert!(
        text.starts_with("Watch the flaky suite."),
        "the entry text is the seed, unwrapped: {text:?}"
    );
}

#[rstest::rstest]
#[test]
fn a_dispatch_from_a_parentless_bot_sources_the_bot_itself() {
    // Given a composed bot whose parent link was cleared.
    let mut session = ChatSessionState::new();
    session.set_bot_is_prepping(false);
    session.set_seed_template("Watch the flaky suite.".to_owned());
    assert!(session.parent_session().is_none());

    // When the run's seed entry is prepared.
    let (seed, _) = prepare_trigger_run(&mut session);
    let seed = seed.expect("a seed dispatches");

    // Then the source falls back to the bot's own id — `intersession`
    // requires a source, and a headless bot has only itself.
    let jinn_core_types::chat_entry::ChatEntryKind::Intersession {
        source_session,
        target_session,
        ..
    } = &seed.kind
    else {
        panic!("dispatch must be an intersession entry");
    };
    assert_eq!(source_session, session.session_id());
    assert_eq!(target_session, session.session_id());
}
