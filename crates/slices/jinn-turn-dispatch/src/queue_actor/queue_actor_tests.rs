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
//!
//! Every dispatch asks the phase actor for admission before publishing, so
//! the helpers spawn the real one (`jinn_session_turn::phase_actor`) on the
//! same bus and state — production composition spawns it before any
//! publisher runs.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code"
)]

use super::QueueActor;
use jinn_chat_input_msg::ChatEntrySubmitted;
use jinn_core_types::SessionId;
use jinn_inference_msg::{SendToLlmProvider, StreamOrigin};
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::common::services::Services;
use jinn_kernel::common::services::bus_service::BusAudit;
use jinn_kernel::common::state::State;
use jinn_kernel::protocol::ChatEntry;
use jinn_kernel::{ProviderEntry, ProvidersConfig};
use jinn_session_msg::PhaseKind;
use jinn_session_msg::SessionPhaseChanged;
use jinn_session_msg::phase_command::DispatchKind;
use jinn_session_msg::phase_command::PhaseCommand;
use jinn_session_store_msg::PersistSession;
use jinn_turn_dispatch_msg::DispatchTurn;
use jinn_turn_dispatch_msg::QueueItem;

/// The model the endpoint-pin tests configure, as a full provider id.
const PINNED_MODEL: &str = "openrouter/anthropic/claude-sonnet-4";

async fn create_actor() -> (QueueActor, State, BusAudit) {
    let (bus, audit) = jinn_kernel::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus.clone()).await;
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
    let state = State::new(AppState::default_with_scope_focus());
    // The real phase actor answers this actor's admission asks; production
    // composition spawns it, so tests must too, on the same bus and state.
    let _ = jinn_session_turn::phase_actor::ensure_spawned(
        &services.trouper_system,
        state.clone(),
        bus,
    );
    (
        QueueActor {
            state: state.clone(),
            services,
        },
        state,
        audit,
    )
}

fn session_id() -> SessionId {
    SessionId::new()
}

/// Mints a live turn generation through the real phase actor — the shape
/// the session actor's dispatch paths produce (enqueue-resume mints
/// `FreshTurn`) before the queue actor's prepared/resume bodies ask to
/// join it. Without this, a `ResumeTurn`/`ToolContinuation` admission is
/// refused under the new policy and the dispatch publishes nothing.
async fn mint_live_generation(services: &Services, sid: &SessionId) {
    let decision = jinn_kernel::common::phase_command::apply_phase(
        services,
        PhaseCommand::BeginStream {
            session_id: sid.clone(),
            kind: DispatchKind::FreshTurn,
            dispatched_at: jiff::Timestamp::now(),
        },
    )
    .await
    .expect("phase actor is spawned by the test helpers");
    assert!(
        decision.admitted,
        "a fresh mint is always admitted, got: {decision:?}"
    );
}

// ── SessionPhaseChanged → Idle trigger ────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_dispatches_user_message() {
    // Given a session with a queued user message.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged → Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Sending,
        new_phase: PhaseKind::Idle,
        at: jiff::Timestamp::now(),
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::ToolContinuation);
    }
    mint_live_generation(&actor.services, &sid).await;

    // When receiving SessionPhaseChanged → Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Sending,
        new_phase: PhaseKind::Idle,
        at: jiff::Timestamp::now(),
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged → Sending (not Idle).
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Idle,
        new_phase: PhaseKind::Sending,
        at: jiff::Timestamp::now(),
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
        at: jiff::Timestamp::now(),
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.steering_buffer_mut().push_fragment("stay focused");
    }

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid.clone(),
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
        at: jiff::Timestamp::now(),
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
            jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } if expanded == "stay focused"
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
        at: jiff::Timestamp::now(),
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
async fn idle_with_both_buffers_dispatches_steering_first_and_keeps_queue() {
    // Given a session with BOTH a queued user message and a steering fragment.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user(
            "queued msg",
        ))));
        session.steering_buffer_mut().push_fragment("stay focused");
    }

    // When receiving SessionPhaseChanged -> Idle.
    actor
        .handle_session_phase_changed(&SessionPhaseChanged {
            session_id: sid.clone(),
            old_phase: PhaseKind::Streaming,
            new_phase: PhaseKind::Idle,
            at: jiff::Timestamp::now(),
        })
        .await;

    // Then the steering entry won the idle slot: it is the submitted user
    // message and the single dispatched turn.
    let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
    assert_eq!(submitted.len(), 1, "exactly one turn dispatched");
    let dispatched = match &submitted.first().expect("one submission").entry.kind {
        jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } => expanded.as_str(),
        other => panic!("expected a user entry dispatch, got {other:?}"),
    };
    assert_eq!(
        dispatched, "stay focused",
        "steering must win the idle slot over the queue"
    );
    // And the steering entry is in history.
    let state = state.read();
    let session = state.session(&sid);
    let has_steering = session.history().iter().any(|e| {
        matches!(
            &e.kind,
            jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } if expanded == "stay focused"
        )
    });
    assert!(
        has_steering,
        "dispatched steering entry must appear in history; history = {:?}",
        session.history()
    );
    // And the queued item is still queued (waits for the next idle slot).
    assert_eq!(
        session.queue_len(),
        1,
        "queued item must stay queued while steering dispatches"
    );
    // And the steering buffer was drained (not orphaned).
    assert!(
        session.steering_buffer().is_empty(),
        "steering must be dispatched, not left buffered"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn queued_item_dispatches_after_steering_turn_completes() {
    // Given a session with BOTH a queued user message and a steering fragment.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user(
            "queued msg",
        ))));
        session.steering_buffer_mut().push_fragment("stay focused");
    }

    // When the idle slot opens (steering turn) and then opens again (the
    // steering turn completed).
    for _ in 0..2 {
        actor
            .handle_session_phase_changed(&SessionPhaseChanged {
                session_id: sid.clone(),
                old_phase: PhaseKind::Streaming,
                new_phase: PhaseKind::Idle,
                at: jiff::Timestamp::now(),
            })
            .await;
    }

    // Then the steering entry was submitted first and the queued message
    // second — each as its own turn, in that order.
    let submitted: Vec<ChatEntrySubmitted> = audit.of_type::<ChatEntrySubmitted>();
    let texts: Vec<&str> = submitted
        .iter()
        .filter_map(|s| match &s.entry.kind {
            jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["stay focused", "queued msg"],
        "steering dispatches first; the queued item dispatches at the next idle slot"
    );
    // And the queue is empty after both turns.
    let state = state.read();
    let session = state.session(&sid);
    assert!(session.message_queue().is_empty(), "queue fully consumed");
    // And the steering buffer is empty after both turns.
    assert!(
        session.steering_buffer().is_empty(),
        "steering buffer fully consumed"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn idle_transition_publishes_idle_to_streaming_phase_change() {
    // Given a session in Idle with a queued user message.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.enqueue(QueueItem::UserMessage(Box::new(ChatEntry::user("hello"))));
    }

    // When receiving SessionPhaseChanged -> Idle.
    let msg = SessionPhaseChanged {
        session_id: sid,
        old_phase: PhaseKind::Streaming,
        new_phase: PhaseKind::Idle,
        at: jiff::Timestamp::now(),
    };
    actor.handle_session_phase_changed(&msg).await;

    // Then the Idle -> Streaming transition was published — by the phase
    // actor, whose fused admission edge mints the turn; the queue actor
    // publishes no phase event of its own.
    let phases: Vec<SessionPhaseChanged> = audit.of_type::<SessionPhaseChanged>();
    let streaming = phases
        .iter()
        .find(|p| p.old_phase == PhaseKind::Idle && p.new_phase == PhaseKind::Streaming);
    assert!(
        streaming.is_some(),
        "Idle -> Streaming transition must be published, got: {phases:?}"
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
        let mut state = state.write();
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
async fn dispatch_user_message_transitions_to_streaming() {
    // Given a queue actor.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    let entry = ChatEntry::user("hello");

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then the session is in Streaming phase — the admission ask IS the
    // phase write now, and its fused edge lands on Streaming.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.phase(), PhaseKind::Streaming);
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
    if let jinn_kernel::protocol::ChatEntryKind::User { outcome, .. } = &mut entry.kind {
        outcome.degraded.push(jinn_kernel::protocol::ResolvedToken {
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
            jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } => Some(expanded.clone()),
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Single(
            "my-uncatalogued-llama".to_owned(),
        );
    }
    // Build an entry that already carries an attachment (as if resolved).
    let mut entry = ChatEntry::user("describe this");
    if let jinn_kernel::protocol::ChatEntryKind::User { attachments, .. } = &mut entry.kind {
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
        .any(|e| matches!(&e.kind, jinn_kernel::protocol::ChatEntryKind::Error(_)));
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
        let mut state = state.write();
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
async fn dispatch_user_message_does_not_absorb_steering_fragments() {
    // Given a session with steering fragments pending (submitted mid-turn by
    // the user) and a queued entry about to dispatch.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.steering_buffer_mut().push_fragment("system note");
    }
    let entry = ChatEntry::user("hello");

    // When dispatching a user message from the queue.
    actor
        .dispatch_user_message(&sid, &entry, StreamOrigin::User)
        .await;

    // Then ONLY the queued entry was pushed — the steering fragment is NOT
    // co-injected into this turn's history push.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(
        session.history().len(),
        1,
        "queued dispatch pushes exactly the queued entry; steering waits for the next idle slot"
    );
    // And the steering fragment is still buffered for its own turn.
    assert_eq!(
        session.steering_buffer().len(),
        1,
        "steering fragments must stay buffered, not absorbed into a queued dispatch"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_does_not_absorb_steering_fragments() {
    // Given a session with steering fragments pending.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        state
            .session_mut_or_create(&sid)
            .steering_buffer_mut()
            .push_fragment("system note");
    }
    // And a live generation, as the enqueue path mints before a queued
    // resume drains at the idle slot.
    mint_live_generation(&actor.services, &sid).await;

    // When dispatching a tool continuation (queued resume).
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then no steering entry was pushed — the fragment stays buffered for
    // the next idle slot (it will steer the following turn).
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(
        session.steering_buffer().len(),
        1,
        "queued resume must not absorb steering fragments"
    );
    assert!(
        session.history().is_empty(),
        "resume pushes no steering entry; history = {:?}",
        session.history()
    );
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
    // Given a queue actor and a live generation (the enqueue path mints it
    // before a queued continuation drains at the idle slot).
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();
    mint_live_generation(&actor.services, &sid).await;

    // When dispatching a resume.
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then SendToLlmProvider was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(sends.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_leaves_history_unchanged() {
    // Given a session with existing history (prepared by the session actor).
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("earlier turn"));
    }
    // And a live generation, as the enqueue path mints before a queued
    // resume drains at the idle slot.
    mint_live_generation(&actor.services, &sid).await;

    // When dispatching a resume.
    actor.dispatch_resume(&sid, StreamOrigin::User).await;

    // Then the history is unchanged — a queued resume only re-sends the
    // current history (no entry push, no steering drain).
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.history().len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_resume_does_not_emit_chat_entry_submitted_or_persist() {
    // Given a queue actor and a live generation (the enqueue path mints it
    // before a queued continuation drains at the idle slot).
    let (actor, _state, audit) = create_actor().await;
    let sid = session_id();
    mint_live_generation(&actor.services, &sid).await;

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
    // session actor before publishing, whose enqueue path minted the turn).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("already pushed"));
    }
    mint_live_generation(&actor.services, &sid).await;

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
        let mut state = state.write();
        state.session_mut_or_create(&sid);
    }
    mint_live_generation(&actor.services, &sid).await;
    {
        let mut app_state = actor.services.app_state_storage.read();
        app_state.reasoning_effort = Some(jinn_kernel::ReasoningEffort::High);
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().reasoning_effort = Some(jinn_kernel::ReasoningEffort::Low);
    }
    mint_live_generation(&actor.services, &sid).await;
    {
        let mut app_state = actor.services.app_state_storage.read();
        app_state.reasoning_effort = Some(jinn_kernel::ReasoningEffort::High);
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
        Some(jinn_kernel::ReasoningEffort::Low),
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
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("prepared"));
        session.steering_buffer_mut().push_fragment("late note");
    }
    mint_live_generation(&actor.services, &sid).await;

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
            jinn_kernel::protocol::ChatEntryKind::User { expanded, .. } if expanded == "late note"
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
    // Given a session whose prepared turn is ready and whose generation the
    // enqueue path minted before publishing DispatchTurn.
    let (actor, state, _audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("history"));
    }
    mint_live_generation(&actor.services, &sid).await;

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the session is in Streaming phase — the admission ask IS the
    // phase write now.
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
async fn dispatch_turn_publishes_idle_to_streaming_phase_change() {
    // Given an Idle session whose prepared turn is ready and whose
    // generation the enqueue path minted before publishing DispatchTurn.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.push_entry(ChatEntry::user("history"));
    }
    mint_live_generation(&actor.services, &sid).await;

    // When handling DispatchTurn.
    actor
        .handle_dispatch_turn(&DispatchTurn {
            session_id: sid.clone(),
        })
        .await;

    // Then the Idle -> Streaming transition was published by the phase
    // actor — at the enqueue path's admission mint in the Given. The
    // DispatchTurn admission itself joins the live generation without a
    // further event, and the queue actor publishes no phase event of its
    // own.
    let phases: Vec<SessionPhaseChanged> = audit.of_type::<SessionPhaseChanged>();
    let streaming = phases
        .iter()
        .find(|p| p.old_phase == PhaseKind::Idle && p.new_phase == PhaseKind::Streaming);
    assert!(
        streaming.is_some(),
        "Idle -> Streaming transition must be published, got: {phases:?}"
    );
    // And it is the only phase transition on the bus.
    assert_eq!(
        phases.len(),
        1,
        "no queue-actor phase events, got: {phases:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_preserves_round_robin_index_mutation() {
    // Given a session on an alloy with a round-robin strategy, and the
    // live generation the enqueue path mints before the queue receives
    // the prepared dispatch — without it the admission ask refuses, by
    // design (see `dispatch_turn_refused_when_no_generation_was_minted`).
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Alloy {
            models: vec!["p/a".to_owned(), "p/b".to_owned()],
            strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 },
        };
    }
    mint_live_generation(&actor.services, &sid).await;

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
async fn dispatch_turn_refused_when_no_generation_was_minted() {
    // Given a Sending session with a queued item (busy-session sends wait
    // for Idle; DispatchTurn must not steal them) and NO live generation —
    // the DispatchTurn arrived without the enqueue path's mint behind it.
    let (actor, state, audit) = create_actor().await;
    let sid = session_id();
    {
        let mut state = state.write();
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

    // Then the admission ask refused the prepared turn (no live generation
    // behind it — the phase-flap fix), so nothing was published.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert!(
        sends.is_empty(),
        "a prepared dispatch with no minted generation is refused and publishes nothing"
    );
    // And the queued item is still queued.
    let state = state.read();
    let session = state.session(&sid);
    assert_eq!(session.queue_len(), 1, "queue must be untouched");
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_turn_with_assembly_failure_publishes_nothing() {
    // Given a session whose assembly will fail (no service spawned).
    let (bus, audit) = jinn_kernel::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus.clone()).await;
    let state = State::new(AppState::default_with_scope_focus());
    // The real phase actor, so the admission ask succeeds and the failure
    // under test is assembly, not admission. NOTE:
    // jinn_context_assembly::service::ensure_spawned deliberately NOT
    // called — that ask fails.
    let _ = jinn_session_turn::phase_actor::ensure_spawned(
        &services.trouper_system,
        state.clone(),
        bus,
    );
    let actor = QueueActor {
        state: state.clone(),
        services,
    };
    let sid = session_id();
    {
        let mut state = state.write();
        state.session_mut_or_create(&sid).begin_sending();
    }
    mint_live_generation(&actor.services, &sid).await;

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

// ── Routing endpoint resolution ─────────────────────────────────────────

/// A services set whose provider config pins `model` to `tag`.
///
/// The pin lives in `providers.toml`, not in session state, so a dispatch
/// test sets it on the registry rather than on the session profile.
async fn services_pinning(model: &str, tag: Option<&str>) -> Services {
    let config = ProvidersConfig {
        providers: std::collections::BTreeMap::from([(
            "openrouter".to_owned(),
            ProviderEntry {
                model_info: Vec::new(),
                backend: "openrouter".to_owned(),
                models: vec!["anthropic/claude-sonnet-4".to_owned()],
                base_url: None,
                api_key_env: None,
                requires_key: false,
                extra_body: None,
                context_length: None,
            },
        )]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: tag
            .map(|tag| {
                vec![jinn_kernel::EndpointDefault {
                    model: model.to_owned(),
                    tag: tag.to_owned(),
                }]
            })
            .unwrap_or_default(),
    };
    let (bus, _audit) = jinn_kernel::BusService::new_recording();
    let services = Services::new_fake_with_bus(bus).await;
    services
        .provider_registry
        .replace(jinn_kernel::ProviderRegistry::from_config(config).expect("registry"));
    services
}

/// A queue actor on `services`, with `sid`'s model set to `model`.
fn actor_on(services: Services, model: &str) -> (QueueActor, State, SessionId, BusAudit) {
    let (bus, audit) = jinn_kernel::BusService::new_recording();
    let mut services = services;
    services.bus = bus.clone();
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
    let state = State::new(AppState::default_with_scope_focus());
    // The real phase actor answers the dispatch's admission ask; a
    // `FreshTurn` mint is always admitted, so this only needs the actor
    // live, on the same system the queue actor asks through.
    let _ = jinn_session_turn::phase_actor::ensure_spawned(
        &services.trouper_system,
        state.clone(),
        bus,
    );
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model =
            jinn_core_types::model_selection::ModelSelection::Single(model.to_owned());
    }
    (
        QueueActor {
            state: state.clone(),
            services,
        },
        state,
        sid,
        audit,
    )
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_forces_the_endpoint_pinned_for_the_model() {
    // Given a provider config pinning the model to an upstream.
    let services = services_pinning(PINNED_MODEL, Some("anthropic")).await;
    let (actor, _state, sid, audit) = actor_on(services, PINNED_MODEL);

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &ChatEntry::user("hello"), StreamOrigin::User)
        .await;

    // Then the send carries the pinned routing tag.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert_eq!(
        sends.first().expect("one send").endpoint_tag.as_deref(),
        Some("anthropic")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_auto_routes_a_model_with_no_pinned_endpoint() {
    // Given a provider config with no endpoint pins.
    let services = services_pinning(PINNED_MODEL, None).await;
    let (actor, _state, sid, audit) = actor_on(services, PINNED_MODEL);

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &ChatEntry::user("hello"), StreamOrigin::User)
        .await;

    // Then the send forces no upstream, leaving OpenRouter to auto-route.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert!(
        sends.first().expect("one send").endpoint_tag.is_none(),
        "a model with no [[endpoint_defaults]] row must auto-route"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_ignores_a_pin_keyed_to_a_different_model() {
    // Given a provider config pinning one model.
    let services = services_pinning("openrouter/some-other-model", Some("anthropic")).await;

    // And a session on a different model.
    let (actor, _state, sid, audit) = actor_on(services, PINNED_MODEL);

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &ChatEntry::user("hello"), StreamOrigin::User)
        .await;

    // Then no upstream is forced — the pin belongs to another model.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert!(
        sends.first().expect("one send").endpoint_tag.is_none(),
        "a pin keyed to another model must not apply"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn dispatch_does_not_force_an_endpoint_for_an_alloy() {
    // Given a provider config pinning one model.
    let services = services_pinning(PINNED_MODEL, Some("anthropic")).await;
    let (bus, audit) = jinn_kernel::BusService::new_recording();
    let mut services = services;
    services.bus = bus.clone();
    let _ = jinn_context_assembly::service::ensure_spawned(&services.trouper_system);
    let state = State::new(AppState::default_with_scope_focus());
    let _ = jinn_session_turn::phase_actor::ensure_spawned(
        &services.trouper_system,
        state.clone(),
        bus,
    );
    let sid = session_id();
    {
        let mut state = state.write();
        let session = state.session_mut_or_create(&sid);
        session.profile_mut().model = jinn_core_types::model_selection::ModelSelection::Alloy {
            models: vec![PINNED_MODEL.to_owned()],
            strategy: jinn_core_types::AlloyStrategy::RoundRobin { index: 0 },
        };
    }
    let actor = QueueActor {
        state: state.clone(),
        services,
    };

    // When dispatching a user message.
    actor
        .dispatch_user_message(&sid, &ChatEntry::user("hello"), StreamOrigin::User)
        .await;

    // Then no upstream is forced — an alloy chooses its own routing.
    let sends: Vec<SendToLlmProvider> = audit.of_type::<SendToLlmProvider>();
    assert!(
        sends.first().expect("one send").endpoint_tag.is_none(),
        "an alloy must never have a pinned endpoint forced"
    );
}
