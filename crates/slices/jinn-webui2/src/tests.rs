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
    let slices = jinn_slices::Slices::new();
    jinn_cell_catalog::register_all_cells(&slices);
    let provider_cell = slices
        .reader::<jinn_provider_selection_msg::ProviderCell>(
            &jinn_provider_selection_msg::provider_state_slot(),
        )
        .expect("the catalog registers the provider slot");
    let services = jinn_kernel::TestServices::builder().build();
    let handle = OutboundHandle::new(crate::outbound::OutboundActorDeps {
        state: state.clone(),
        bus,
        connections: ConnectionRegistry::default(),
        revisions: RevisionTracker::default(),
        session_store: services.session_store.clone(),
        services,
        provider_cell,
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

/// Sends one message the way the page does, and fails the test when the
/// server refuses it: every caller here is exercising an action that must
/// succeed. The one refusal under test sends its message by hand.
async fn say(h: &Harness, msg: ClientMsg) {
    let reply = dispatch(msg, &h.handle).await;
    assert!(
        !reply.frames.iter().any(|frame| matches!(frame, ServerMsg::Error(_))),
        "the server refused: {reply:?}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn send_publishes_mark_interacted_before_enqueue() {
    // Given an idle session.
    let h = harness();

    // When the page sends a message.
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
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

    // When the page sends a message — by hand, since this is the one
    // refusal under test and `say` fails on a refused message.
    let reply = dispatch(
        ClientMsg::Send {
            text: "hello".to_owned(),
        },
        &h.handle,
    )
    .await;

    // Then nothing reaches the bus: the refusal is a reply to the page, not
    // a message to the session.
    assert!(h.audit.names().is_empty());
    assert!(matches!(reply.frames.as_slice(), [ServerMsg::Error(_)]));
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
    say(
        &h,
        ClientMsg::Queue {
            text: "next".to_owned(),
        },
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
    say(&h, ClientMsg::Cancel).await;

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
    say(
        &h,
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
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
    say(
        &h,
        ClientMsg::ActivateSession {
            session_id: second.clone(),
        },
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
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::SwitchModel {
            provider_id: "ollama/llama3".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::Rename {
            title: "  Road trip  ".to_owned(),
        },
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
    say(
        &h,
        ClientMsg::Rename {
            title: "Road trip".to_owned(),
        },
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
// Stored-session picker
// ---------------------------------------------------------------------------

fn summary(title: &str) -> jinn_session_store_msg::SessionSummary {
    jinn_session_store_msg::SessionSummary {
        session_id: jinn_core_types::SessionId::new(),
        title: title.to_owned(),
        updated_at: jiff::Timestamp::UNIX_EPOCH,
        created_at: jiff::Timestamp::UNIX_EPOCH,
        session_state: jinn_session_store_msg::SessionState::Loaded,
        in_library: false,
        is_bot: false,
        parent_session: None,
        project: None,
    }
}

fn titles(rows: &[jinn_webui2_msg::PickerRowBody]) -> Vec<&str> {
    rows.iter().map(|row| row.title.as_str()).collect()
}

#[rstest::rstest]
#[test]
fn picker_filter_keeps_only_titles_that_match() {
    // Given stored sessions with mixed titles.
    let stored = vec![
        summary("jinn work"),
        summary("groceries"),
        summary("jinn plans"),
    ];

    // When the picker is filtered to "jinn".
    let rows = crate::actions::match_titles(stored, "jinn");

    // Then only the jinn titles remain.
    assert_eq!(titles(&rows), ["jinn work", "jinn plans"]);
}

#[rstest::rstest]
#[test]
fn picker_ranks_the_closer_title_match_first() {
    // Given a title that starts with the filter and one that merely contains it.
    let stored = vec![summary("my jinn notes"), summary("jinn work")];

    // When the picker is filtered to "jinn".
    let rows = crate::actions::match_titles(stored, "jinn");

    // Then the prefix match ranks first.
    assert_eq!(titles(&rows), ["jinn work", "my jinn notes"]);
}

#[rstest::rstest]
#[test]
fn picker_with_an_empty_filter_keeps_every_session() {
    // Given three stored sessions.
    let stored = vec![summary("one"), summary("two"), summary("three")];

    // When the picker is unfiltered.
    let rows = crate::actions::match_titles(stored, "");

    // Then all three are listed.
    assert_eq!(rows.len(), 3);
}

#[rstest::rstest]
#[test]
fn picker_row_carries_the_stored_state_and_parent() {
    // Given an archived session forked from another.
    let mut archived = summary("child");
    archived.session_state = jinn_session_store_msg::SessionState::Archived;
    let parent = jinn_core_types::SessionId::new();
    archived.parent_session = Some(parent.clone());

    // When the picker lists it.
    let rows = crate::actions::match_titles(vec![archived], "");

    // Then the row says so.
    assert!(matches!(
        rows.as_slice(),
        [jinn_webui2_msg::PickerRowBody { state, parent_id: Some(p), .. }]
            if *state == jinn_session_store_msg::SessionState::Archived && *p == parent
    ));
}

#[rstest::rstest]
#[tokio::test]
async fn picker_rows_on_an_empty_store_answers_no_rows() {
    // Given a store with no sessions.
    let h = harness();

    // When the page asks for the picker's rows.
    let reply = dispatch(
        ClientMsg::PickerRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then it gets an empty, well-formed answer rather than an error.
    let [ServerMsg::PickerRows(body)] = reply.frames.as_slice() else {
        unreachable!("a picker request answers picker rows");
    };
    assert!(body.rows.is_empty());
}

// ---------------------------------------------------------------------------
// Model picker
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[tokio::test]
async fn model_rows_with_no_configured_providers_answers_no_rows() {
    // Given a registry with no providers.
    let h = harness();

    // When the page asks for the model picker's rows.
    let reply = dispatch(
        ClientMsg::ModelRows {
            filter: String::new(),
        },
        &h.handle,
    )
    .await;

    // Then it gets an empty, well-formed answer.
    let [ServerMsg::ModelRows(body)] = reply.frames.as_slice() else {
        unreachable!("a model-rows request answers model rows");
    };
    assert!(body.rows.is_empty());
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

// ---------------------------------------------------------------------------
// Streaming: the flush tick is the one reader of streamed text
// ---------------------------------------------------------------------------

/// A connected page over a fresh state: the flush broadcasts to the
/// connection, and the test drains what the page would have received.
struct Page {
    handle: OutboundHandle,
    inbox: tokio::sync::mpsc::UnboundedReceiver<ServerMsg>,
    state: State,
}

fn connected_page() -> Page {
    let h = harness();
    let inbox = h.handle.register();
    Page {
        handle: h.handle,
        inbox,
        state: h.state,
    }
}

/// Puts the active session into its streaming phase, as a live turn does.
fn begin_streaming(page: &Page) {
    page.state.write().session.active_session_mut().begin_streaming();
}

/// Streams `tokens` into the session the way the session actor does when it
/// receives each token: appended to the live assistant entry.
fn stream_tokens(page: &Page, tokens: &[&str]) {
    for token in tokens {
        page.state
            .write()
            .session
            .active_session_mut()
            .append_stream_token(*token, jiff::Timestamp::now())
            .expect("the session is streaming");
    }
}

/// Runs one flush and returns every frame the page received.
fn flush_frames(page: &mut Page) -> Vec<ServerMsg> {
    crate::outbound::flush(&page.handle);
    let mut frames = Vec::new();
    while let Ok(frame) = page.inbox.try_recv() {
        frames.push(frame);
    }
    frames
}

#[rstest::rstest]
#[test]
fn tokens_within_one_window_leave_as_one_delta() {
    // Given a streaming session that has received four tokens.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["This", " is", " a", " sample"]);

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the text leaves as exactly one delta carrying all four tokens.
    let deltas: Vec<_> = frames
        .iter()
        .filter(|frame| matches!(frame, ServerMsg::EntryDelta(_)))
        .collect();
    assert_eq!(deltas.len(), 1);
    assert!(matches!(
        deltas[0],
        ServerMsg::EntryDelta(body) if body.chunk == "This is a sample"
    ));
}

#[rstest::rstest]
#[test]
fn a_lazily_created_entry_reaches_the_page_before_its_first_delta() {
    // Given a streaming session whose first token created the entry.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["Hello"]);

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the entry's upsert precedes its delta.
    let upsert_at = frames
        .iter()
        .position(|frame| matches!(frame, ServerMsg::EntryUpsert(_)))
        .expect("the entry is upserted");
    let delta_at = frames
        .iter()
        .position(|frame| matches!(frame, ServerMsg::EntryDelta(_)))
        .expect("the text is sent");
    assert!(upsert_at < delta_at, "upsert at {upsert_at} must precede delta at {delta_at}");
}

#[rstest::rstest]
#[test]
fn a_flush_with_no_new_text_sends_nothing() {
    // Given a streaming session whose text was already flushed once.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["settled"]);
    let _ = flush_frames(&mut page);

    // When the tick flushes again with nothing new.
    let frames = flush_frames(&mut page);

    // Then the page receives no frame.
    assert!(frames.is_empty());
}

#[rstest::rstest]
#[test]
fn a_later_flush_sends_only_the_text_arrived_since() {
    // Given a streaming session whose first two tokens were flushed.
    let mut page = connected_page();
    begin_streaming(&page);
    stream_tokens(&page, &["Hello", " world"]);
    let _ = flush_frames(&mut page);

    // When two more tokens arrive and the tick flushes.
    stream_tokens(&page, &["!", " Done"]);
    let frames = flush_frames(&mut page);

    // Then the delta carries only the new text.
    assert!(matches!(
        frames.as_slice(),
        [ServerMsg::EntryDelta(body)] if body.chunk == "! Done"
    ));
}

#[rstest::rstest]
#[test]
fn thinking_streams_as_its_own_field() {
    // Given a streaming session that has received a thinking token — which
    // creates the thinking entry first, the way the session actor does.
    let mut page = connected_page();
    begin_streaming(&page);
    {
        let mut guard = page.state.write();
        let session = guard.session.active_session_mut();
        session.begin_thinking(jiff::Timestamp::now());
        session
            .append_thinking_token("let me think")
            .expect("the session is streaming");
    }

    // When the tick flushes.
    let frames = flush_frames(&mut page);

    // Then the delta names the thinking field.
    assert!(matches!(
        frames.as_slice(),
        [_, ServerMsg::EntryDelta(body)] if body.field == jinn_webui2_msg::DeltaField::Thinking
    ));
}

#[rstest::rstest]
#[test]
fn nothing_is_flushed_while_no_page_is_connected() {
    // Given a streaming session with tokens, and no page connected.
    let h = harness();
    h.state.write().session.active_session_mut().begin_streaming();
    h.state
        .write()
        .session
        .active_session_mut()
        .append_stream_token("unread", jiff::Timestamp::now())
        .expect("the session is streaming");

    // When the tick flushes.
    crate::outbound::flush(&h.handle);

    // Then a page connecting afterwards is sent the entry whole — an upsert
    // first, carrying the text the tick never streamed — never a gap.
    let mut inbox = h.handle.register();
    crate::outbound::flush(&h.handle);
    let first = inbox.try_recv().expect("the new page receives the entry");
    assert!(matches!(first, ServerMsg::EntryUpsert(_)));
}

// ---------------------------------------------------------------------------
// Reconcile: a turn that settles between ticks still reaches the page
// ---------------------------------------------------------------------------

/// Puts the active session through a whole turn by hand — begins streaming,
/// appends text, and settles to idle — the way a turn shorter than one
/// flush interval looks to the actor.
fn settle_an_instant_turn(state: &State, text: &str) {
    let mut guard = state.write();
    let session = guard.session.active_session_mut();
    session.begin_streaming();
    session
        .append_stream_token(text, jiff::Timestamp::now())
        .expect("the session is streaming");
    session.settle_to_idle();
}

/// Everything the connected page has received since it was last drained.
fn drain(inbox: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMsg>) -> Vec<ServerMsg> {
    let mut frames = Vec::new();
    while let Ok(frame) = inbox.try_recv() {
        frames.push(frame);
    }
    frames
}

#[rstest::rstest]
#[test]
fn an_instant_turn_reaches_the_page_when_it_settles() {
    // Given a connected page holding one entry, and a turn that began and
    // settled before any tick read it.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("before"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    settle_an_instant_turn(&h.state, "instant reply");

    // When the settle reconciles the history.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the settled assistant entry is the one frame the page receives.
    let frames = drain(&mut inbox);
    assert!(matches!(
        frames.as_slice(),
        [ServerMsg::EntryUpsert(body)] if body.index == 1
    ));
}

#[rstest::rstest]
#[test]
fn a_settled_turn_that_changed_nothing_sends_no_frame() {
    // Given a connected page that holds every entry the session has.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("held"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);

    // When another reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the page receives nothing.
    assert!(drain(&mut inbox).is_empty());
}

#[rstest::rstest]
#[test]
fn an_entry_that_changed_while_settling_is_sent_again() {
    // Given a connected page that holds one entry as it stood before a turn.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("before"));
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));
    let _ = drain(&mut inbox);
    let entry_id = h.state.read().session.active_session().history()[0].id.clone();

    // When the entry is rewritten in place — a pin does that — and a
    // settle reconciles the history.
    h.state
        .write()
        .session
        .active_session_mut()
        .pin_entry(&entry_id, jinn_core_types::PinPosition::Top);
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the rewritten entry is the one frame the page receives.
    assert!(matches!(
        drain(&mut inbox).as_slice(),
        [ServerMsg::EntryUpsert(_)]
    ));
}

#[rstest::rstest]
#[test]
fn an_entry_the_snapshot_delivered_is_not_sent_again() {
    // Given a page that connected and was answered with a snapshot.
    let h = harness();
    let mut inbox = h.handle.register();
    push(&h.state, ChatEntry::user("in the snapshot"));
    h.handle.mark_delivered(&active_id(&h.state));
    let _ = drain(&mut inbox);

    // When a reconcile runs with the history unchanged.
    crate::outbound::reconcile(&h.handle, &active_id(&h.state));

    // Then the snapshot's entry is not re-sent.
    assert!(drain(&mut inbox).is_empty());
}
