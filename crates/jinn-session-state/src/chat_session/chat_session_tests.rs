#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::uninlined_format_args,
    reason = "test code"
)]

use jinn_core_types::SessionProfile;

use jinn_chat_log_view_msg::visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
};
use jinn_token_count_msg::TokenRecord;

use jinn_core_types::{
    ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, EntryTiming, PinPosition, SessionId,
};
use std::path::PathBuf;

use super::*;
use jinn_core_types::model_selection::ModelSelection;

#[rstest::rstest]
fn captures_are_coherent_and_strictly_newer() {
    // Given a session with durable history, metadata, and token accounting.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.set_title("Snapshot".to_owned());
    session.push_token_record(TokenRecord {
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 10,
        tokens_received: 20,
        cost: Some(0.5),
        model_used: Some("test/model".to_owned()),
        prompt_tokens: Some(8),
        cached_tokens: Some(2),
    });

    // When capturing the authoritative session twice.
    let first = session.capture_snapshot();
    let second = session.capture_snapshot();

    // Then each capture is coherent and the revision strictly advances.
    assert_eq!(first.metadata.title.as_deref(), Some("Snapshot"));
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.token_ledger.len(), 1);
    assert!(second.revision > first.revision);
}

#[rstest::rstest]
fn push_entry_adds_to_history() {
    // Given a new ChatSessionState.
    let mut session = ChatSessionState::new();

    // When pushing a user entry.
    let index = session.push_entry(ChatEntry::user("hello"));

    // Then the index is 0 and history has one entry.
    assert_eq!(index, 0);
    assert_eq!(session.history().len(), 1);
}

#[rstest::rstest]
fn push_entry_expands_known_prompt_token_in_user_entry() {
    // Given a session whose store has a `#name` template.
    use jinn_context::PromptTemplate;
    use jinn_context::PromptTemplateStore;

    let mut session = ChatSessionState::new();
    session.set_discovered_prompt_templates(PromptTemplateStore::from_vec(vec![PromptTemplate {
        name: "name".to_owned(),
        description: "d".to_owned(),
        body: "BODY".to_owned(),
    }]));

    // When pushing a user entry containing the token.
    let index = session.push_entry(ChatEntry::user("#name"));

    // Then `display` keeps the raw token (UI unchanged) but the model-facing
    // `expanded` field carries the resolved body.
    let ChatEntryKind::User {
        display, expanded, ..
    } = &session.history()[index].kind
    else {
        panic!("expected a user entry");
    };
    assert_eq!(display, "#name");
    assert_eq!(expanded, "BODY");
}

#[rstest::rstest]
fn push_entry_leaves_unknown_prompt_token_literal() {
    // Given a session with a store that has no matching template.
    let mut session = ChatSessionState::new();
    session.set_discovered_prompt_templates(jinn_context::PromptTemplateStore::from_vec(vec![]));

    // When pushing a user entry with an unknown token.
    let index = session.push_entry(ChatEntry::user("#nope"));

    // Then both fields keep the raw token literal.
    let ChatEntryKind::User {
        display, expanded, ..
    } = &session.history()[index].kind
    else {
        panic!("expected a user entry");
    };
    assert_eq!(display, "#nope");
    assert_eq!(expanded, "#nope");
}

#[rstest::rstest]
fn push_entry_does_not_expand_non_user_entries() {
    // Given an empty store (no templates).
    let mut session = ChatSessionState::new();

    // When pushing an assistant entry with literal `#name`.
    let index = session.push_entry(ChatEntry::assistant("#name"));

    // Then the assistant text is unchanged.
    assert!(
        matches!(
            &session.history()[index].kind,
            ChatEntryKind::Assistant(t) if t == "#name"
        ),
        "assistant entry should not be touched by expansion"
    );
}

/// Minimal valid PNG (1×1 transparent) for attachment-path tests.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[rstest::rstest]
fn push_entry_at_path_creates_attachment_and_file_uri() {
    // Given a session and a temp png file referenced via @path.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("img.png");
    std::fs::write(&path, TINY_PNG).expect("write");
    let abs = path.to_string_lossy().into_owned();
    let mut session = ChatSessionState::new();

    // When pushing a user entry referencing the image via @path.
    let index = session.push_entry(ChatEntry::user(format!("describe @{abs}")));

    // Then the entry has no attachments yet — `push_entry` only rewrites
    // text and collects pending paths. Attachment resolution happens in the
    // session actor via `spawn_blocking`.
    let ChatEntryKind::User {
        display, expanded, ..
    } = &session.history()[index].kind
    else {
        panic!("expected a user entry");
    };
    // And display keeps the raw @path (UI unchanged).
    assert_eq!(display, &format!("describe @{abs}"));
    // And expanded rewrites @path to the file:// URI.
    assert!(expanded.contains(&format!("(file://{abs})")));
}

#[rstest::rstest]
fn push_entry_email_at_is_not_treated_as_attachment() {
    // Given a session.
    let mut session = ChatSessionState::new();

    // When pushing a user entry containing an email address.
    let index = session.push_entry(ChatEntry::user("contact foo@bar.com"));

    // Then no attachments are created and the text is unchanged.
    let ChatEntryKind::User {
        expanded,
        attachments,
        ..
    } = &session.history()[index].kind
    else {
        panic!("expected a user entry");
    };
    assert!(attachments.is_empty());
    assert_eq!(expanded, "contact foo@bar.com");
}

#[rstest::rstest]
fn first_stream_token_creates_assistant_entry() {
    // Given a session with one entry, streaming started.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.begin_streaming();

    // When appending the first token.
    session
        .append_stream_token("Hello", jiff::Timestamp::now())
        .expect("ok");

    // Then the assistant entry is created.
    assert_eq!(session.history().len(), 2);
    assert!(matches!(
        session.history()[1].kind,
        ChatEntryKind::Assistant(ref text) if text == "Hello"
    ));
}

#[rstest::rstest]
fn begin_streaming_sets_is_streaming() {
    // Given a session with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When beginning streaming.
    session.begin_streaming();

    // Then is_streaming is true.
    assert_eq!(session.phase(), PhaseKind::Streaming);
}

#[rstest::rstest]
fn append_stream_token_appends_to_assistant_entry() {
    // Given a session that is streaming.
    let mut session = attached_session();
    session.begin_streaming();

    // When appending a token.
    session
        .append_stream_token("Hello", jiff::Timestamp::now())
        .expect("ok");
    session
        .append_stream_token(" world", jiff::Timestamp::now())
        .expect("ok");

    // Then the assistant entry text is "Hello world".
    assert_eq!(
        session.history()[0].kind,
        ChatEntryKind::Assistant("Hello world".to_owned())
    );
}

#[rstest::rstest]
fn finish_streaming_clears_streaming_state() {
    // Given a session that is streaming with some tokens.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .append_stream_token("Hi", jiff::Timestamp::now())
        .expect("ok");

    // When finishing streaming.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then is_streaming is false and text is preserved.
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(
        session.history()[0].kind,
        ChatEntryKind::Assistant("Hi".to_owned())
    );
}

#[rstest::rstest]
fn cancel_streaming_keeps_partial_text() {
    // Given a session that is streaming with partial tokens.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .append_stream_token("Partial", jiff::Timestamp::now())
        .expect("ok");

    // When cancelling streaming.
    session.cancel_streaming(jiff::Timestamp::now());

    // Then is_streaming is false but partial text is kept.
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(
        session.history()[0].kind,
        ChatEntryKind::Assistant("Partial".to_owned())
    );
}

#[rstest::rstest]
fn begin_streaming_twice_is_noop() {
    // Given a session that is already streaming.
    let mut session = attached_session();
    session.begin_streaming();

    // When calling begin_streaming again.
    session.begin_streaming();

    // Then phase stays Streaming (no panic, no double-transition).
    assert_eq!(session.phase(), PhaseKind::Streaming);
}

#[rstest::rstest]
fn push_entry_bumps_last_history_activity_at() {
    // Given a session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.core.identity.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When pushing an entry.
    let before = jiff::Timestamp::now();
    session.push_entry(ChatEntry::user("hi"));

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.identity.last_history_activity_at >= before);
}

#[rstest::rstest]
fn append_stream_token_bumps_last_history_activity_at() {
    // Given a streaming session with a stale activity timestamp.
    let mut session = attached_session();
    session.begin_streaming();
    session.core.identity.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When appending a token.
    let before = jiff::Timestamp::now();
    session
        .append_stream_token("Hello", jiff::Timestamp::now())
        .expect("ok");

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.identity.last_history_activity_at >= before);
}

#[rstest::rstest]
fn append_thinking_token_bumps_last_history_activity_at() {
    // Given a session with a streaming assistant entry that has begun thinking.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    session.begin_thinking(jiff::Timestamp::now());
    session.core.identity.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When appending a thinking token.
    let before = jiff::Timestamp::now();
    session.append_thinking_token("reasoning").expect("ok");

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.identity.last_history_activity_at >= before);
}

#[rstest::rstest]
fn begin_sending_seeds_last_history_activity_at() {
    // Given a session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.core.identity.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When beginning sending.
    let before = jiff::Timestamp::now();
    session.begin_sending();

    // Then the activity timestamp was seeded to ~now.
    assert!(session.core.identity.last_history_activity_at >= before);
}

#[rstest::rstest]
fn begin_streaming_seeds_last_history_activity_at() {
    // Given a session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.core.identity.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When beginning streaming.
    let before = jiff::Timestamp::now();
    session.begin_streaming();

    // Then the activity timestamp was seeded to ~now.
    assert!(session.core.identity.last_history_activity_at >= before);
}

#[rstest::rstest]
fn append_stream_token_when_not_streaming_returns_error() {
    // Given a session that is not streaming.
    let mut session = ChatSessionState::new();

    // When calling append_stream_token.
    let result = session.append_stream_token("oops", jiff::Timestamp::now());

    // Then it returns an error (no panic).
    assert!(result.is_err());
}

#[rstest::rstest]
fn scroll_up_from_bottom_decrements_offset() {
    // Given a session at the bottom with last_max_offset = 100.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.scroll_to_bottom();
    assert!(session.scroll_offset().is_none());

    // When scrolling up by 10.
    session.scroll_up(10);

    // Then the offset is 90 (100 − 10).
    assert_eq!(session.scroll_offset(), Some(90));
}

#[rstest::rstest]
fn scroll_up_from_known_offset_decrements() {
    // Given a session with scroll_offset = 50 and last_max_offset = 100.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(50));

    // When scrolling up by 10.
    session.scroll_up(10);

    // Then the offset is 40.
    assert_eq!(session.scroll_offset(), Some(40));
}

#[rstest::rstest]
fn scroll_up_saturates_at_zero() {
    // Given a session with scroll_offset = 5 and last_max_offset = 100.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(5));

    // When scrolling up by 20.
    session.scroll_up(20);

    // Then the offset saturates at 0.
    assert_eq!(session.scroll_offset(), Some(0));
}

#[rstest::rstest]
fn scroll_down_increments_offset() {
    // Given a session with scroll_offset = 0 and last_max_offset = 100.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(0));

    // When scrolling down by 10.
    session.scroll_down(10);

    // Then the offset increased by 10.
    assert_eq!(session.scroll_offset(), Some(10));
}

#[rstest::rstest]
fn scroll_down_past_bottom_resets_to_auto() {
    // Given a session with scroll_offset = 95 and last_max_offset = 100.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(95));

    // When scrolling down by 10.
    session.scroll_down(10);

    // Then the offset resets to None (auto-scroll to bottom).
    assert!(session.scroll_offset().is_none());
}

#[rstest::rstest]
fn scroll_to_top_sets_offset_to_zero() {
    // Given a session scrolled to the middle.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(50));

    // When scrolling to top.
    session.scroll_to_top();

    // Then the offset is 0.
    assert_eq!(session.scroll_offset(), Some(0));
}

#[rstest::rstest]
fn scroll_to_bottom_resets_to_auto_scroll() {
    // Given a session scrolled to the top.
    let mut session = ChatSessionState::new();
    session.set_last_max_offset(100);
    session.set_scroll_offset(Some(0));

    // When scrolling to bottom.
    session.scroll_to_bottom();

    // Then the offset is None (auto-scroll).
    assert!(session.scroll_offset().is_none());
}

#[rstest::rstest]
fn reset_scroll_clears_offset() {
    // Given a session with scroll_offset = 50.
    let mut session = ChatSessionState::new();
    session.set_scroll_offset(Some(50));

    // When resetting scroll.
    session.scroll_to_bottom();

    // Then the offset is None (at bottom).
    assert!(session.scroll_offset().is_none());
}

#[rstest::rstest]
fn push_entry_resets_scroll() {
    // Given a session with scroll_offset = 50.
    let mut session = ChatSessionState::new();
    session.set_scroll_offset(Some(50));

    // When pushing an entry.
    session.push_entry(ChatEntry::user("hello"));

    // Then scroll_offset is None (reset by push_entry).
    assert!(session.scroll_offset().is_none());
}

#[rstest::rstest]
fn is_at_bottom_true_when_auto_scroll() {
    // Given a new session (auto-scroll to bottom).
    let session = ChatSessionState::new();

    // When the at-bottom state is queried.
    // Then is_at_bottom is true.
    assert!(session.is_at_bottom());
}

#[rstest::rstest]
fn is_at_bottom_false_when_scrolled_up() {
    // Given a session scrolled to offset 50.
    let mut session = ChatSessionState::new();
    session.set_scroll_offset(Some(50));

    // When the at-bottom state is queried.
    // Then is_at_bottom is false.
    assert!(!session.is_at_bottom());
}

#[rstest::rstest]
fn enqueue_message_adds_to_queue() {
    // Given a new session with an empty queue.
    let mut session = ChatSessionState::new();
    assert_eq!(session.queue_len(), 0);

    // When enqueuing a message.
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("hello"),
    )));

    // Then the queue has one message.
    assert_eq!(session.queue_len(), 1);
    assert!(matches!(
        &session.message_queue().items()[0],
        jinn_turn_dispatch_msg::QueueItem::UserMessage(e) if e.kind == ChatEntryKind::User {
            display: "hello".to_owned(),
            expanded: "hello".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    ));
}

#[rstest::rstest]
fn dequeue_message_returns_first_in_order() {
    // Given a session with two queued messages.
    let mut session = ChatSessionState::new();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("first"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("second"),
    )));

    // When dequeuing a message.
    let msg = session.dequeue();

    // Then it returns the first message and the queue has one left.
    assert!(msg.is_some());
    let item = msg.unwrap();
    let jinn_turn_dispatch_msg::QueueItem::UserMessage(entry) = item else {
        panic!("expected UserMessage")
    };
    assert_eq!(
        entry.kind,
        ChatEntryKind::User {
            display: "first".to_owned(),
            expanded: "first".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    );
    assert_eq!(session.queue_len(), 1);
}

#[rstest::rstest]
fn dequeue_message_returns_none_when_empty() {
    // Given a session with an empty queue.
    let mut session = ChatSessionState::new();

    // When dequeuing a message.
    let msg = session.dequeue();

    // Then it returns None.
    assert!(msg.is_none());
}

#[rstest::rstest]
fn drain_returns_all_in_order() {
    // Given a session with three queued messages.
    let mut session = ChatSessionState::new();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("a"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("b"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("c"),
    )));

    // When draining the queue.
    let drained = session.message_queue_mut().drain();

    // Then all messages are returned in order.
    assert_eq!(drained.len(), 3);
    let entries: Vec<ChatEntry> = drained
        .into_iter()
        .map(|item| match item {
            jinn_turn_dispatch_msg::QueueItem::UserMessage(e) => *e,
            jinn_turn_dispatch_msg::QueueItem::ToolContinuation => {
                panic!("expected UserMessage")
            }
        })
        .collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries[0].kind,
        ChatEntryKind::User {
            display: "a".to_owned(),
            expanded: "a".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    );
    assert_eq!(
        entries[1].kind,
        ChatEntryKind::User {
            display: "b".to_owned(),
            expanded: "b".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    );
    assert_eq!(
        entries[2].kind,
        ChatEntryKind::User {
            display: "c".to_owned(),
            expanded: "c".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    );
}

#[rstest::rstest]
fn drain_empties_queue() {
    // Given a session with three queued messages.
    let mut session = ChatSessionState::new();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("a"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("b"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("c"),
    )));

    // When draining the queue.
    let _ = session.message_queue_mut().drain();

    // Then the queue is empty.
    assert_eq!(session.queue_len(), 0);
}

#[rstest::rstest]
fn begin_sending_sets_is_sending() {
    // Given a new session (idle).
    let mut session = ChatSessionState::new();
    assert_ne!(session.phase(), PhaseKind::Sending);

    // When beginning sending.
    session.begin_sending();

    // Then is_sending is true.
    assert_eq!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
fn begin_sending_is_noop_when_already_sending() {
    // Given a session that is already sending.
    let mut session = ChatSessionState::new();
    session.begin_sending();

    // When calling begin_sending again.
    session.begin_sending();

    // Then phase stays Sending (no panic).
    assert_eq!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
fn begin_sending_is_noop_when_streaming() {
    // Given a session that is streaming.
    let mut session = attached_session();
    session.begin_streaming();

    // When calling begin_sending.
    session.begin_sending();

    // Then phase stays Streaming (no panic).
    assert_eq!(session.phase(), PhaseKind::Streaming);
}

#[rstest::rstest]
fn finish_sending_via_machine_transitions_to_idle_when_disabled() {
    // Given a session that is sending with tool_loop_disabled set.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.set_tool_loop_disabled();

    // When finishing sending via machine.
    session.finish_sending_via_machine();

    // Then the session is Idle (tool_loop_disabled triggered Sending → Idle).
    assert_eq!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn finish_sending_via_machine_is_noop_when_not_sending() {
    // Given a session that is not sending.
    let mut session = ChatSessionState::new();

    // When calling finish_sending_via_machine.
    session.finish_sending_via_machine();

    // Then phase stays Idle (no panic, just a logged warning).
    assert_eq!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn is_idle_true_when_not_sending_or_streaming() {
    // Given a fresh session.
    let session = ChatSessionState::new();

    // When its phase is inspected.
    // Then it is Idle.
    assert_eq!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn is_idle_false_when_sending() {
    // Given a session that is sending.
    let mut session = ChatSessionState::new();
    session.begin_sending();

    // When its phase is inspected.
    // Then it is not idle.
    assert_ne!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn is_idle_false_when_streaming() {
    // Given a session that has begun streaming.
    let mut session = attached_session();
    session.begin_streaming();

    // When its phase is inspected.
    // Then it is not Idle.
    assert_ne!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn cancel_streaming_returns_to_idle() {
    // Given a session in streaming phase.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    assert_eq!(session.phase(), PhaseKind::Streaming);

    // When cancelling streaming.
    session.cancel_streaming(jiff::Timestamp::now());

    // Then the session is idle.
    assert_eq!(session.phase(), PhaseKind::Idle);
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_ne!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
fn cancel_streaming_from_sending_phase_returns_to_idle() {
    // Given a session in sending phase (simulating tool execution).
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.finish_streaming(true, jiff::Timestamp::now());
    session.begin_sending();
    assert_eq!(session.phase(), PhaseKind::Sending);

    // When cancelling streaming (user presses ESC during tool execution).
    session.cancel_streaming(jiff::Timestamp::now());

    // Then the session returns to idle.
    assert_eq!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn finish_streaming_returns_to_idle() {
    // Given a session in streaming phase with an assistant entry.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    let idx = session.push_entry(ChatEntry::assistant(""));
    session
        .core
        .ephemeral
        .machine
        .set_streaming_entry_index(idx);

    // When finishing streaming.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then the session is idle.
    assert_eq!(session.phase(), PhaseKind::Idle);
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_ne!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
fn begin_tool_call_creates_entry_with_empty_arguments() {
    // Given a streaming session.
    let mut session = attached_session();
    session.begin_streaming();

    // When beginning a tool call.
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());

    // Then history has an assistant entry and a tool call entry with empty arguments.
    assert_eq!(session.history().len(), 2);
    assert!(matches!(
        session.history()[0].kind,
        ChatEntryKind::Assistant(_)
    ));
    assert_eq!(
        session.history()[1].kind,
        ChatEntryKind::ToolCall {
            id: "call_1".to_owned(),
            name: "echo".to_owned(),
            arguments: String::new(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn append_tool_call_delta_accumulates_arguments() {
    // Given a streaming session with a tool call entry.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());

    // When appending tool call deltas.
    session
        .append_tool_call_delta(0, r#"{"input":"#)
        .expect("ok");
    session
        .append_tool_call_delta(0, r#""hello"}"#)
        .expect("ok");

    // Then the tool call entry has the accumulated arguments.
    assert_eq!(
        session.history()[1].kind,
        ChatEntryKind::ToolCall {
            id: "call_1".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"input":"hello"}"#.to_owned(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn finalize_tool_call_overwrites_arguments() {
    // Given a streaming session with a tool call that has partial arguments.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());
    session
        .append_tool_call_delta(0, r#"{"input":"#)
        .expect("ok");

    // When finalizing the tool call with the complete arguments.
    session.finalize_tool_call("call_1", "echo", r#"{"input":"world"}"#);

    // Then the arguments are overwritten with the final value.
    assert_eq!(
        session.history()[1].kind,
        ChatEntryKind::ToolCall {
            id: "call_1".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"input":"world"}"#.to_owned(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn finalize_tool_call_pushes_new_entry_when_not_found() {
    // Given a streaming session with no tool call entry for the given ID.
    let mut session = attached_session();
    session.begin_streaming();

    // When finalizing a tool call that was never started (shouldn't happen normally).
    session.finalize_tool_call("call_99", "echo", r#"{"input":"hi"}"#);

    // Then a new entry is pushed (no assistant entry yet \xe2\x80\x94 lazy creation).
    assert_eq!(session.history().len(), 1); // tool call only
    assert_eq!(
        session.history()[0].kind,
        ChatEntryKind::ToolCall {
            id: "call_99".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"input":"hi"}"#.to_owned(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn first_tool_call_tracks_arguments() {
    // Given a streaming session.
    let mut session = attached_session();
    session.begin_streaming();

    // When beginning a tool call and appending a delta.
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());
    session.append_tool_call_delta(0, r#"{"a":1}"#).expect("ok");

    // Then the tool call entry tracks its own arguments.
    assert_eq!(
        session.history()[1].kind,
        ChatEntryKind::ToolCall {
            id: "call_1".to_owned(),
            name: "echo".to_owned(),
            arguments: r#"{"a":1}"#.to_owned(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn second_tool_call_tracks_independent_arguments() {
    // Given a streaming session with one tool call already started.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());
    session.append_tool_call_delta(0, r#"{"a":1}"#).expect("ok");

    // When beginning a second tool call with a different index.
    session.begin_tool_call(1, "call_2", "get_time", jiff::Timestamp::now());
    session.append_tool_call_delta(1, "{}").expect("ok");

    // Then the second tool call entry tracks its own arguments independently.
    assert_eq!(
        session.history()[2].kind,
        ChatEntryKind::ToolCall {
            id: "call_2".to_owned(),
            name: "get_time".to_owned(),
            arguments: "{}".to_owned(),
            child_session: None,
        }
    );
}

#[rstest::rstest]
fn finish_streaming_clears_tool_call_indices() {
    // Given a streaming session with a tool call entry.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());

    // When finishing streaming.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then the tool call indices are cleared (entries remain in history).
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(session.history().len(), 2); // assistant + tool call still there
}

#[rstest::rstest]
fn cancel_streaming_clears_tool_call_indices() {
    // Given a streaming session with a tool call entry.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "call_1", "echo", jiff::Timestamp::now());

    // When cancelling streaming.
    session.cancel_streaming(jiff::Timestamp::now());

    // Then the tool call indices are cleared (entries remain in history).
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(session.history().len(), 2); // assistant + tool call still there
}

#[rstest::rstest]
fn pin_state_sets_position() {
    // Given a session with two entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("first"));
    let id0 = session.history()[0].id.clone();
    session.push_entry(ChatEntry::user("second"));

    // When pinning the first entry as Top.
    session.pin_entry(&id0, PinPosition::Top);

    // Then the first entry has pin_position set to Top.
    assert_eq!(session.history()[0].pin_position, Some(PinPosition::Top));
}

#[rstest::rstest]
fn pin_state_does_not_affect_other_entries() {
    // Given a session with two entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("first"));
    let id0 = session.history()[0].id.clone();
    session.push_entry(ChatEntry::user("second"));

    // When pinning the first entry as Top.
    session.pin_entry(&id0, PinPosition::Top);

    // Then the second entry is still unpinned.
    assert_eq!(session.history()[1].pin_position, None);
}

#[rstest::rstest]
fn pin_entry_is_noop_for_nonexistent_id() {
    // Given a session with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When pinning a random ID.
    let fake_id = ChatEntryId::new();
    session.pin_entry(&fake_id, PinPosition::Top);

    // Then no entries changed.
    assert_eq!(session.history()[0].pin_position, None);
}

#[rstest::rstest]
fn unpin_entry_clears_position() {
    // Given a session with a pinned entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("test"));
    let id = session.history()[0].id.clone();
    session.pin_entry(&id, PinPosition::Top);
    assert_eq!(session.history()[0].pin_position, Some(PinPosition::Top));

    // When unpinning.
    session.unpin_entry(&id);

    // Then the pin position is cleared.
    assert_eq!(session.history()[0].pin_position, None);
}

#[rstest::rstest]
fn unpin_entry_is_noop_for_nonexistent_id() {
    // Given a session with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When unpinning a random ID.
    let fake_id = ChatEntryId::new();
    session.unpin_entry(&fake_id);

    // Then no panic and no entries changed.
    assert_eq!(session.history()[0].pin_position, None);
}

#[rstest::rstest]
fn pinned_entries_returns_only_pinned() {
    // Given a session with three entries, two pinned.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("first"));
    session.push_entry(ChatEntry::user("second"));
    session.push_entry(ChatEntry::user("third"));
    let id0 = session.history()[0].id.clone();
    let id2 = session.history()[2].id.clone();
    session.pin_entry(&id0, PinPosition::Top);
    session.pin_entry(&id2, PinPosition::Bottom);

    // When getting pinned entries.
    let pinned = session.pinned_entries();

    // Then only the pinned entries are returned.
    assert_eq!(pinned.len(), 2);
    assert_eq!(pinned[0].id, id0);
    assert_eq!(pinned[1].id, id2);
}

#[rstest::rstest]
fn pinned_entries_returns_correct_count() {
    // Given a session with five entries, three pinned at indices 0, 2, 4.
    let mut session = ChatSessionState::new();
    for i in 0..5 {
        session.push_entry(ChatEntry::user(format!("msg {i}")));
    }
    let id0 = session.history()[0].id.clone();
    let id2 = session.history()[2].id.clone();
    let id4 = session.history()[4].id.clone();
    session.pin_entry(&id4, PinPosition::Relative);
    session.pin_entry(&id0, PinPosition::Top);
    session.pin_entry(&id2, PinPosition::Bottom);

    // When getting pinned entries.
    let pinned = session.pinned_entries();

    // Then three entries are returned.
    assert_eq!(pinned.len(), 3);
}

#[rstest::rstest]
fn pinned_entries_returns_in_order() {
    // Given a session with five entries, three pinned at indices 0, 2, 4.
    let mut session = ChatSessionState::new();
    for i in 0..5 {
        session.push_entry(ChatEntry::user(format!("msg {i}")));
    }
    let id0 = session.history()[0].id.clone();
    let id2 = session.history()[2].id.clone();
    let id4 = session.history()[4].id.clone();
    // Pin in reverse order to verify ordering is by history, not pin order.
    session.pin_entry(&id4, PinPosition::Relative);
    session.pin_entry(&id0, PinPosition::Top);
    session.pin_entry(&id2, PinPosition::Bottom);

    // When getting pinned entries.
    let pinned = session.pinned_entries();

    // Then they are in history order (0, 2, 4).
    assert_eq!(pinned[0].id, id0);
    assert_eq!(pinned[1].id, id2);
    assert_eq!(pinned[2].id, id4);
}

#[rstest::rstest]
fn pinned_entries_returns_empty_when_none_pinned() {
    // Given a session with entries, none pinned.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));

    // When getting pinned entries.
    let pinned = session.pinned_entries();

    // Then the result is empty.
    assert!(pinned.is_empty());
}

#[rstest::rstest]
fn pin_entry_can_change_position() {
    // Given a session with a pinned entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("test"));
    let id = session.history()[0].id.clone();
    session.pin_entry(&id, PinPosition::Top);
    assert_eq!(session.history()[0].pin_position, Some(PinPosition::Top));

    // When re-pinning with a different position.
    session.pin_entry(&id, PinPosition::Bottom);

    // Then the position is updated.
    assert_eq!(session.history()[0].pin_position, Some(PinPosition::Bottom));
}

#[rstest::rstest]
fn pin_position_survives_restore_history() {
    // Given a history with pinned entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a").with_pin(PinPosition::Top));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c").with_pin(PinPosition::Bottom));

    let original_history = session.history().to_vec();

    // When restoring history from a snapshot.
    let mut new_session = ChatSessionState::new();
    new_session.restore_history(original_history);

    // Then pinned entries survive.
    let pinned = new_session.pinned_entries();
    assert_eq!(pinned.len(), 2);
    assert_eq!(pinned[0].pin_position, Some(PinPosition::Top));
    assert_eq!(pinned[1].pin_position, Some(PinPosition::Bottom));
}

#[rstest::rstest]
fn pin_entry_propagates_shown_to_forward_sub_block() {
    // Given: a shown (expanded) ignored block with entries before and after the pin target.
    // Layout: [user] [ignored-A] [ignored-B] [ignored-C] [ignored-D] [user]
    // Block rep = ignored-A. Pin ignored-B → splits into:
    //   backward sub-block: [ignored-A] (rep=ignored-A, already shown)
    //   pinned: ignored-B
    //   forward sub-block: [ignored-C, ignored-D] (rep=ignored-C, must be auto-shown)
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true));
    session.push_entry(ChatEntry::assistant("c").with_ignored(true));
    session.push_entry(ChatEntry::assistant("d").with_ignored(true));
    session.push_entry(ChatEntry::user("after"));

    // Expand the ignored block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));

    // When ignored-B (entry at idx 2) is pinned to the top.
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the forward sub-block representative (ignored-C) is auto-shown.
    let forward_rep = session.history()[3].id.clone();
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep),
        "forward sub-block should be auto-shown after pin inside shown block"
    );

    // And the original block representative should still be shown.
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

#[rstest::rstest]
fn pin_entry_no_propagation_for_non_ignored() {
    // Given a session holding a single non-ignored entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("normal"));
    let id = session.history()[0].id.clone();

    // When the entry is pinned to the top.
    session.pin_entry(&id, PinPosition::Top);

    // Then shown_ignored_blocks is never touched.
    assert!(session.shown_ignored_blocks_snapshot().is_empty());
}

#[rstest::rstest]
fn pin_entry_no_propagation_for_collapsed_block() {
    // Given a collapsed (never expanded) ignored block of three entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true));
    session.push_entry(ChatEntry::assistant("c").with_ignored(true));
    session.push_entry(ChatEntry::user("after"));
    let pin_id = session.history()[2].id.clone();

    // When the middle ignored entry is pinned to the top.
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the forward sub-block is NOT auto-shown.
    let forward_rep = session.history()[3].id.clone();
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep),
        "forward sub-block should NOT be auto-shown when parent block was collapsed"
    );
}

#[rstest::rstest]
fn pin_entry_at_block_end_no_forward_propagation() {
    // Given an expanded ignored block whose last entry is about to be pinned.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // pin this (last in block)
    session.push_entry(ChatEntry::user("after")); // non-ignored, breaks block
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[2].id.clone();

    // When the last ignored entry in the block is pinned to the top.
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then no forward sub-block exists, so only the original rep stays shown.
    assert_eq!(session.shown_ignored_blocks_snapshot().len(), 1);
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

/// Regression test for: pinning an ignored entry inside an expanded block
/// should keep all entries visible. Before the fix, the forward sub-block
/// would collapse because `shown_ignored_blocks` didn't cover it.
#[rstest::rstest]
fn regression_pin_in_expanded_block_keeps_all_visible() {
    use jinn_chat_log_view_msg::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    };

    // Given a layout of [user] [ignored-A] [ignored-B] [ignored-C]
    // [ignored-D] [user], with the ignored block expanded and ignored-B
    // pinned to the top.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true));
    session.push_entry(ChatEntry::assistant("c").with_ignored(true));
    session.push_entry(ChatEntry::assistant("d").with_ignored(true));
    session.push_entry(ChatEntry::user("after"));

    // Expand the ignored block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin ignored-B (idx 2).
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // When building visual items - all 4 ignored entries should be
    // individually visible (backward sub-block + pinned entry + forward
    // sub-block).
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );

    // Then there is no CollapsedIgnoredBlock - all entries are shown.
    let collapsed = items
        .iter()
        .any(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }));
    assert!(
        !collapsed,
        "no entries should be collapsed after pinning in expanded block"
    );

    // And all history entries appear as VisualItem::Entry.
    let entry_count = items
        .iter()
        .filter(|item| matches!(item, VisualItem::Entry(_)))
        .count();
    assert_eq!(entry_count, 6, "all 6 entries should be visible");
}

/// Regression test for: after pinning inside an expanded block, pressing `h`
/// on a forward sub-block entry should toggle only that sub-block.
#[rstest::rstest]
fn regression_toggle_h_after_pin_split_toggles_correct_sub_block() {
    // Given: layout [user] [ignored-A] [ignored-B(pinned)] [ignored-C] [ignored-D]
    // [user] with the block expanded, ignored-B pinned, and both sub-blocks shown.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    session.push_entry(ChatEntry::user("after")); // idx 5
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);
    let forward_rep = session.history()[3].id.clone();
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep)
    );

    // When the forward sub-block's visibility is toggled (the `h` key).
    session.toggle_ignored_block_visibility(&forward_rep);

    // Then only the forward sub-block collapses; the backward one stays shown.
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep)
    );
    // And the backward sub-block is still shown.
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

/// Regression test for: unpinning an entry re-merges the sub-blocks into
/// one block controlled by the original representative.
#[rstest::rstest]
fn regression_unpin_remerges_block_correctly() {
    use jinn_chat_log_view_msg::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    };

    // Given: layout [user] [ignored-A] [ignored-B] [ignored-C] [user] with the
    // block expanded, ignored-B pinned, and then unpinned again.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::user("after")); // idx 4
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);
    session.unpin_entry(&pin_id);

    // When visual items are built from the re-merged history.
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );

    // Then no block is collapsed and all five entries are visible.
    let collapsed = items
        .iter()
        .any(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }));
    assert!(!collapsed, "no collapsed block after unpin re-merge");

    // And the re-merged block exposes all five history entries.
    let entry_count = items
        .iter()
        .filter(|item| matches!(item, VisualItem::Entry(_)))
        .count();
    assert_eq!(
        entry_count, 5,
        "all 5 entries should be visible after unpin"
    );
}

/// Regression test for: the original representative controls the block that
/// unpinning re-merges the sub-blocks into.
#[rstest::rstest]
fn regression_toggle_after_unpin_collapses_remerged_block() {
    // Given: a block expanded, split by a pin, then re-merged by unpinning.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::user("after")); // idx 4
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);
    session.unpin_entry(&pin_id);

    // When the original representative is toggled.
    session.toggle_ignored_block_visibility(&rep_id);

    // Then the re-merged block collapses.
    assert!(!session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

#[rstest::rstest]
fn select_next_entry_starts_at_first_when_no_selection() {
    // Given a session with 3 entries and no selection.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // push_entry auto-selects, so clear to test the "no selection" case.
    session.clear_selection();
    assert_eq!(session.selected_entry_index(), None);

    // When selecting next.
    session.select_next_entry();

    // Then the first entry (index 0) is selected.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_next_entry_increments_from_current() {
    // Given a session with 3 entries and selection at index 1.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.select_next_entry(); // 0
    session.select_next_entry(); // 1

    // When selecting next again.
    session.select_next_entry();

    // Then the index is 2.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_next_entry_clamps_at_last_index() {
    // Given a session with 3 entries and selection at last index.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.select_next_entry(); // 0
    session.select_next_entry(); // 1
    session.select_next_entry(); // 2

    // When selecting next again.
    session.select_next_entry();

    // Then the index stays at 2.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_prev_entry_starts_at_last_when_no_selection() {
    // Given a session with 3 entries and no selection.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // push_entry auto-selects last, so clear to test the "no selection" case.
    session.clear_selection();

    // When selecting prev.
    session.select_prev_entry();

    // Then the last entry (index 2) is selected.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_prev_entry_decrements_from_current() {
    // Given a session with 3 entries and selection at index 2.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // push_entry auto-selects last (index 2).
    assert_eq!(session.selected_entry_index(), Some(2));

    // When selecting prev again.
    session.select_prev_entry();

    // Then the index is 1.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_prev_entry_clamps_at_zero() {
    // Given a session with 3 entries and selection at index 0.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // push_entry auto-selects last (2). Move to index 0.
    session.set_selected_entry_index(0);

    // When selecting prev.
    session.select_prev_entry();

    // Then the index stays at 0.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_next_is_noop_on_empty_history() {
    // Given an empty session.
    let mut session = ChatSessionState::new();

    // When selecting next.
    session.select_next_entry();

    // Then no selection is set.
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn select_prev_is_noop_on_empty_history() {
    // Given an empty session.
    let mut session = ChatSessionState::new();

    // When selecting prev.
    session.select_prev_entry();

    // Then no selection is set.
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn clear_selection_resets_to_none() {
    // Given a session with a selection.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    // push_entry auto-selects index 0.
    assert_eq!(session.selected_entry_index(), Some(0));

    // When clearing selection.
    session.clear_selection();

    // Then selection is None.
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn selected_entry_returns_entry_at_index() {
    // Given a session with entries, second selected.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // push_entry auto-selects last (2). Move to index 1.
    session.set_selected_entry_index(1);

    // When getting the selected entry.
    let entry = session.selected_entry();

    // Then it returns the entry at index 1.
    assert!(entry.is_some());
    assert_eq!(
        entry.unwrap().kind,
        ChatEntryKind::User {
            display: "b".to_owned(),
            expanded: "b".to_owned(),
            attachments: Vec::new(),
            outcome: jinn_core_types::AttachmentOutcome::default(),
        }
    );
}

#[rstest::rstest]
fn selected_entry_id_returns_id_at_index() {
    // Given a session with a selected entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    let expected_id = session.history()[0].id.clone();
    // push_entry auto-selects index 0.

    // When getting the selected entry ID.
    let id = session.selected_entry_id();

    // Then it matches the first entry's ID.
    assert_eq!(id, Some(&expected_id));
}

#[rstest::rstest]
fn push_entry_auto_selects_new_entry_when_at_last() {
    // Given a session with a selected last entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    assert_eq!(session.selected_entry_index(), Some(0));

    // When pushing a new entry.
    session.push_entry(ChatEntry::user("b"));

    // Then the cursor advances to the new entry.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn restore_history_auto_selects_last_entry() {
    // Given a session with a selected entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    assert_eq!(session.selected_entry_index(), Some(0));

    // When restoring history.
    session.restore_history(vec![ChatEntry::user("new")]);

    // Then the last entry is auto-selected.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn begin_thinking_appends_before_assistant_is_created() {
    // Given a streaming session (no assistant entry yet - lazy creation).
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    // begin_streaming no longer creates an entry.
    assert_eq!(session.history().len(), 1);

    // When beginning thinking.
    session.begin_thinking(jiff::Timestamp::now());

    // Then the Thinking entry is appended (index 1).
    // No Assistant entry yet - it will be created on first token.
    assert_eq!(session.history().len(), 2);
    assert!(matches!(
        session.history()[1].kind,
        ChatEntryKind::Thinking(_)
    ));
    assert_eq!(session.streaming_thinking_entry_index(), Some(1));
}

#[rstest::rstest]
fn append_thinking_token_appends_to_thinking_entry() {
    // Given a session with a streaming Assistant entry that has begun thinking.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    session.begin_thinking(jiff::Timestamp::now());

    // When appending thinking tokens.
    session.append_thinking_token("reasoning").expect("ok");
    session.append_thinking_token(" more").expect("ok");

    // Then the Thinking entry has the accumulated text.
    match &session.history()[1].kind {
        ChatEntryKind::Thinking(text) => assert_eq!(text, "reasoning more"),
        other => panic!("expected Thinking, got {other:?}"),
    }
}

#[rstest::rstest]
fn finish_streaming_clears_thinking_entry_index() {
    // Given a session with a thinking entry and assistant entry.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    session.begin_thinking(jiff::Timestamp::now());
    session.append_thinking_token("reasoning").expect("ok");
    session
        .append_stream_token("response", jiff::Timestamp::now())
        .expect("ok");

    // When finishing streaming.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then the thinking entry index is cleared.
    assert_eq!(session.streaming_thinking_entry_index(), None);
    // And the thinking text is preserved in history.
    assert!(
        matches!(session.history()[1].kind, ChatEntryKind::Thinking(ref t) if t == "reasoning")
    );
    assert!(
        matches!(session.history()[2].kind, ChatEntryKind::Assistant(ref t) if t == "response")
    );
}

#[rstest::rstest]
fn cancel_streaming_preserves_partial_thinking() {
    // Given a session with partial thinking text.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    session.begin_thinking(jiff::Timestamp::now());
    session
        .append_thinking_token("partial reasoning")
        .expect("ok");

    // When cancelling streaming.
    session.cancel_streaming(jiff::Timestamp::now());

    // Then the partial thinking text is preserved.
    assert_eq!(session.streaming_thinking_entry_index(), None);
    assert!(
        matches!(session.history()[1].kind, ChatEntryKind::Thinking(ref t) if t == "partial reasoning")
    );
}

#[rstest::rstest]
fn finish_streaming_without_preserve_skips_assistant_entry() {
    // Given a session that is streaming with no tokens received.
    let mut session = attached_session();
    session.begin_streaming();

    // When finishing streaming without preserving assistant.
    session.finish_streaming(false, jiff::Timestamp::now());

    // Then no assistant entry was created.
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert!(session.history().is_empty());
}

#[rstest::rstest]
fn finish_streaming_with_preserve_creates_assistant_entry() {
    // Given a session that is streaming with no tokens received.
    let mut session = attached_session();
    session.begin_streaming();

    // When finishing streaming with preserving assistant.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then an empty assistant entry was created.
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(session.history().len(), 1);
    assert!(matches!(session.history()[0].kind, ChatEntryKind::Assistant(ref t) if t.is_empty()));
}

#[rstest::rstest]
fn finish_streaming_without_preserve_keeps_existing_assistant() {
    // Given a session that is streaming and has received tokens.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .append_stream_token("Hello", jiff::Timestamp::now())
        .expect("ok");

    // When finishing streaming without preserving assistant.
    session.finish_streaming(false, jiff::Timestamp::now());

    // Then the existing assistant entry is still there (ensure_assistant_entry was a no-op since entry already existed).
    assert_ne!(session.phase(), PhaseKind::Streaming);
    assert_eq!(session.history().len(), 1);
    assert!(matches!(session.history()[0].kind, ChatEntryKind::Assistant(ref t) if t == "Hello"));
}

#[rstest::rstest]
fn push_entry_auto_selects_first_entry() {
    // Given an empty session.
    let mut session = ChatSessionState::new();

    // When pushing the first entry.
    session.push_entry(ChatEntry::user("hello"));

    // Then the new entry is auto-selected.
    assert_eq!(session.selected_entry_index(), Some(0));
    // And scroll is reset to bottom.
    assert!(session.is_at_bottom());
}

#[rstest::rstest]
fn push_entry_preserves_selection_when_not_at_last() {
    // Given a session with 3 entries, cursor on first.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // Move cursor to first entry (not last).
    session.set_selected_entry_index(0);

    // When pushing a new entry.
    session.push_entry(ChatEntry::user("d"));

    // Then the cursor stays on index 0.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn push_entry_resets_scroll_only_when_at_last() {
    // Given a session with entries, scrolled up.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    // Scroll up and move cursor away from last.
    session.set_scroll_offset(Some(0));
    session.set_selected_entry_index(0);

    // When pushing a new entry.
    session.push_entry(ChatEntry::user("d"));

    // Then the scroll is NOT reset (cursor was not at last).
    assert_eq!(session.scroll_offset(), Some(0));
}

#[rstest::rstest]
fn push_entry_resets_scroll_when_at_last() {
    // Given a session with entries, scrolled up, cursor on last.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    // push auto-selects last (1).
    session.set_scroll_offset(Some(0));
    assert_eq!(session.selected_entry_index(), Some(1));

    // When pushing a new entry.
    session.push_entry(ChatEntry::user("c"));

    // Then scroll is reset to bottom.
    assert!(session.is_at_bottom());
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn restore_history_auto_selects_last() {
    // Given history entries to restore.
    let mut session = ChatSessionState::new();

    // When restoring history with 3 entries.
    session.restore_history(vec![
        ChatEntry::user("a"),
        ChatEntry::user("b"),
        ChatEntry::user("c"),
    ]);

    // Then the last entry is auto-selected.
    assert_eq!(session.selected_entry_index(), Some(2));
    // And scroll is at bottom.
    assert!(session.is_at_bottom());
}

#[rstest::rstest]
fn restore_history_empty_clears_selection() {
    // Given a session with entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));

    // When restoring empty history.
    session.restore_history(vec![]);

    // Then selection is None.
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn begin_thinking_auto_selects_new_last_when_at_last() {
    // Given a streaming session with cursor on user (last entry - no assistant created yet).
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    // begin_streaming doesn't create an entry. Cursor is on user (index 0).
    assert_eq!(session.selected_entry_index(), Some(0));

    // When beginning thinking (appends Thinking at index 1).
    session.begin_thinking(jiff::Timestamp::now());

    // Then cursor advances to the new last entry (thinking at index 1).
    // history: [user(0), thinking(1)]
    assert_eq!(session.selected_entry_index(), Some(1));
    assert!(matches!(
        session.history()[1].kind,
        ChatEntryKind::Thinking(_)
    ));
}

#[rstest::rstest]
fn begin_thinking_preserves_selection_when_not_at_last() {
    // Given a streaming session with cursor NOT on assistant.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .with_user_entry("other")
        .begin_streaming()
        .build();
    // Move cursor to user entry (not the assistant).
    session.set_selected_entry_index(0);

    // When beginning thinking.
    session.begin_thinking(jiff::Timestamp::now());

    // Then cursor stays on the user entry.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn visible_entry_range_returns_visible_entries() {
    // Given a session with viewport state.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.push_entry(ChatEntry::user("d"));
    session.push_entry(ChatEntry::user("e"));

    // Entry ranges: [0..2), [2..4), [4..6), [6..8), [8..10)
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]);
    session.set_viewport_height(5);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(2);

    // When computing visible range.
    // viewport_top=2, viewport_bottom=7
    // Entry 1 (lines 2..4), Entry 2 (lines 4..6), Entry 3 (lines 6..8) are visible.
    let range = session.visible_entry_range();

    // Then entries 1..4 are visible.
    assert_eq!(range, 1..4);
}

#[rstest::rstest]
fn visible_entry_range_empty_when_no_ranges() {
    // Given a session with no viewport state.
    let session = ChatSessionState::new();

    // When computing visible range.
    let range = session.visible_entry_range();

    // Then it returns an empty range.
    assert!(range.is_empty());
}

#[rstest::rstest]
fn move_cursor_to_first_visible_sets_index() {
    // Given a session with viewport state.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));

    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(2);

    // When moving cursor to first visible.
    session.move_cursor_to_first_visible();

    // Then cursor is on the first visible entry.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn move_cursor_to_last_visible_sets_index() {
    // Given a session with viewport state.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));

    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(2);

    // When moving cursor to last visible.
    session.move_cursor_to_last_visible();

    // Then cursor is on the last visible entry.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn visible_entry_range_uses_rendered_scroll_offset_not_scroll_offset() {
    // Given a session where scroll_offset (user intent) disagrees with
    // rendered_scroll_offset (actual viewport position).
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.push_entry(ChatEntry::user("d"));
    session.push_entry(ChatEntry::user("e"));

    // Entry ranges: [0..2), [2..4), [4..6), [6..8), [8..10)
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]);
    session.set_viewport_height(5);
    session.set_blank_count(0);

    // Stale scroll_offset: None (auto-scroll → bottom = offset 5).
    // Actual rendered position: offset 2 (viewport showing entries 1-3).
    session.set_rendered_scroll_offset(2);

    // When computing visible range.
    let range = session.visible_entry_range();

    // Then it uses the rendered offset (2), not the stale scroll_offset.
    // viewport_top=2, viewport_bottom=7
    // Entry 1 (lines 2..4), Entry 2 (lines 4..6), Entry 3 (lines 6..8) are visible.
    assert_eq!(range, 1..4);
}

#[rstest::rstest]
fn cwd_preserved_across_serialization_round_trip() {
    // Given a session with a specific CWD.
    let mut session = ChatSessionState::new();
    session.set_cwd(PathBuf::from("/tmp"));

    // When serializing and deserializing.
    let json = serde_json::to_string(&session).expect("serialize");
    let restored: ChatSessionState = serde_json::from_str(&json).expect("deserialize");

    // Then the CWD is preserved.
    assert_eq!(restored.cwd(), PathBuf::from("/tmp"));
}

#[rstest::rstest]
fn cwd_defaults_to_dot_when_missing_from_snapshot() {
    // Given a JSON snapshot of a session without a `cwd` field.
    let mut session = ChatSessionState::new();
    session.set_title("test".to_owned());
    let mut json = serde_json::to_value(&session).expect("serialize");
    // Remove the cwd field to simulate an old snapshot.
    json.as_object_mut().expect("object").remove("cwd");

    let json_str = serde_json::to_string(&json).expect("re-serialize");

    // When deserializing.
    let restored: ChatSessionState = serde_json::from_str(&json_str).expect("deserialize");

    // Then the CWD defaults to "." (resolves to current directory).
    assert_eq!(restored.cwd(), PathBuf::from("."));
}

#[rstest::rstest]
fn serde_round_trips_lifecycle_fields() {
    // Given a session with lifecycle fields set.
    let mut session = ChatSessionState::new();
    session.set_lifecycle_name(Some("fossil branch".to_owned()));
    session.set_lifecycle_args(vec!["feature-x".to_owned()]);

    // When serializing and deserializing.
    let json = serde_json::to_string(&session).expect("serialize");
    let back: ChatSessionState = serde_json::from_str(&json).expect("deserialize");

    // Then lifecycle fields are preserved.
    assert_eq!(back.lifecycle_name(), Some("fossil branch"));
    assert_eq!(back.lifecycle_args(), &["feature-x".to_owned()]);
}

#[rstest::rstest]
fn serde_lifecycle_group_remains_flat() {
    // Given a session with lifecycle fields set.
    let mut session = ChatSessionState::new();
    session.set_title("Lifecycle title".to_owned());
    session.set_cwd(std::path::PathBuf::from("/workspace/project"));
    session.set_lifecycle_name(Some("dev".to_owned()));
    session.set_lifecycle_args(vec!["--fast".to_owned()]);
    session.advance_lifecycle_after_setup();
    session.set_persist(false);

    // When serializing the session.
    let json = serde_json::to_value(&session).expect("serialize");
    let object = json.as_object().expect("session is an object");

    // Then lifecycle fields stay at the top level without a nested wrapper.
    assert!(!object.contains_key("lifecycle"));
    assert_eq!(
        object.get("title"),
        Some(&serde_json::json!("Lifecycle title"))
    );
    assert_eq!(
        object.get("cwd"),
        Some(&serde_json::json!("/workspace/project"))
    );
    assert_eq!(
        object.get("lifecycle_name"),
        Some(&serde_json::json!("dev"))
    );
    assert_eq!(
        object.get("lifecycle_args"),
        Some(&serde_json::json!(["--fast"]))
    );
    assert_eq!(
        object.get("lifecycle_script_state"),
        Some(&serde_json::json!("setup_ran"))
    );
    assert_eq!(object.get("persist"), Some(&serde_json::json!(false)));
}

/// A [`SessionCore`] populated with a representative value from every broad
/// group, used to assert that serialization keeps all groups flat.
fn composed_five_group_core() -> SessionCore {
    let mut core = SessionCore::default();
    core.identity.title = Some("Composed session".to_owned());
    core.identity.parent_session = Some(SessionId::new());
    core.identity.origin = SessionOrigin::Fork;
    core.identity.has_interacted = true;
    core.lifecycle.cwd = PathBuf::from("/workspace/project");
    core.lifecycle.lifecycle_name = Some("release".to_owned());
    core.lifecycle.lifecycle_args = vec!["--verbose".to_owned()];
    core.lifecycle.lifecycle_script_state = LifecycleScriptState::SetupRan;
    core.restore_history(vec![ChatEntry::user("hello")]);
    core.history_work.task_list.add_phase("Ship it");
    core.integrations.profile.model = ModelSelection::Single("ollama/llama3".to_owned());
    core.integrations
        .blobs
        .insert("future".to_owned(), serde_json::json!({"enabled": true}));
    core.integrations
        .enabled_mcp_servers
        .insert("files".to_owned());
    core.storage.session_state = SessionState::Archived;
    core.storage.persist = false;
    core
}

#[rstest::rstest]
fn session_core_five_group_serialization_remains_flat() {
    // Given a session core with representative values from every broad group.
    let core = composed_five_group_core();

    // When serializing the composed core.
    let json = serde_json::to_value(&core).expect("serialize");
    let object = json.as_object().expect("core is an object");

    // Then all five groups remain flat and their representative fields are top-level.
    for wrapper in [
        "identity",
        "lifecycle",
        "history_work",
        "integrations",
        "storage",
    ] {
        assert!(
            !object.contains_key(wrapper),
            "unexpected {wrapper} wrapper"
        );
    }
    assert_eq!(
        object.get("title"),
        Some(&serde_json::json!("Composed session"))
    );
    assert_eq!(
        object.get("cwd"),
        Some(&serde_json::json!("/workspace/project"))
    );
    assert_eq!(
        object
            .get("history")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        object
            .get("task_list")
            .and_then(|value| value.get("phases"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        object.get("enabled_mcp_servers"),
        Some(&serde_json::json!(["files"]))
    );
    assert_eq!(
        object.get("session_state"),
        Some(&serde_json::json!("archived"))
    );
    assert_eq!(object.get("persist"), Some(&serde_json::json!(false)));
}

#[rstest::rstest]
fn session_core_loads_legacy_flat_fields_into_broad_groups() {
    // Given a hand-written legacy flat session snapshot containing all five groups.
    let legacy = concat!(
        r#"{"session_id":"10000000-0000-0000-0000-000000000071","#,
        r#""updated_at":"2024-01-01T00:00:00Z","created_at":"2024-01-01T00:00:00Z","#,
        r#""title":"Legacy composed","parent_session":"10000000-0000-0000-0000-000000000070","#,
        r#""fork_ordinal":2,"origin":"fork","project":"/legacy/project","has_interacted":true,"#,
        r#""cwd":"/legacy/repo","lifecycle_name":"release","lifecycle_args":["--verbose"],"#,
        r#""lifecycle_script_state":"setup_ran","history":[],"token_ledger":[],"#,
        r#""task_list":{"phases":[]},"profile":{"model":{"single":"ollama/llama3"},"#,
        r#""persona_name":"coding-assistant","disabled_tools":[],"disabled_skills":[],"#,
        r#""reasoning_effort":null,"endpoint":null},"blobs":{"future":true},"#,
        r#""enabled_mcp_servers":["files"],"session_state":"archived","persist":false}"#
    );

    // When deserializing the flat snapshot.
    let core: SessionCore = serde_json::from_str(legacy).expect("deserialize legacy core");

    // Then every representative field is restored into its broad group.
    assert_eq!(core.identity.title.as_deref(), Some("Legacy composed"));
    assert_eq!(core.lifecycle.cwd, PathBuf::from("/legacy/repo"));
    assert!(core.history_work.history.is_empty());
    assert_eq!(
        core.integrations.profile.model,
        ModelSelection::Single("ollama/llama3".to_owned())
    );
    assert_eq!(core.storage.session_state, SessionState::Archived);
    assert!(!core.storage.persist);
}

#[rstest::rstest]
fn serde_defaults_lifecycle_fields_when_missing() {
    // Given a JSON object without lifecycle fields.
    let json = r#"{"session_id":"00000000-0000-0000-0000-000000000001","updated_at":"2026-01-01T00:00:00Z","created_at":"2026-01-01T00:00:00Z","history":[],"profile":{"model":{"single":""},"strategy":"passthrough"},"cwd":"."}"#;

    // When deserializing.
    let back: ChatSessionState = serde_json::from_str(json).expect("deserialize");

    // Then lifecycle fields default to None/empty.
    assert!(back.lifecycle_name().is_none());
    assert!(back.lifecycle_args().is_empty());
}

#[rstest::rstest]
fn is_empty_true_for_new_session() {
    // Given a newly created session.
    let session = ChatSessionState::new();

    // When its emptiness is queried.
    // Then it is empty.
    assert!(session.is_empty());
}

#[rstest::rstest]
fn is_empty_false_after_pushing_entry() {
    // Given a new session with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When its emptiness is queried.
    // Then it is not empty.
    assert!(!session.is_empty());
}

#[rstest::rstest]
#[test]
fn begin_tool_result_creates_pending_entry() {
    // Given a session in streaming state.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();

    // When beginning a tool result.
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // Then the history has a pending ToolResult entry.
    assert_eq!(session.history().len(), 1);
    let entry = &session.history()[0];
    match &entry.kind {
        ChatEntryKind::ToolResult {
            id,
            name,
            content,
            status,
            ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
            assert!(content.is_empty());
            assert_eq!(*status, ToolResultStatus::Pending);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn begin_tool_result_tracks_history_index() {
    // Given a session in streaming state with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.begin_sending();
    session.begin_streaming();

    // When beginning a tool result.
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // Then the tracking index points to the second entry.
    assert!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()
            .contains_key("call_1")
    );
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()["call_1"],
        1
    );
}

#[rstest::rstest]
#[test]
fn append_tool_result_output_appends_to_pending_entry() {
    // Given a session in streaming state with a pending tool result.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // When appending output.
    session.append_tool_result_output("call_1", "line 1\n", jinn_tools_msg::ToolOutputKind::Normal);
    session.append_tool_result_output("call_1", "line 2\n", jinn_tools_msg::ToolOutputKind::Normal);

    // Then the entry content has both outputs.
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult { content, .. } => {
            assert_eq!(
                content,
                "line 1
line 2
"
            );
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn append_tool_result_output_bumps_history_activity_timestamp() {
    // Given a session whose last activity is in the past.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());
    session.core.identity.last_history_activity_at = jiff::Timestamp::now()
        .checked_sub(jiff::Span::new().hours(1))
        .expect("past");

    // When appending streaming output.
    let before = session.core.identity.last_history_activity_at;
    std::thread::sleep(std::time::Duration::from_millis(10));
    session.append_tool_result_output("call_1", "tick", jinn_tools_msg::ToolOutputKind::Normal);

    // Then the activity timestamp advanced past its pre-append value.
    assert!(session.core.identity.last_history_activity_at > before);
}

#[rstest::rstest]
#[test]
fn append_tool_result_output_ignores_unknown_call_id() {
    // Given a session with no pending tool result.
    let mut session = ChatSessionState::new();

    // When appending output for an unknown call ID.
    // Then it does not panic (defensive).
    session.append_tool_result_output("unknown", "output", jinn_tools_msg::ToolOutputKind::Normal);
    assert!(session.history().is_empty());
}

#[rstest::rstest]
#[test]
fn finalize_tool_result_completes_pending_entry() {
    // Given a session in streaming state with a pending tool result.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());
    session.append_tool_result_output(
        "call_1",
        "building...\n",
        jinn_tools_msg::ToolOutputKind::Normal,
    );

    // When finalizing with success.
    session.finalize_tool_result("call_1", "bash", "final output", true, None, None, None);

    // Then the entry is updated with final content and Success status.
    assert_eq!(session.history().len(), 1);
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            content, status, ..
        } => {
            assert_eq!(content, "final output");
            assert_eq!(*status, ToolResultStatus::Success);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    // And the tracking index is removed.
    assert!(
        !session
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()
            .contains_key("call_1")
    );
}

#[rstest::rstest]
#[test]
fn tool_result_entry_gets_finished_at_on_finalize() {
    // Given a session in streaming state with a pending tool result.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // When finalizing the tool result.
    session.finalize_tool_result("call_1", "bash", "done", true, None, None, None);

    // Then the ToolResult entry has finished_at set.
    let entry = session
        .history()
        .iter()
        .find(|e| matches!(&e.kind, ChatEntryKind::ToolResult { id, .. } if id == "call_1"))
        .expect("tool result entry");
    match &entry.timing {
        EntryTiming::Streamed { finished_at, .. } => {
            assert!(
                finished_at.is_some(),
                "finished_at should be set after finalize"
            );
        }
        other => panic!("expected Streamed, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn finalize_tool_result_pushes_new_entry_for_unknown_id() {
    // Given a session with no pending tool result.
    let mut session = ChatSessionState::new();

    // When finalizing for a tool that never streamed.
    session.finalize_tool_result("call_1", "bash", "output", true, None, None, None);

    // Then a new entry is pushed.
    assert_eq!(session.history().len(), 1);
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            id,
            name,
            content,
            status,
            ..
        } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
            assert_eq!(content, "output");
            assert_eq!(*status, ToolResultStatus::Success);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn begin_tool_result_does_not_push_when_not_streaming() {
    // Given a session NOT in streaming phase (defaults to Idle).
    let mut session = ChatSessionState::new();

    // When beginning a tool result.
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // Then no entry is pushed (the early return prevented it).
    assert!(
        session.history().is_empty(),
        "expected no entry when not streaming, got {} entries",
        session.history().len()
    );
}

#[rstest::rstest]
#[test]
fn begin_tool_result_in_sending_creates_a_pending_entry() {
    // Given a session in Sending phase — the ordinary case, since the stream
    // ends in ToolUse before the tool batch runs.
    let mut session = ChatSessionState::new();
    session.begin_sending();

    // When beginning a tool result.
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // Then the pending entry is created rather than dropped.
    assert_eq!(
        session.history().len(),
        1,
        "a tool result in Sending must still be recorded"
    );
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            id,
            content,
            status,
            ..
        } => {
            assert_eq!(id, "call_1");
            assert!(content.is_empty());
            assert_eq!(*status, ToolResultStatus::Pending);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn tool_result_output_appends_in_sending_phase() {
    // Given a pending tool result created in Sending phase.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // When streaming output arrives.
    session.append_tool_result_output("call_1", "line 1\n", jinn_tools_msg::ToolOutputKind::Normal);

    // Then the output lands in that same entry.
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult { content, .. } => assert_eq!(content, "line 1\n"),
        other => panic!("expected ToolResult, got {other:?}"),
    }
    assert_eq!(
        session.history().len(),
        1,
        "output must not spawn a second entry"
    );
}

#[rstest::rstest]
#[test]
fn tool_result_finalizes_in_place_in_sending_phase() {
    // Given a pending tool result created in Sending phase with streamed output.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());
    session.append_tool_result_output("call_1", "partial", jinn_tools_msg::ToolOutputKind::Normal);

    // When the tool finishes.
    session.finalize_tool_result("call_1", "bash", "final output", true, None, None, None);

    // Then it is finalized in place — one entry, no duplicate pushed at the end.
    assert_eq!(
        session.history().len(),
        1,
        "finalize must reuse the pending entry"
    );
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            content, status, ..
        } => {
            assert_eq!(content, "final output");
            assert_eq!(*status, ToolResultStatus::Success);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn tool_result_in_sending_stays_adjacent_to_its_tool_call() {
    // Given a completed tool call followed by the tool batch executing in Sending.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call(
        "call_1",
        "bash",
        r#"{"command":"ls"}"#,
    ));
    session.begin_sending();
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // When the tool finishes.
    session.finalize_tool_result("call_1", "bash", "file.txt", true, None, None, None);

    // Then the result sits directly after its call — the pairing the user
    // sees in the chat log, and that context assembly depends on.
    let kinds: Vec<_> = session
        .history()
        .iter()
        .map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, .. } => format!("result:{id}"),
            ChatEntryKind::ToolCall { id, .. } => format!("call:{id}"),
            _ => "other".to_owned(),
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["other", "other", "call:call_1", "result:call_1"]
    );
}

#[rstest::rstest]
#[test]
fn finalize_tool_result_updates_existing_pending_by_kind_id() {
    // Given a session with a manually-pushed pending ToolResult
    // (simulating the case where begin_tool_result pushed before
    // the streaming index was available).
    let mut session = ChatSessionState::new();
    let pending = ChatEntry::tool_result("call_1", "bash", "", ToolResultStatus::Pending);
    let pending_id = pending.id.clone();
    session.push_entry(pending);
    assert_eq!(session.history().len(), 1);

    // When finalizing the tool result (no streaming index available).
    session.finalize_tool_result("call_1", "bash", "actual output", true, None, None, None);

    // Then the existing entry was updated in-place (same ChatEntryId).
    assert_eq!(
        session.history().len(),
        1,
        "expected no new entry, got {} entries",
        session.history().len()
    );
    assert_eq!(
        session.history()[0].id,
        pending_id,
        "entry should be the same (updated in-place)"
    );
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            content, status, ..
        } => {
            assert_eq!(content, "actual output");
            assert_eq!(*status, ToolResultStatus::Success);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn finalize_tool_result_updates_existing_with_truncation() {
    // Given a session with a pending ToolResult.
    let mut session = ChatSessionState::new();
    let pending = ChatEntry::tool_result("call_1", "bash", "", ToolResultStatus::Pending);
    let pending_id = pending.id.clone();
    session.push_entry(pending);

    // When finalizing with truncation data.
    let meta = jinn_core_types::tool_types::TruncationMeta {
        truncated_by: jinn_core_types::tool_types::TruncatedBy::Bytes,
        total_lines: 50,
        total_bytes: 1000,
        output_lines: 25,
        output_bytes: 500,
    };
    session.finalize_tool_result(
        "call_1",
        "bash",
        "truncated output",
        true,
        Some("full output...".to_owned()),
        Some(meta),
        None,
    );

    // Then the existing entry was updated in-place with truncation data.
    assert_eq!(session.history().len(), 1);
    assert_eq!(session.history()[0].id, pending_id);
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            content,
            status,
            full_content,
            truncation,
            ..
        } => {
            assert_eq!(content, "truncated output");
            assert_eq!(*status, ToolResultStatus::Success);
            assert_eq!(full_content.as_deref(), Some("full output..."));
            assert!(truncation.is_some(), "expected truncation meta");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn finalize_tool_result_pushes_new_with_truncation_when_no_existing() {
    // Given an empty session.
    let mut session = ChatSessionState::new();

    // When finalizing with truncation but no existing entry.
    let meta = jinn_core_types::tool_types::TruncationMeta {
        truncated_by: jinn_core_types::tool_types::TruncatedBy::Bytes,
        total_lines: 50,
        total_bytes: 1000,
        output_lines: 25,
        output_bytes: 500,
    };
    session.finalize_tool_result(
        "call_1",
        "bash",
        "truncated",
        true,
        Some("full".to_owned()),
        Some(meta),
        None,
    );

    // Then a new truncated entry was pushed.
    assert_eq!(session.history().len(), 1);
    match &session.history()[0].kind {
        ChatEntryKind::ToolResult {
            content,
            full_content,
            truncation,
            ..
        } => {
            assert_eq!(content, "truncated");
            assert_eq!(full_content.as_deref(), Some("full"));
            assert!(truncation.is_some());
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[rstest::rstest]
fn session_state_defaults_to_loaded() {
    // Given a new session.
    let session = ChatSessionState::new();

    // When its session state is read.
    // Then session state is Loaded.
    assert_eq!(session.session_state(), SessionState::Loaded);
}

#[rstest::rstest]
fn lifecycle_script_state_defaults_to_nothing_ran() {
    // Given a new session.
    let session = ChatSessionState::new();

    // When its lifecycle script state is read.
    // Then it is NothingRan.
    assert_eq!(
        session.lifecycle_script_state(),
        LifecycleScriptState::NothingRan
    );
}

#[rstest::rstest]
fn session_state_can_be_set_to_archived() {
    // Given a new session.
    let mut session = ChatSessionState::new();

    // When setting to Archived.
    session.set_session_state(SessionState::Archived);

    // Then it is Archived.
    assert_eq!(session.session_state(), SessionState::Archived);
}

#[rstest::rstest]
fn session_state_can_be_set_back_to_loaded_from_archived() {
    // Given a session in Archived state.
    let mut session = ChatSessionState::new();
    session.set_session_state(SessionState::Archived);

    // When setting back to Loaded.
    session.set_session_state(SessionState::Loaded);

    // Then it is Loaded.
    assert_eq!(session.session_state(), SessionState::Loaded);
}

#[rstest::rstest]
fn chat_session_advance_lifecycle_after_setup() {
    // Given a new session (NothingRan).
    let mut session = ChatSessionState::new();
    assert_eq!(
        session.lifecycle_script_state(),
        LifecycleScriptState::NothingRan
    );

    // When advancing after setup.
    session.advance_lifecycle_after_setup();

    // Then state is SetupRan.
    assert_eq!(
        session.lifecycle_script_state(),
        LifecycleScriptState::SetupRan
    );
}

#[rstest::rstest]
fn chat_session_advance_lifecycle_after_teardown() {
    // Given a session with SetupRan.
    let mut session = ChatSessionState::new();
    session.advance_lifecycle_after_setup();

    // When advancing after teardown.
    session.advance_lifecycle_after_teardown();

    // Then state is TeardownRan.
    assert_eq!(
        session.lifecycle_script_state(),
        LifecycleScriptState::TeardownRan
    );
}

// ---------------------------------------------------------------------------
// Saved history position
// ---------------------------------------------------------------------------

#[rstest::rstest]
fn has_saved_history_position_returns_false_by_default() {
    // Given a new session.
    let session = ChatSessionState::new();

    // When a saved history position is queried.
    // Then none exists.
    assert!(!session.has_saved_history_position());
}

#[rstest::rstest]
fn save_history_position_captures_current_state() {
    // Given a session with known scroll offset and selected entry.
    let mut session = ChatSessionState::builder()
        .with_user_entry("first")
        .with_user_entry("second")
        .with_user_entry("third")
        .build();
    session.set_scroll_offset(Some(10));
    let second_id = session.history()[1].id.clone();
    session.set_selected_entry_index(1);

    // When saving history position.
    session.save_history_position();

    // Then the saved position matches the current state.
    assert!(session.has_saved_history_position());
    let saved = session.saved_history_position().expect("saved");
    assert_eq!(saved.scroll_offset, Some(10));
    assert_eq!(saved.selected_cursor_id, Some(second_id));
}

#[rstest::rstest]
fn restore_history_position_restores_and_clears() {
    // Given a session with a saved position.
    let mut session = ChatSessionState::builder()
        .with_user_entry("first")
        .with_user_entry("second")
        .build();
    session.set_scroll_offset(Some(5));
    session.set_selected_entry_index(0);
    session.save_history_position();

    // When modifying the state and then restoring.
    session.set_scroll_offset(Some(99));
    session.set_selected_entry_index(1);
    session.restore_history_position();

    // Then the state is restored to the saved values.
    assert_eq!(session.scroll_offset(), Some(5));
    assert_eq!(session.selected_entry_index(), Some(0));
    // And the saved position is cleared.
    assert!(!session.has_saved_history_position());
}

#[rstest::rstest]
fn discard_saved_history_position_clears_without_restoring() {
    // Given a session with a saved position.
    let mut session = ChatSessionState::builder()
        .with_user_entry("first")
        .with_user_entry("second")
        .build();
    session.set_scroll_offset(Some(5));
    session.set_selected_entry_index(0);
    session.save_history_position();

    // When modifying state and then discarding.
    session.set_scroll_offset(Some(99));
    session.set_selected_entry_index(1);
    session.discard_saved_history_position();

    // Then the state is NOT restored.
    assert_eq!(session.scroll_offset(), Some(99));
    assert_eq!(session.selected_entry_index(), Some(1));
    // And the saved position is cleared.
    assert!(!session.has_saved_history_position());
}

#[rstest::rstest]
fn save_history_position_does_not_overwrite_existing() {
    // Given a session with a saved position.
    let mut session = ChatSessionState::builder()
        .with_user_entry("first")
        .with_user_entry("second")
        .build();
    session.set_scroll_offset(Some(5));
    let first_id = session.history()[0].id.clone();
    session.set_selected_entry_index(0);
    session.save_history_position();

    // When modifying state and saving again.
    session.set_scroll_offset(Some(99));
    session.set_selected_entry_index(1);
    session.save_history_position();

    // Then the original saved position is kept.
    let saved = session.saved_history_position().expect("saved");
    assert_eq!(saved.scroll_offset, Some(5));
    assert_eq!(saved.selected_cursor_id, Some(first_id));
}

#[rstest::rstest]
fn restore_is_noop_when_nothing_saved() {
    // Given a session with no saved position.
    let mut session = ChatSessionState::builder().with_user_entry("first").build();
    session.set_scroll_offset(Some(10));
    session.set_selected_entry_index(0);

    // When restoring with nothing saved.
    session.restore_history_position();

    // Then the state is unchanged.
    assert_eq!(session.scroll_offset(), Some(10));
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_next_entry_skips_empty_assistant() {
    // Given history [user, empty_assistant, user] with selection at 0.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::user("c"));
    session.set_selected_entry_index(0);

    // When selecting next.
    session.select_next_entry();

    // Then selection skips the empty assistant and lands on index 2.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_prev_entry_skips_empty_assistant() {
    // Given history [user, empty_assistant, user] with selection at 2.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::user("c"));
    session.set_selected_entry_index(2);

    // When selecting previous.
    session.select_prev_entry();

    // Then selection skips the empty assistant and lands on index 0.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_next_entry_stays_put_when_only_empty_assistant_remains() {
    // Given history [user, empty_assistant] with selection at 0.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::assistant(""));
    session.set_selected_entry_index(0);

    // When selecting next.
    session.select_next_entry();

    // Then selection stays at 0 (can't skip to empty assistant).
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_prev_entry_stays_put_when_only_empty_assistant_remains() {
    // Given history [empty_assistant, user] with selection at 1.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::user("b"));
    session.set_selected_entry_index(1);

    // When selecting previous.
    session.select_prev_entry();

    // Then selection stays at 1 (can't skip to empty assistant).
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn is_tool_call_streaming_returns_false_for_non_streaming_entry() {
    // Given a session with a finalized tool call entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::tool_call(
        "tc-1",
        "write",
        r#"{"path":"foo.rs"}"#,
    ));

    // When checking if the tool call entry is streaming.
    let entry_id = session.history()[1].id.clone();

    // Then it returns false (no active streaming).
    assert!(
        !session.is_tool_call_streaming(&entry_id),
        "finalized tool call should not be streaming"
    );
}

#[rstest::rstest]
fn is_tool_call_streaming_returns_true_for_active_streaming_entry() {
    // Given a streaming session with an active tool call.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "tc-1", "write", jiff::Timestamp::now());

    // When checking if the tool call entry is streaming.
    let entry_id = session.history()[1].id.clone();

    // Then it returns true (actively streaming).
    assert!(
        session.is_tool_call_streaming(&entry_id),
        "active tool call should be streaming"
    );
}

#[rstest::rstest]
fn is_tool_call_streaming_returns_false_after_finish_streaming() {
    // Given a streaming session with a tool call that has been finalized.
    let mut session = attached_session();
    session.begin_streaming();
    session.begin_tool_call(0, "tc-1", "write", jiff::Timestamp::now());
    let entry_id = session.history()[1].id.clone();

    // When finishing streaming.
    session.finish_streaming(true, jiff::Timestamp::now());

    // Then the tool call entry is no longer streaming.
    assert!(
        !session.is_tool_call_streaming(&entry_id),
        "tool call should not be streaming after finish_streaming"
    );
}

#[rstest::rstest]
fn is_tool_call_streaming_returns_false_for_non_tool_call_entry() {
    // Given a streaming session with a user entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    let user_id = session.history()[0].id.clone();
    session.begin_streaming();

    // When checking a user entry.
    // Then it returns false (not a tool call).
    assert!(
        !session.is_tool_call_streaming(&user_id),
        "user entry should never be a streaming tool call"
    );
}

#[rstest::rstest]
fn is_tool_call_streaming_returns_false_for_unknown_id() {
    // Given a session.
    let session = ChatSessionState::new();

    // When checking a random ID.
    let fake_id = ChatEntryId::new();

    // Then it returns false.
    assert!(
        !session.is_tool_call_streaming(&fake_id),
        "unknown ID should not be streaming"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_expands_block() {
    // Given a session with 3 non-ignored + 5 ignored + 2 non-ignored entries.
    let mut session = ChatSessionState::new();
    for _ in 0..3 {
        session.push_entry(ChatEntry::user("visible"));
    }
    for _ in 0..5 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    }
    for _ in 0..2 {
        session.push_entry(ChatEntry::user("visible"));
    }

    let block_start_id = session.history()[3].id.clone();

    // When toggling visibility of an entry in the ignored block.
    let mid_id = session.history()[5].id.clone();
    session.toggle_ignored_block_visibility(&mid_id);

    // Then the block is shown (first entry's ID is in shown_ignored_blocks).
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&block_start_id),
        "block should be shown after toggle"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_collapses_expanded_block() {
    // Given a session with an expanded ignored block.
    let mut session = ChatSessionState::new();
    for _ in 0..3 {
        session.push_entry(ChatEntry::user("visible"));
    }
    for _ in 0..5 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    }
    for _ in 0..2 {
        session.push_entry(ChatEntry::user("visible"));
    }

    let block_start_id = session.history()[3].id.clone();
    let mid_id = session.history()[5].id.clone();

    // Expand first.
    session.toggle_ignored_block_visibility(&mid_id);
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&block_start_id)
    );

    // When toggling again (same entry).
    session.toggle_ignored_block_visibility(&mid_id);

    // Then the block is collapsed (removed from shown_ignored_blocks).
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&block_start_id),
        "block should be collapsed after second toggle"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_noop_for_non_ignored() {
    // Given a session with only non-ignored entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::user("world"));

    let entry_id = session.history()[0].id.clone();

    // When toggling a non-ignored entry.
    session.toggle_ignored_block_visibility(&entry_id);

    // Then nothing is in shown_ignored_blocks.
    assert!(
        session.shown_ignored_blocks_snapshot().is_empty(),
        "no blocks should be shown for non-ignored entry"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_noop_for_unknown_id() {
    // Given a session.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When toggling an unknown ID.
    let fake_id = ChatEntryId::new();
    session.toggle_ignored_block_visibility(&fake_id);

    // Then nothing changes.
    assert!(session.shown_ignored_blocks_snapshot().is_empty());
}

/// A forced-exclude ("ignored") user entry, optionally pinned to the top.
fn ignored_user_entry(text: &str, pinned: bool) -> ChatEntry {
    let mut entry = ChatEntry::user(text);
    entry.apply_context_override(
        ContextOverride::ForcedExclude,
        ChangeSource::Internal {
            label: "test".into(),
        },
    );
    entry.pin_position = pinned.then_some(PinPosition::Top);
    entry
}

/// A session whose history is 3 non-ignored, 3 ignored-unpinned, 1 ignored-pinned,
/// 3 ignored-unpinned, then 2 non-ignored — an ignored region split in two by
/// the pinned entry. History indices: 0-2 visible, 3-5 ignored, 6 ignored+pinned,
/// 7-9 ignored, 10-11 visible.
fn session_with_pinned_split_ignored_region() -> ChatSessionState {
    let mut session = ChatSessionState::new();
    for _ in 0..3 {
        session.push_entry(ChatEntry::user("visible"));
    }
    for _ in 0..3 {
        session.push_entry(ignored_user_entry("ignored", false));
    }
    session.push_entry(ignored_user_entry("ignored-pinned", true));
    for _ in 0..3 {
        session.push_entry(ignored_user_entry("ignored", false));
    }
    for _ in 0..2 {
        session.push_entry(ChatEntry::user("visible"));
    }
    session
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_stops_at_pinned_entry() {
    // Given an ignored region split in two by a pinned entry.
    let mut session = session_with_pinned_split_ignored_region();
    let second_sub_block_id = session.history()[7].id.clone();
    let first_sub_block_start_id = session.history()[3].id.clone();

    // When toggling an entry in the second sub-block (after the pinned entry).
    session.toggle_ignored_block_visibility(&second_sub_block_id);

    // Then the representative is the first entry of the second sub-block,
    // not the first entry of the whole ignored region.
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&second_sub_block_id),
        "representative should be the second sub-block start (index 7)"
    );
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&first_sub_block_start_id),
        "representative should NOT be the first sub-block start (index 3)"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_visibility_stops_at_pinned_entry_in_first_sub_block() {
    // Given an ignored region split in two by a pinned entry.
    let mut session = session_with_pinned_split_ignored_region();
    let first_sub_block_mid_id = session.history()[4].id.clone();
    let first_sub_block_start_id = session.history()[3].id.clone();
    let second_sub_block_start_id = session.history()[7].id.clone();

    // When toggling an entry in the first sub-block (before the pinned entry).
    session.toggle_ignored_block_visibility(&first_sub_block_mid_id);

    // Then the representative is the first entry of the first sub-block only.
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&first_sub_block_start_id),
        "representative should be the first sub-block start (index 3)"
    );
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&second_sub_block_start_id),
        "representative should NOT be the second sub-block start (index 7)"
    );
}

#[rstest::rstest]
fn select_next_walks_visual_items_with_collapsed_block() {
    // Given a session with visual items: [Entry, CollapsedBlock, Entry].
    use jinn_chat_log_view_msg::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
    };

    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // index 0
    for _ in 0..15 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    } // indices 1..15, collapsed into one block
    session.push_entry(ChatEntry::user("b")); // index 16

    // Force visual items computation.
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);

    // Select first entry (visual-item index 0).
    session.set_selected_entry_index(0);
    assert_eq!(session.selected_entry_index(), Some(0));

    // When selecting next.
    session.select_next_entry();

    // Then selection moves to the collapsed block (visual-item index 1).
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_prev_walks_visual_items_with_collapsed_block() {
    // Given a session with visual items: [Entry, CollapsedBlock, Entry, ...].
    use jinn_chat_log_view_msg::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
    };

    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // index 0
    for _ in 0..15 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    } // indices 1..15, collapsed
    session.push_entry(ChatEntry::user("b")); // index 16

    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items.clone());

    // Select last entry.
    let last_vi_idx = items.len() - 1;
    session.set_selected_entry_index(last_vi_idx);

    // When selecting prev.
    session.select_prev_entry();

    // Then selection moves to the collapsed block.
    assert_eq!(session.selected_entry_index(), Some(last_vi_idx - 1));
}

#[rstest::rstest]
fn selected_entry_returns_none_for_collapsed_block() {
    // Given a session whose visual items contain a collapsed ignored block.
    use jinn_chat_log_view_msg::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
    };

    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    for _ in 0..15 {
        session.push_entry(ignored_user_entry("ignored", false));
    }
    session.push_entry(ChatEntry::user("b"));
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);

    // When the collapsed block (visual-item index 1) is selected.
    session.set_selected_entry_index(1);

    // Then selected_entry() returns None.
    assert!(
        session.selected_entry().is_none(),
        "collapsed block should not resolve to an entry"
    );
    // And selected_entry_id() returns None.
    assert!(
        session.selected_entry_id().is_none(),
        "collapsed block should not have an entry ID"
    );
    // And selected_entry_index() returns the visual-item index.
    assert_eq!(session.selected_entry_index(), Some(1));
    // And selected_history_index() returns None (no history index for the block).
    assert!(
        session.selected_history_index().is_none(),
        "collapsed block has no history index"
    );
}

#[rstest::rstest]
fn toggle_entry_ignored_flips_false_to_true() {
    // Given a session with a selected user entry (default ignored=false).
    let mut session = ChatSessionState::new();
    let idx = session.push_entry(ChatEntry::user("hello"));
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);
    // Select the entry.
    session.set_selected_entry_index(0);
    assert!(
        !session.history()[idx].ignored(),
        "entry should start un-ignored"
    );

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry is ignored.
    assert!(
        session.history()[idx].ignored(),
        "entry should be ignored after toggle"
    );
}

#[rstest::rstest]
fn toggle_entry_ignored_flips_forced_exclude_to_forced_include() {
    // Given a session with a selected ignored entry.
    let mut session = ChatSessionState::new();
    let idx = session.push_entry(ChatEntry::user("hello").with_ignored(true));
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);
    // Select the entry.
    session.set_selected_entry_index(0);
    assert!(
        session.history()[idx].ignored(),
        "entry should start ignored"
    );

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry is ForcedInclude (brought into context).
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedInclude
    );
}

#[rstest::rstest]
fn toggle_default_user_entry_goes_to_forced_exclude() {
    // Given a session with a selected Default User entry (in context).
    let mut session = ChatSessionState::new();
    let idx = session.push_entry(ChatEntry::user("hello"));
    session.set_selected_entry_index(0);
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::Default
    );

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry becomes ForcedExclude (taken out of context).
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
fn toggle_default_system_entry_goes_to_forced_include() {
    // Given a session with a selected Default System entry (out of context by kind).
    let mut session = ChatSessionState::new();
    let idx = session.push_entry(ChatEntry::system("note"));
    session.set_selected_entry_index(0);
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::Default
    );
    assert!(!session.history()[idx].is_in_context());

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry becomes ForcedInclude (brought into context).
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedInclude
    );
}

#[rstest::rstest]
fn toggle_forced_include_goes_to_forced_exclude() {
    // Given a session with a selected ForcedInclude User entry.
    let mut session = ChatSessionState::new();
    let mut entry = ChatEntry::user("hello");
    entry.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
    let idx = session.push_entry(entry);
    session.set_selected_entry_index(0);

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry becomes ForcedExclude.
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
fn toggle_forced_exclude_goes_to_forced_include() {
    // Given a session with a selected ForcedExclude User entry.
    let mut session = ChatSessionState::new();
    let mut entry = ChatEntry::user("hello");
    entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
    let idx = session.push_entry(entry);
    session.set_selected_entry_index(0);

    // When toggling ignored.
    session.toggle_entry_ignored();

    // Then the entry becomes ForcedInclude.
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedInclude
    );
}

#[rstest::rstest]
fn toggle_twice_on_default_user_ends_forced_include() {
    // Given a session with a selected Default User entry (in context).
    let mut session = ChatSessionState::new();
    let idx = session.push_entry(ChatEntry::user("hello"));
    session.set_selected_entry_index(0);

    // When toggling ignored twice.
    session.toggle_entry_ignored();
    session.set_selected_entry_index(0);
    session.toggle_entry_ignored();

    // Then the entry is back in context but marked ForcedInclude (not Default).
    assert_eq!(
        session.history()[idx].context_override(),
        ContextOverride::ForcedInclude
    );
    assert!(session.history()[idx].is_in_context());
}

#[rstest::rstest]
#[test]
fn new_session_is_not_persistable() {
    // Given a new session with no history or interaction.
    let session = ChatSessionState::new();

    // When persistability is checked.
    // Then the session is not persistable.
    assert!(!session.is_persistable());
}

#[rstest::rstest]
#[test]
fn session_becomes_persistable_after_mark_interacted() {
    // Given a new session.
    let mut session = ChatSessionState::new();

    // When marking the session as interacted.
    session.mark_interacted();

    // Then the session is persistable.
    assert!(session.is_persistable());
}

#[rstest::rstest]
#[test]
fn lifecycle_session_is_always_persistable() {
    // Given a new session with a lifecycle name but no interaction.
    let mut session = ChatSessionState::new();
    session.core.lifecycle.lifecycle_name = Some("test-lifecycle".to_owned());

    // When persistability is checked.
    // Then the session is persistable even without interaction.
    assert!(session.is_persistable());
    assert!(!session.has_interacted());
}

#[rstest::rstest]
#[test]
fn forked_session_is_always_persistable() {
    // Given a new session with a parent session but no interaction.
    let mut session = ChatSessionState::new();
    session.core.identity.parent_session = Some(SessionId::new());

    // When persistability is checked.
    // Then the session is persistable even without interaction.
    assert!(session.is_persistable());
    // And it still reports no interaction.
    assert!(!session.has_interacted());
}

#[rstest::rstest]
#[test]
fn force_exclude_excludes_dangling_tool_call_and_empty_assistant() {
    // Given a history with an empty Assistant and a ToolCall with no matching ToolResult.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then both the ToolCall and empty Assistant are ForcedExclude.
    let history = session.history();
    assert_eq!(history[0].context_override(), ContextOverride::Default);
    assert_eq!(
        history[1].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn force_exclude_preserves_complete_tool_loop() {
    // Given a history with a complete tool loop (ToolCall + ToolResult).
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
    session.push_entry(ChatEntry::tool_result(
        "tc-1",
        "bash",
        "file.txt",
        jinn_core_types::ToolResultStatus::Success,
    ));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then no entries are ForcedExclude.
    let history = session.history();
    for entry in history {
        assert_eq!(
            entry.context_override(),
            ContextOverride::Default,
            "expected Default for entry {:?}",
            entry.kind
        );
    }
}

#[rstest::rstest]
#[test]
fn force_exclude_excludes_non_empty_assistant_with_its_dangling_call() {
    // Given a history with a non-empty Assistant and a dangling ToolCall.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant("let me check"));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then the whole loop chunk is excluded; the user entry is not.
    // The assistant text is excluded with its call: half-chunk exclusion is
    // the bug class the history editor exists to prevent.
    let history = session.history();
    assert_eq!(history[0].context_override(), ContextOverride::Default);
    assert_eq!(
        history[1].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn force_exclude_handles_multiple_dangling_calls() {
    // Given a history with multiple dangling ToolCalls.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
    session.push_entry(ChatEntry::tool_call("tc-2", "read", r#"{"file":"a.rs"}"#));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then all ToolCalls and the empty Assistant are ForcedExclude.
    let history = session.history();
    assert_eq!(history[0].context_override(), ContextOverride::Default);
    assert_eq!(
        history[1].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        history[3].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn force_exclude_mixed_complete_and_incomplete() {
    // Given a history with a complete loop and an incomplete loop.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
    session.push_entry(ChatEntry::tool_result(
        "tc-1",
        "bash",
        "file.txt",
        jinn_core_types::ToolResultStatus::Success,
    ));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-2", "read", r#"{"file":"a.rs"}"#));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then only tc-2 and its empty Assistant are excluded; tc-1 entries are untouched.
    let history = session.history();
    assert_eq!(history[0].context_override(), ContextOverride::Default); // User
    assert_eq!(history[1].context_override(), ContextOverride::Default); // Assistant ""
    assert_eq!(history[2].context_override(), ContextOverride::Default); // ToolCall tc-1
    assert_eq!(history[3].context_override(), ContextOverride::Default); // ToolResult tc-1
    assert_eq!(
        history[4].context_override(),
        ContextOverride::ForcedExclude
    ); // Assistant ""
    assert_eq!(
        history[5].context_override(),
        ContextOverride::ForcedExclude
    ); // ToolCall tc-2
}

#[rstest::rstest]
#[test]
fn force_exclude_registers_dangling_loops_as_an_expanded_ignored_block() {
    // Given an interrupted attempt whose dangling loop chunk is at least the
    // collapse threshold, so exclusion alone would hide the whole attempt.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("run it"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", r#"{"command":"ls"}"#));
    session.push_entry(ChatEntry::tool_call("tc-2", "read", r#"{"file":"a.rs"}"#));

    // When force-excluding dangling tool calls.
    let excluded = session.force_exclude_dangling_tool_calls();

    // Then every excluded entry is registered as a shown ignored block.
    let shown = session.shown_ignored_blocks_snapshot();
    for id in &excluded {
        assert!(shown.contains(id), "excluded entry {id:?} is not shown");
    }
}

#[rstest::rstest]
#[test]
fn force_exclude_leaves_ignored_blocks_untouched_when_nothing_is_dangling() {
    // Given a history with no tool calls.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::assistant("hi"));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then no ignored block is registered.
    assert!(session.shown_ignored_blocks_snapshot().is_empty());
}

#[rstest::rstest]
#[test]
fn force_exclude_no_tool_calls_is_noop() {
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::assistant("hi"));

    // When force-excluding dangling tool calls.
    session.force_exclude_dangling_tool_calls();

    // Then nothing is excluded.
    let history = session.history();
    for entry in history {
        assert_eq!(entry.context_override(), ContextOverride::Default);
    }
}

#[rstest::rstest]
fn cancel_stream_and_drain_puts_user_display_text_in_input() {
    // Given a streaming session with queued user messages.
    let mut session = attached_session();
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("hello world"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("second message"),
    )));

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains the drained display texts joined by the cancel separator.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "hello world\n\n---\n\nsecond message");
}

#[rstest::rstest]
fn cancel_stream_and_drain_discards_non_user_items() {
    // Given a streaming session with mixed queue items.
    let mut session = attached_session();
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("keep this"),
    )));

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then only user display text appears in the input buffer.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "keep this");
}

#[rstest::rstest]
fn cancel_stream_and_drain_with_empty_queue_leaves_input_empty() {
    // Given a streaming session with an empty queue.
    let mut session = attached_session();
    session.begin_streaming();
    assert_eq!(session.queue_len(), 0);

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer remains empty.
    assert!(session.with_input(|i| i.text().is_empty(), || true));
}

#[rstest::rstest]
fn cancel_stream_and_drain_skips_tool_continuation() {
    // Given a streaming session with a tool continuation in the queue.
    let mut session = attached_session();
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then input buffer is empty (ToolContinuation was silently discarded).
    assert!(session.with_input(|i| i.text().is_empty(), || true));
}

#[rstest::rstest]
fn cancel_stream_and_drain_uses_display_not_expanded() {
    // Given a user entry where display differs from expanded.
    let mut entry = ChatEntry::user("short");
    if let ChatEntryKind::User {
        ref mut expanded, ..
    } = entry.kind
    {
        *expanded = "short\nwith\nextra\nlines".to_owned();
    }
    let mut session = attached_session();
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        entry,
    )));

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains the display text, not expanded.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "short");
}

#[rstest::rstest]
fn cancel_stream_and_drain_puts_steering_in_input() {
    // Given a streaming session with two steering fragments.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .steering_buffer_mut()
        .push_fragment("frag1".to_owned());
    session
        .steering_buffer_mut()
        .push_fragment("frag2".to_owned());

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains both fragments joined by the cancel separator.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "frag1\n\n---\n\nfrag2");
}

#[rstest::rstest]
fn cancel_stream_and_drain_single_steering_fragment_no_separator() {
    // Given a streaming session with one steering fragment.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .steering_buffer_mut()
        .push_fragment("frag1".to_owned());

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains the fragment with no separator.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "frag1");
}

#[rstest::rstest]
fn cancel_stream_and_drain_clears_steering_buffer() {
    // Given a streaming session with a steering fragment.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .steering_buffer_mut()
        .push_fragment("frag1".to_owned());

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the steering buffer is empty.
    assert!(
        session.steering_buffer().is_empty(),
        "steering buffer must be drained on cancel"
    );
}

#[rstest::rstest]
fn cancel_stream_and_drain_flattens_steering_and_queue() {
    // Given a streaming session with two steering fragments and two queued messages.
    let mut session = attached_session();
    session.begin_streaming();
    session.steering_buffer_mut().push_fragment("s1".to_owned());
    session.steering_buffer_mut().push_fragment("s2".to_owned());
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("m1"),
    )));
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("m2"),
    )));

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains all units flattened, steering first, joined by the cancel separator.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "s1\n\n---\n\ns2\n\n---\n\nm1\n\n---\n\nm2");
}

#[rstest::rstest]
fn cancel_stream_and_drain_both_empty_leaves_input_unchanged() {
    // Given a streaming session with a pre-filled input box and empty buffers.
    let mut session = attached_session();
    session.begin_streaming();
    session.update_input(|i| i.replace_all("pre-existing".to_owned()));
    assert_eq!(session.queue_len(), 0);
    assert!(session.steering_buffer().is_empty());

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer is left unchanged.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "pre-existing");
}

#[rstest::rstest]
fn cancel_stream_and_drain_single_queue_message_no_separator() {
    // Given a streaming session with one queued user message.
    let mut session = attached_session();
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("keep this"),
    )));

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains the single message with no separator.
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "keep this");
}

#[rstest::rstest]
fn cancel_stream_and_drain_steering_with_only_tool_continuation() {
    // Given a streaming session with a steering fragment and a ToolContinuation in the queue.
    let mut session = attached_session();
    session.begin_streaming();
    session
        .steering_buffer_mut()
        .push_fragment("frag1".to_owned());
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);

    // When cancelling and draining.
    session.cancel_stream_and_drain();

    // Then the input buffer contains only the steering fragment (continuation discarded).
    let text = session.with_input(|i| i.text().to_owned(), String::new);
    assert_eq!(text, "frag1");
}

#[rstest::rstest]
fn insert_entry_at_shifts_streaming_entry_index() {
    // Given a streaming session with a streaming entry at index 3.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::user("c")); // idx 2
    session.push_entry(ChatEntry::assistant("streaming")); // idx 3
    session.begin_sending();
    session.begin_streaming();
    session.core.ephemeral.machine.set_streaming_entry_index(3);

    // When inserting at index 1 (before the streaming entry).
    let result = session.insert_entry_at(1, ChatEntry::system("inserted"));

    // Then the insertion happened at index 1.
    assert_eq!(result, 1);
    assert_eq!(session.history().len(), 5);
    // And streaming_entry_index was shifted from 3 to 4.
    assert_eq!(
        session.core.ephemeral.machine.streaming_entry_index(),
        Some(4)
    );
}

#[rstest::rstest]
fn insert_entry_at_does_not_shift_streaming_index_before_insertion() {
    // Given a streaming session with streaming entry at index 1.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("streaming")); // idx 1
    session.begin_sending();
    session.begin_streaming();
    session.core.ephemeral.machine.set_streaming_entry_index(1);

    // When inserting at index 2 (after the streaming entry).
    session.insert_entry_at(2, ChatEntry::system("inserted"));

    // Then streaming_entry_index stays at 1.
    assert_eq!(
        session.core.ephemeral.machine.streaming_entry_index(),
        Some(1)
    );
}

#[rstest::rstest]
fn insert_entry_at_shifts_thinking_entry_index() {
    // Given a session with thinking entry at index 2.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::assistant("thinking")); // idx 2
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .set_streaming_thinking_entry_index(2);

    // When inserting at index 0.
    session.insert_entry_at(0, ChatEntry::system("inserted"));

    // Then thinking index shifted from 2 to 3.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_thinking_entry_index(),
        Some(3)
    );
}

#[rstest::rstest]
fn insert_entry_at_does_not_shift_thinking_index_before_insertion() {
    // Given a session with thinking entry at index 0.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("thinking")); // idx 0
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .set_streaming_thinking_entry_index(0);

    // When inserting at index 1 (after thinking).
    session.insert_entry_at(1, ChatEntry::system("inserted"));

    // Then thinking index stays at 0.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_thinking_entry_index(),
        Some(0)
    );
}

#[rstest::rstest]
fn insert_entry_at_shifts_tool_result_indices() {
    // Given a session with a tool result index at position 4.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::user("c")); // idx 2
    session.push_entry(ChatEntry::user("d")); // idx 3
    session.push_entry(ChatEntry::tool_result(
        "tr-1",
        "bash",
        "ok",
        ToolResultStatus::Success,
    )); // idx 4
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .streaming_tool_result_indices_mut()
        .insert("tr-1".to_owned(), 4);

    // When inserting at index 2.
    session.insert_entry_at(2, ChatEntry::system("inserted"));

    // Then tool result index shifted from 4 to 5.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()
            .get("tr-1"),
        Some(&5)
    );
}

#[rstest::rstest]
fn insert_entry_at_does_not_shift_tool_result_indices_before_insertion() {
    // Given a session with tool result index at position 1.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::tool_result(
        "tr-1",
        "bash",
        "ok",
        ToolResultStatus::Success,
    )); // idx 1
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .streaming_tool_result_indices_mut()
        .insert("tr-1".to_owned(), 1);

    // When inserting at index 2 (after the tool result).
    session.insert_entry_at(2, ChatEntry::system("inserted"));

    // Then tool result index stays at 1.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_result_indices()
            .get("tr-1"),
        Some(&1)
    );
}

#[rstest::rstest]
fn insert_entry_at_shifts_tool_call_indices() {
    // Given a session with tool call stream index 0 → history index 3.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::user("c")); // idx 2
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", "{}")); // idx 3
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .streaming_tool_call_indices_mut()
        .insert(0, 3);

    // When inserting at index 1.
    session.insert_entry_at(1, ChatEntry::system("inserted"));

    // Then tool call history index shifted from 3 to 4.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_call_indices()
            .get(&0),
        Some(&4)
    );
}

#[rstest::rstest]
fn insert_entry_at_does_not_shift_tool_call_indices_before_insertion() {
    // Given a session with tool call stream index 0 → history index 1.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::tool_call("tc-1", "bash", "{}")); // idx 1
    session.begin_sending();
    session.begin_streaming();
    session
        .core
        .ephemeral
        .machine
        .streaming_tool_call_indices_mut()
        .insert(0, 1);

    // When inserting at index 3 (after the tool call).
    session.insert_entry_at(3, ChatEntry::system("inserted"));

    // Then tool call history index stays at 1.
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_call_indices()
            .get(&0),
        Some(&1)
    );
}

#[rstest::rstest]
fn insert_entry_at_shifts_at_exact_boundary() {
    // Given a streaming session with streaming entry at index 2.
    // The boundary condition: inserting at exactly the streaming index.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::assistant("streaming")); // idx 2
    session.begin_sending();
    session.begin_streaming();
    session.core.ephemeral.machine.set_streaming_entry_index(2);

    // When inserting at index 2 (exact boundary).
    session.insert_entry_at(2, ChatEntry::system("inserted"));

    // Then streaming_entry_index was shifted (>= check, not just >).
    assert_eq!(
        session.core.ephemeral.machine.streaming_entry_index(),
        Some(3)
    );
}

#[rstest::rstest]
fn insert_entry_at_clamps_index_beyond_length() {
    // Given a session with 2 entries.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1

    // When inserting at index 10 (beyond length).
    let result = session.insert_entry_at(10, ChatEntry::system("inserted"));

    // Then the index is clamped to the end (index 2).
    assert_eq!(result, 2);
    assert_eq!(session.history().len(), 3);
    // And the inserted entry is at the end.
    assert!(matches!(
        session.history()[2].kind,
        ChatEntryKind::System(_)
    ));
}

#[rstest::rstest]
fn insert_entry_at_shifts_multiple_indices() {
    // Given a session with streaming, thinking, and tool call indices.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("streaming")); // idx 1
    session.begin_sending();
    session.begin_streaming();
    session.core.ephemeral.machine.set_streaming_entry_index(1);
    session
        .core
        .ephemeral
        .machine
        .set_streaming_thinking_entry_index(1);
    session
        .core
        .ephemeral
        .machine
        .streaming_tool_call_indices_mut()
        .insert(0, 1);

    // When inserting at index 0.
    session.insert_entry_at(0, ChatEntry::system("inserted"));

    // Then ALL indices at or after insertion point are shifted.
    assert_eq!(
        session.core.ephemeral.machine.streaming_entry_index(),
        Some(2)
    );
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_thinking_entry_index(),
        Some(2)
    );
    assert_eq!(
        session
            .core
            .ephemeral
            .machine
            .streaming_tool_call_indices()
            .get(&0),
        Some(&2)
    );
}

#[rstest::rstest]
#[case::idle("idle", PhaseKind::Idle)]
#[case::sending("sending", PhaseKind::Sending)]
#[case::streaming("streaming", PhaseKind::Streaming)]

fn phase_kind_from_str_roundtrips(#[case] input: &str, #[case] expected: PhaseKind) {
    // Given a phase string.
    // When parsing.
    let result: Result<PhaseKind, _> = input.parse();
    // Then it matches the expected phase.
    assert_eq!(result.unwrap(), expected);
}

#[rstest::rstest]
#[case::uppercase("IDLE")]
#[case::mixed_case("Streaming")]
fn phase_kind_from_str_is_case_insensitive(#[case] input: &str) {
    // Given a phase string with non-lowercase.
    // When parsing.
    let result: Result<PhaseKind, _> = input.parse();
    // Then it still parses correctly.
    assert!(result.is_ok());
}

#[rstest::rstest]
fn phase_kind_from_str_rejects_unknown() {
    // Given an unknown phase string.
    let phase = "unknown_phase";

    // When it is parsed into a PhaseKind.
    let result: Result<PhaseKind, _> = phase.parse();

    // Then parsing returns an error.
    assert!(result.is_err());
}

#[rstest::rstest]
fn phase_kind_from_str_rejects_empty() {
    // Given an empty phase string.
    let phase = "";

    // When it is parsed into a PhaseKind.
    let result: Result<PhaseKind, _> = phase.parse();

    // Then parsing returns an error.
    assert!(result.is_err());
}

#[rstest::rstest]
fn scroll_to_selected_noop_when_no_selection() {
    // Given a session with no selection.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.clear_selection();
    session.set_entry_line_ranges(vec![(0, 2)]);
    session.set_viewport_height(10);
    session.set_last_max_offset(20);
    session.set_rendered_scroll_offset(0);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then scroll offset is unchanged.
    assert_eq!(session.scroll_offset(), None);
}

#[rstest::rstest]
fn scroll_to_selected_entry_fits_in_viewport_above() {
    // Given a session where the selected entry is above the viewport.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(3);
    session.set_blank_count(0);
    session.set_last_max_offset(10);
    // Viewport showing lines 5–8 (entry 2 visible).
    session.set_rendered_scroll_offset(5);
    // Select entry 0 (lines 0–2) which is above viewport.
    session.set_selected_entry_index(0);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then scroll offset moves to show the entry (abs_start = 0).
    assert_eq!(session.scroll_offset(), Some(0));
}

#[rstest::rstest]
fn scroll_to_selected_entry_fits_in_viewport_below() {
    // Given a session where the selected entry is below the viewport.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(3);
    session.set_blank_count(0);
    session.set_last_max_offset(10);
    // Viewport showing lines 0–3 (entry 0 visible).
    session.set_rendered_scroll_offset(0);
    // Select entry 2 (lines 4–6) which is below viewport.
    session.set_selected_entry_index(2);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then scroll offset adjusts: abs_end - viewport_height = 6 - 3 = 3.
    assert_eq!(session.scroll_offset(), Some(3));
}

#[rstest::rstest]
fn scroll_to_selected_entry_already_visible() {
    // Given a session where the selected entry is already visible.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(6);
    session.set_blank_count(0);
    session.set_last_max_offset(10);
    // Viewport showing lines 0–6 (all visible).
    session.set_rendered_scroll_offset(0);
    session.set_selected_entry_index(1);
    session.set_scroll_offset(Some(0));

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then scroll offset is unchanged (entry already visible).
    assert_eq!(session.scroll_offset(), Some(0));
}

#[rstest::rstest]
fn scroll_to_selected_taller_than_viewport_above() {
    // Given an entry taller than the viewport, positioned above the viewport.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.set_entry_line_ranges(vec![(0, 10), (10, 12)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_last_max_offset(20);
    // Viewport showing lines 10–14.
    session.set_rendered_scroll_offset(10);
    session.set_selected_entry_index(0);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then the offset is the entry's end minus the viewport height, because the
    // entry is taller than the viewport: abs_end(10) <= current_offset(10) →
    // new_offset = 10 - 4 = 6.
    assert_eq!(session.scroll_offset(), Some(6));
}

#[rstest::rstest]
fn scroll_to_selected_taller_than_viewport_below() {
    // Given an entry taller than the viewport, positioned below the viewport.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.set_entry_line_ranges(vec![(0, 2), (2, 12)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_last_max_offset(20);
    // Viewport at top.
    session.set_rendered_scroll_offset(0);
    session.set_selected_entry_index(1);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then the offset is left untouched: the entry is taller than the viewport
    // and already overlaps it. abs_start(2) >= current_offset(0)+4 is false, and
    // abs_end(12) <= current_offset(0) is false, so nothing changes.
    assert_eq!(session.scroll_offset(), None);
}

#[rstest::rstest]
fn scroll_to_selected_resets_to_auto_when_at_bottom() {
    // Given a session where scrolling puts us at the bottom.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // lines 0..2
    session.set_entry_line_ranges(vec![(0, 2)]);
    session.set_viewport_height(10);
    session.set_blank_count(0);
    session.set_last_max_offset(2);
    session.set_rendered_scroll_offset(0);
    session.set_selected_entry_index(0);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Then scroll offset resets to auto (None) since clamped >= max_offset.
    assert!(session.scroll_offset().is_none());
}

#[rstest::rstest]
fn visible_entry_range_boundary_at_viewport_edge() {
    // Given entries where one entry's end exactly equals viewport_top.
    // Entry 0: lines 0..3, Entry 1: lines 3..6, Entry 2: lines 6..9
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.set_entry_line_ranges(vec![(0, 3), (3, 6), (6, 9)]);
    session.set_viewport_height(3);
    session.set_blank_count(0);
    // Viewport top = 3, bottom = 6.
    session.set_rendered_scroll_offset(3);

    // When computing visible range.
    let range = session.visible_entry_range();

    // Then entry 0 (lines 0..3) is NOT visible: abs_end(3) > viewport_top(3) is false.
    // Wait - the check is abs_end > viewport_top, and abs_start < viewport_bottom.
    // Entry 0: abs_end=3, viewport_top=3 → 3 > 3 is false → not visible. Correct.
    // Entry 1: abs_start=3, abs_end=6, viewport_top=3, bottom=6 → 6>3 && 3<6 → visible.
    // Entry 2: abs_start=6, abs_end=9 → 9>3 && 6<6 → 6<6 is false → not visible.
    assert_eq!(range, 1..2);
}

#[rstest::rstest]
fn visible_entry_range_includes_entry_at_viewport_bottom_edge() {
    // Given entries where one entry's start exactly equals viewport_bottom.
    // Entry 0: lines 0..2, Entry 1: lines 2..4, Entry 2: lines 4..6
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    // viewport_top=0, viewport_bottom=4.
    session.set_rendered_scroll_offset(0);

    // When computing visible range.
    let range = session.visible_entry_range();

    // Then entries 0 and 1 are visible but entry 2 is not: it starts exactly
    // at the viewport bottom (4<4 is false).
    assert_eq!(range, 0..2);
}

#[rstest::rstest]
fn move_cursor_to_first_visible_skips_empty_assistant() {
    // Given a session where the first visible entry is an empty assistant.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("")); // idx 0, empty
    session.push_entry(ChatEntry::user("hello")); // idx 1
    session.push_entry(ChatEntry::assistant("world")); // idx 2

    // Set viewport to show all.
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(6);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(0);

    // Must set visual items for the skipping logic to work.
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);

    // When moving cursor to first visible.
    session.move_cursor_to_first_visible();

    // Then cursor lands on the user entry (index 1), skipping the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn move_cursor_to_last_visible_skips_empty_assistant() {
    // Given a session where the last visible entry is an empty assistant.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello")); // idx 0
    session.push_entry(ChatEntry::assistant("world")); // idx 1
    session.push_entry(ChatEntry::assistant("")); // idx 2, empty

    // Set viewport to show all.
    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(6);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(0);

    // Must set visual items for the skipping logic to work.
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);

    // When moving cursor to last visible.
    session.move_cursor_to_last_visible();

    // Then cursor lands on the non-empty assistant (index 1), skipping the empty one.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn move_cursor_to_first_visible_all_selectable() {
    // Given a session where all visible entries are selectable.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));

    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(6);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(0);

    // When moving cursor to first visible.
    session.move_cursor_to_first_visible();

    // Then cursor is on the first entry.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn move_cursor_to_last_visible_all_selectable() {
    // Given a session where all visible entries are selectable.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.push_entry(ChatEntry::user("b"));
    session.push_entry(ChatEntry::user("c"));

    session.set_entry_line_ranges(vec![(0, 2), (2, 4), (4, 6)]);
    session.set_viewport_height(6);
    session.set_blank_count(0);
    session.set_rendered_scroll_offset(0);

    // When moving cursor to last visible.
    session.move_cursor_to_last_visible();

    // Then cursor is on the last entry.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn move_cursor_to_first_visible_noop_when_empty_range() {
    // Given a session with entries but no visible range.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    session.clear_selection();

    // No viewport state → empty range.
    // When moving cursor.
    session.move_cursor_to_first_visible();

    // Then selection stays None.
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn select_next_entry_skips_empty_assistant_at_start() {
    // Given a session: [empty-assistant, user, user].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("")); // idx 0
    session.push_entry(ChatEntry::user("hello")); // idx 1
    session.push_entry(ChatEntry::user("world")); // idx 2
    session.clear_selection();

    // When selecting the next entry.
    session.select_next_entry();

    // Then selection is on the user entry (index 1), not the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_next_entry_skips_empty_assistant_in_middle() {
    // Given: [user, empty-assistant, user] with selection starting at index 0.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1
    session.push_entry(ChatEntry::user("b")); // idx 2
    session.set_selected_entry_index(0);

    // When selecting the next entry.
    session.select_next_entry();

    // Then selection jumps to index 2, skipping the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_next_entry_clamps_when_only_empty_assistants() {
    // Given: [empty-assistant, empty-assistant].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1

    // When selecting next from nothing - fallback path skips non-selectable.
    session.clear_selection();
    session.select_next_entry();

    // Then no selection is made (all entries are empty assistants, not selectable).
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn select_prev_entry_skips_empty_assistant_at_end() {
    // Given: [user, user, empty-assistant].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::assistant("")); // idx 2

    // When clearing the selection and selecting prev (starts at last, so the
    // trailing empty assistant is skipped).
    session.clear_selection();
    session.select_prev_entry();

    // Then selection is on the user entry (index 1), not the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_prev_entry_skips_empty_assistant_in_middle() {
    // Given: [user, empty-assistant, user] with selection starting at index 2.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1
    session.push_entry(ChatEntry::user("b")); // idx 2
    session.set_selected_entry_index(2);

    // When selecting the previous entry.
    session.select_prev_entry();

    // Then selection jumps to index 0, skipping the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn select_prev_entry_clamps_when_only_empty_assistants() {
    // Given: [empty-assistant, empty-assistant].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1

    // When selecting prev from nothing - fallback path skips non-selectable.
    session.clear_selection();
    session.select_prev_entry();

    // Then no selection is made (all entries are empty assistants, not selectable).
    assert_eq!(session.selected_entry_index(), None);
}

#[rstest::rstest]
fn pin_entry_scans_backward_to_find_block_start() {
    // Given a contiguous ignored block at indices 1-4, expanded, whose entry at
    // index 3 is about to be pinned. The pin_entry propagation scans backward
    // from the pinned entry to find the block start (the first ignored entry with
    // no pin and with an in-context neighbour).
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before")); // idx 0
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1 - block rep
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3 - pin target
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    session.push_entry(ChatEntry::user("after")); // idx 5
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[3].id.clone();

    // When the entry at idx 3 is pinned to the top.
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the block_start scan found index 1 (the first entry in the contiguous
    // ignored block), so the forward sub-block starting at index 4 is shown.
    let forward_rep = session.history()[4].id.clone();
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep),
        "forward sub-block should be shown - backward scan found correct block start"
    );
}

#[rstest::rstest]
fn pin_entry_block_start_stops_at_non_ignored_boundary() {
    // Given two separate expanded ignored blocks with a non-ignored entry
    // between them, and the entry at idx 4 about to be pinned.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 0
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::user("boundary")); // idx 2 - in-context
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    let rep1 = session.history()[0].id.clone();
    let rep2 = session.history()[3].id.clone();
    session.toggle_ignored_block_visibility(&rep1);
    session.toggle_ignored_block_visibility(&rep2);
    let pin_id = session.history()[4].id.clone();

    // When the entry at idx 4 (in the second block) is pinned to the top.
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the block_start scan stops at idx 3 (it does not cross the non-ignored
    // boundary), and idx 4 is last in its block so no forward sub-block appears.
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep2));
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep1));
}

#[rstest::rstest]
fn pin_entry_forward_start_at_history_end_is_noop() {
    // Given an expanded ignored block whose only entry ends the history.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before")); // idx 0
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[1].id.clone();

    // When the last entry is pinned to the top (no forward sub-block possible).
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then no new forward sub-block is added.
    assert_eq!(session.shown_ignored_blocks_snapshot().len(), 1);
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

#[rstest::rstest]
fn toggle_ignored_block_scans_backward_from_entry() {
    // Given: [user, ignored-A, ignored-B, ignored-C, user].
    // Toggling on ignored-C should find the block start at ignored-A.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before")); // idx 0
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::user("after")); // idx 4

    // When toggling on ignored-C (idx 3).
    let id_c = session.history()[3].id.clone();
    session.toggle_ignored_block_visibility(&id_c);

    // Then the block representative is ignored-A (idx 1), not ignored-C.
    let rep_id = session.history()[1].id.clone();
    assert!(
        session.shown_ignored_blocks_snapshot().contains(&rep_id),
        "block representative should be the first entry in the contiguous block"
    );
}

#[rstest::rstest]
fn toggle_ignored_block_does_not_cross_pinned_entry() {
    // Given: [ignored-A, ignored-B(pinned), ignored-C].
    // Toggling ignored-C should find block start at ignored-C itself,
    // not crossing the pinned entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 0
    session.push_entry(
        ChatEntry::assistant("b")
            .with_ignored(true)
            .with_pin(PinPosition::Top),
    ); // idx 1
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 2

    // When toggling on ignored-C (idx 2).
    let id_c = session.history()[2].id.clone();
    session.toggle_ignored_block_visibility(&id_c);

    // Then block start is idx 2 (didn't cross pinned entry at idx 1).
    assert!(
        session.shown_ignored_blocks_snapshot().contains(&id_c),
        "block representative should be the entry after the pinned boundary"
    );
    // And idx 0 is NOT in shown_ignored_blocks.
    let id_a = session.history()[0].id.clone();
    assert!(!session.shown_ignored_blocks_snapshot().contains(&id_a));
}

#[rstest::rstest]
fn select_next_entry_at_last_index_stays() {
    // Given a session with 3 entries, cursor on last.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::user("c")); // idx 2
    session.set_selected_entry_index(2);

    // When selecting next.
    session.select_next_entry();

    // Then cursor stays at 2.
    assert_eq!(session.selected_entry_index(), Some(2));
}

#[rstest::rstest]
fn select_prev_entry_at_first_index_stays() {
    // Given a session with 3 entries, cursor on first.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::user("b")); // idx 1
    session.push_entry(ChatEntry::user("c")); // idx 2
    session.set_selected_entry_index(0);

    // When selecting prev.
    session.select_prev_entry();

    // Then cursor stays at 0.
    assert_eq!(session.selected_entry_index(), Some(0));
}

#[rstest::rstest]
fn new_with_profile_preserves_profile() {
    // Given a profile with a custom model.
    let profile = SessionProfile {
        model: ModelSelection::Single("ollama/llama3".to_owned()),
        ..SessionProfile::default()
    };

    // When creating a session with that profile.
    let session = ChatSessionState::new_with_profile(profile);

    // Then the session carries the profile.
    assert_eq!(
        session.profile().model,
        ModelSelection::Single("ollama/llama3".to_owned())
    );
}

#[rstest::rstest]
fn profile_mut_returns_mutable_reference() {
    // Given a session.
    let mut session = ChatSessionState::new();

    // When mutating the profile.
    session.profile_mut().model = ModelSelection::Single("openai/gpt-4".to_owned());

    // Then the change is visible via the immutable accessor.
    assert_eq!(
        session.profile().model,
        ModelSelection::Single("openai/gpt-4".to_owned())
    );
}

#[rstest::rstest]
fn restore_token_ledger_sets_records() {
    // Given a session with no token records.
    let mut session = ChatSessionState::new();
    assert!(session.token_ledger().is_empty());

    // When restoring a token ledger.
    let records = vec![TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 100,
        tokens_received: 50,
        cost: Some(0.01),
        prompt_tokens: None,
        cached_tokens: None,
    }];
    session.restore_token_ledger(records);

    // Then the ledger contains the records.
    assert_eq!(session.token_ledger().len(), 1);
    assert_eq!(session.token_ledger()[0].tokens_sent, 100);
}

#[rstest::rstest]
fn finalize_last_token_record_sets_provider_prompt_tokens_without_overwriting_estimate() {
    // Given a session with a pending record carrying a local estimate.
    let mut session = ChatSessionState::new();
    session.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 100,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When finalizing with the provider-reported prompt token count.
    session
        .finalize_last_token_record(50, Some(0.01), None, Some(120), None)
        .expect("finalize");

    // Then the provider count is recorded but the estimate is retained.
    let record = &session.token_ledger()[0];
    assert_eq!(record.tokens_sent, 100, "local estimate must be retained");
    assert_eq!(record.prompt_tokens, Some(120));
}

#[rstest::rstest]
fn finalize_last_token_record_leaves_new_fields_none_on_no_usage() {
    // Given a session with a pending record (simulating a cancelled turn).
    let mut session = ChatSessionState::new();
    session.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 100,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When finalizing without provider usage data (None for both).
    session
        .finalize_last_token_record(0, None, None, None, None)
        .expect("finalize");

    // Then both provider-reported fields stay None.
    let record = &session.token_ledger()[0];
    assert_eq!(record.prompt_tokens, None);
    assert_eq!(record.cached_tokens, None);
}

#[rstest::rstest]
fn finalize_last_token_record_sets_cached_tokens() {
    // Given a session with a pending record.
    let mut session = ChatSessionState::new();
    session.push_token_record(TokenRecord {
        model_used: None,
        timestamp: jiff::Timestamp::now(),
        tokens_sent: 1000,
        tokens_received: 0,
        cost: None,
        prompt_tokens: None,
        cached_tokens: None,
    });

    // When finalizing with a reported cache-hit count.
    session
        .finalize_last_token_record(50, Some(0.01), None, Some(1000), Some(400))
        .expect("finalize");

    // Then cached_tokens is recorded.
    assert_eq!(session.token_ledger()[0].cached_tokens, Some(400));
}

#[rstest::rstest]
fn restore_updated_at_sets_timestamp() {
    // Given a session.
    let mut session = ChatSessionState::new();
    let original = *session.updated_at();

    // When restoring updated_at to a different time.
    let ts = jiff::Timestamp::UNIX_EPOCH;
    session.restore_updated_at(ts);

    // Then updated_at reflects the restored value.
    assert_eq!(*session.updated_at(), ts);
    assert_ne!(*session.updated_at(), original);
}

#[rstest::rstest]
fn restore_created_at_sets_timestamp() {
    // Given a session.
    let mut session = ChatSessionState::new();
    let ts = jiff::Timestamp::UNIX_EPOCH;

    // When restoring created_at.
    session.restore_created_at(ts);

    // Then created_at reflects the restored value.
    assert_eq!(*session.created_at(), ts);
}

#[rstest::rstest]
fn touch_updates_timestamp() {
    // Given a session with a known updated_at.
    let mut session = ChatSessionState::new();
    session.restore_updated_at(jiff::Timestamp::UNIX_EPOCH);
    let before = *session.updated_at();

    // When touching.
    session.touch();

    // Then updated_at is newer than before.
    assert!(*session.updated_at() > before);
}

#[rstest::rstest]
fn blobs_returns_data() {
    // Given a session with a blob entry.
    let mut session = ChatSessionState::new();
    session
        .blobs_mut()
        .insert("key".to_owned(), serde_json::json!({"v": 42}));

    // When the immutable accessor is queried.
    // Then it returns the blob.
    assert!(session.blobs().contains_key("key"));
}

#[rstest::rstest]
fn blobs_mut_allows_modification() {
    // Given a session.
    let mut session = ChatSessionState::new();

    // When inserting via mutable accessor.
    session
        .blobs_mut()
        .insert("k".to_owned(), serde_json::json!(true));

    // Then the immutable accessor sees it.
    assert_eq!(session.blobs().len(), 1);
}

#[rstest::rstest]
fn viewport_height_value_returns_stored_value() {
    // Given a session with viewport height 42 set.
    let session = ChatSessionState::new();
    session.set_viewport_height(42);

    // When viewport_height_value is read.
    // Then it returns 42.
    assert_eq!(session.viewport_height_value(), 42);
}

#[rstest::rstest]
fn selected_visual_item_returns_some_when_selected() {
    // Given a session with entries and visual items.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.push_entry(ChatEntry::assistant("world"));
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);
    session.set_selected_entry_index(0);

    // When calling selected_visual_item.
    let item = session.selected_visual_item();

    // Then it returns Some.
    assert!(item.is_some());
}

#[rstest::rstest]
fn selected_visual_item_returns_none_when_no_selection() {
    // Given a session with entries but no selection.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.clear_selection();

    // When calling selected_visual_item.
    let item = session.selected_visual_item();

    // Then it returns None.
    assert!(item.is_none());
}

#[rstest::rstest]
fn enqueue_front_puts_item_at_front_of_queue() {
    // Given a session with a UserMessage in the queue.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("first"));
    let _first_id = session.history()[0].id.clone();

    // Enqueue a user message normally (back of queue).
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        session.history()[0].clone(),
    )));

    // When enqueuing a ToolContinuation at the front.
    session.enqueue_front(jinn_turn_dispatch_msg::QueueItem::ToolContinuation);

    // Then dequeue returns ToolContinuation first.
    let front = session.dequeue();
    assert!(matches!(
        front,
        Some(jinn_turn_dispatch_msg::QueueItem::ToolContinuation)
    ));
}

#[rstest::rstest]
fn steering_buffer_not_persisted_across_serialization() {
    // Given a session with a non-empty steering buffer.
    let mut session = ChatSessionState::new();
    session
        .ui
        .steering_buffer
        .push_fragment("fragment one".to_owned());
    session
        .ui
        .steering_buffer
        .push_fragment("fragment two".to_owned());

    // When serializing to JSON.
    let json = serde_json::to_string(&session).expect("serialize");

    // Then the serialized output does not contain any steering buffer fields.
    assert!(
        !json.contains("steering_buffer"),
        "steering_buffer must not appear in serialized output: {}",
        json
    );
    assert!(
        !json.contains("fragments"),
        "fragments must not appear in serialized output: {}",
        json
    );

    // And deserializing produces an empty buffer.
    let restored: ChatSessionState = serde_json::from_str(&json).expect("deserialize");
    assert!(
        restored.ui.steering_buffer.is_empty(),
        "deserialized steering buffer must be empty"
    );
}

#[rstest::rstest]
fn discovered_skills_default_empty() {
    // Given a freshly constructed session.
    let session = ChatSessionState::new();

    // When its discovered sets are read.
    // Then the skill, prompt-template, and context-file sets are all empty.
    assert!(session.discovered_skills().is_empty());
    assert!(session.discovered_prompt_templates().is_empty());
    assert!(session.discovered_context_files().is_empty());
}

#[rstest::rstest]
fn discovered_sets_are_independent_between_sessions() {
    // Given two sessions with different discovered skills.
    let mut a = ChatSessionState::new();
    let mut b = ChatSessionState::new();
    a.set_discovered_skills(vec![jinn_skills_msg::Skill {
        name: "session-a-only".into(),
        description: String::new(),
        body: String::new(),
        file_path: PathBuf::new(),
        base_dir: PathBuf::new(),
        source: jinn_skills_msg::SkillSource::Global,
    }]);
    b.set_discovered_skills(vec![jinn_skills_msg::Skill {
        name: "session-b-only".into(),
        description: String::new(),
        body: String::new(),
        file_path: PathBuf::new(),
        base_dir: PathBuf::new(),
        source: jinn_skills_msg::SkillSource::Global,
    }]);

    // When each session's discovered skills are read.
    // Then session A sees only its skill, B sees only its own — no clobbering.
    assert_eq!(a.discovered_skills().len(), 1);
    assert_eq!(a.discovered_skills()[0].name, "session-a-only");
    assert_eq!(b.discovered_skills().len(), 1);
    assert_eq!(b.discovered_skills()[0].name, "session-b-only");
}

#[rstest::rstest]
fn discovered_context_files_round_trip_empty_after_serialization() {
    // Given a session with discovered context files populated in ephemeral state.
    let mut session = ChatSessionState::new();
    session.set_discovered_context_files(vec![jinn_context::ContextFile {
        path: PathBuf::from("/nonexistent/AGENTS.md"),
        content: "should not persist".into(),
    }]);

    // When serialized then deserialized (ephemeral fields are not persisted).
    let json = serde_json::to_string(&session).expect("serialize");
    let restored: ChatSessionState = serde_json::from_str(&json).expect("deserialize");

    // Then the discovered context files are empty after round-trip (transient).
    assert!(
        restored.discovered_context_files().is_empty(),
        "discovered context files must not be persisted"
    );
}

/// Helper: build a pinned skill-shaped ToolResult entry and add it.
fn push_pinned_tool_result(
    session: &mut super::ChatSessionState,
    call_id: &str,
    tool_name: &str,
    content: &str,
    status: ToolResultStatus,
) {
    let mut entry = ChatEntry::tool_result(call_id, tool_name, content, status);
    entry.pin_position = Some(PinPosition::Relative);
    session.push_entry(entry);
}

#[rstest::rstest]
fn loaded_skills_returns_only_valid_pinned_skill_names() {
    // Given a session with: one pinned valid skill, one pinned non-skill tool
    // result, one unpinned skill, and one pinned malformed skill.
    let mut session = super::ChatSessionState::default();

    push_pinned_tool_result(
        &mut session,
        "call-valid",
        "skill",
        "<skill name=\"phased-task-loop\" location=\"/x\">body</skill>",
        ToolResultStatus::Success,
    );
    push_pinned_tool_result(
        &mut session,
        "call-non-skill",
        "read",
        "file contents",
        ToolResultStatus::Success,
    );

    // Unpinned skill: should be ignored.
    let mut unpinned = ChatEntry::tool_result(
        "call-unpinned",
        "skill",
        "<skill name=\"web-coder\" location=\"/x\">body</skill>",
        ToolResultStatus::Success,
    );
    unpinned.pin_position = None;
    session.push_entry(unpinned);

    push_pinned_tool_result(
        &mut session,
        "call-malformed",
        "skill",
        "not a skill xml",
        ToolResultStatus::Success,
    );

    // When computing loaded skills.
    let loaded = super::ChatSessionState::loaded_skills(&session);

    // Then only the one valid pinned skill name is returned.
    assert_eq!(
        loaded,
        std::collections::HashSet::from(["phased-task-loop".to_owned()]),
        "loaded_skills() should return exactly the one valid pinned skill name"
    );
}

use jinn_core_types::HistoryMutation;

/// A session with the whole cell catalog registered and attached, so the
/// chat-input draft has a real cell to live in.
///
/// Tests that write a draft through the facade need this: the draft is a
/// `jinn-chat-input` cell, and an unattached session has nowhere to put one.
fn attached_session() -> ChatSessionState {
    let session = ChatSessionState::new();
    let slices = jinn_slices::Slices::new();
    jinn_cell_catalog::register_all_cells(&slices);
    session.attach_slices(slices);
    session
}

fn session_with_excluded_entry(
    source: jinn_core_types::ChangeSource,
) -> (super::ChatSessionState, ChatEntryId) {
    let mut entry = ChatEntry::assistant("excluded");
    let id = entry.id.clone();
    entry.apply_context_override(ContextOverride::ForcedExclude, source);
    let session = super::ChatSessionState::builder().with_entry(entry).build();
    (session, id)
}

fn session_with_toggled_back_entry() -> (super::ChatSessionState, ChatEntryId) {
    let mut entry = ChatEntry::assistant("toggled back");
    let id = entry.id.clone();
    entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
    entry.apply_context_override(ContextOverride::Default, ChangeSource::User);
    let session = super::ChatSessionState::builder().with_entry(entry).build();
    (session, id)
}

fn session_with_included_entry() -> (super::ChatSessionState, ChatEntryId) {
    let mut entry = ChatEntry::assistant("included");
    let id = entry.id.clone();
    entry.apply_context_override(ContextOverride::ForcedInclude, ChangeSource::User);
    let session = super::ChatSessionState::builder().with_entry(entry).build();
    (session, id)
}

#[rstest::rstest]
#[test]
fn apply_mutations_worker_forced_include_blocked_on_user_force_excluded() {
    // Given an entry with ForcedExclude set by User.
    let (mut session, entry_id) = session_with_excluded_entry(ChangeSource::User);

    // When apply_mutations receives ForcedInclude from a Worker.
    let changed = session.apply_mutations(vec![HistoryMutation::SetContextOverride {
        entry_id: entry_id.clone(),
        value: ContextOverride::ForcedInclude,
        source: ChangeSource::Worker {
            name: "auto-prune-todo".into(),
        },
    }]);

    // Then the mutation is blocked; entry stays ForcedExclude.
    assert!(changed.is_empty(), "mutation should be blocked");
    let entry = session
        .history()
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry");
    assert_eq!(
        entry.context_override(),
        ContextOverride::ForcedExclude,
        "entry should remain ForcedExclude"
    );
}

#[rstest::rstest]
#[test]
fn apply_mutations_worker_forced_include_allowed_on_worker_force_excluded() {
    // Given an entry with ForcedExclude set by Worker.
    let (mut session, entry_id) = session_with_excluded_entry(ChangeSource::Worker {
        name: "other-worker".into(),
    });

    // When apply_mutations receives ForcedInclude from a Worker.
    let changed = session.apply_mutations(vec![HistoryMutation::SetContextOverride {
        entry_id: entry_id.clone(),
        value: ContextOverride::ForcedInclude,
        source: ChangeSource::Worker {
            name: "auto-prune-todo".into(),
        },
    }]);

    // Then the mutation is applied; entry becomes ForcedInclude.
    assert_eq!(changed.len(), 1, "mutation should be applied");
    let entry = session
        .history()
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry");
    assert_eq!(
        entry.context_override(),
        ContextOverride::ForcedInclude,
        "entry should be upgraded to ForcedInclude"
    );
}

#[rstest::rstest]
#[test]
fn apply_mutations_internal_forced_include_bypasses_user_guard() {
    // Given an entry with ForcedExclude set by User.
    let (mut session, entry_id) = session_with_excluded_entry(ChangeSource::User);

    // When apply_mutations receives ForcedInclude from Internal.
    let changed = session.apply_mutations(vec![HistoryMutation::SetContextOverride {
        entry_id: entry_id.clone(),
        value: ContextOverride::ForcedInclude,
        source: ChangeSource::Internal {
            label: "dangling-tool-call-sweep".into(),
        },
    }]);

    // Then the mutation is applied; entry becomes ForcedInclude.
    assert_eq!(changed.len(), 1, "internal mutation should bypass guard");
    let entry = session
        .history()
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry");
    assert_eq!(
        entry.context_override(),
        ContextOverride::ForcedInclude,
        "internal sweep should override user exclusion"
    );
}

#[rstest::rstest]
#[test]
fn apply_mutations_user_toggled_back_to_default_allows_worker_re_include() {
    // Given an entry whose most recent audit event is User → Default
    // (was previously ForcedExclude by user, then toggled back).
    let (mut session, entry_id) = session_with_toggled_back_entry();

    // When apply_mutations receives ForcedInclude from a Worker.
    let changed = session.apply_mutations(vec![HistoryMutation::SetContextOverride {
        entry_id: entry_id.clone(),
        value: ContextOverride::ForcedInclude,
        source: ChangeSource::Worker {
            name: "auto-prune-todo".into(),
        },
    }]);

    // Then the mutation is applied; entry becomes ForcedInclude.
    assert_eq!(
        changed.len(),
        1,
        "mutation should be applied after toggle-back"
    );
    let entry = session
        .history()
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry");
    assert_eq!(
        entry.context_override(),
        ContextOverride::ForcedInclude,
        "entry should be re-includable after user toggled back"
    );
}

#[rstest::rstest]
#[test]
fn apply_mutations_existing_forced_include_guard_still_works() {
    // Given an entry at ForcedInclude.
    let (mut session, entry_id) = session_with_included_entry();

    // When apply_mutations receives ForcedExclude from a Worker.
    let changed = session.apply_mutations(vec![HistoryMutation::SetContextOverride {
        entry_id: entry_id.clone(),
        value: ContextOverride::ForcedExclude,
        source: ChangeSource::Worker {
            name: "some-worker".into(),
        },
    }]);

    // Then the mutation is blocked (existing guard).
    assert!(
        changed.is_empty(),
        "existing guard should block ForcedExclude on ForcedInclude"
    );
    let entry = session
        .history()
        .iter()
        .find(|e| e.id == entry_id)
        .expect("entry");
    assert_eq!(
        entry.context_override(),
        ContextOverride::ForcedInclude,
        "entry should remain ForcedInclude"
    );
}

// ─── EntryTiming integration tests ────────────────────────────────
// ─── EntryTiming integration tests ────────────────────────────────

fn streaming_session() -> ChatSessionState {
    let mut session = attached_session();
    session.begin_streaming();
    session
}

fn dispatched_at() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

#[rstest::rstest]
#[test]
fn streamed_entry_begins_with_dispatched_at_only() {
    // Given a session in streaming phase.
    let mut session = streaming_session();
    let da = dispatched_at();

    // When starting a thinking entry.
    session.begin_thinking(da);

    // Then the entry has dispatched_at set and first_token_at is Some.
    let entry = session.history().last().expect("entry exists");
    match &entry.timing {
        EntryTiming::Streamed {
            dispatched_at: d,
            first_token_at: Some(ft),
            finished_at: None,
            ..
        } => {
            assert_eq!(*d, da, "dispatched_at should match");
            assert!(*ft >= da, "first_token_at should be >= dispatched_at");
        }
        other => {
            panic!("expected Streamed with first_token_at=Some, finished_at=None, got {other:?}")
        }
    }
}

#[rstest::rstest]
#[test]
fn streamed_entry_gets_first_token_at_on_creation() {
    // Given a session in streaming phase.
    let mut session = streaming_session();
    let da = dispatched_at();

    // When appending a stream token (lazily creates assistant entry).
    let _ = session.append_stream_token("Hello", da);

    // Then the assistant entry has first_token_at set.
    let entry = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
        .expect("assistant entry");
    match &entry.timing {
        EntryTiming::Streamed {
            dispatched_at: d,
            first_token_at: Some(ft),
            finished_at: None,
            ..
        } => {
            assert_eq!(*d, da);
            assert!(*ft >= da);
        }
        other => panic!("expected Streamed with first_token_at=Some, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn streamed_entry_gets_finished_at_on_stream_complete() {
    // Given a session in streaming phase with an assistant entry.
    let mut session = streaming_session();
    let da = dispatched_at();
    let _ = session.append_stream_token("Hello", da);

    // When finishing the stream.
    session.finish_streaming_entry(
        session
            .history()
            .iter()
            .rposition(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
            .expect("assistant index"),
    );

    // Then the assistant entry has finished_at set.
    let entry = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
        .expect("assistant entry");
    match &entry.timing {
        EntryTiming::Streamed {
            dispatched_at: d,
            first_token_at: Some(ft),
            finished_at: Some(fin),
            ..
        } => {
            assert_eq!(*d, da);
            assert!(*ft >= da);
            assert!(*fin >= *ft);
        }
        other => panic!("expected Streamed with all timestamps set, got {other:?}"),
    }
}

#[rstest::rstest]
#[test]
fn tool_call_entry_gets_dispatched_at_from_tool_use_started() {
    // Given a session in streaming phase.
    let mut session = streaming_session();
    let da = dispatched_at();

    // When beginning a tool call.
    session.begin_tool_call(0, "call_1", "echo", da);

    // Then the tool call entry has dispatched_at from the event.
    let entry = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::ToolCall { .. }))
        .expect("tool call entry");
    match &entry.timing {
        EntryTiming::Streamed {
            dispatched_at: d,
            first_token_at: Some(_),
            finished_at: None,
            ..
        } => {
            assert_eq!(*d, da, "dispatched_at should come from ToolUseStarted");
        }
        other => panic!("expected Streamed, got {other:?}"),
    }
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_keeps_partial_entry_out_of_context() {
    // Given a streaming session with a partial assistant entry.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    let history_len_before = session.history().len();
    assert!(history_len_before >= 1, "streaming entry should exist");

    // When resetting streaming entries for retry.
    let excluded = session.reset_streaming_entries_for_retry();

    // Then the partial entry is still in history — the user must be able to
    // see the attempt that was discarded.
    assert_eq!(
        session.history().len(),
        history_len_before,
        "the partial entry must survive the reset"
    );
    // And it is excluded from context so the retried prompt is valid.
    assert_eq!(excluded.len(), 1, "exactly one entry was excluded");
    let partial = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
        .expect("partial assistant entry");
    assert_eq!(partial.context_override(), ContextOverride::ForcedExclude);
    assert!(
        !partial.is_in_context(),
        "the discarded attempt must not reach the provider"
    );
    assert_eq!(
        session.streaming_thinking_entry_index(),
        None,
        "streaming indices cleared",
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_keeps_a_discarded_attempt_from_collapsing_away() {
    // Given a streaming session whose stalled attempt produced an assistant
    // entry, a thinking entry, and a tool call — three entries, which is
    // exactly the block size the chat log collapses by default.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    session.begin_thinking(dispatched_at());
    session
        .append_thinking_token("thinking")
        .expect("append thinking token");
    session.finish_thinking_entry(session.streaming_thinking_entry_index().unwrap());
    let tool_call_history_len = session.history().len();
    session.begin_tool_call(tool_call_history_len, "call_1", "read", dispatched_at());
    let excluded_ids = session
        .reset_streaming_entries_for_retry()
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    assert!(
        excluded_ids.len() >= 3,
        "expected assistant+thinking+tool call to be excluded, got {}",
        excluded_ids.len()
    );

    // When the retried generation appends enough entries to push the stalled
    // attempt out of the proximity window (3) at the tail.
    for n in 0..8 {
        session.push_entry(ChatEntry::assistant(format!("retry{n}")));
    }

    // Then building visual items does NOT collapse the discarded attempt into
    // a single "N hidden entries" line — the user's evidence stays readable.
    let items = jinn_chat_log_view_msg::build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );

    for id in &excluded_ids {
        let entry_index = session
            .history()
            .iter()
            .position(|e| &e.id == id)
            .expect("excluded entry is still in history");
        let rendered_individually = items.contains(&VisualItem::Entry(entry_index));
        assert!(
            rendered_individually,
            "the discarded attempt must stay visible on screen, not collapse into a \
             hidden-entries block; entry at history index {entry_index} was swallowed \
             by {items:?}"
        );
    }
    assert!(
        !items
            .iter()
            .any(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. })),
        "no ignored block may be collapsed, since every excluded entry belongs to \
         the discarded attempt that must stay visible; got {items:?}"
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_block_can_still_be_collapsed_by_the_user() {
    // Given a discarded attempt registered as a shown (expanded) block.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    session.begin_thinking(dispatched_at());
    session
        .append_thinking_token("thinking")
        .expect("append thinking token");
    session.finish_thinking_entry(session.streaming_thinking_entry_index().unwrap());
    let tool_call_history_len = session.history().len();
    session.begin_tool_call(tool_call_history_len, "call_1", "read", dispatched_at());
    let excluded_ids = session.reset_streaming_entries_for_retry();
    for n in 0..8 {
        session.push_entry(ChatEntry::assistant(format!("retry{n}")));
    }

    // When the user toggles the block shut.
    let last_excluded = excluded_ids
        .last()
        .expect("at least one excluded entry")
        .clone();
    session.toggle_ignored_block_visibility(&last_excluded);

    // Then it collapses like any other ignored block — the stall path set a
    // default-expanded state, it did not lock the block open.
    let items = jinn_chat_log_view_msg::build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    assert!(
        items
            .iter()
            .any(|i| matches!(i, VisualItem::CollapsedIgnoredBlock { .. })),
        "toggling the discarded attempt's block must collapse it, got {items:?}"
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_keeps_partial_assistant_visible() {
    // Given a streaming session with a partial assistant entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.begin_streaming();
    session
        .append_stream_token("partial", jiff::Timestamp::now())
        .expect("append token");
    assert_eq!(session.history().len(), 2, "user + partial assistant");

    // When resetting streaming entries for retry.
    session.reset_streaming_entries_for_retry();

    // Then the partial assistant entry is still rendered in the chat log.
    let partial = session
        .history()
        .iter()
        .find(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
        .expect("partial assistant entry survives for display");
    let ChatEntryKind::Assistant(text) = &partial.kind else {
        panic!("expected an assistant entry");
    };
    assert_eq!(text, "partial", "the streamed text must be readable");
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_excludes_partial_thinking_entry() {
    // Given a streaming session with committed user + assistant entries
    // and a partial thinking entry (e.g. a stalled reasoning stream).
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    // First token creates the committed streaming assistant entry.
    session
        .append_stream_token("partial", jiff::Timestamp::now())
        .expect("append token");
    session.begin_thinking(jiff::Timestamp::now());
    session
        .append_thinking_token("partial reasoning")
        .expect("append thinking token");
    let history_len_before = session.history().len();
    assert_eq!(history_len_before, 3, "user + assistant + thinking");

    // When resetting streaming entries for retry.
    let excluded = session.reset_streaming_entries_for_retry();

    // Then nothing was deleted — the user can still see what was discarded.
    assert_eq!(
        session.history().len(),
        history_len_before,
        "partial assistant and thinking entries must survive the reset"
    );
    // And every one of them is out of context, so the retried request does
    // not carry a half-finished turn.
    assert_eq!(excluded.len(), 2, "both streaming entries were excluded");
    for entry in session.history().iter().skip(1) {
        assert_eq!(
            entry.context_override(),
            ContextOverride::ForcedExclude,
            "expected ForcedExclude for {:?}",
            entry.kind
        );
        assert!(!entry.is_in_context());
    }
    assert_eq!(
        session.history()[0].context_override(),
        ContextOverride::Default,
        "the committed user entry stays in context"
    );
    assert_eq!(
        session.streaming_thinking_entry_index(),
        None,
        "thinking streaming index cleared"
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_excludes_a_partial_tool_call() {
    // Given a stream that built a tool call before dying — the tool call is
    // in flight, so there is no result to match it.
    let mut session = streaming_session();
    session.begin_tool_call(0, "call_1", "read", dispatched_at());
    assert_eq!(session.history().len(), 2, "assistant + tool call");

    // When resetting streaming entries for retry.
    let excluded = session.reset_streaming_entries_for_retry();

    // Then both entries stay visible but leave the context, because a
    // `tool_calls` block with no matching result is rejected by providers.
    assert_eq!(session.history().len(), 2, "the tool call stays visible");
    assert_eq!(excluded.len(), 2);
    for entry in session.history() {
        assert_eq!(entry.context_override(), ContextOverride::ForcedExclude);
        assert!(!entry.is_in_context());
    }
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_is_idempotent() {
    // Given a stalled stream that was already reset once.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    assert_eq!(session.reset_streaming_entries_for_retry().len(), 1);

    // When resetting again.
    let second = session.reset_streaming_entries_for_retry();

    // Then nothing changed — no entry reports a second override change.
    assert!(
        second.is_empty(),
        "the reset is a no-op when nothing is streaming"
    );
}

#[rstest::rstest]
fn rewind_for_retry_returns_the_session_to_sending() {
    // Given a stalled stream with a partial assistant entry.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    assert_eq!(session.phase(), PhaseKind::Streaming);

    // When rewinding for the retry.
    session.rewind_for_retry();

    // Then the session is back in Sending, ready for the retried dispatch's
    // first token instead of wedged in Streaming.
    assert_eq!(session.phase(), PhaseKind::Sending);
}

#[rstest::rstest]
fn begin_streaming_after_rewind_for_retry_streams_cleanly() {
    // Given a session rewound from a stalled stream.
    let mut session = streaming_session();
    session.rewind_for_retry();

    // When the retried stream begins.
    session.begin_streaming();

    // Then it is streaming with a fresh assistant entry rather than logging
    // a rejected transition.
    assert_eq!(session.phase(), PhaseKind::Streaming);
    session
        .append_stream_token("Fresh", dispatched_at())
        .expect("append token after rewind");
    let assistants: Vec<_> = session
        .history()
        .iter()
        .filter(|e| matches!(e.kind, ChatEntryKind::Assistant(_)))
        .collect();
    assert_eq!(assistants.len(), 1, "only the retried entry was written");
}

#[rstest::rstest]
fn rewind_for_retry_from_idle_leaves_the_session_untouched() {
    // Given a session that never dispatched.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

    // When rewinding for a retry.
    session.rewind_for_retry();

    // Then nothing changed — the invalid transition is warned about, not applied.
    assert_eq!(session.phase(), PhaseKind::Idle);
    assert_eq!(session.history().len(), 1);
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_leaves_committed_history_in_context() {
    // Given a session with a completed turn followed by a stalled one.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("first"));
    session.push_entry(ChatEntry::assistant("done"));
    session.begin_streaming();
    session
        .append_stream_token("stalled", jiff::Timestamp::now())
        .expect("append token");

    // When resetting streaming entries for retry.
    session.reset_streaming_entries_for_retry();

    // Then the completed turn is untouched and still in context.
    for entry in session.history().iter().take(2) {
        assert_eq!(entry.context_override(), ContextOverride::Default);
        assert!(entry.is_in_context());
    }
}

// ---------------------------------------------------------------------------
// MCP server enablement
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn new_session_has_no_mcp_servers_enabled() {
    // Given a freshly created session.
    let session = ChatSessionState::new();

    // When its enabled MCP servers are read.
    // Then none are enabled by default.
    assert!(session.enabled_mcp_servers().is_empty());
}

#[rstest::rstest]
#[test]
fn enabling_mcp_server_adds_it_to_the_set() {
    // Given a session with no servers enabled.
    let mut session = ChatSessionState::new();

    // When enabling a server.
    let changed = session.enable_mcp_server("excalimate");

    // Then the server is now enabled and the call reported a change.
    assert!(changed, "enable should report a change when newly enabled");
    assert!(session.is_mcp_server_enabled("excalimate"));
}

#[rstest::rstest]
#[test]
fn enabling_already_enabled_mcp_server_reports_no_change() {
    // Given a session with a server already enabled.
    let mut session = ChatSessionState::new();
    session.enable_mcp_server("excalimate");

    // When enabling it again.
    let changed = session.enable_mcp_server("excalimate");

    // Then no change is reported.
    assert!(!changed);
}

#[rstest::rstest]
#[test]
fn disabling_mcp_server_removes_it_from_the_set() {
    // Given a session with a server enabled.
    let mut session = ChatSessionState::new();
    session.enable_mcp_server("excalimate");

    // When disabling it.
    let changed = session.disable_mcp_server("excalimate");

    // Then the server is no longer enabled and the call reported a change.
    assert!(changed);
    assert!(!session.is_mcp_server_enabled("excalimate"));
}

#[rstest::rstest]
#[test]
fn disabling_unknown_mcp_server_reports_no_change() {
    // Given a session with no servers enabled.
    let mut session = ChatSessionState::new();

    // When disabling a never-enabled server.
    let changed = session.disable_mcp_server("excalimate");

    // Then no change is reported.
    assert!(!changed);
}

#[rstest::rstest]
#[test]
fn set_enabled_mcp_servers_replaces_the_set() {
    // Given a session with one server enabled.
    let mut session = ChatSessionState::new();
    session.enable_mcp_server("excalimate");

    // When replacing with a different set.
    let mut new_set = std::collections::BTreeSet::new();
    new_set.insert("filesystem".to_owned());
    session.set_enabled_mcp_servers(new_set);

    // Then only the new server is enabled.
    assert!(!session.is_mcp_server_enabled("excalimate"));
    assert!(session.is_mcp_server_enabled("filesystem"));
}

#[rstest::rstest]
fn set_model_to_alloy_stores_the_alloy_selection() {
    // Given a session on a single model.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single(
        "openrouter/anthropic/claude".to_owned(),
    ));

    // When switching the model to an alloy.
    session.set_model(ModelSelection::Alloy {
        models: vec!["openrouter/anthropic/claude".to_owned()],
        strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 },
    });

    // Then the alloy is the session's selection, with its rotation index
    // untouched by the switch.
    match &session.profile().model {
        ModelSelection::Alloy { models, strategy } => {
            assert_eq!(models, &vec!["openrouter/anthropic/claude".to_owned()]);
            assert_eq!(
                strategy,
                &jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 }
            );
        }
        ModelSelection::Single(_) => panic!("set_model must store the alloy selection"),
    }
}

#[rstest::rstest]
#[test]
fn default_origin_is_user() {
    // Given a newly created session.
    let session = ChatSessionState::new();

    // When its origin is read.
    // Then its origin is User.
    assert_eq!(session.origin(), SessionOrigin::User);
}

#[rstest::rstest]
#[test]
fn new_child_origin_is_subagent() {
    // Given an existing parent session.
    let parent = ChatSessionState::new();

    // When creating a child via the task-tool constructor.
    let child = ChatSessionState::new_child(&parent.session_id().clone(), true);

    // Then the child's origin is Subagent.
    // And the parent link is still set.
    assert_eq!(child.origin(), SessionOrigin::Subagent);
    assert!(child.parent_session().is_some());
}

#[rstest::rstest]
#[case(true)]
#[case(false)]
fn new_child_preserves_persistence_argument(#[case] persist: bool) {
    // Given a parent session ID.

    // When creating a child with the requested persistence policy.
    let child = ChatSessionState::new_child(&SessionId::new(), persist);

    // Then the child preserves that exact policy.
    assert_eq!(child.persist(), persist);
}

#[rstest::rstest]
fn view_writes_roundtrip_without_the_cell() {
    // Given an unattached session (no slice registry handle) with one entry.
    let mut session = ChatSessionState::builder().with_user_entry("hello").build();
    let entry_id = session.history()[0].id.clone();

    // When performing view mutations through the facade.
    session.set_scroll_offset(Some(42));
    session.set_selected_cursor_id(entry_id.clone());

    // Then writes land on the in-struct fallback and reads round-trip.
    assert_eq!(session.scroll_offset(), Some(42));
    assert_eq!(session.selected_cursor_id(), Some(entry_id));
}

#[rstest::rstest]
fn view_reads_default_without_the_cell() {
    // Given an unattached session with no writes.
    let session = ChatSessionState::new();

    // When reading view fields through the facade.
    let scroll_offset = session.scroll_offset();

    // Then every view field reads as its default.
    assert_eq!(scroll_offset, None);
    assert!(!session.has_saved_history_position());
    assert!(session.visual_items_snapshot().is_empty());
}

#[rstest::rstest]
fn attached_view_writes_land_in_the_cell() {
    // Given a session attached to a registry holding the chat-log-view cell.
    let slices = jinn_slices::Slices::new();
    slices
        .register(
            jinn_chat_log_view_msg::chat_log_views_slot(),
            jinn_chat_log_view_msg::ChatLogViews::new(),
        )
        .expect("fresh registry");
    let mut session = ChatSessionState::new();
    session.attach_slices(slices.clone());

    // When writing a scroll offset through the facade.
    session.set_scroll_offset(Some(7));

    // Then the write lands in the cell, keyed by this session's id.
    let cell = slices
        .reader::<jinn_chat_log_view_msg::ChatLogViews>(
            &jinn_chat_log_view_msg::chat_log_views_slot(),
        )
        .expect("cell");
    let stored = cell.read().get(session.session_id()).cloned();
    assert_eq!(
        stored.and_then(|v| v.scroll_offset),
        Some(7),
        "attached writes must resolve the cell, not the fallback"
    );
}

#[rstest::rstest]
fn input_writes_roundtrip_through_the_cell() {
    // Given a session wired like production, with the chat-input cell.
    let session = attached_session();

    // When performing input mutations through the facade.
    session.update_input(|i| {
        i.insert_text("draft text");
        i.move_cursor_to_start();
    });

    // Then the writes land in the cell and reads round-trip.
    assert_eq!(
        session.with_input(|i| i.text().to_owned(), String::new),
        "draft text"
    );
    assert_eq!(
        session.with_input(
            jinn_chat_input_msg::ChatInputBoxState::cursor_pos,
            Default::default
        ),
        0
    );
}

#[rstest::rstest]
fn input_writes_are_a_no_op_without_the_cell() {
    // Given an unattached session (no slice registry handle).
    let session = ChatSessionState::new();

    // When performing input mutations through the facade.
    session.update_input(|i| i.insert_text("draft text"));

    // Then there is no draft anywhere: the cell is the only storage, so an
    // absent one means the write had nothing to land in.
    assert_eq!(
        session.with_input(|i| i.text().to_owned(), String::new),
        String::new()
    );
}

#[rstest::rstest]
fn input_reads_default_without_the_cell() {
    // Given an unattached session with no writes.
    let session = ChatSessionState::new();

    // When reading input fields through the facade.
    // Then the draft reads as its default.
    assert_eq!(session.with_input(|i| i.text().to_owned(), String::new), "");
    assert!(session.with_input(jinn_chat_input_msg::ChatInputBoxState::is_empty, || true));
}

#[rstest::rstest]
fn attached_input_writes_land_in_the_cell() {
    // Given a session attached to a registry holding the chat-input cell.
    let slices = jinn_slices::Slices::new();
    slices
        .register(
            jinn_chat_input_msg::chat_inputs_slot(),
            jinn_chat_input_msg::ChatInputs::new(),
        )
        .expect("fresh registry");
    let session = ChatSessionState::new();
    session.attach_slices(slices.clone());

    // When writing a draft through the facade.
    session.update_input(|i| i.insert_text("attached draft"));

    // Then the write lands in the cell, keyed by this session's id.
    let cell = slices
        .reader::<jinn_chat_input_msg::ChatInputs>(&jinn_chat_input_msg::chat_inputs_slot())
        .expect("cell");
    let stored = cell.read().get(session.session_id()).cloned();
    assert_eq!(
        stored.map(|i| i.text().to_owned()),
        Some("attached draft".to_owned()),
        "attached writes must resolve the cell, not the fallback"
    );
    // And a read through the facade resolves the same entry.
    assert_eq!(
        session.with_input(|i| i.text().to_owned(), String::new),
        "attached draft"
    );
}

#[rstest::rstest]
fn attached_input_reads_do_not_grow_the_cell() {
    // Given an attached registry with no entry for this session.
    let slices = jinn_slices::Slices::new();
    slices
        .register(
            jinn_chat_input_msg::chat_inputs_slot(),
            jinn_chat_input_msg::ChatInputs::new(),
        )
        .expect("fresh registry");
    let session = ChatSessionState::new();
    session.attach_slices(slices.clone());

    // When reading through the facade.
    let text = session.with_input(|i| i.text().to_owned(), String::new);

    // Then the read yields the default without growing the map.
    assert_eq!(text, "");
    let cell = slices
        .reader::<jinn_chat_input_msg::ChatInputs>(&jinn_chat_input_msg::chat_inputs_slot())
        .expect("cell");
    assert!(
        !cell.read().contains_key(session.session_id()),
        "reads must not insert map entries"
    );
}
#[rstest::rstest]
#[test]
fn worker_include_on_todo_tool_call_covers_whole_tool_loop() {
    // Given a session with a todo tool loop (Assistant + ToolCall + ToolResult).
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("check tasks"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "todo_get_task_list", "{}"));
    session.push_entry(ChatEntry::tool_result(
        "tc-1",
        "todo_get_task_list",
        "task list",
        jinn_core_types::ToolResultStatus::Success,
    ));
    let call_id = session.history()[2].id.clone();

    // When a worker force-includes the ToolCall entry.
    let changed = session.edit_history().set_context(
        &call_id,
        ContextOverride::ForcedInclude,
        &ChangeSource::Worker {
            name: "auto-prune-todo".to_owned(),
        },
    );

    // Then the whole loop is covered: the Assistant, the ToolCall, and its
    // ToolResult all become ForcedInclude.
    let history = session.history();
    assert_eq!(changed.len(), 3, "the whole tool loop chunk must change");
    assert_eq!(
        history[1].context_override(),
        ContextOverride::ForcedInclude
    );
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedInclude
    );
    assert_eq!(
        history[3].context_override(),
        ContextOverride::ForcedInclude
    );
}

#[rstest::rstest]
#[test]
fn tool_age_window_exclude_refused_on_included_todo_pair() {
    // Given a todo tool loop already force-included by the todo worker.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("check tasks"));
    session.push_entry(ChatEntry::assistant(""));
    session.push_entry(ChatEntry::tool_call("tc-1", "todo_get_task_list", "{}"));
    session.push_entry(ChatEntry::tool_result(
        "tc-1",
        "todo_get_task_list",
        "task list",
        jinn_core_types::ToolResultStatus::Success,
    ));
    let call_id = session.history()[2].id.clone();
    session.edit_history().set_context(
        &call_id,
        ContextOverride::ForcedInclude,
        &ChangeSource::Worker {
            name: "auto-prune-todo".to_owned(),
        },
    );

    // When another worker (tool_age_window) tries to exclude the pair.
    let changed = session.edit_history().set_context(
        &call_id,
        ContextOverride::ForcedExclude,
        &ChangeSource::Worker {
            name: "auto-prune-tool-age-window".to_owned(),
        },
    );

    // Then the exclusion is refused — the include is sticky, so the current
    // task list cannot be dropped by another pruner.
    assert!(changed.is_empty(), "worker exclude must be refused");
    let history = session.history();
    assert_eq!(
        history[2].context_override(),
        ContextOverride::ForcedInclude
    );
    assert_eq!(
        history[3].context_override(),
        ContextOverride::ForcedInclude
    );
}

#[rstest::rstest]
#[test]
fn new_attendant_links_parent_without_inheriting_conversation() {
    // Given a parent session with a project stamp, cwd, home, and MCP servers.
    let mut parent = ChatSessionState::new();
    parent.set_project(Some(PathBuf::from("/tmp/demo-project")));
    parent.set_cwd(PathBuf::from("/tmp/demo-cwd"));
    parent.set_home(PathBuf::from("/tmp/demo-home"));
    parent.set_enabled_mcp_servers(std::collections::BTreeSet::from(["filesystem".to_owned()]));
    parent.push_entry(ChatEntry::user("parent conversation"));

    // When creating an attendant of that parent.
    let attendant = ChatSessionState::new_attendant(&parent, true);

    // Then the attendant references the parent.
    // And it is an attendant by origin.
    // And its history is empty — environment is inherited, never conversation.
    assert_eq!(
        attendant.parent_session().as_ref(),
        Some(parent.session_id())
    );
    assert!(attendant.is_attendant());
    assert!(attendant.is_empty());
}

#[rstest::rstest]
#[test]
fn new_attendant_copies_parent_environment() {
    // Given a parent session with environment values set.
    let mut parent = ChatSessionState::new();
    parent.set_project(Some(PathBuf::from("/tmp/demo-project")));
    parent.set_cwd(PathBuf::from("/tmp/demo-cwd"));
    parent.set_home(PathBuf::from("/tmp/demo-home"));
    parent.set_enabled_mcp_servers(std::collections::BTreeSet::from(["filesystem".to_owned()]));

    // When creating an attendant of that parent.
    let attendant = ChatSessionState::new_attendant(&parent, true);

    // Then every environment field matches the parent.
    assert_eq!(
        attendant.project(),
        parent.project(),
        "project association follows the parent"
    );
    assert_eq!(attendant.cwd(), parent.cwd(), "cwd follows the parent");
    assert_eq!(
        attendant.enabled_mcp_servers(),
        parent.enabled_mcp_servers(),
        "MCP enablement follows the parent"
    );
    assert_eq!(
        attendant.profile().persona_name,
        parent.profile().persona_name,
        "persona follows the parent"
    );
}

#[rstest::rstest]
#[test]
fn new_attendant_starts_composing_with_a_manual_trigger() {
    // Given a parent session.

    // When creating an attendant of that parent.
    let attendant = ChatSessionState::new_attendant(&parent_of_new_session(), true);

    // Then it is in prep mode — the user is still composing its
    // instructions, so nothing may run.
    assert!(attendant.attendant_is_prepping());
    // And the trigger is manual — it fires for no one until configured.
    assert_eq!(
        attendant.attendant_trigger(),
        jinn_attendant_msg::AttendantTrigger::Manual
    );
}

#[rstest::rstest]
#[test]
fn attendant_run_settings_start_at_their_defaults() {
    // Given a fresh attendant.

    // When reading the two settings a run would obey.
    let attendant = ChatSessionState::new_attendant(&parent_of_new_session(), true);
    let behavior = attendant.attendant_behavior();
    let trigger = attendant.attendant_trigger();

    // Then they are the defaults, independently of the prep mode above: a
    // composing attendant still holds a run configuration, it simply does
    // not apply yet.
    assert_eq!(behavior, jinn_attendant_msg::AttendantBehavior::Reset);
    assert_eq!(trigger, jinn_attendant_msg::AttendantTrigger::Manual);
}

#[rstest::rstest]
#[test]
fn a_prepping_attendant_answers_the_trigger_question_still() {
    // Given a fresh attendant, whose trigger is manual.

    // When asking whether it fires on its parent's completion.
    let fires = ChatSessionState::new_attendant(&parent_of_new_session(), true)
        .attendant_fires_on_parent_completion();

    // Then it does not. The trigger is a fact about the configuration; prep
    // mode is a separate fact about whether the attendant may run, and
    // conflating them would make the sidebar unable to mark either.
    assert!(!fires);
}

fn parent_of_new_session() -> ChatSessionState {
    ChatSessionState::new()
}

#[rstest::rstest]
#[test]
fn new_attendant_preserves_persistence_argument() {
    // Given a parent session and an explicit persistence policy.

    // When creating an attendant with that policy.
    let attendant = ChatSessionState::new_attendant(&ChatSessionState::new(), false);

    // Then the attendant preserves that exact policy.
    assert!(!attendant.persist());
}

#[rstest::rstest]
#[test]
fn attendant_reports_append_in_run_order() {
    // Given a fresh attendant.
    let mut session = ChatSessionState::new_attendant(&ChatSessionState::new(), true);

    // When the attendant publishes two reports.
    session.append_attendant_report("first finding".to_owned());
    session.append_attendant_report("second finding".to_owned());

    // Then the log holds both, oldest first, with run numbers from one.
    let reports = session.attendant_reports();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].run, 1);
    assert_eq!(reports[1].run, 2);
    assert_eq!(reports[0].body, "first finding");
    assert_eq!(reports[1].body, "second finding");
}

#[rstest::rstest]
#[test]
fn latest_attendant_report_is_none_before_the_first_report() {
    // Given an attendant that has never reported.

    // When the latest report is read.
    let latest = ChatSessionState::new_attendant(&ChatSessionState::new(), true)
        .latest_attendant_report()
        .cloned();

    // Then there is nothing to seed the next run from.
    assert!(latest.is_none());
}

#[rstest::rstest]
#[test]
fn session_fields_round_trip_through_serialization() {
    // Given an attendant with behavior, trigger, prep mode, template, and
    // reports — one of each, and the ones that differ from the default.
    let mut session = ChatSessionState::new_attendant(&ChatSessionState::new(), true);
    session.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Preserve);
    session.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
    session.set_attendant_is_prepping(false);
    session.set_seed_template("custom template".to_owned());
    session.append_attendant_report("prior finding".to_owned());

    // When the whole state survives a serialization round trip.
    let json = serde_json::to_string(&session.core).expect("serialize");
    let restored: SessionCore = serde_json::from_str(&json).expect("deserialize");

    // Then every attendant field is preserved.
    assert_eq!(
        restored.attendant.behavior,
        jinn_attendant_msg::AttendantBehavior::Preserve
    );
    assert_eq!(
        restored.attendant.trigger,
        jinn_attendant_msg::AttendantTrigger::ParentCompleted
    );
    assert!(!restored.attendant.prep_mode);
    assert_eq!(restored.attendant.seed_template, "custom template");
    assert_eq!(restored.attendant.reports.len(), 1);
    assert_eq!(restored.attendant.reports[0].body, "prior finding");
}

#[rstest::rstest]
#[test]
fn attendant_fields_default_when_absent_from_persisted_blob() {
    // Given a serialized session core written before attendants existed.
    let legacy = r#"{
        "session_id": "019912ac-0000-7000-8000-000000000001",
        "updated_at": "2026-01-01T00:00:00Z",
        "created_at": "2026-01-01T00:00:00Z",
        "cwd": ".",
        "history": [],
        "profile": { "model": { "single": "__no_provider__" } },
        "blobs": {},
        "lifecycle_args": [],
        "lifecycle_script_state": "nothing_ran",
        "token_ledger": [],
        "session_state": "loaded",
        "persist": true,
        "has_interacted": false,
        "origin": "user"
    }"#;

    // When that blob is deserialized.
    let core: SessionCore = serde_json::from_str(legacy).expect("deserialize legacy blob");

    // Then the attendant group takes its defaults — no migration needed.
    assert_eq!(
        core.attendant.behavior,
        jinn_attendant_msg::AttendantBehavior::Reset
    );
    assert_eq!(
        core.attendant.trigger,
        jinn_attendant_msg::AttendantTrigger::Manual
    );
    assert!(core.attendant.reports.is_empty());
    assert_eq!(
        core.attendant.seed_template,
        jinn_attendant_msg::default_seed_template()
    );
}

#[rstest::rstest]
fn cancel_streaming_leaves_the_input_draft_where_the_user_typed_it() {
    // Given a streaming session attached to a registry holding the
    // chat-input cell, with a draft the user typed.
    let slices = jinn_slices::Slices::new();
    slices
        .register(
            jinn_chat_input_msg::chat_inputs_slot(),
            jinn_chat_input_msg::ChatInputs::new(),
        )
        .expect("fresh registry");
    let mut session = ChatSessionState::new();
    session.attach_slices(slices.clone());
    session.update_input(|i| i.replace_all("draft the user typed".to_owned()));
    session.push_entry(ChatEntry::user("in flight"));
    session.begin_streaming();

    // When the turn is cancelled the way a trigger supersedes one — the
    // plain phase transition, not the Esc drain.
    session.cancel_streaming(jiff::Timestamp::now());

    // Then the session is Idle, so the enqueue that follows dispatches
    // rather than queueing behind a still-hot phase.
    assert_eq!(session.phase(), jinn_session_msg::PhaseKind::Idle);
    // And the draft is untouched: draining into the input box is an Esc
    // affordance for recovering text, and a caller that has no use for the
    // abandoned fragment must not put it where the user can see it.
    assert_eq!(
        session.with_input(|i| i.text().to_owned(), String::new),
        "draft the user typed",
        "cancel_streaming must not steer the abandoned fragment into the input box"
    );
}

#[rstest::rstest]
fn a_queued_message_is_handed_back_to_the_user_as_an_editable_draft() {
    // Given a streaming session attached to the chat-input cell, with a
    // seeded turn already sitting in the queue. This is the state a trigger
    // produces when it publishes `CancelStream` without also dropping the
    // phase: the enqueue sees a hot session and queues instead of
    // dispatching.
    let slices = jinn_slices::Slices::new();
    slices
        .register(
            jinn_chat_input_msg::chat_inputs_slot(),
            jinn_chat_input_msg::ChatInputs::new(),
        )
        .expect("fresh registry");
    let mut session = ChatSessionState::new();
    session.attach_slices(slices.clone());
    session.push_entry(ChatEntry::user("in flight"));
    session.begin_streaming();
    session.enqueue(jinn_turn_dispatch_msg::QueueItem::UserMessage(Box::new(
        ChatEntry::user("seeded: prior report"),
    )));

    // When a cancel drains the session the way the Esc path does.
    session.cancel_stream_and_drain();

    // Then the seeded turn surfaces as editable draft text instead of
    // having run. This is what a user sees when an attendant is triggered
    // while busy: a templated prompt they never sent, sitting in the input
    // box. The fix is upstream — the trigger must drop the phase so the
    // enqueue dispatches — and this test exists to keep the cost of getting
    // that wrong visible.
    assert_eq!(
        session.with_input(|i| i.text().to_owned(), String::new),
        "seeded: prior report",
        "a queued message is recovered as draft text by design"
    );
}

/// An attendant with the given trigger, out of prep mode.
fn attendant_configured(trigger: jinn_attendant_msg::AttendantTrigger) -> ChatSessionState {
    let mut attendant = ChatSessionState::new_attendant(&ChatSessionState::new(), true);
    attendant.set_attendant_trigger(trigger);
    attendant.set_attendant_is_prepping(false);
    attendant
}

#[rstest::rstest]
#[test]
fn a_composed_attendant_is_not_prepping() {
    // Given an attendant that has finished composing.
    let session = attendant_configured(jinn_attendant_msg::AttendantTrigger::ParentCompleted);

    // When asking whether it may run.
    let prepping = session.attendant_is_prepping();

    // Then it may. The behavior above it is a separate fact and cannot
    // decide this one.
    assert!(!prepping);
}

#[rstest::rstest]
#[test]
fn a_fresh_attendant_is_prepping() {
    // Given an attendant straight from `N`.
    let session = ChatSessionState::new_attendant(&ChatSessionState::new(), true);

    // When asking whether it may run.
    let prepping = session.attendant_is_prepping();

    // Then it may not: its pins are half-written, so nothing dispatches —
    // not the `R` key, and not the trigger.
    assert!(prepping);
}

#[rstest::rstest]
#[case(jinn_attendant_msg::AttendantTrigger::Manual)]
#[case(jinn_attendant_msg::AttendantTrigger::ParentCompleted)]
fn a_manual_trigger_does_not_stop_a_composed_attendant(
    #[case] trigger: jinn_attendant_msg::AttendantTrigger,
) {
    // Given a composed attendant on each trigger.
    let session = attendant_configured(trigger);

    // When asking whether it is prepping.
    let prepping = session.attendant_is_prepping();

    // Then it is not. A manual trigger declines to fire on its own; the
    // user pressing `R` still runs it. Marking that attendant as stopped
    // is the ambiguity the old `or` of two fields produced.
    assert!(!prepping);
}

#[rstest::rstest]
#[test]
fn only_a_parent_completed_trigger_fires_on_its_own() {
    // Given a composed attendant on each trigger.
    let manual = attendant_configured(jinn_attendant_msg::AttendantTrigger::Manual);
    let automatic = attendant_configured(jinn_attendant_msg::AttendantTrigger::ParentCompleted);

    // When asking which one fires when its parent finishes.
    let fires = [
        manual.attendant_fires_on_parent_completion(),
        automatic.attendant_fires_on_parent_completion(),
    ];

    // Then only the parent-completed one does.
    assert_eq!(fires, [false, true]);
}

#[rstest::rstest]
#[test]
fn the_behavior_does_not_change_the_trigger_question() {
    // Given the same trigger under each behavior.
    let mut preserve = ChatSessionState::new_attendant(&ChatSessionState::new(), true);
    preserve.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Preserve);
    preserve.set_attendant_trigger(jinn_attendant_msg::AttendantTrigger::ParentCompleted);
    preserve.set_attendant_is_prepping(false);
    let mut reset = preserve.clone();
    reset.set_attendant_behavior(jinn_attendant_msg::AttendantBehavior::Reset);

    // When asking which fires on the parent's completion.
    let fires = [
        preserve.attendant_fires_on_parent_completion(),
        reset.attendant_fires_on_parent_completion(),
    ];

    // Then both do, identically. What a run sees and when it happens are
    // two independent facts, and the sidebar marks them separately.
    assert_eq!(fires, [true, true]);
}

#[rstest::rstest]
#[test]
fn a_history_removal_ahead_of_a_streaming_tool_call_does_not_strand_its_delta() {
    // Given a sending session with a tool call mid-argument-stream and an
    // entry ahead of it that is about to be removed.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.push_entry(ChatEntry::user("edit the file"));
    session.push_entry(ChatEntry::tool_call(
        "tc-keep",
        "write",
        r#"{"content":"first"}"#,
    ));
    session.begin_tool_call(0, "tc-live", "write", jiff::Timestamp::now());
    let live = session
        .history()
        .iter()
        .position(|e| matches!(&e.kind, jinn_core_types::ChatEntryKind::ToolCall { id, .. } if id == "tc-live"))
        .expect("live tool call entry");

    // When an entry ahead of it is removed, shrinking every later index.
    session.remove_history_entry_at(0);

    // Then the delta still lands on the entry it started on.
    let r = session.append_tool_call_delta(0, r#"-second"}"#);
    assert!(r.is_ok(), "delta refused after a removal: {r:?}");
    let moved = session
        .history()
        .iter()
        .position(|e| matches!(&e.kind, jinn_core_types::ChatEntryKind::ToolCall { id, .. } if id == "tc-live"))
        .expect("live tool call entry after removal");
    assert_eq!(moved, live - 1, "the entry did not move with the removal");
    if let jinn_core_types::ChatEntryKind::ToolCall { arguments, .. } =
        &session.history()[moved].kind
    {
        assert_eq!(arguments, r#"-second"}"#, "delta landed on the wrong entry");
    } else {
        panic!("expected a ToolCall entry");
    }
}

/// A session mid-response with a tool call whose arguments are still arriving.
fn streaming_tool_call_session(id: &str, name: &str) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session.begin_tool_call(0, id, name, jiff::Timestamp::now());
    session
        .append_tool_call_delta(0, r#"{"command":"rm -rf "#)
        .expect("append argument delta");
    session
}

#[rstest::rstest]
fn an_interrupted_call_is_paired_with_a_result() {
    // Given a session interrupted mid-arguments.
    let mut session = streaming_tool_call_session("tc_1", "bash");

    // When the interrupted call is explained.
    let completed = session.explain_interrupted_tool_calls();

    // Then exactly one call is completed, by a result carrying its id.
    assert_eq!(completed, 1);
    let result = session
        .history()
        .iter()
        .find_map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, .. } if id == "tc_1" => Some(e),
            _ => None,
        })
        .expect("a result for the interrupted call");
    assert!(
        matches!(
            result.kind,
            ChatEntryKind::ToolResult {
                status: jinn_core_types::ToolResultStatus::Failure,
                ..
            }
        ),
        "the call did not run, so the result is a failure"
    );
}

#[rstest::rstest]
fn an_interrupted_calls_result_sits_next_to_its_call() {
    // Given a session interrupted mid-arguments.
    let mut session = streaming_tool_call_session("tc_1", "bash");

    // When the interrupted call is explained.
    session.explain_interrupted_tool_calls();

    // Then the result immediately follows the call it answers.
    let kinds: Vec<&str> = session.history().iter().map(ChatEntry::kind_str).collect();
    let call_at = kinds
        .iter()
        .position(|k| *k == "tool_call")
        .expect("the call is in history");
    assert_eq!(kinds.get(call_at + 1).copied(), Some("tool_result"));
}

#[rstest::rstest]
fn an_interrupted_result_quotes_the_arguments_truncated() {
    // Given a session interrupted mid-arguments.
    let mut session = streaming_tool_call_session("tc_1", "bash");

    // When the interrupted call is explained.
    session.explain_interrupted_tool_calls();

    // Then the result carries the fragment verbatim, labelled as truncated.
    let content = session
        .history()
        .iter()
        .find_map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, content, .. } if id == "tc_1" => Some(content.clone()),
            _ => None,
        })
        .expect("a result for the interrupted call");
    assert!(
        content.contains(r#"{"command":"rm -rf "#),
        "the fragment must survive byte-for-byte, got: {content}"
    );
    assert!(
        content.contains("truncated"),
        "the fragment must be labelled as incomplete, got: {content}"
    );
}

#[rstest::rstest]
fn an_interrupted_result_names_the_tool() {
    // Given a session interrupted mid-arguments.
    let mut session = streaming_tool_call_session("tc_1", "bash");

    // When the interrupted call is explained.
    session.explain_interrupted_tool_calls();

    // Then the result names the tool that did not run.
    let content = session
        .history()
        .iter()
        .find_map(|e| match &e.kind {
            ChatEntryKind::ToolResult { id, content, .. } if id == "tc_1" => Some(content.clone()),
            _ => None,
        })
        .expect("a result for the interrupted call");
    assert!(content.contains("bash"), "got: {content}");
}

#[rstest::rstest]
fn explaining_a_prose_interrupt_completes_nothing() {
    // Given a session streaming prose with no tool call.
    let mut session = ChatSessionState::new();
    session.begin_sending();
    session.begin_streaming();
    session
        .append_stream_token("some prose", jiff::Timestamp::now())
        .expect("append prose");
    let before = session.history().len();

    // When the intercept explains interrupted calls.
    let completed = session.explain_interrupted_tool_calls();

    // Then nothing was added, because there was no call to explain.
    assert_eq!(completed, 0);
    assert_eq!(session.history().len(), before);
}

#[rstest::rstest]
fn an_interrupted_call_stays_in_context_on_retry() {
    // Given a session whose tool call has been paired with a result.
    let mut session = streaming_tool_call_session("tc_1", "bash");
    session.explain_interrupted_tool_calls();

    // When the attempt is prepared for retry.
    session.reset_streaming_entries_for_retry();

    // Then the call is still in context, so the model can read its failure.
    assert!(
        session.history().iter().any(
            |e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc_1")
                && e.is_in_context()
        ),
        "an explained call must stay in the resumed request"
    );
}

#[rstest::rstest]
fn an_answered_call_is_kept_with_the_prose_that_introduced_it() {
    // Given a session with partial prose beside an explained tool call.
    let mut session = streaming_tool_call_session("tc_1", "bash");
    session
        .append_stream_token("I will now run ", jiff::Timestamp::now())
        .expect("append prose");
    session.explain_interrupted_tool_calls();

    // When the attempt is prepared for retry.
    session.reset_streaming_entries_for_retry();

    // Then the prose stays too: it is the sentence that introduced the call,
    // and keeping the call without it hands the model a call with no
    // explanation of what it was doing.
    assert!(
        session.history().iter().any(
            |e| matches!(&e.kind, ChatEntryKind::Assistant(t) if t.contains("I will now"))
                && e.is_in_context()
        ),
        "the prose introducing a retained call must stay in the resumed request"
    );
}

#[rstest::rstest]
fn prose_with_no_retained_call_is_still_excluded_on_retry() {
    // Given a session whose only partial output is prose.
    let mut session = streaming_session();
    session
        .append_stream_token("I will now think about this", jiff::Timestamp::now())
        .expect("append prose");

    // When the attempt is prepared for retry.
    session.reset_streaming_entries_for_retry();

    // Then it is an abandoned attempt with nothing completed to pair against,
    // so it stays out as it always has.
    assert!(
        session.history().iter().all(
            |e| !matches!(&e.kind, ChatEntryKind::Assistant(t) if t.contains("think about"))
                || !e.is_in_context()
        ),
        "an attempt that made no call is still excluded"
    );
}

#[rstest::rstest]
fn reasoning_beside_a_retained_call_is_still_excluded_on_retry() {
    // Given a session that thought, then made a call the rule interrupted.
    let mut session = streaming_tool_call_session("tc_1", "bash");
    session.explain_interrupted_tool_calls();
    session.reset_streaming_entries_for_retry();

    // Then an empty assistant host is retained but adds nothing: the call is
    // carried by a synthesized empty assistant, which is what the assembler
    // would have done regardless.
    assert!(
        session.history().iter().any(
            |e| matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc_1")
                && e.is_in_context()
        ),
        "the call and its result stay"
    );
}

#[rstest::rstest]
fn an_unanswered_call_is_still_excluded_on_retry() {
    // Given a session interrupted mid-arguments with nothing explaining it.
    let mut session = streaming_tool_call_session("tc_1", "bash");

    // When the attempt is prepared for retry.
    session.reset_streaming_entries_for_retry();

    // Then the dangling call is taken out of context, as before: providers
    // reject a request whose calls have no results.
    assert!(
        session.history().iter().all(
            |e| !matches!(&e.kind, ChatEntryKind::ToolCall { id, .. } if id == "tc_1")
                || !e.is_in_context()
        ),
        "an unanswered call cannot be sent to a provider"
    );
}

#[rstest::rstest]
fn an_interrupted_call_reaches_the_model_with_its_own_prose() {
    // Given a session interrupted mid-arguments, explained and prepared for retry.
    let mut session = streaming_tool_call_session("tc_1", "bash");
    session
        .append_stream_token("I will now run ", jiff::Timestamp::now())
        .expect("append prose");
    session.explain_interrupted_tool_calls();
    session.reset_streaming_entries_for_retry();

    // When the resumed request is assembled from what stayed in context.
    let kept: Vec<jinn_core_types::ChatEntry> = session
        .history()
        .iter()
        .filter(|e| e.is_in_context())
        .cloned()
        .collect();
    let messages = jinn_llm_support::entries_to_messages::entries_to_messages(&kept);

    // Then the assistant message carries both the model's own words and the
    // call it made, rather than arriving as a bare tool call from nowhere.
    let assistant = messages.iter().find_map(|m| match m {
        jinn_provider::LlmMessage::Assistant {
            content,
            tool_calls,
        } => Some((content, tool_calls)),
        _ => None,
    });
    let (content, tool_calls) = assistant.expect("an assistant message");
    assert_eq!(
        content, "I will now run ",
        "the model's preamble must survive"
    );
    assert!(
        tool_calls
            .as_ref()
            .is_some_and(|calls| calls.iter().any(|c| c.id == "tc_1")),
        "the interrupted call must ride on the same message"
    );
}
