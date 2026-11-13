//! End-to-end crossing test for the sidebar slice: the session store publishes
//! `SessionRemoved` on the kernel topic → route rule → `jinn.sidebar`
//! topic → the slice's trouper state actor → sidebar cursor clamped.
//!
//! The cursor is observable through the sidebar sections cell — the same
//! view the renderer reads — so only a true crossing can satisfy the
//! assertion.

#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use std::sync::Arc;
use std::time::Duration;

use jinn_chat_log_view_msg::PreviewSessionRequested;
use jinn_core_types::ChatEntry;
use jinn_kernel::common::bridge::Bridge;
use jinn_session_msg::SessionRemoved;
use jinn_session_state::ChatSessionState;

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
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not observed within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A session removal crosses to the sidebar and the cursor falls back:
/// bus → relay → trouper actor → sections cell.
#[rstest::rstest]
#[tokio::test]
async fn session_closed_crosses_to_sidebar_and_moves_cursor() {
    // Given a composed app whose sidebar cursor names the third of three
    // sessions, with that session already removed (the close itself — the
    // actor's job is the cursor fallback).
    let app = test_app().await;
    let removed_id = {
        let mut state = app.core.state.write();
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
            .update_sections(|s| s.sessions.selected_id = Some(id3.clone()));
        state.session.remove_without_replacement(&id3);
        id3
    };

    // When the session store publishes `SessionRemoved` on the bus.
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(SessionRemoved {
            session_id: removed_id.clone(),
            removed_parent: None,
            was_active: true,
        }));

    // Then the sidebar cursor names a session that still exists. It used to
    // be clamped to the last valid index; an identity that names a removed
    // session is replaced with one that does not exist no more.
    let cursor = await_condition(Duration::from_secs(5), || {
        let state = app.core.state.read();
        state
            .frontend
            .with_sections(|s| s.sessions.selected_id.clone(), || None)
            .filter(|id| state.session.contains(id))
    })
    .await;
    assert_ne!(cursor, removed_id);
}

/// The preview deadline actually reaches the layout supervisor.
///
/// The sidebar state actor arms a deadline for every preview it handles, and the
/// supervisor owns the timer that stops a stuck render from spinning forever.
/// That crossing is invisible to every unit test: the actor's handler is
/// exercised directly, and the supervisor's handler likewise, so both pass while
/// the message between them is dropped by the bus's flush gate — the actor never
/// declaring it emits. The observable consequence is a preview that spins
/// forever once a render is genuinely lost.
#[rstest::rstest]
#[tokio::test]
// The preview deadline is 5s of real time, so the test's own wait has to
// outlast it. The `#[timeout]` above rstest's default of 10s must be raised to
// match, or the harness kills the test mid-wait.
#[timeout(Duration::from_secs(20))]
async fn a_preview_request_reaches_the_layout_supervisor_as_a_deadline() {
    // Given a composed app with a session whose preview is being requested.
    let app = test_app().await;
    let session_id = {
        let state = app.core.state.read();
        state.session.active_session_id().clone()
    };

    // When the preview request crosses the bus.
    let generation = {
        let state = app.core.state.read();
        state
            .frontend
            .update_sections(|s| s.sessions.preview.request(session_id.clone(), 7, 103))
            .expect("the sections cell is attached")
    };
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(PreviewSessionRequested {
            session_id: session_id.clone(),
            content_width: 103,
            generation,
            entries: Arc::from(Vec::<ChatEntry>::new()),
            tool_entry_max_lines: 6,
            signature: 7,
        }));

    // Then the request is abandoned on its deadline, which can only happen if
    // the actor emitted `ArmPreviewDeadline` and the supervisor handled it.
    let abandoned = await_condition(Duration::from_secs(20), || {
        let state = app.core.state.read();
        (!state.frontend.with_sections(
            |s| s.sessions.preview.is_in_flight_for(&session_id),
            || false,
        ))
        .then_some(true)
    })
    .await;
    assert!(
        abandoned,
        "the preview deadline must fire and abandon the in-flight request (in_flight at start: {})",
        app.core.state.read().frontend.with_sections(
            |s| s.sessions.preview.is_in_flight_for(&session_id),
            || false,
        ),
    );
}

/// A published preview request must reach a worker and be cached.
///
/// `PreviewSessionRequested` is a command, so trouper routes it to exactly one
/// handler by round-robin over the actors that declare it. A second handler
/// that only wanted a side effect — arming a timer, say — competes with the
/// workers and eats its share of requests without rendering anything, so those
/// popups spin on work that was thrown away. The fix is that only the preview
/// workers declare the command; this test pins the behaviour that the command
/// renders when it is published.
///
/// Scope, honestly stated: the composed harness does not reproduce the
/// interception (the workers win every draw there), so this test guards the
/// request-to-cache crossing rather than the routing choice itself. The routing
/// guard is structural — a handler for `PreviewSessionRequested` on any actor
/// that does not render is a compile-time mistake to make deliberately, and the
/// reasoning is recorded at the declaration site in `sidebar_state_actor.rs`.
#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn a_published_preview_request_is_rendered_and_cached() {
    // Given a composed app with a session carrying text worth previewing.
    let app = test_app().await;
    let session_id = {
        let mut state = app.core.state.write();
        let mut session = ChatSessionState::new();
        session.push_entry(ChatEntry::user("a line the preview must wrap"));
        let id = session.session_id().clone();
        state.session.insert(session);
        id
    };

    // And an armed in-flight entry, as the keyboard path does.
    let generation = {
        let state = app.core.state.read();
        state
            .frontend
            .update_sections(|s| s.sessions.preview.request(session_id.clone(), 7, 103))
            .expect("the sections cell is attached")
    };

    // When the request crosses the bus, the way the keyboard path sends it.
    let _ = app
        .core
        .bridge
        .send(Bridge::publish_closure(PreviewSessionRequested {
            session_id: session_id.clone(),
            content_width: 103,
            generation,
            entries: Arc::from(vec![ChatEntry::user("a line the preview must wrap")]),
            tool_entry_max_lines: 6,
            signature: 7,
        }));

    // Then the rendered lines arrive and are cached for that session.
    await_condition(Duration::from_secs(15), || {
        let state = app.core.state.read();
        state
            .frontend
            .with_sections(
                |s| s.sessions.preview.cached(&session_id, 7, 103).is_some(),
                || false,
            )
            .then_some(true)
    })
    .await;
}

/// The sidebar column paints its sections.
///
/// This asserts the column reaches the screen at all. The sidebar is
/// drawn by a draw function the slice registers at activation, so a
/// missing or mis-registered column leaves the frame with an empty
/// sidebar and every other sidebar test still passes — they read the
/// sections cell, not the painted column.
#[rstest::rstest]
#[tokio::test]
async fn the_sidebar_column_paints_its_sections() {
    // Given a launched app with the sidebar slice active.
    let app = test_app().await;
    let (mut terminal, area) = jinn_testutil::setup_term(100, 30);

    // When rendering a frame.
    let mut app = app;
    terminal
        .draw(|frame| {
            app.render(frame);
        })
        .expect("terminal draw");

    // Then a section header is present in the sidebar column.
    let buffer = terminal.backend().buffer();
    let sidebar_width = app.core.state.read().frontend.sidebar_width;
    // The sidebar is the rightmost column; the chat column and its border
    // occupy everything to its left.
    let sidebar_x = area.width.saturating_sub(sidebar_width);
    let mut painted = false;
    for y in 0..area.height {
        let row: String = (sidebar_x..area.width)
            .filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_owned()))
            .collect();
        if row.contains("Sessions") {
            painted = true;
            break;
        }
    }
    assert!(
        painted,
        "the sidebar column should paint its sections; no section header found \
         in the {sidebar_width}-column sidebar"
    );
}
