//! Fan-out actor tests: the settled-boundary lifecycle, the tail
//! replace, per-session scoping, and the boundary-clear-before-settle
//! ordering — driven through the actor's plain handler methods
//! (`MsgHandler` impls are one-line delegations to these, and
//! `MsgCtx` is trouper-internal so tests cannot fabricate one).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions use expect/panic to state what went wrong"
)]

use jinn_kernel::State;
use jinn_slices::PhaseView;
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

/// Inserts a fresh live session and returns its id.
fn seed_session(state: &State) -> jinn_core_types::SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    id
}

/// Puts a session in a working phase the way production's turn ledger
/// writes it (the projection write lands before the phase-change
/// event) and drives the Idle→working phase change through the actor —
/// the turn-start path that captures the settled boundary.
fn start_turn(actor: &mut WebUiFanoutActor, state: &State, id: &jinn_core_types::SessionId) {
    state
        .write()
        .session
        .get_mut(id)
        .expect("seeded session")
        .begin_streaming();
    actor.on_phase_changed(&phase_change(id, PhaseView::Idle, PhaseView::Streaming));
}

/// Empties a receiver of already-observed envelopes. The phase-change
/// paths broadcast composer controls and sessions-list refreshes
/// beside the transcript kinds; a test asserting on one kind drains
/// the channel first so stale envelopes from setup don't leak into
/// its assertions.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<SseEnvelope>) {
    while rx.try_recv().is_ok() {}
}

#[rstest::rstest]
#[tokio::test]
async fn the_turn_start_captures_the_settled_boundary() {
    // Given a session holding two settled entries, and a subscriber.
    let (mut actor, _rx, state, _marks) = fixture();
    let id = seed_session(&state);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("seeded session");
        session.push_entry(jinn_core_types::ChatEntry::user("one"));
        session.push_entry(jinn_core_types::ChatEntry::assistant("two"));
    }

    // When the turn starts (Idle → Streaming).
    start_turn(&mut actor, &state, &id);

    // Then the handle reports the boundary at the settled count.
    assert_eq!(actor.handle().settled_boundary(&id), Some(2));
}

#[rstest::rstest]
#[tokio::test]
async fn the_flush_tick_renders_the_inflight_tail_through_the_real_projection() {
    // Given a session whose turn started with two settled entries and
    // then gained a streamed thinking entry.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("seeded session");
        session.push_entry(jinn_core_types::ChatEntry::user("prompt"));
        session.push_entry(jinn_core_types::ChatEntry::assistant("settled"));
    }
    start_turn(&mut actor, &state, &id);
    let mut thinking = jinn_core_types::ChatEntry::assistant("");
    thinking.kind = jinn_core_types::ChatEntryKind::Thinking("pondering the prompt".to_owned());
    thinking.token_count = Some(2);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(thinking);
    drain(&mut rx);

    // When the flush tick runs.
    actor.flush_for_test();

    // Then exactly one streaming envelope lands: it names the session,
    // its cursor parks at the boundary (2), its HTML carries ONLY the
    // tail entry rendered as a real disclosure article — open (the
    // in-flight disclosure rule), with the token-count summary — and
    // none of the settled entries.
    let envelope = rx.try_recv().expect("the streaming flush");
    assert_eq!(envelope.kind, SseKind::Streaming);
    assert_eq!(envelope.session_id, id);
    assert_eq!(envelope.cursor, 2);
    assert!(
        envelope.html.contains("<article class=\"entry thinking\""),
        "{}",
        envelope.html
    );
    assert!(
        envelope
            .html
            .contains("<details class=\"disclosure\" open>"),
        "{}",
        envelope.html
    );
    assert!(
        envelope.html.contains("reasoning · ~2 tokens"),
        "{}",
        envelope.html
    );
    assert!(
        !envelope.html.contains("settled"),
        "no settled entry in the tail: {}",
        envelope.html
    );
    assert!(
        !envelope.html.contains("tok-"),
        "no span-class HTML anywhere: {}",
        envelope.html
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_tool_result_lands_in_the_next_ticks_tail_render() {
    // Given a session mid-turn whose tail holds one tool call.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    start_turn(&mut actor, &state, &id);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::tool_call(
            "t1",
            "read_file",
            "/tmp/x",
        ));
    drain(&mut rx);

    // When the tick flushes, then a tool result lands and the next
    // tick flushes.
    actor.flush_for_test();
    let _ = rx.try_recv().expect("the first flush");
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::tool_result(
            "t1",
            "read_file",
            "the file body",
            jinn_core_types::ToolResultStatus::Success,
        ));
    actor.flush_for_test();

    // Then the second flush carries BOTH tool entries — the in-flight
    // tool loop is visible at the tick cadence, not only at settle —
    // with the disclosures collapsed (only thinking opens mid-stream).
    let envelope = rx.try_recv().expect("the second flush");
    assert_eq!(envelope.kind, SseKind::Streaming);
    assert!(envelope.html.contains("read_file"), "{}", envelope.html);
    assert!(envelope.html.contains("the file body"), "{}", envelope.html);
    assert!(
        !envelope
            .html
            .contains("<details class=\"disclosure\" open>"),
        "tool disclosures stay collapsed mid-stream: {}",
        envelope.html
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_flush_tick_with_an_unchanged_tail_pushes_nothing() {
    // Given a session mid-turn whose tail already flushed once.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    start_turn(&mut actor, &state, &id);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::assistant("only so far"));
    drain(&mut rx);
    actor.flush_for_test();
    let _ = rx.try_recv().expect("the first flush");

    // When the next tick runs with no new content.
    actor.flush_for_test();

    // Then nothing is broadcast — the quiet-stretch skip keeps the
    // tick from churning the DOM.
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_with_no_observed_turn_start_flushes_nothing() {
    // Given a session whose turn is in flight but whose start the
    // fan-out never observed (the boot race: jinn started mid-turn).
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .begin_streaming();
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::assistant("in-flight"));

    // When the flush tick runs.
    actor.flush_for_test();

    // Then nothing is pushed — the safe degrade direction. The page
    // renders as if idle and the settle's convergence push heals the
    // tail; a push here would duplicate once the boundary exists.
    assert!(rx.try_recv().is_err());
}

#[rstest::rstest]
#[tokio::test]
async fn an_idle_settle_clears_the_boundary_before_the_full_render() {
    // Given a mid-turn session whose boundary is tracked.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    start_turn(&mut actor, &state, &id);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::assistant("the whole turn"));
    drain(&mut rx);

    // When the session settles to idle.
    actor.on_phase_changed(&phase_change(&id, PhaseView::Streaming, PhaseView::Idle));

    // Then the boundary is gone, and the convergence push lands FIRST
    // in the settle sequence carrying the whole turn (the boundary was
    // already cleared when it rendered), followed by the idle
    // controls. No streaming envelope rides the settle. The seeded
    // session is not the active one, so its settle also marks it
    // unread — the trailing sessions refreshes are that mark's fan,
    // not transcript pushes.
    assert_eq!(actor.handle().settled_boundary(&id), None);
    let first = rx.try_recv().expect("full render");
    assert_eq!(first.kind, SseKind::TranscriptFull, "full render first");
    assert!(first.html.contains("the whole turn"), "{}", first.html);
    let second = rx.try_recv().expect("idle controls");
    assert_eq!(second.kind, SseKind::Controls, "controls last");
    while let Ok(envelope) = rx.try_recv() {
        assert_eq!(
            envelope.kind,
            SseKind::Sessions,
            "only the unread-mark's sessions refresh follows"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_settled_full_render_carries_no_open_disclosure_and_ends_with_the_container() {
    // Given a mid-turn session with a streamed thinking entry.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    start_turn(&mut actor, &state, &id);
    let mut thinking = jinn_core_types::ChatEntry::assistant("");
    thinking.kind = jinn_core_types::ChatEntryKind::Thinking("reasoning away".to_owned());
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(thinking);
    drain(&mut rx);

    // When the session settles.
    actor.on_phase_changed(&phase_change(&id, PhaseView::Streaming, PhaseView::Idle));
    let full = {
        let mut found = None;
        while let Ok(envelope) = rx.try_recv() {
            if envelope.kind == SseKind::TranscriptFull {
                found = Some(envelope);
            }
        }
        found.expect("the convergence push")
    };

    // Then the full render carries the entry exactly once, as a
    // COLLAPSED disclosure (no open attribute), and the fragment ends
    // with the empty streaming container — the structural wipe.
    let occurrences = full
        .html
        .matches("<article class=\"entry thinking\"")
        .count();
    assert_eq!(occurrences, 1, "{}", full.html);
    assert!(
        !full.html.contains("<details class=\"disclosure\" open>"),
        "the settled render opens nothing: {}",
        full.html
    );
    assert!(full.html.contains("reasoning away"), "{}", full.html);
    assert!(
        full.html.trim_end().ends_with(&format!(
            "<div id=\"{}\"></div>",
            crate::contract::STREAMING_ID
        )),
        "the fragment ends with the streaming container: {}",
        full.html
    );
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
    // Given two sessions mid-turn with distinct tails on one actor.
    let (mut actor, mut rx, state, _marks) = fixture();
    let a = seed_session(&state);
    let b = seed_session(&state);
    start_turn(&mut actor, &state, &a);
    start_turn(&mut actor, &state, &b);
    state
        .write()
        .session
        .get_mut(&a)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::assistant("aaa"));
    state
        .write()
        .session
        .get_mut(&b)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::assistant("bbb"));
    drain(&mut rx);

    // When flushing session A's tail.
    actor.flush_session_tail_for_test(&a);
    let envelope = rx.try_recv().expect("session A flush");

    // Then the envelope names A and carries A's tail only — the SSE
    // endpoint filters on this, so a page left on A never renders B.
    assert_eq!(envelope.session_id, a);
    assert!(envelope.html.contains("aaa"), "{}", envelope.html);
    assert!(!envelope.html.contains("bbb"), "{}", envelope.html);
    // And B's tail is untouched by A's flush.
    actor.flush_session_tail_for_test(&b);
    let envelope = rx.try_recv().expect("session B flush");
    assert_eq!(envelope.session_id, b);
    assert!(envelope.html.contains("bbb"), "{}", envelope.html);
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

#[rstest::rstest]
#[tokio::test]
async fn a_chat_entry_submitted_pushes_a_transcript_convergence_for_that_session() {
    // Given a fan-out actor, a subscriber, and a seeded session with
    // the entry whose submission the event announces (the queue drain
    // or idle dispatch pushed it before publishing).
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    if let Some(session) = state.write().session.get_mut(&id) {
        session.push_entry(jinn_core_types::ChatEntry::user("queued words"));
    }

    // When the entry-submitted event lands.
    actor.on_chat_entry_submitted(&jinn_chat_input_msg::ChatEntrySubmitted {
        session_id: id.clone(),
        entry: jinn_core_types::ChatEntry::user("queued words"),
    });

    // Then the pushed envelope is a transcript-full convergence for
    // that session — cursor 0, full replace, carrying the entry — and
    // nothing else rides the event (no controls push: the composer's
    // single writer is untouched).
    let envelope = rx.try_recv().expect("the transcript push");
    assert_eq!(envelope.kind, SseKind::TranscriptFull);
    assert_eq!(envelope.session_id, id);
    assert_eq!(envelope.cursor, 0);
    assert!(envelope.html.contains("queued words"), "{}", envelope.html);
    assert!(
        rx.try_recv().is_err(),
        "no other push rides the entry submission"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_entry_submission_flushes_the_partial_buffer_before_the_convergence() {
    // Given tokens buffered for a live session mid-stream.
    let (mut actor, mut rx, state, _marks) = fixture();
    let id = seed_session(&state);
    actor.append_token(&token(&id, "partial"));

    // When an entry-submitted event lands (the dispatch that drains a
    // queued message, mid-stream).
    actor.on_chat_entry_submitted(&jinn_chat_input_msg::ChatEntrySubmitted {
        session_id: id.clone(),
        entry: jinn_core_types::ChatEntry::user("queued words"),
    });

    // Then the partial flush lands BEFORE the convergence replace —
    // the reverse order would let a later FlushTick re-append the
    // buffered tokens on top of the full render for one batch window.
    let first = rx.try_recv().expect("partial flush");
    assert_eq!(first.kind, SseKind::Chat, "flush first");
    assert!(first.html.contains("partial"), "{}", first.html);
    let second = rx.try_recv().expect("convergence render");
    assert_eq!(second.kind, SseKind::TranscriptFull, "convergence second");
    assert!(rx.try_recv().is_err(), "exactly two pushes per event");
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

// ── The unread mark: cleared on activation ──────────────────────────

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

    // When Foo becomes the active session and its load completes.
    assert!(state.write().session.set_active(foo.clone()), "activates");
    actor.on_load_completed(&jinn_session_store_msg::SessionLoadCompleted {
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

    // Then Foo keeps its mark — its entries are still unviewed; only
    // activation clears. (Baz is untouched too.)
    assert!(
        marks.read().is_marked(&foo),
        "a background member keeps its mark"
    );
    assert!(marks.read().is_marked(&baz));
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
