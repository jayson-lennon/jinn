//! Tests for the handoff dispatch intercept: gate fire shape, non-fire
//! pass-through, and per-session scope.

use crate::session_actor::SessionPersistenceActor;
use jinn_core_types::ChatEntry;
use jinn_core_types::ContextOverride;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::ChatEntryKind;
use jinn_preferences_config::schemas::HandoffCompactionConfig;
use jinn_session_state::ChatSessionState;

/// Builds a recording-bus actor with handoff compaction enabled at
/// `threshold` over an explicit 1000-token limit — the gate's inputs
/// fully determined, no model cache involved.
async fn handoff_actor(
    threshold: f64,
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
            threshold,
            context_limit: Some(1000),
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

/// Caches the session's assembled-estimate size directly — the gate's
/// context input. A full assembly is not needed to decide the gate.
fn set_context_size(
    actor: &SessionPersistenceActor,
    session_id: &jinn_core_types::SessionId,
    size: u32,
) {
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(session_id)
            .set_context_size(size);
    });
}

#[rstest::rstest]
#[tokio::test]
async fn gate_fire_pushes_nudge_sets_flag_and_holds_the_queue() {
    // Given a handoff-enabled actor whose session reads 700 of 1000
    // tokens (0.7 ≥ 0.6 threshold).
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);

    // When the intercept runs at the boundary.
    let nudge = actor.handoff_nudge_if_gated(&session_id, None).await;

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
async fn gate_pass_below_threshold_dispatches_normally() {
    // Given a handoff-enabled actor whose session reads 300 of 1000.
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 300);

    // When the intercept runs.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, None)
        .await
        .is_some();

    // Then nothing happened: no flag, no entries, no queue.
    assert!(!fired);
    let state = actor.state.read();
    let session = state.session(&session_id);
    assert!(!session.is_handoff_pending());
    assert!(session.history().is_empty());
    assert_eq!(session.queue_len(), 0);
}

#[rstest::rstest]
#[tokio::test]
async fn gate_passes_when_disabled_even_past_the_line() {
    // Given a handoff-enabled actor whose session reads 990 of 1000,
    // then handoff disabled before the boundary.
    let (actor, _audit) = handoff_actor(0.1).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor
        .services
        .config
        .put(&HandoffCompactionConfig::default())
        .expect("disable handoff");
    set_context_size(&actor, &session_id, 990);

    // When the intercept runs.
    let fired = actor
        .handoff_nudge_if_gated(&session_id, None)
        .await
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
    let (actor, _audit) = handoff_actor(0.6).await;
    let active_id = actor.state.read().session.active_session_id().clone();
    let second = ChatSessionState::new();
    let second_id = second.session_id().clone();
    actor.state.write().session.insert(second);
    set_context_size(&actor, &active_id, 700);
    set_context_size(&actor, &second_id, 100);

    // When the intercept runs for the active session, then for the
    // second.
    let active_nudge = actor.handoff_nudge_if_gated(&active_id, None).await;
    let second_nudge = actor.handoff_nudge_if_gated(&second_id, None).await;

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
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);
    let held = jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        jinn_kernel::protocol::ChatEntry::user("the user's actual request"),
    ));

    // When the intercept runs with the held request.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, Some(held))
        .await
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
async fn a_pending_handoff_never_re_triggers_the_gate() {
    // Given an actor whose session just fired the gate.
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);
    assert!(
        actor
            .handoff_nudge_if_gated(&session_id, None)
            .await
            .is_some()
    );

    // When the recap turn's own dispatch boundary evaluates the gate.
    let re_nudge = actor.handoff_nudge_if_gated(&session_id, None).await;

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
async fn fold_excludes_non_protected_entries_and_inserts_the_recap() {
    // Given a handoff-pending session with user, assistant, tool call,
    // tool result, nudge, and the recap response entries.
    use jinn_core_types::ToolResultStatus;
    use jinn_kernel::protocol::ChatEntryKind;
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    set_context_size(&actor, &session_id, 700);
    // The mid-loop caller pushes the returned nudge (the idle callers
    // dispatch it as the turn's entry); same observable state either way.
    let nudge = actor
        .handoff_nudge_if_gated(&session_id, None)
        .await
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
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .begin_handoff()
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
    let (actor, _audit) = handoff_actor(0.6).await;
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
    // Given a handoff-pending session with uncompacted history.
    let (actor, _audit) = handoff_actor(0.6).await;
    let session_id = actor.state.read().session.active_session_id().clone();
    actor.state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&session_id)
            .begin_handoff()
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
            .abandon_handoff()
    });

    // Then the flag is gone and history is untouched — no fold applied.
    use jinn_kernel::protocol::ChatEntryKind;
    let state = actor.state.read();
    assert!(!state.session(&session_id).is_handoff_pending());
    let history = state.session(&session_id).history();
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
