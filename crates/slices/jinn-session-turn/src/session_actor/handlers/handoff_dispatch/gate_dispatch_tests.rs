//! Tests for the handoff dispatch intercept: gate fire shape, non-fire
//! pass-through, and per-session scope.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use crate::session_actor::SessionPersistenceActor;
use crate::session_actor::handlers::handoff_dispatch::HandoffBoundary;
use jinn_core_types::ChatEntry;
use jinn_core_types::ContextOverride;
use jinn_core_types::ToolResultStatus;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::ChatEntryKind;
use jinn_preferences_config::schemas::HandoffCompactionConfig;
use jinn_session_state::ChatSessionState;

/// Builds a recording-bus actor with handoff compaction enabled at the
/// given work-token trigger — the gate's inputs fully determined, no
/// model cache involved.
async fn handoff_actor(
    workset_trigger_tokens: usize,
) -> (
    SessionPersistenceActor,
    jinn_kernel::common::services::BusAudit,
) {
    let (bus, audit) = jinn_kernel::common::services::BusService::new_recording();
    let services = jinn_kernel::common::services::Services::new_fake_with_bus(bus.clone()).await;
    services
        .config
        .put(&HandoffCompactionConfig {
            enabled: true,
            workset_trigger_tokens: Some(workset_trigger_tokens),
        })
        .expect("test config update");
    crate::session_actor::helpers::ensure_context_assembly(&services.trouper_system);

    let state = jinn_kernel::common::state::State::new(AppState::default_with_scope_focus());
    {
        let session_id = state.read().session.active_session_id().clone();
        let _ = services.turn_ledger.mint(
            &session_id,
            jinn_kernel::common::turn_ledger::MintKind::FreshTurn,
        );
    }
    let actor = SessionPersistenceActor {
        state,
        services,
        counter: jinn_llm_support::token_estimator::TiktokenCounter::o200k_base(),
        token_cache: jinn_token_count_msg::HistoryWorkerChatEntryTokenCache::default(),
        image_converter: crate::session_actor::helpers::test_image_converter(),
        handoff_prompt: "recap prompt".to_owned(),
    };
    (actor, audit)
}

/// Caches the session's size pair directly — the gate's work-size input
/// is the total minus the system share. A full assembly is not needed to
/// decide the gate.
fn set_context_size(
    actor: &SessionPersistenceActor,
    session_id: &jinn_core_types::SessionId,
    size: u32,
) {
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(session_id)
            .set_context_sizes(size, 0);
    });
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_pushes_nudge_sets_flag_and_holds_the_queue() {
    // Given a handoff-enabled actor whose session reads 700 tokens of
    // work (700 ≥ the 600-token work trigger).
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger

    // When the intercept runs at the boundary.
    let nudge = actor.handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None);

    // Then it fired: the nudge is returned for dispatch, the flag is
    // set, and the held request (none here) was not queued.
    let nudge = nudge.expect("gate should fire");
    assert!(matches!(&nudge.kind, ChatEntryKind::RuleInterrupt { rule, .. } if rule == "handoff"));
    let state = actor.state.read();
    assert!(state.session(&session_id).is_handoff_pending());
    assert_eq!(state.session(&session_id).queue_len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn gate_pass_below_the_trigger_dispatches_normally() {
    // Given a handoff-enabled actor whose session reads 300 tokens of work.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 300);

    // When the intercept runs.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then nothing happened: no flag, no entries, no queue.
    assert!(!fired);
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(!session.is_handoff_pending());
    assert!(session.history().is_empty());
    assert_eq!(session.queue_len(), 0);
}

/// Caches the session's size pair through the real assembly service —
/// the honest path the context-size actor uses — so tool schemas land
/// in the floor the way production computes it.
async fn recalculate_via_real_assembly(
    actor: &SessionPersistenceActor,
    session_id: &jinn_core_types::SessionId,
) {
    let prompt = {
        let guard = actor.state.read();
        let inputs =
            jinn_context_assembly::inputs_snapshot::build_assembly_inputs(&guard, session_id);
        jinn_context_assembly::inputs_snapshot::assemble_via_service(&actor.services, inputs)
            .await
            .expect("real assembly at the boundary")
    };
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(session_id)
            .set_context_sizes(prompt.estimated_tokens(), prompt.system_tokens());
    });
}

#[rstest::rstest]
#[tokio::test]
async fn an_empty_workset_passes_a_small_trigger_at_a_real_boundary() {
    // Given a handoff-enabled actor at a 1000-token trigger, an empty
    // history, and one registered tool — so the request ships a real
    // schema floor several thousand tokens wide.
    let (actor, _audit) = handoff_actor(1000).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    let cell = actor
        .state
        .read()
        .tool_registry()
        .expect("registry cell attached");
    cell.update(|r| {
        r.global.insert(
            "schema-heavy".to_owned(),
            jinn_core_types::ToolDefinition {
                name: "schema-heavy".to_owned(),
                description: "A".repeat(200),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "BBBB".repeat(20)}
                    }
                }),
                prompt_snippet: Some("SNIPPET-ONLY-IN-SYSTEM-PROMPT".to_owned()),
                prompt_guidelines: vec!["GUIDELINE-ONLY-IN-SYSTEM-PROMPT".to_owned()],
                server_tool_type: None,
            },
        );
    });

    // When the boundary's size pair is cached through the real assembly.
    recalculate_via_real_assembly(&actor, &session_id).await;

    // Then the measured work is the conversation content alone — the
    // schema floor rode the system share — and the boundary dispatches
    // normally instead of firing on an empty conversation.
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert_eq!(session.work_size(), Some(0));
    drop(state);
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();
    assert!(!fired);
    assert!(!actor.state.read().session(&session_id).is_handoff_pending());
}

#[rstest::rstest]
#[tokio::test]
async fn gate_passes_when_disabled_even_past_the_line() {
    // Given a handoff-enabled actor whose session reads 990 tokens of
    // work, then handoff disabled before the boundary.
    let (actor, _audit) = handoff_actor(990).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor
        .services
        .config
        .put(&HandoffCompactionConfig::default())
        .expect("disable handoff");
    set_context_size(&actor, &session_id, 990);

    // When the intercept runs.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then the boundary dispatches normally.
    assert!(!fired);
    let state = actor.state.read();
    assert!(state.session(&session_id).history().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn other_sessions_are_unaffected_by_one_session_gate() {
    // Given a handoff-enabled actor with a second session below the line
    // while the active session sits above it.
    let (actor, _audit) = handoff_actor(600).await;
    let active_id = actor.state.read().session.active_session_id().clone();
    let second = ChatSessionState::new();
    let second_id = second.session_id().clone();
    actor.state.write().session.insert(second);
    set_context_size(&actor, &active_id, 700);
    set_context_size(&actor, &second_id, 100);

    // When the intercept runs for the active session, then for the
    // second.
    let active_nudge = actor.handoff_nudge_if_gated(&active_id, HandoffBoundary::IdleDirect, None);
    let second_nudge = actor.handoff_nudge_if_gated(&second_id, HandoffBoundary::IdleDirect, None);

    // Then only the active session's boundary fired, and its state did
    // not leak into the second session.
    assert!(active_nudge.is_some());
    assert!(second_nudge.is_none());
    let state = actor.state.read();
    assert!(state.session(&active_id).is_handoff_pending());
    assert!(!state.session(&second_id).is_handoff_pending());
    assert!(state.session(&second_id).history().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_holds_the_replaced_user_entry_at_the_queue_front() {
    // Given a handoff-enabled actor above the line, with a user entry
    // the boundary was about to dispatch.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger
    let held = jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        jinn_kernel::protocol::ChatEntry::user("the user's actual request"),
    ));

    // When the intercept runs with the held request.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, Some(held))
        .expect("gate should fire");

    // Then the nudge is returned for dispatch and the user entry waits
    // at the queue front for the post-fold drain.
    assert!(matches!(&nudge.kind, ChatEntryKind::RuleInterrupt { rule, .. } if rule == "handoff"));
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert_eq!(session.queue_len(), 1);
    let queue = session.message_queue();
    assert!(matches!(
        queue.items().front(),
        Some(jinn_turn_dispatch_msg::QueueItem::UserMessage(_))
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn fold_pair_race_never_refires() {
    // Given a handoff-enabled actor whose cached size pair still reads
    // pre-compaction (the recalc has not landed), replaying the observed
    // duplicate: fold applies, the drain invalidates the cache, and the
    // gate re-checks immediately after.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // stale pre-fold pair
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("first fire");
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(nudge);
        session.push_entry(ChatEntry::assistant("## Recap\nobjective: ..."));
    }
    actor.run_handoff_fold(&session_id).await;

    // When the gate re-checks before any recalc has landed (the race
    // window that used to read the stale pair and fire again).
    let re_fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then it passes: the drain invalidated the cache, so the gate reads
    // no work size rather than the stale pre-fold pair.
    assert!(!re_fired);
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert_eq!(session.work_size(), None);
    assert!(!session.is_handoff_pending());
}

#[rstest::rstest]
#[tokio::test]
async fn fixed_point_regime_folds_once_per_growth() {
    // Given a handoff-enabled actor replaying the 50k-loop signature: a
    // large protected set (pins), a fire-line work size, and a fold
    // that gathers the same entries every time.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);
    let mut pinned = ChatEntry::assistant("pinned grounding doc");
    pinned.pin_position = Some(jinn_core_types::PinPosition::Top);
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(pinned);
        session.push_entry(ChatEntry::user("the task"));
        session.push_entry(ChatEntry::assistant("long work prose"));
    }
    let first = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(ChatEntry::assistant("## Recap\nobjective: ..."));
    }
    actor.run_handoff_fold(&session_id).await;

    // When the gate re-checks post-fold with the size pair still cached
    // at the fire line and no new gatherable entries.
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_context_sizes(700, 0);
    });
    let re_fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then the first fire happened once and the repeat is refused: same
    // gather set as the recorded fold — the fixed point is broken.
    assert!(first);
    assert!(!re_fired);
}

#[rstest::rstest]
#[tokio::test]
async fn identical_gather_set_refuses_the_fold() {
    // Given an actor whose fold already ran with no user turn since.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(ChatEntry::assistant("will fold"));
        session.set_last_fold_gathered();
    }

    // When the gate evaluates a fire whose fold would gather nothing new
    // (the size pair re-cached at the fire line).
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_context_sizes(700, 0);
    });
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then the fire is refused — no nudge, no pending flag.
    assert!(!fired);
    let state = actor.state.read();
    assert!(!state.session(&session_id).is_handoff_pending());
}

#[rstest::rstest]
#[tokio::test]
async fn distinct_gather_set_fires() {
    // Given an actor whose fold ran, with a new user turn since it.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.set_last_fold_gathered();
        session.push_entry(ChatEntry::user("new work since the fold"));
    }
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_context_sizes(700, 0);
    });

    // When the gate evaluates a fire over the new work.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then it fires — new user work re-armed the fire line.
    assert!(fired);
}

#[rstest::rstest]
#[tokio::test]
async fn healthy_growth_still_fires() {
    // Given a handoff-enabled actor with no fold history at all.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(ChatEntry::user("real work"));
        session.push_entry(ChatEntry::assistant("more real work"));
    }
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_context_sizes(700, 0);
    });

    // When the gate evaluates the first boundary past the trigger.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .is_some();

    // Then it fires exactly as before the guard existed.
    assert!(fired);
    let state = actor.state.read();
    assert!(state.session(&session_id).is_handoff_pending());
}

#[rstest::rstest]
#[tokio::test]
async fn a_pending_handoff_never_re_triggers_the_gate() {
    // Given an actor whose session just fired the gate.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger
    assert!(
        actor
            .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
            .is_some()
    );

    // When the recap turn's own dispatch boundary evaluates the gate.
    let re_nudge = actor.handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None);

    // Then it passes — the recap turn must not recurse.
    assert!(re_nudge.is_none());
    let state = actor.state.read();
    assert_eq!(
        state.session(&session_id).queue_len(),
        0,
        "no held item queued"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn mid_loop_gate_fire_records_owed_but_holds_nothing() {
    // Given a handoff-enabled actor above the line.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger

    // When the intercept runs at the mid-loop boundary (no held request —
    // the loop's continuation rides the live turn, not the queue).
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::MidLoop, None)
        .expect("gate should fire");

    // Then the nudge is returned and the owed field records that the
    // recap owes its post-fold continuation — with nothing queued.
    assert!(matches!(&nudge.kind, ChatEntryKind::RuleInterrupt { rule, .. } if rule == "handoff"));
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(session.is_handoff_pending());
    assert!(session.handoff_continuation_owed());
    assert_eq!(session.queue_len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn idle_direct_gate_fire_does_not_record_owed() {
    // Given a handoff-enabled actor above the line.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger

    // When the intercept runs at an idle-direct boundary.
    actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("gate should fire");

    // Then the recap turn is pending but owes no loop continuation — the
    // held entry's drain, not a live turn, follows the fold.
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(session.is_handoff_pending());
    assert!(!session.handoff_continuation_owed());
}

#[rstest::rstest]
#[tokio::test]
async fn fold_clears_the_owed_field_with_the_pending_flag() {
    // Given a session whose mid-loop gate fired (pending + owed) and whose
    // history holds the nudge and the recap response.
    use jinn_core_types::ToolResultStatus;
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::MidLoop, None)
        .expect("gate should fire");
    {
        let mut state = actor.state.write();
        let session = state.session.get_or_create(&session_id);
        session.push_entry(nudge);
        session.push_entry(ChatEntry::assistant("long working text"));
        session.push_entry(ChatEntry::tool_call("tc-1", "bash", "ls"));
        session.push_entry(ChatEntry::tool_result(
            "tc-1",
            "bash",
            "files...",
            ToolResultStatus::Success,
        ));
        session.push_entry(ChatEntry::assistant("## Recap\nobjective: ..."));
    }
    assert!(
        actor
            .state
            .read()
            .session(&session_id)
            .handoff_continuation_owed()
    );

    // When the fold runs (the top of `continue_tool_loop`, after the recap
    // completion acked `Continue`).
    actor.run_handoff_fold(&session_id).await;

    // Then both handoff fields are clear — the fold is the success-path
    // owner, and nothing is left pending or owed.
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(!session.is_handoff_pending());
    assert!(!session.handoff_continuation_owed());
}

#[rstest::rstest]
#[tokio::test]
async fn fold_excludes_non_protected_entries_and_inserts_the_recap() {
    // Given a handoff-pending session with user, assistant, tool call,
    // tool result, nudge, and the recap response entries.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700); // past the 600-token work trigger
    // The mid-loop caller pushes the returned nudge (the idle callers
    // dispatch it as the turn's entry); same observable state either way.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("gate should fire");
    {
        let mut state = actor.state.write();
        state.session.get_or_create(&session_id).push_entry(nudge);
        let session = state.session.get_or_create(&session_id);

        session.push_entry(ChatEntry::assistant("long working text"));
        session.push_entry(ChatEntry::tool_call("tc-1", "bash", "ls"));
        session.push_entry(ChatEntry::tool_result(
            "tc-1",
            "bash",
            "files...",
            ToolResultStatus::Success,
        ));
        // The recap response: the assistant entry the handoff turn produced.
        session.push_entry(ChatEntry::assistant("## Recap\nobjective: ..."));
        assert!(
            session
                .history()
                .iter()
                .any(|e| matches!(e.kind, ChatEntryKind::RuleInterrupt { .. }))
        );
    }

    // When the fold runs (the completion hook's success branch).
    actor.run_handoff_fold(&session_id).await;

    // Then every non-protected entry is excluded, the recap rides
    // context as a Compaction entry, and user entries survive verbatim.
    let state = actor.state.read();
    let session = state.session(&session_id);
    let recap = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::Compaction { .. }))
        .expect("recap inserted");
    assert_eq!(recap.context_override, ContextOverride::Default);
    for entry in session.history() {
        let protected = matches!(
            entry.kind,
            ChatEntryKind::User { .. }
                | ChatEntryKind::Intersession { .. }
                | ChatEntryKind::Compaction { .. }
        );
        if protected {
            assert_eq!(entry.context_override, ContextOverride::Default);
        } else {
            assert_eq!(
                entry.context_override,
                ContextOverride::ForcedExclude,
                "non-protected entry must be excluded"
            );
        }
    }
}

#[rstest::rstest]
#[tokio::test]
async fn end_handoff_owes_the_fold_only_for_terminal_success() {
    // Given a handoff-pending session.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .begin_handoff();
    });

    // When the turn ends WITHOUT success (cancel or error outcome).
    let owes_on_failure = actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .end_handoff(false)
    });

    // Then nothing owes the fold and the flag is cleared.
    assert!(!owes_on_failure);
    let state = actor.state.read();
    assert!(!state.session(&session_id).is_handoff_pending());
}

#[rstest::rstest]
#[tokio::test]
async fn end_handoff_on_a_non_pending_session_owes_nothing() {
    // Given a session with no handoff in flight.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();

    // When its (ordinary) turn completes successfully.
    let owes = actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .end_handoff(true)
    });

    // Then the completion hook learns nothing owed — the ordinary turn
    // never folds.
    assert!(!owes);
}

#[rstest::rstest]
#[tokio::test]
async fn cancel_sweep_clears_the_handoff_flag_without_a_fold() {
    // Given a handoff-pending session (mid-loop: owed recorded) with
    // uncompacted history.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        let session = view.session.map().get_or_create(&session_id);
        session.begin_handoff();
        session.set_handoff_continuation_owed();
    });
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .push_entry(ChatEntry::assistant("uncompacted work"));
    });

    // When the cancel sweep's abandonment runs (the settle Cancel arm).
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .abandon_handoff();
    });

    // Then both fields are gone and history is untouched — no fold applied.
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(!session.is_handoff_pending());
    assert!(!session.handoff_continuation_owed());
    let history = session.history();
    assert!(
        history
            .iter()
            .all(|e| e.context_override == ContextOverride::Default)
    );
    assert!(
        history
            .iter()
            .all(|e| !matches!(e.kind, ChatEntryKind::Compaction { .. }))
    );
}

/// Seeds the actor's personas cell (the fake `Services` builder registers
/// the full cell catalog) with one block-carrying persona — the catalog
/// resolution surface the nudge helper reads through.
fn seed_persona_catalog(actor: &SessionPersistenceActor, persona: jinn_slices::Persona) {
    actor
        .services
        .slices
        .reader::<jinn_persona_msg::Personas>(&jinn_persona_msg::personas_slot())
        .expect("the fake services builder registers the personas cell")
        .update(|p: &mut jinn_persona_msg::Personas| p.entries = vec![persona]);
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_uses_the_persona_handoff_block_as_the_nudge() {
    // Given a handoff-enabled actor whose session's persona carries a
    // compaction block, past the trigger.
    let (actor, _audit) = handoff_actor(600).await;
    seed_persona_catalog(
        &actor,
        jinn_slices::Persona {
            name: "brainstorm".to_owned(),
            description: String::new(),
            body: String::new(),
            handoff_prompt: "Recap the thesis and the open critiques.".to_owned(),
        },
    );
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_persona_name("brainstorm".to_owned());
    });
    set_context_size(&actor, &session_id, 700);

    // When the intercept runs at the boundary.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("gate should fire");

    // Then the nudge body is exactly the persona block, not the shipped
    // recap prompt ("recap prompt", the launch-copy fallback).
    let ChatEntryKind::RuleInterrupt { body, .. } = &nudge.kind else {
        panic!("expected a rule interrupt");
    };
    assert!(
        body.contains("Recap the thesis and the open critiques."),
        "nudge should carry the persona block, got: {body}"
    );
    assert!(
        !body.contains("recap prompt"),
        "nudge must not fall back to the shipped prompt, got: {body}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_falls_back_to_the_shipped_prompt_for_a_blockless_persona() {
    // Given a handoff-enabled actor whose persona carries no block, past
    // the trigger.
    let (actor, _audit) = handoff_actor(600).await;
    seed_persona_catalog(
        &actor,
        jinn_slices::Persona {
            name: "plain".to_owned(),
            description: String::new(),
            body: String::new(),
            handoff_prompt: String::new(),
        },
    );
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .set_persona_name("plain".to_owned());
    });
    set_context_size(&actor, &session_id, 700);

    // When the intercept runs at the boundary.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("gate should fire");

    // Then the nudge body is the shipped recap prompt (the launch copy
    // the harness seeds as "recap prompt").
    let ChatEntryKind::RuleInterrupt { body, .. } = &nudge.kind else {
        panic!("expected a rule interrupt");
    };
    assert!(
        body.contains("recap prompt"),
        "nudge should use the shipped fallback, got: {body}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_falls_back_when_the_catalog_cell_is_missing() {
    // Given a handoff-enabled actor with no personas cell registered at
    // all (persona slice not activated), past the trigger.
    let (actor, _audit) = handoff_actor(600).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);

    // When the intercept runs at the boundary.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, HandoffBoundary::IdleDirect, None)
        .expect("gate should fire");

    // Then the nudge still fires on the shipped fallback — a missing
    // catalog degrades, it does not panic.
    let ChatEntryKind::RuleInterrupt { body, .. } = &nudge.kind else {
        panic!("expected a rule interrupt");
    };
    assert!(
        body.contains("recap prompt"),
        "nudge should use the shipped fallback, got: {body}"
    );
}
