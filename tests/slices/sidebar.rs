//! End-to-end crossing test for the sidebar slice: kernel publishes
//! `SessionClosed` on the kameo bus → forward relay → `jinn.sidebar`
//! topic → the slice's trouper state actor → sidebar cursor clamped.
//!
//! The cursor is observable through the sidebar sections cell — the same
//! view the renderer reads — so only a true crossing can satisfy the
//! assertion.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::time::Duration;

use jinn_domain::common::bridge::Bridge;
use jinn_domain::feat::session::chat_session::ChatSessionState;
use jinn_domain::feat::session::protocol::session_closed::SessionClosed;

use crate::common::test_app;

/// Polls `read` until it returns `Some` (up to `timeout`), else panics.
async fn await_condition<R>(timeout: Duration, mut read: impl FnMut() -> Option<R>) -> R
where
    R: std::fmt::Debug + PartialEq + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = read() {
            return value;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("condition not observed within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The sidebar cursor is clamped after the kernel closes a session:
/// bus → relay → trouper actor → sections cell.
#[rstest::rstest]
#[tokio::test]
async fn session_closed_crosses_to_sidebar_and_clamps_cursor() {
    // Given a composed app whose sidebar cursor sits at index 2 of
    // three sessions, with the third session already removed (the
    // close itself — the actor's job is the cursor clamp).
    let app = test_app().await;
    let removed_id = {
        let mut state = app.core.state.write_test_no_cap();
        let default_id = state.session.active_session_id().clone();
        state.session.remove_without_replacement(&default_id);
        let s1 = ChatSessionState::new();
        let s2 = ChatSessionState::new();
        let s3 = ChatSessionState::new();
        let id3 = s3.session_id().clone();
        state.session.insert(s1);
        state.session.insert(s2);
        state.session.insert(s3);
        state.session.set_active(id3.clone());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(2));
        state.session.remove_without_replacement(&id3);
        id3
    };

    // When the kernel publishes `SessionClosed` on the bus.
    let _ = app.core.bridge.send(Bridge::publish_closure(SessionClosed {
        session_id: removed_id,
    }));

    // Then the sidebar cursor is clamped to 1 (max valid index).
    let clamped = await_condition(Duration::from_secs(5), || {
        app.core
            .state
            .read()
            .frontend
            .with_sections(|s| s.sessions.selected_index, || None)
            .filter(|index| *index == 1)
    })
    .await;
    assert_eq!(clamped, 1);
}
