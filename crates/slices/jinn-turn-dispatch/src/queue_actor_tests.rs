// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Queue-actor tests — ported struct-direct from the kernel suite, plus
//! `DispatchTurn` coverage. Each test constructs the actor directly
//! (`Services::new_fake_with_bus` + `BusAudit`), calls the plain handler
//! method, and asserts on state or the recorded bus traffic.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code"
)]

use super::QueueActor;
use jinn_domain::common::app_state::AppState;
use jinn_domain::common::services::Services;
use jinn_domain::common::services::bus_service::BusAudit;
use jinn_domain::common::state::State;
use jinn_domain::feat::chat_input::protocol::event::ChatEntrySubmitted;
use jinn_domain::feat::session::phase_machine::PhaseKind;
use jinn_domain::feat::session::protocol::session_phase_changed::SessionPhaseChanged;
use jinn_domain::feat::session_lifecycle::protocol::command::PersistSession;
use jinn_domain::protocol::ChatEntry;
use jinn_domain::protocol::SessionId;
use jinn_inference_msg::{SendToLlmProvider, StreamOrigin};
use jinn_turn_dispatch_msg::DispatchTurn;
use jinn_turn_dispatch_msg::QueueItem;

async fn create_actor() -> (QueueActor, State, BusAudit) {
    let (bus, audit) = jinn_domain::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus).await;
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
    let state = State::new(AppState::default_with_scope_focus());
    (
        QueueActor {
            state: state.clone(),
            services,
            cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
        },
        state,
        audit,
    )
}

fn session_id() -> SessionId {
    SessionId::new()
}

// ── SessionPhaseChanged → Idle trigger ────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_dispatches_user_message() {
    // Given a session with a queued user message.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged → Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Sending,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    // And ChatEntrySubmitted was published.
    let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
    assert_eq!(submitted.len(), 1);
    // And PersistSession was published.
    let persists: Vec<PersistSession> = audit.of_type::<PersistSession>();
    assert_eq!(persists.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_dispatches_tool_continuation() {
    // Given a session with a queued tool continuation.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::ToolContinuation);
    }

    // When receiving SessionPhaseChanged → Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Sending,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    // And ChatEntrySubmitted was NOT published (tool continuations don't emit it).
    let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
    assert!(submitted.is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn non_idle_transition_does_nothing() {
    // Given a session with a queued message.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged → Sending (not Idle).
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Idle,
        new_phase: PhaseKind::Sending,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then nothing was published.
    assert!(audit.of_type::<SendToLlmProvider>().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_with_empty_queue_does_nothing() {
    // Given a session with nothing queued.
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();

    // When receiving SessionPhaseChanged → Idle.
    let msg = SessionPhaseChanged {
        session_id: sid,
        old_phase: PhaseKind::Sending,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then nothing was published.
    assert!(audit.of_type::<SendToLlmProvider>().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn idle_with_empty_queue_and_steering_dispatches_steering() {
    // Given a session with a steering fragment and an empty queue.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.steering_buffer_mut().push_fragment("stay focused");
    }

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    // And the drained steering entry is in history.
    let state = state.read();
    let session = state.session(&sid);
    let has_steering = session.history().iter().any(|e| {
        matches!(
            &e.kind,
            jinn_domain::protocol::ChatEntryKind::User { expanded, .. } if expanded == "stay focused"
        )
    });
    assert!(
        has_steering,
        "drained steering entry must appear in history; history = {:?}",
        session.history()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn idle_with_empty_queue_and_empty_steering_does_nothing() {
    // Given a session with nothing queued and no steering fragment.
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid,
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then nothing was published.
    assert!(
        audit.of_type::<SendToLlmProvider>().is_empty(),
        "empty queue and steering must not dispatch"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn idle_with_queued_item_and_steering_dispatches_queue_item_first() {
    // Given a session with BOTH a queued user message and a steering fragment.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user(
            "queued msg",
        ))));
        session.steering_buffer_mut().push_fragment("stay focused");
    }

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then the queue item won dispatch (SendToLlmProvider x1).
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1, "queue item must win dispatch");

    // And the queue is now empty.
    let state = state.read();
    let session = state.session(&sid);
    assert!(
        session.queue().is_empty(),
        "queue must be drained after dispatch"
    );
    // And the steering fragment was co-injected (buffer empty).
    assert!(
        session.steering_buffer().is_empty(),
        "steering must be co-injected, not orphaned"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_publishes_idle_to_sending_phase_change() {
    // Given a session in Idle with a queued user message.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid,
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then the Idle -> Sending transition was published (not just mutated silently).
    let phases: Vec<SessionPhaseChanged> = audit.of_type::<SessionPhaseChanged>();
    let sending = phases
        .iter()
        .find(|p| p.old_phase == PhaseKind::Idle && p.new_phase == PhaseKind::Sending);
    assert!(
        sending.is_some(),
        "Idle -> Sending transition must be published, got: {phases:?}"
    );
}

// ── User-message dispatch body ────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_sets_title_on_first_message() {
    // Given a session with no title.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    let entry = ChatEntry::user("first message here");

    // When dispatching the first user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the session title was set to the first line.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.title(), Some("first message here"));
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_does_not_overwrite_existing_title() {
    // Given a session with a title already set.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.set_title("original title".to_owned());
    }
    let entry = ChatEntry::user("second message");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the title is unchanged.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.title(), Some("original title"));
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_transitions_to_sending() {
    // Given a queue actor.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the session is in Sending phase.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_keeps_degraded_token_literal_through_re_expand() {
    // Given a resolved-but-queued entry carrying the degraded marker and a literal token.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    let token = "@/nonexistent/whatever";
    let mut entry = ChatEntry::user(format!("describe {token}"));
    // Simulate the post-resolution state: outcome set, expanded still containing the literal.
    if let jinn_domain::protocol::ChatEntryKind::User { outcome, .. } = &mut entry.kind {
        outcome.degraded.push(jinn_domain::protocol::ResolvedToken {
            raw: "/nonexistent/whatever".to_owned(),
            abs: std::path::PathBuf::from("/nonexistent/whatever"),
        });
    }

    // When dispatching (which calls push_entry -> expand_user_entry, re-running the scan).
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the AI-facing expanded text keeps the literal token (no file:// revert).
    let state = state.read();
    let session = state.session(&sid);
    let expanded = session
        .history()
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            jinn_domain::protocol::ChatEntryKind::User { expanded, .. } => Some(expanded.clone()),
            _ => None,
        })
        .expect("user entry");
    assert!(
        expanded.contains(token),
        "queue drain must keep degraded token literal: {expanded}"
    );
    assert!(
        !expanded.contains("file://"),
        "queue drain must not revert to file://: {expanded}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_blocks_attachment_to_unknown_model() {
    // Given a queued entry carrying an image attachment and an unknown model.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Single(
            "my-uncatalogued-llama".to_owned(),
        );
    }
    // Build an entry that already carries an attachment (as if resolved).
    let mut entry = ChatEntry::user("describe this");
    if let jinn_domain::protocol::ChatEntryKind::User { attachments, .. } = &mut entry.kind {
        attachments.push(jinn_provider::Attachment::image(
            "image/png".to_owned(),
            vec![1, 2, 3],
        ));
    }

    // When dispatching (queue drain).
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then an Error entry was pushed and no SendToLlmProvider was emitted
    // (the turn is blocked, not re-dispatched).
    let state = state.read();
    let session = state.session(&sid);
    let has_error = session
        .history()
        .iter()
        .any(|e| matches!(&e.kind, jinn_domain::protocol::ChatEntryKind::Error(_)));
    assert!(
        has_error,
        "unknown model must block the attachment on drain"
    );
    drop(state);
    assert!(
        audit.of_type::<SendToLlmProvider>().is_empty(),
        "blocked attachment must not dispatch"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_provider_id_is_none_when_no_provider() {
    // Given a queue actor with no model selected.
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then SendToLlmProvider has provider_id = None.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    let send = sends.first().expect("one send");
    assert!(send.provider_id.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_provider_id_is_some_when_model_set() {
    // Given a queue actor with a model selected.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Single(
            "test-provider/test-model".to_owned(),
        );
    }
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then SendToLlmProvider has provider_id = Some.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    let send = sends.first().expect("one send");
    assert!(send.provider_id.is_some());
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_drains_steering_buffer() {
    // Given a session with steering fragments.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.steering_buffer_mut().push_fragment("system note");
    }
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the steering buffer was drained into history.
    let state = state.read();
    let session = state.session(&sid);
    // At minimum: steering entry + user entry.
    assert!(session.history().len() >= 2);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_user_message_assembles_via_service() {
    // Given a queue actor (the context-assembly service is spawned in the helper).
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then SendToLlmProvider carries the assembled system prompt (date
    // section is unconditional, so it proves assembly populated the field).
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    let send = sends.first().expect("one send");
    let prompt = send
        .system_prompt
        .as_deref()
        .expect("system prompt assembled");
    assert!(
        prompt.contains("Current date:"),
        "dispatch system prompt should come from assembly, got: {prompt:?}"
    );
}

// ── Tool-continuation / resume body ──────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_emits_send_to_llm_provider() {
    // Given a queue actor.
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();

    // When dispatching a resume.
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_drains_steering_buffer() {
    // Given a session with steering fragments.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.steering_buffer_mut().push_fragment("system note");
    }

    // When dispatching a resume.
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then the steering buffer was drained into history.
    let state = state.read();
    let session = state.session(&sid);
    assert!(!session.history().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_does_not_emit_chat_entry_submitted_or_persist() {
    // Given a queue actor.
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();

    // When dispatching a resume.
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then ChatEntrySubmitted was NOT published.
    assert!(audit.of_type::<ChatEntrySubmitted>().is_empty());
    // And PersistSession was NOT published.
    assert!(audit.of_type::<PersistSession>().is_empty());
}

// ── DispatchTurn command ─────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_publishes_send_to_llm_provider() {
    // Given a session whose prepared turn is ready (entry pushed by the
    // session actor before publishing).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("already pushed"));
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    // And PersistSession was published (prepared turns persist).
    let persists: Vec<PersistSession> = audit.of_type::<PersistSession>();
    assert_eq!(persists.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_publishes_none_effort_when_session_has_no_own_effort() {
    // Given a global default reasoning effort of High but a session with no
    // own effort. The global is consulted only at session creation, never at
    // request time — so the published effort is None (let the provider decide).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        state.session_mut_or_create(&sid);
    }
    {
        let mut app_state = actor.services.app_state_storage.read();
        app_state.reasoning_effort = Some(jinn_domain::ReasoningEffort::High);
        actor
            .services
            .app_state_storage
            .save(&app_state)
            .expect("save global default");
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the published SendToLlmProvider carries no effort — the session
    // owns None and the global is not consulted at request time.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    let send = sends.first().expect("one send");
    assert_eq!(
        send.reasoning_effort, None,
        "session with no own effort resolves to None; global is not consulted"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_publishes_sessions_own_reasoning_effort() {
    // Given a session with its own effort of Low (and a stale global of High
    // that must be ignored at request time).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().reasoning_effort = Some(jinn_domain::ReasoningEffort::Low);
    }
    {
        let mut app_state = actor.services.app_state_storage.read();
        app_state.reasoning_effort = Some(jinn_domain::ReasoningEffort::High);
        actor
            .services
            .app_state_storage
            .save(&app_state)
            .expect("save global default");
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the published SendToLlmProvider carries the session's own effort
    // (Low); the global is not consulted at request time.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    let send = sends.first().expect("one send");
    assert_eq!(
        send.reasoning_effort,
        Some(jinn_domain::ReasoningEffort::Low),
        "session's own effort is published; global is ignored"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_drains_steering_submitted_after_preparation() {
    // Given a prepared session (entry pushed kernel-side) with a steering
    // fragment submitted in the window between preparation and dispatch.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("prepared"));
        session.steering_buffer_mut().push_fragment("late note");
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the fragment was drained into history (before assembly).
    let state = state.read();
    let session = state.session(&sid);
    let has_late = session.history().iter().any(|e| {
        matches!(
            &e.kind,
            jinn_domain::protocol::ChatEntryKind::User { expanded, .. } if expanded == "late note"
        )
    });
    assert!(has_late, "late steering fragment must make the turn");
    // And SendToLlmProvider was published.
    drop(state);
    assert_eq!(audit.of_type::<SendToLlmProvider>().len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_transitions_to_streaming_and_records_token_record() {
    // Given a session the session actor already transitioned to Sending
    // (the resume path's shape: begin_sending happened kernel-side).
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("history"));
        session.begin_sending();
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the session is in Streaming phase.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.phase(), PhaseKind::Streaming);
    // And the outgoing token record was pushed with the resolved model.
    let ledger = session.token_ledger();
    assert_eq!(ledger.len(), 1, "exactly one outgoing record");
    let record = ledger.first().expect("one record");
    assert!(
        record.tokens_sent > 0,
        "the record carries the assembled prompt's estimate"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_publishes_sending_to_streaming_phase_change() {
    // Given a Sending session (post-begin_sending resume shape).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("history"));
        session.begin_sending();
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the Sending -> Streaming transition was published.
    let phases: Vec<SessionPhaseChanged> = audit.of_type::<SessionPhaseChanged>();
    let streaming = phases
        .iter()
        .find(|p| p.old_phase == PhaseKind::Sending && p.new_phase == PhaseKind::Streaming);
    assert!(
        streaming.is_some(),
        "Sending -> Streaming transition must be published, got: {phases:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_preserves_round_robin_index_mutation() {
    // Given a session on an alloy with a round-robin strategy.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Alloy {
            models: vec!["p/a".to_owned(), "p/b".to_owned()],
            strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 },
        };
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the published model_used is the alloy's first member and the
    // round-robin index advanced.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends.first().expect("one send").model_used.as_deref(),
        Some("p/a")
    );
    drop(audit);
    let state = state.read();
    let session = state.session(&sid);
    match &session.profile().model {
        jinn_core_types::model_selection::ModelSelection::Alloy {
            strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index },
            ..
        } => assert_eq!(*index, 1, "round-robin index must advance on resolve"),
        other => panic!("expected alloy round-robin, got {other:?}"),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_does_not_drain_stale_queue() {
    // Given a Sending session with a queued item (busy-session sends wait
    // for Idle; DispatchTurn must not steal them).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("prepared"));
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user(
            "waiting for idle",
        ))));
        session.begin_sending();
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then exactly one SendToLlmProvider was published (the prepared turn,
    // not the queued item).
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
    // And the queued item is still queued.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.queue_len(), 1, "queue must be untouched");
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_with_assembly_failure_publishes_nothing() {
    // Given a session whose assembly will fail (no service spawned).
    let (bus, audit) = jinn_domain::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus).await;
    // NOTE: jinn_context_assembly::service::ensure_spawned deliberately
    // NOT called — the ask fails.
    let state = State::new(AppState::default_with_scope_focus());
    let actor = QueueActor {
        state: state.clone(),
        services,
        cap: jinn_domain::common::tcaps::mint::mint_session_cap(),
    };
    let sid = session_id();
    {
        let mut state = state.write_test_no_cap();
        state.session_mut_or_create(&sid).begin_sending();
    }

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then no SendToLlmProvider was published.
    assert!(
        audit.of_type::<SendToLlmProvider>().is_empty(),
        "assembly failure must abort the dispatch"
    );
    // And no PersistSession followed.
    assert!(audit.of_type::<PersistSession>().is_empty());
}
