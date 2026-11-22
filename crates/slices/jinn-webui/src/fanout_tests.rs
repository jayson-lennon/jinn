//! Fan-out actor tests: batching, per-session scoping, and the
//! flush-before-full ordering on settle — driven through the actor's
//! plain handler methods (`MsgHandler` impls are one-line delegations
//! to these, and `MsgCtx` is trouper-internal so tests cannot fabricate
//! one).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions use expect/panic to state what went wrong"
)]

use jinn_inference_msg::StreamToken;
use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_slices::cell::TypedCell;
use jinn_webui_msg::SseEnvelope;
use jinn_webui_msg::SseKind;
use jinn_webui_msg::UnreadSessionMarks;

use crate::fanout::WebUiFanoutActor;

/// A fresh actor with a subscribed receiver: `(actor, rx, state,
/// marks)`. The marks handle is the same cell the actor holds, and the
/// registry holding it is attached to the state's session map — the
/// same wiring production `activate` builds — so the actor's writes
/// are visible to the fragment renders the pushed HTML carries.
fn fixture() -> (
    WebUiFanoutActor,
    tokio::sync::broadcast::Receiver<SseEnvelope>,
    State,
    TypedCell<UnreadSessionMarks>,
) {
    let slices = Slices::new();
    let marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once per fixture");
    let state = State::new(jinn_app_state::AppState::default());
    state.write().session.attach_slices(slices);
    let (actor, rx) = WebUiFanoutActor::test_with_receiver(state.clone(), marks.clone());
    (actor, rx, state, marks)
}

fn token(session: &jinn_core_types::SessionId, text: &str) -> StreamToken {
    StreamToken {
        session_id: session.clone(),
        index: 0,
        token: text.to_owned(),
        is_thinking: false,
        dispatched_at: jiff::Timestamp::now(),
    }
}

/// Inserts a fresh live session and returns its id.
fn seed_session(state: &State) -> jinn_core_types::SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    id
}

#[rstest::rstest]
#[tokio::test]
async fn buffered_tokens_flush_as_one_fragment_per_window() {
    // Given a fan-out actor and a subscriber.
    let (mut actor, mut rx, _state, _marks) = fixture();
    let id = jinn_core_types::SessionId::new();

    // When three tokens land for the session and the window flushes.
    actor.append_token(&token(&id, "hel"));
    actor.append_token(&token(&id, "lo "));
    actor.append_token(&token(&id, "world"));
    actor.flush_for_test(&id);

    // Then exactly one chat fragment arrives, carrying every token.
    let first = rx.try_recv().expect("a flush is pending");
    assert_eq!(first.kind, SseKind::Chat);
    assert!(first.html.contains("hel"), "{}", first.html);
    assert!(first.html.contains("world"), "{}", first.html);
    assert!(rx.try_recv().is_err(), "exactly one fragment per window");
}

#[rstest::rstest]
#[tokio::test]
async fn a_flushed_session_buffer_does_not_leak_into_the_next_window() {
    // Given a session whose first window already flushed.
    let (mut actor, mut rx, _state, _marks) = fixture();
    let id = jinn_core_types::SessionId::new();
    actor.append_token(&token(&id, "first"));
    actor.flush_for_test(&id);
    let _ = rx.try_recv().expect("first window flush");

    // When a second window flushes with no new tokens.
    actor.flush_for_test(&id);

    // Then nothing is broadcast — an empty flush is noise, not signal.
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn an_idle_settle_flushes_the_partial_buffer_before_the_full_render() {
    // Given tokens buffered for a live session mid-stream.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    actor.append_token(&token(&id, "partial"));

    // When the session settles to idle.
    actor.settle_to_idle(&id);

    // Then the partial fragment lands BEFORE the full-transcript push —
    // the reverse order would render the final tokens twice.
    let first = rx.try_recv().expect("partial flush");
    assert_eq!(first.kind, SseKind::Chat, "flush first");
    let second = rx.try_recv().expect("full render");
    assert_eq!(second.kind, SseKind::TranscriptFull, "full render second");
    let third = rx.try_recv().expect("idle controls");
    assert_eq!(third.kind, SseKind::Controls, "controls last");
}

#[rstest::rstest]
#[tokio::test]
async fn a_busy_transition_pushes_the_controls_fragment() {
    // Given a live session and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);

    // When a busy phase change arrives.
    actor.push_busy_controls(&id);

    // Then exactly one controls fragment lands.
    let envelope = rx.try_recv().expect("busy controls");
    assert_eq!(envelope.kind, SseKind::Controls);
    assert_eq!(envelope.session_id, id);
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_activation_pushes_a_sessions_list_refresh() {
    // Given a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);

    // When an activation lands.
    actor.push_sessions_refresh(&id);

    // Then exactly one sessions fragment lands.
    let envelope = rx.try_recv().expect("sessions refresh");
    assert_eq!(envelope.kind, SseKind::Sessions);
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn a_cache_stored_event_pushes_one_models_signal_and_one_transcript_per_live_session() {
    // Given a default session plus two seeded ones, and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let first = seed_session(&state);
    let second = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When a cache-stored event lands (the provider cell already
    // holds the cache — the emit is post-write by contract).
    let stored_at = jiff::Timestamp::now();
    actor.push_models_changed(stored_at);

    // Then each live session receives exactly two envelopes in order:
    // the `models` SIGNAL (cursor 0; its data line carries the write
    // timestamp — a NON-EMPTY data line is the dispatch contract: the
    // browser discards an SSE event with an empty data buffer, which
    // is the never-stops-spinner bug) followed by the
    // transcript-full convergence render (cursor 0, full replace —
    // the carrier that puts the refresh summary transient on the page;
    // blank for a session that has not rendered any entry yet, which
    // the client's full replace treats as "clear", the same shape the
    // settle path pushes).
    let mut seen_models = std::collections::HashSet::new();
    let mut seen_transcripts = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        match envelope.kind {
            SseKind::Models => {
                assert_eq!(
                    envelope.html,
                    stored_at.to_string(),
                    "the models push carries the write timestamp as its data line \
                     (empty data never dispatches in a browser): {envelope:?}"
                );
                assert!(!envelope.html.is_empty(), "dispatchable, not empty");
                assert_eq!(envelope.cursor, 0);
                seen_models.insert(envelope.session_id.clone());
            }
            SseKind::TranscriptFull => {
                assert_eq!(envelope.cursor, 0);
                seen_transcripts.insert(envelope.session_id.clone());
            }
            other => panic!("only models and transcript-full pushes land: {other:?}"),
        }
    }
    assert_eq!(
        seen_models.len(),
        live,
        "one models push per session: {seen_models:?}"
    );
    assert_eq!(
        seen_transcripts.len(),
        live,
        "one transcript push per session: {seen_transcripts:?}"
    );
    assert!(
        seen_models.contains(&first) && seen_models.contains(&second),
        "{seen_models:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_runtime_stored_event_pushes_one_mcp_status_signal_per_live_session() {
    // Given a default session plus one seeded one, and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let first = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When a runtime-stored event lands (the coordinator emitted it
    // strictly after its cell write — the ordering contract the push
    // correctness rides).
    let stored_at = jiff::Timestamp::now();
    actor.push_mcp_status_changed(stored_at);

    // Then each live session receives exactly ONE envelope: the
    // `mcp-status` SIGNAL (cursor 0; its data line carries the write
    // timestamp — a NON-EMPTY data line is the dispatch contract) and
    // no transcript push rides it (a status transition touches no
    // transcript entry), and no HTML rows ride any push (the pull
    // endpoint remains the renderer).
    let mut seen = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        match envelope.kind {
            SseKind::McpStatus => {
                assert_eq!(
                    envelope.html,
                    stored_at.to_string(),
                    "the mcp push carries the write timestamp as its data line \
                     (empty data never dispatches in a browser): {envelope:?}"
                );
                assert_eq!(envelope.cursor, 0);
                seen.insert(envelope.session_id.clone());
            }
            other => panic!("only mcp-status pushes land: {other:?}"),
        }
    }
    assert_eq!(seen.len(), live, "one push per session: {seen:?}");
    assert!(seen.contains(&first), "{seen:?}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_runtime_stored_event_pushes_one_tools_signal_per_live_session() {
    // Given a default session plus one seeded one, and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let first = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When the runtime-stored handler's tools push lands (the same
    // handler emits it beside the mcp-status push: the connection set
    // moved, and connection change is the only thing that moves a
    // tools list).
    let stored_at = jiff::Timestamp::now();
    actor.push_tools_changed(stored_at);

    // Then each live session receives exactly ONE envelope: the
    // `tools` SIGNAL (cursor 0; its data line carries the write
    // timestamp — a NON-EMPTY data line is the dispatch contract) and
    // no transcript or HTML-rows push rides it — the client answers
    // the signal by re-pulling the rows route, which remains the
    // renderer.
    let mut seen = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        match envelope.kind {
            SseKind::Tools => {
                assert_eq!(
                    envelope.html,
                    stored_at.to_string(),
                    "the tools push carries the write timestamp as its data line \
                     (empty data never dispatches in a browser): {envelope:?}"
                );
                assert_eq!(envelope.cursor, 0);
                seen.insert(envelope.session_id.clone());
            }
            other => panic!("only tools pushes land: {other:?}"),
        }
    }
    assert_eq!(seen.len(), live, "one push per session: {seen:?}");
    assert!(seen.contains(&first), "{seen:?}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_stored_event_yields_both_the_mcp_and_tools_signals_per_session() {
    // Given the fixture actor with both push helpers fired — the
    // handler's whole body (the `MsgCtx` the real handler takes is
    // trouper-internal, so the coupling these two calls model is what
    // the test pins: one stored event, both signals, nothing else).
    let (mut actor, mut rx, state, _marks) = fixture();
    let first = seed_session(&state);
    let live = state.read().session.sessions().len();
    let stored_at = jiff::Timestamp::now();
    actor.push_mcp_status_changed(stored_at);
    actor.push_tools_changed(stored_at);

    // Then each live session receives exactly two envelopes — one
    // `mcp-status` and one `tools`, both signals with dispatchable
    // data lines carrying the same write timestamp — and nothing else
    // (no transcript, no HTML rows).
    let mut mcp = 0;
    let mut tools = 0;
    let mut sessions = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        sessions.insert(envelope.session_id.clone());
        match envelope.kind {
            SseKind::McpStatus => mcp += 1,
            SseKind::Tools => tools += 1,
            other => panic!("only the two signal kinds land: {other:?}"),
        }
        assert!(
            !envelope.html.is_empty(),
            "dispatchable data line: {envelope:?}"
        );
        assert_eq!(envelope.cursor, 0);
    }
    assert_eq!(mcp, live, "one mcp-status push per session");
    assert_eq!(tools, live, "one tools push per session");
    assert!(sessions.contains(&first), "the seeded session was served");
}

#[rstest::rstest]
#[tokio::test]
async fn a_skills_loaded_event_pushes_one_skills_signal_per_live_session() {
    // Given a default session plus one seeded one, and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let first = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When a skills-loaded event lands (the scan actor emitted it
    // strictly after writing the session's ephemeral set — the
    // ordering contract the push correctness rides).
    actor.push_skills_changed(jiff::Timestamp::now());

    // Then each live session receives exactly ONE envelope: the
    // `skills` SIGNAL (cursor 0; its data line carries the scan's
    // landing timestamp — a NON-EMPTY data line is the dispatch
    // contract) and no transcript push rides it (a discovery scan
    // touches no transcript entry), and no HTML rows ride any push
    // (the pull endpoint remains the renderer).
    let mut seen = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        match envelope.kind {
            SseKind::Skills => {
                assert!(
                    !envelope.html.is_empty(),
                    "the skills push carries a dispatchable data line: {envelope:?}"
                );
                assert_eq!(envelope.cursor, 0);
                seen.insert(envelope.session_id.clone());
            }
            other => panic!("only skills pushes land: {other:?}"),
        }
    }
    assert_eq!(seen.len(), live, "one push per session: {seen:?}");
    assert!(seen.contains(&first), "{seen:?}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_handle_pushes_a_sessions_refresh_from_outside_the_actor() {
    // Given a live session whose title a web rename just changed, and
    // a subscriber to the actor's broadcast channel.
    let (actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_title("Renamed from the web".to_owned());

    // When an HTTP handler renders the live fragment under its own
    // read guard and hands it to the handle (guard dropped first).
    let html = {
        let guard = state.read();
        crate::regions::sessions::sessions_live_fragment(&guard)
    };
    actor.handle().push_sessions_refresh_from(&id, html);

    // Then exactly one sessions envelope arrives, addressed to the
    // renamed session, carrying the new title — the push the other
    // connected pages' sidebars refresh from.
    let envelope = rx.try_recv().expect("handle-pushed sessions refresh");
    assert_eq!(envelope.kind, SseKind::Sessions);
    assert_eq!(envelope.session_id, id);
    assert!(
        envelope.html.contains("Renamed from the web"),
        "{}",
        envelope.html
    );
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn every_broadcast_names_its_own_session() {
    // Given tokens for two sessions buffered on one actor.
    let (mut actor, mut rx, _state, _marks) = fixture();
    let a = jinn_core_types::SessionId::new();
    let b = jinn_core_types::SessionId::new();
    actor.append_token(&token(&a, "aaa"));
    actor.append_token(&token(&b, "bbb"));

    // When flushing session A.
    actor.flush_for_test(&a);
    let envelope = rx.try_recv().expect("session A flush");

    // Then the envelope names A and carries A's tokens only — the SSE
    // endpoint filters on this, so a page left on A never renders B.
    assert_eq!(envelope.session_id, a);
    assert!(envelope.html.contains("aaa"), "{}", envelope.html);
    assert!(!envelope.html.contains("bbb"), "{}", envelope.html);
    // And B's buffer is untouched by A's flush.
    actor.flush_for_test(&b);
    let envelope = rx.try_recv().expect("session B flush");
    assert_eq!(envelope.session_id, b);
    assert!(envelope.html.contains("bbb"), "{}", envelope.html);
}

#[rstest::rstest]
#[tokio::test]
async fn thinking_tokens_render_in_the_thinking_span() {
    // Given a thinking token and a text token buffered together.
    let (mut actor, mut rx, _state, _marks) = fixture();
    let id = jinn_core_types::SessionId::new();
    let mut thinking = token(&id, "pondering");
    thinking.is_thinking = true;

    // When both flush.
    actor.append_token(&thinking);
    actor.append_token(&token(&id, "answer"));
    actor.flush_for_test(&id);
    let envelope = rx.try_recv().expect("flush");

    // Then each token carries its own span class.
    assert!(
        envelope.html.contains("tok-thinking\">pondering"),
        "{}",
        envelope.html
    );
    assert!(
        envelope.html.contains("tok-text\">answer"),
        "{}",
        envelope.html
    );
}

#[rstest::rstest]
#[tokio::test]
async fn session_created_pushes_a_sessions_list_refresh() {
    // Given a fan-out actor and a subscriber.
    let (mut actor, mut rx, _state, _marks) = fixture();
    let id = jinn_core_types::SessionId::new();

    // When a browser-created session event lands.
    actor.on_session_created(&jinn_session_lifecycle_msg::SessionCreated {
        session_id: id.clone(),
        cwd: jinn_chat_input_msg::home_dir(),
    });

    // Then the pushed envelope is a sessions-list refresh for that
    // session — every connected page's sidebar updates.
    let envelope = rx.try_recv().expect("the sessions push");
    assert_eq!(envelope.kind, SseKind::Sessions);
    assert_eq!(envelope.session_id, id);
}

#[rstest::rstest]
#[tokio::test]
async fn session_setup_completed_pushes_a_transcript_convergence_for_that_session() {
    // Given a fan-out actor, a subscriber, and a seeded session with
    // one entry (the entry the setup failure/success note wrote).
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    if let Some(session) = state.write().session.get_mut(&id) {
        session.push_entry(jinn_core_types::ChatEntry::user("setup ran"));
    }

    // When the setup-completed event lands.
    actor.on_session_setup_completed(&jinn_session_lifecycle_msg::SessionSetupCompleted {
        session_id: id.clone(),
        cwd: jinn_chat_input_msg::home_dir(),
        error: None,
    });

    // Then the pushed envelope is a transcript-full convergence for
    // that session, carrying the entry — cursor 0, full replace; and
    // the composer controls are untouched (one push only).
    let envelope = rx.try_recv().expect("the transcript push");
    assert_eq!(envelope.kind, SseKind::TranscriptFull);
    assert_eq!(envelope.session_id, id);
    assert_eq!(envelope.cursor, 0);
    assert!(envelope.html.contains("setup ran"), "{}", envelope.html);
    assert!(
        rx.try_recv().is_err(),
        "no controls push rides the setup completion"
    );
}

// ── The unread mark: set on an unseen settle ────────────────────────

/// A phase-change event naming the transition.
fn phase_change(
    session: &jinn_core_types::SessionId,
    old_phase: jinn_slices::PhaseView,
    new_phase: jinn_slices::PhaseView,
) -> jinn_session_msg::SessionPhaseChanged {
    jinn_session_msg::SessionPhaseChanged {
        session_id: session.clone(),
        old_phase,
        new_phase,
        at: jiff::Timestamp::now(),
    }
}

/// Drains `count` sessions-list envelopes off the receiver, returning
/// the last one's HTML (they all carry the same render).
fn last_sessions_html(
    rx: &mut tokio::sync::broadcast::Receiver<SseEnvelope>,
    count: usize,
) -> String {
    let mut html = String::new();
    let mut seen = 0;
    while seen < count {
        match rx.try_recv() {
            Ok(envelope) => {
                if envelope.kind == SseKind::Sessions {
                    html = envelope.html;
                    seen += 1;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                panic!("expected {count} sessions envelopes, got {seen}")
            }
            Err(other) => panic!("broadcast error: {other:?}"),
        }
    }
    html
}

/// Seeds a session with the given origin, returning its id.
fn seed_session_with_origin(
    state: &State,
    origin: jinn_session_msg::SessionOrigin,
) -> jinn_core_types::SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_origin(origin);
    id
}

#[rstest::rstest]
#[tokio::test]
async fn an_unseen_settle_marks_the_session_and_refreshes_every_list() {
    // Given two live sessions, Bar active, and a subscriber.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    assert!(
        state.write().session.set_active(bar.clone()),
        "bar activates"
    );
    let live = state.read().session.sessions().len();

    // When Foo settles Streaming → Idle (the settle the user did not
    // watch).
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));

    // Then Foo's mark landed, and every live session's stream received
    // one addressed sessions refresh carrying the unread class — the
    // per-stream filter means an addressed push would miss the page
    // viewing Bar, which is exactly where the news is.
    assert!(marks.read().is_marked(&foo), "the mark landed");
    assert!(!marks.read().is_marked(&bar));
    let html = last_sessions_html(&mut rx, live);
    assert!(html.contains("unread"), "the fragment marks Foo: {html}");
    assert!(
        !marks_of(&state, &bar),
        "the active session is never marked"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_active_session_never_marks_on_its_own_settle() {
    // Given one live session, active, with a subscriber.
    let (mut actor, mut rx, state, marks) = fixture();
    let id = seed_session(&state);
    assert!(state.write().session.set_active(id.clone()), "activates");

    // When the session runs its own Idle → Streaming → Idle cycle
    // while the user watches it.
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Idle,
        jinn_slices::PhaseView::Streaming,
    ));
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));

    // Then no mark ever lands — a settle the user is watching is not
    // news — and every sessions envelope the working flips fan carries
    // no unread class for it. (The flips themselves DO fan now: the
    // row spinner is rendered truth for every viewer.)
    assert!(!marks.read().is_marked(&id));
    let mut any_unread = false;
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions && envelope.html.contains("unread") {
            any_unread = true;
        }
    }
    assert!(
        !any_unread,
        "no sessions refresh ever marks the viewed session"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_subagent_session_never_marks() {
    // Given a subagent-origin session while another session is active.
    let (mut actor, mut rx, state, marks) = fixture();
    let sub = seed_session_with_origin(&state, jinn_session_msg::SessionOrigin::Subagent);
    let other = seed_session(&state);
    assert!(
        state.write().session.set_active(other.clone()),
        "the other session activates"
    );
    let _ = rx.try_recv();

    // When the subagent settles Streaming → Idle.
    actor.on_phase_changed(&phase_change(
        &sub,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));

    // Then no mark lands — the exclusion is decided at set time — and
    // any sessions envelope the working flip fans carries no unread
    // class for it. (The flip fans now: the row spinner clears for
    // every viewer; the mark stays silent.)
    assert!(!marks.read().is_marked(&sub));
    let mut any_unread = false;
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions && envelope.html.contains("unread") {
            any_unread = true;
        }
    }
    assert!(!any_unread, "no refresh ever marks a subagent settle");
    // Both seeds are live (plus the map's default session) — the
    // exclusion decided nothing about liveness, only about marking.
    let live = state.read().session.sessions().len();
    assert_eq!(live, 3, "both seeds are live (exclusion is by origin)");
}

#[rstest::rstest]
#[tokio::test]
async fn a_cancel_settle_marks_like_any_other_settle() {
    // Given a non-active session draining a cancel.
    let (mut actor, _rx, state, marks) = fixture();
    let id = seed_session(&state);
    let other = seed_session(&state);
    assert!(
        state.write().session.set_active(other.clone()),
        "the other session activates"
    );

    // When the cancel drain lands the session at Idle.
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Cancelling,
        jinn_slices::PhaseView::Idle,
    ));

    // Then the mark is set — a cancelled turn delivered entries the
    // user has not seen.
    assert!(marks.read().is_marked(&id));
}

#[rstest::rstest]
#[tokio::test]
async fn a_self_settle_from_idle_marks_nothing() {
    // Given a non-active session that reports Idle → Idle (a no-op
    // transition the phase writers can emit).
    let (mut actor, _rx, state, marks) = fixture();
    let id = seed_session(&state);
    let other = seed_session(&state);
    assert!(
        state.write().session.set_active(other.clone()),
        "the other session activates"
    );

    // When an Idle → Idle change arrives.
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Idle,
        jinn_slices::PhaseView::Idle,
    ));

    // Then nothing is marked — there was no work delivered.
    assert!(!marks.read().is_marked(&id));
}

// ── The working flip: the sessions list refreshes only on it ────────

/// Whether any rendered row's class list carries the working hook —
/// token-parsed, the way the browser reads `class`.
fn has_working_row(html: &str) -> bool {
    html.split("class=\"").skip(1).any(|rest| {
        rest.split('"')
            .next()
            .unwrap_or_default()
            .split(' ')
            .any(|part| part == crate::contract::WORKING_CLASS)
    })
}

#[rstest::rstest]
#[tokio::test]
async fn a_working_flip_fans_one_refresh_to_every_live_session() {
    // Given three live sessions and a subscriber, none active-marked.
    let (mut actor, mut rx, state, _marks) = fixture();
    let busy = seed_session(&state);
    let _ = seed_session(&state);
    let _ = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When a turn starts (Idle → Streaming — the working flip that a
    // turn start is). The projection write lands BEFORE the event, the
    // production order (the ledger's mint writes, then publishes).
    state
        .write()
        .session
        .get_mut(&busy)
        .expect("seeded session")
        .begin_streaming();
    actor.on_phase_changed(&phase_change(
        &busy,
        jinn_slices::PhaseView::Idle,
        jinn_slices::PhaseView::Streaming,
    ));

    // Then exactly one addressed sessions envelope per live session
    // lands, and every copy renders the working class — the list
    // refresh is the spinner's only live path.
    let html = last_sessions_html(&mut rx, live);
    assert!(
        has_working_row(&html),
        "the pushed fragment carries the spinner: {html}"
    );
    assert!(
        rx.try_recv().is_err(),
        "one wave, nothing else: no extra envelopes after the fan"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_streaming_to_working_boundary_fans_nothing() {
    // Given a live session and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);

    // When a Streaming → Working tool-loop boundary lands (busy →
    // busy: no rendered fact changed). The projection write lands
    // before the event, the production order.
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .begin_sending();
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Working,
    ));

    // Then no sessions envelope fans — the composer's busy controls
    // push is the event's whole visible surface, and it is not a
    // sessions envelope.
    let mut any_sessions = false;
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions {
            any_sessions = true;
        }
    }
    assert!(
        !any_sessions,
        "a busy→busy boundary never refreshes the sessions list"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_settling_unseen_session_fans_one_wave_carrying_both_facts() {
    // Given two live sessions, Bar active, Foo busy.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    assert!(
        state.write().session.set_active(bar.clone()),
        "bar activates"
    );
    let live = state.read().session.sessions().len();

    // When Foo settles Streaming → Idle — the working flip AND the
    // unseen-settle mark in one event. The projection write lands
    // before the event, the production order.
    state
        .write()
        .session
        .get_mut(&foo)
        .expect("seeded session")
        .settle_to_idle();
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));

    // Then exactly `live` sessions envelopes land (the single shared
    // fan — not one wave for the flip and a second for the mark), and
    // the fragment carries the mark while carrying no working class
    // for Foo (the settle cleared it).
    let html = last_sessions_html(&mut rx, live);
    assert!(marks.read().is_marked(&foo), "the mark landed");
    assert!(
        html.contains("unread"),
        "the shared fan carries the mark: {html}"
    );
    assert!(
        rx.try_recv().is_err(),
        "one wave total — flip and mark share it: no extra envelopes"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_watched_settle_still_fans_the_flip_wave() {
    // Given one live session, active, and a subscriber.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    assert!(state.write().session.set_active(id.clone()), "activates");
    let live = state.read().session.sessions().len();

    // When the watched session settles Streaming → Idle (no mark —
    // the user is watching it). The projection write lands before the
    // event, the production order.
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .settle_to_idle();
    actor.on_phase_changed(&phase_change(
        &id,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));

    // Then the flip still fans one wave — the spinner clearing is a
    // rendered fact regardless of who was watching.
    let html = last_sessions_html(&mut rx, live);
    assert!(
        !has_working_row(&html),
        "the settled fragment carries no spinner: {html}"
    );
    assert!(rx.try_recv().is_err(), "one wave, nothing else");
}

// ── The unread mark: cleared when the session is viewed ─────────────

#[rstest::rstest]
#[tokio::test]
async fn activating_a_marked_session_clears_it_and_refreshes_every_list() {
    // Given a marked session Foo and an active session Bar.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    assert!(state.write().session.set_active(bar.clone()), "activates");
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    assert!(marks.read().is_marked(&foo), "precondition: Foo is marked");
    // Drain the mark transition's traffic before observing the clear.
    while let Ok(_envelope) = rx.try_recv() {}

    // When the activation event for Foo lands (the store actor
    // publishes it on every activation — disk load or in-memory).
    actor.on_active_session_changed(&jinn_kernel::protocol::system::ActiveSessionChanged {
        session_id: foo.clone(),
    });

    // Then the mark is gone, and every page's list refreshed without
    // the unread class — one addressed refresh for the activated
    // session plus one addressed envelope per live session id.
    assert!(!marks.read().is_marked(&foo));
    let live = state.read().session.sessions().len();
    let mut sessions_envelopes = 0;
    let mut last_html = String::new();
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions {
            sessions_envelopes += 1;
            last_html = envelope.html;
        }
    }
    assert_eq!(
        sessions_envelopes,
        live + 1,
        "the addressed refresh plus one envelope per live session: got {sessions_envelopes}"
    );
    assert!(
        !last_html.contains("unread"),
        "the post-clear fragment is clean: {last_html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn opening_a_marked_session_page_clears_its_mark() {
    // Given a marked session Foo and an active session Bar — Foo's
    // mark landed without any activation anywhere.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    assert!(state.write().session.set_active(bar.clone()), "activates");
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    assert!(marks.read().is_marked(&foo), "precondition: Foo is marked");
    while let Ok(_envelope) = rx.try_recv() {}

    // When a page arrives at Foo — the GET page render's event, which
    // activates nothing.
    actor.on_session_page_opened(&jinn_webui_msg::SessionPageOpened {
        session_id: foo.clone(),
    });

    // Then the mark is gone: opening the page IS the viewing, no
    // activation required.
    assert!(!marks.read().is_marked(&foo));
    let live = state.read().session.sessions().len();
    let mut sessions_envelopes = 0;
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions {
            sessions_envelopes += 1;
        }
    }
    assert_eq!(
        sessions_envelopes,
        live + 1,
        "the addressed refresh plus one envelope per live session"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_second_clear_of_the_same_session_fans_nothing() {
    // Given a marked session whose clear already landed.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    assert!(marks.read().is_marked(&foo), "precondition: Foo is marked");
    actor.on_active_session_changed(&jinn_kernel::protocol::system::ActiveSessionChanged {
        session_id: foo.clone(),
    });
    assert!(!marks.read().is_marked(&foo));
    while let Ok(_envelope) = rx.try_recv() {}

    // When the same activation event arrives again (a TUI switch
    // landing on an already-active session, say).
    actor.on_active_session_changed(&jinn_kernel::protocol::system::ActiveSessionChanged {
        session_id: foo.clone(),
    });

    // Then the transition gate keeps the re-clear quiet — one
    // addressed refresh for the event, no per-live-session fan.
    let mut sessions_envelopes = 0;
    while let Ok(envelope) = rx.try_recv() {
        if envelope.kind == SseKind::Sessions {
            sessions_envelopes += 1;
        }
    }
    assert_eq!(sessions_envelopes, 1, "addressed refresh only, no fan");
}

#[rstest::rstest]
#[tokio::test]
async fn a_background_tree_member_keeps_its_mark_on_load() {
    // Given a marked session Foo, active Bar, and a third session Baz.
    let (mut actor, mut rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    let baz = seed_session(&state);
    assert!(state.write().session.set_active(bar.clone()), "activates");
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    actor.on_phase_changed(&phase_change(
        &baz,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    assert!(marks.read().is_marked(&foo), "Foo marked");
    assert!(marks.read().is_marked(&baz), "Baz marked");
    let _ = rx.try_recv();

    // When a tree load restores Foo as a BACKGROUND member — its
    // load-completed event arrives while Baz is the active session.
    actor.on_load_completed(&jinn_session_store_msg::SessionLoadCompleted {
        session_id: foo.clone(),
    });

    // Then Foo keeps its mark — a load-completed refreshes the list
    // only; the clear lives on the viewed events (activation, page
    // arrival), and nobody viewed Foo. (Baz is untouched too.)
    assert!(
        marks.read().is_marked(&foo),
        "a background member keeps its mark"
    );
    assert!(marks.read().is_marked(&baz));
}

#[rstest::rstest]
#[tokio::test]
async fn a_tui_activation_event_clears_a_mark_that_never_touched_the_web() {
    // Given a marked session Foo and an active session Bar.
    let (mut actor, _rx, state, marks) = fixture();
    let foo = seed_session(&state);
    let bar = seed_session(&state);
    assert!(state.write().session.set_active(bar.clone()), "activates");
    actor.on_phase_changed(&phase_change(
        &foo,
        jinn_slices::PhaseView::Streaming,
        jinn_slices::PhaseView::Idle,
    ));
    assert!(marks.read().is_marked(&foo), "precondition: Foo is marked");

    // When the TUI switches to Foo — the kernel's intent handler
    // publishes the same ActiveSessionChanged event the store actor
    // does, so the fan-out needs no TUI-specific path.
    actor.on_active_session_changed(&jinn_kernel::protocol::system::ActiveSessionChanged {
        session_id: foo.clone(),
    });

    // Then the mark is gone — activation anywhere in the instance is
    // the viewing fact, whatever surface drove it.
    assert!(!marks.read().is_marked(&foo));
}

/// The marks set through the state's attached registry — the read side
/// the render paths use (asserts the same cell the actor writes).
fn marks_of(state: &State, id: &jinn_core_types::SessionId) -> bool {
    let guard = state.read();
    let slices = guard.session.slices().expect("registry attached");
    let cell = slices
        .reader::<UnreadSessionMarks>(&jinn_webui_msg::webui_unread_slot())
        .expect("the unread cell resolves");
    cell.read().is_marked(id)
}
