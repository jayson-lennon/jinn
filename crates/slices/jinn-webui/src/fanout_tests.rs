//! Fan-out actor tests: batching, per-session scoping, and the
//! flush-before-full ordering on settle — driven through the actor's
//! plain handler methods (`MsgHandler` impls are one-line delegations
//! to these, and `MsgCtx` is trouper-internal so tests cannot fabricate
//! one).

#![allow(clippy::expect_used, reason = "test assertions use expect")]

use jinn_inference_msg::StreamToken;
use jinn_kernel::State;
use jinn_webui_msg::SseEnvelope;
use jinn_webui_msg::SseKind;

use crate::fanout::WebUiFanoutActor;

/// A fresh actor with a subscribed receiver: `(actor, rx, state)`.
fn fixture() -> (
    WebUiFanoutActor,
    tokio::sync::broadcast::Receiver<SseEnvelope>,
    State,
) {
    let state = State::new(jinn_app_state::AppState::default());
    let (actor, rx) = WebUiFanoutActor::test_with_receiver(state.clone());
    (actor, rx, state)
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
    let (mut actor, mut rx, _state) = fixture();
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
    let (mut actor, mut rx, _state) = fixture();
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
    let (mut actor, mut rx, state) = fixture();
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
    let (mut actor, mut rx, state) = fixture();
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
    let (mut actor, mut rx, state) = fixture();
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
async fn a_cache_stored_event_pushes_one_models_signal_per_live_session() {
    // Given a default session plus two seeded ones, and a subscriber.
    let (mut actor, mut rx, state) = fixture();
    let first = seed_session(&state);
    let second = seed_session(&state);
    let live = state.read().session.sessions().len();

    // When a cache-stored event lands (the provider cell already
    // holds the cache — the emit is post-write by contract).
    actor.push_models_changed();

    // Then exactly one `models` signal lands per live session — each
    // session-filtered stream receives its own envelope — and each is
    // a SIGNAL: empty html, cursor 0; rows arrive by the client's
    // re-pull, never in the push.
    let mut seen = std::collections::HashSet::new();
    while let Ok(envelope) = rx.try_recv() {
        assert_eq!(envelope.kind, SseKind::Models, "a models push, and only that");
        assert!(envelope.html.is_empty(), "the push is a signal: {envelope:?}");
        assert_eq!(envelope.cursor, 0);
        seen.insert(envelope.session_id);
    }
    assert_eq!(seen.len(), live, "one push per live session: {seen:?}");
    assert!(seen.contains(&first) && seen.contains(&second), "{seen:?}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_handle_pushes_a_sessions_refresh_from_outside_the_actor() {
    // Given a live session whose title a web rename just changed, and
    // a subscriber to the actor's broadcast channel.
    let (actor, mut rx, state) = fixture();
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
    let (mut actor, mut rx, _state) = fixture();
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
    let (mut actor, mut rx, _state) = fixture();
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
