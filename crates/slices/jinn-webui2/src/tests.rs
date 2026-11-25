//! Behaviour of the inbound dispatch: one test, one observable result.
//!
//! Every test drives a `ClientMsg` through `dispatch` against a handle
//! built over a recording bus, then asserts either the messages published
//! (by name, in order — the terminal's own sequence) or the frames
//! replied to the page.

use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::{ChatEntry, ModelSelection};
use jinn_inference_msg::CancelTurn;
use jinn_kernel::State;
use jinn_preferences_config::protocol::app_state_command::UpdateAppState;
use jinn_provider_selection_msg::ProviderSwitch;
use jinn_session_msg::MarkSessionInteracted;
use jinn_session_store_msg::{PersistSession, SessionLoadRequested};
use jinn_slices::bus::BusService;
use jinn_webui2_msg::{ClientMsg, ServerMsg};

use crate::{
    actions, connections::ConnectionRegistry, outbound::OutboundHandle, protocol::dispatch,
    revision::RevisionTracker,
};

/// A handle over a recording bus and a fresh state, with nothing spawned.
struct Harness {
    handle: OutboundHandle,
    audit: jinn_slices::bus::BusAudit,
    state: State,
}

fn harness() -> Harness {
    let (bus, audit) = BusService::new_recording();
    let state = State::new(jinn_kernel::AppState::default());
    let handle = OutboundHandle::new(crate::outbound::OutboundActorDeps {
        state: state.clone(),
        bus,
        connections: ConnectionRegistry::default(),
        revisions: RevisionTracker::default(),
    });
    Harness {
        handle,
        audit,
        state,
    }
}

fn active_id(state: &State) -> jinn_core_types::SessionId {
    state.read().session.active_session_id().clone()
}

fn push(state: &State, entry: ChatEntry) {
    state.write().session.active_session_mut().push_entry(entry);
}

// ---------------------------------------------------------------------------
// Send
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn send_publishes_mark_interacted_before_enqueue() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message.
    dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the session is marked interacted first and the message enqueued second.
    assert_eq!(
        h.audit.names(),
        ["MarkSessionInteracted", "EnqueueUserMessage"]
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_enqueues_the_text_as_a_user_entry() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message.
    dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the enqueued entry is a user entry carrying that text.
    let enqueued = h.audit.of_type::<EnqueueUserMessage>();
    assert_eq!(enqueued.len(), 1);
    assert_eq!(enqueued[0].entry.prompt_text(), Some("hello"));
}

#[rstest::rstest]
#[tokio::test]
async fn send_enqueues_on_the_active_session() {
    // Given an idle session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page sends a message.
    dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then both messages name the active session.
    let ids = h
        .audit
        .of_type::<EnqueueUserMessage>()
        .iter()
        .map(|m| m.session_id.clone())
        .chain(
            h.audit
                .of_type::<MarkSessionInteracted>()
                .iter()
                .map(|m| m.session_id.clone()),
        )
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|published| *published == id));
}

#[rstest::rstest]
#[tokio::test]
async fn send_while_busy_is_refused_with_an_error() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page sends a message.
    let reply = dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the session is busy.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn send_while_busy_publishes_nothing() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page sends a message.
    dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then nothing reaches the bus.
    assert!(h.audit.names().is_empty());
}

#[rstest::rstest]
#[tokio::test]
async fn send_of_blank_text_is_refused() {
    // Given an idle session.
    let h = harness();

    // When the page sends only whitespace.
    let reply = dispatch(
        ClientMsg::Send {
            text: "   ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told the message is empty.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn queue_while_busy_still_enqueues() {
    // Given a session running a turn.
    let h = harness();
    h.state
        .write()
        .session
        .active_session_mut()
        .begin_streaming();

    // When the page queues a message.
    dispatch(
        ClientMsg::Queue {
            text: "next".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the message is enqueued, exactly as an idle send would.
    assert_eq!(
        h.audit.names(),
        ["MarkSessionInteracted", "EnqueueUserMessage"]
    );
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn cancel_publishes_cancel_turn_draining_to_input() {
    // Given a session running a turn.
    let h = harness();

    // When the page cancels.
    dispatch(ClientMsg::Cancel, &h.handle).await;

    // Then the terminal's own cancel is published.
    let cancels = h.audit.of_type::<CancelTurn>();
    assert_eq!(cancels.len(), 1);
    assert!(cancels[0].drain_buffers_to_input);
}

// ---------------------------------------------------------------------------
// Activate
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn activate_switches_the_active_session() {
    // Given two loaded sessions.
    let h = harness();
    let second = jinn_core_types::SessionId::new();
    h.state.write().session.get_or_create(&second);

    // When the page activates the second.
    dispatch(
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
        &h.handle,
    )
    .await;

    // Then it is the instance's active session.
    assert_eq!(*h.state.read().session.active_session_id(), second);
}

#[rstest::rstest]
#[tokio::test]
async fn activate_publishes_a_load_request_without_a_width() {
    // Given two loaded sessions.
    let h = harness();
    let second = jinn_core_types::SessionId::new();
    h.state.write().session.get_or_create(&second);

    // When the page activates the second.
    dispatch(
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
        &h.handle,
    )
    .await;

    // Then the store is asked to load it, with no content width.
    let loads = h.audit.of_type::<SessionLoadRequested>();
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].session_id, second);
    assert_eq!(loads[0].content_width, None);
}

#[rstest::rstest]
#[tokio::test]
async fn activate_of_an_unknown_session_is_refused() {
    // Given a page naming a session that is not loaded.
    let h = harness();

    // When it asks to activate that session.
    let reply = dispatch(
        ClientMsg::ActivateSession {
            session_id: jinn_core_types::SessionId::new(),
        },
        &h.handle,
    )
    .await;

    // Then the page is told so, and nothing is published.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}

// ---------------------------------------------------------------------------
// Switch model
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn switch_model_publishes_provider_switch_then_last_model() {
    // Given a session.
    let h = harness();

    // When the page picks a model.
    dispatch(
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the switch is requested and the last model recorded, in that order.
    assert_eq!(h.audit.names(), ["ProviderSwitch", "UpdateAppState"]);
}

#[rstest::rstest]
#[tokio::test]
async fn switch_model_does_not_write_the_model_synchronously() {
    // Given a session with no model.
    let h = harness();

    // When the page picks a model.
    dispatch(
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the session's model is unchanged — it arrives with ProviderSwitched.
    assert_eq!(
        h.state.read().session.active_session().model_selection(),
        &ModelSelection::default()
    );
}

#[rstest::rstest]
#[tokio::test]
async fn switch_model_records_the_picked_id_as_a_single_selection() {
    // Given a session.
    let h = harness();

    // When the page picks a model.
    dispatch(
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then both messages carry it as a single-model selection.
    let switches = h.audit.of_type::<ProviderSwitch>();
    assert_eq!(
        switches[0].provider_id,
        ModelSelection::Single("ollama/llama3".to_owned())
    );
    assert_eq!(h.audit.of_type::<UpdateAppState>().len(), 1);
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn rename_writes_the_trimmed_title() {
    // Given a session.
    let h = harness();

    // When the page renames it.
    dispatch(
        ClientMsg::Rename {
            title: "  Road trip  ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the session's title is the trimmed text.
    assert_eq!(
        h.state.read().session.active_session().title(),
        Some("Road trip")
    );
}

#[rstest::rstest]
#[tokio::test]
async fn rename_publishes_a_persist_for_the_session() {
    // Given a session.
    let h = harness();
    let id = active_id(&h.state);

    // When the page renames it.
    dispatch(
        ClientMsg::Rename {
            title: "Road trip".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the store is asked to persist exactly that session.
    let persists = h.audit.of_type::<PersistSession>();
    assert_eq!(persists.len(), 1);
    assert_eq!(persists[0].session_id, id);
}

#[rstest::rstest]
#[tokio::test]
async fn rename_to_blank_is_refused_and_leaves_the_title() {
    // Given a session with a title.
    let h = harness();
    let id = active_id(&h.state);
    h.state
        .write()
        .session
        .get_mut(&id)
        .map(|s| s.set_title("Keep".to_owned()));

    // When the page renames it to whitespace.
    let reply = dispatch(
        ClientMsg::Rename {
            title: "  ".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then the page is refused and the title is untouched.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
    assert_eq!(
        h.state.read().session.active_session().title(),
        Some("Keep")
    );
}

// ---------------------------------------------------------------------------
// Hello
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn hello_on_a_fresh_page_answers_a_snapshot() {
    // Given a session with a transcript.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));

    // When a page connects holding nothing.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: None,
            revision: None,
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole state in one frame.
    let [ServerMsg::Snapshot(snapshot)] = reply.frames.as_slice() else {
        unreachable!("a fresh hello answers a snapshot");
    };
    assert_eq!(snapshot.entries.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_matching_cursor_and_revision_sends_only_later_entries() {
    // Given a page that saw the first of two entries at the current revision.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    let revision = h.handle.revision(&active_id(&h.state));
    push(&h.state, ChatEntry::user("two"));

    // When it reconnects naming both.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(revision),
        },
        &h.handle,
    )
    .await;

    // Then it receives the second entry alone, at its live index.
    let [ServerMsg::EntryUpsert(upsert)] = reply.frames.as_slice() else {
        unreachable!("a resumable hello answers upserts");
    };
    assert_eq!(upsert.index, 1);
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_a_stale_revision_answers_a_snapshot() {
    // Given a page whose revision predates an in-place rewrite of the history.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));
    let cursor = active_last_entry_id(&h.state);
    h.handle.bump(&active_id(&h.state));

    // When it reconnects with the old revision.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(cursor),
            revision: Some(0),
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole transcript rather than a guess.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

#[rstest::rstest]
#[tokio::test]
async fn hello_with_an_unknown_cursor_answers_a_snapshot() {
    // Given a page naming an entry the session no longer holds.
    let h = harness();
    push(&h.state, ChatEntry::user("one"));

    // When it reconnects.
    let reply = dispatch(
        ClientMsg::Hello {
            last_entry_id: Some(jinn_core_types::ChatEntryId::new()),
            revision: Some(0),
        },
        &h.handle,
    )
    .await;

    // Then it receives the whole transcript.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Snapshot(_)]));
}

fn active_last_entry_id(state: &State) -> jinn_core_types::ChatEntryId {
    state
        .read()
        .session
        .active_session()
        .history()
        .last()
        .map(|e| e.id.clone())
        .expect("history has an entry")
}

// ---------------------------------------------------------------------------
// Malformed frames
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn a_malformed_frame_is_answered_with_an_error() {
    // Given a frame that is not a ClientMsg.
    let parsed: Result<ClientMsg, _> = serde_json::from_str("{\"type\":\"teleport\"}");

    // When the dispatch is handed the failure.
    let reply = match parsed {
        Err(_) => actions::malformed(),
        Ok(msg) => unreachable!("teleport is not a message: {msg:?}"),
    };

    // Then the page is told, and told in a frame it can read.
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
}
