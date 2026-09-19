#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    clippy::uninlined_format_args,
    reason = "test code"
)]

use crate::feat::session::profile::SessionProfile;

use crate::feat::session::token_stats::TokenRecord;
use crate::feat::ui::chat_log::visual_item::{
    DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
};
use crate::protocol::ToolResultStatus;
use crate::protocol::{
    ChatEntry, ChatEntryId, ChatEntryKind, ContextOverride, EntryTiming, PinPosition, SessionId,
};
use std::path::PathBuf;

use super::*;
use jinn_core_types::model_selection::ModelSelection;

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
    use crate::feat::context::prompt_template::PromptTemplateStore;
    use crate::feat::context::protocol::prompt_template::PromptTemplate;

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
    session.set_discovered_prompt_templates(
        crate::feat::context::prompt_template::PromptTemplateStore::from_vec(vec![]),
    );

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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    session.core.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When pushing an entry.
    let before = jiff::Timestamp::now();
    session.push_entry(ChatEntry::user("hi"));

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.last_history_activity_at >= before);
}

#[rstest::rstest]
fn append_stream_token_bumps_last_history_activity_at() {
    // Given a streaming session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.begin_streaming();
    session.core.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When appending a token.
    let before = jiff::Timestamp::now();
    session
        .append_stream_token("Hello", jiff::Timestamp::now())
        .expect("ok");

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.last_history_activity_at >= before);
}

#[rstest::rstest]
fn append_thinking_token_bumps_last_history_activity_at() {
    // Given a session with a streaming assistant entry that has begun thinking.
    let mut session = ChatSessionState::builder()
        .with_user_entry("hello")
        .begin_streaming()
        .build();
    session.begin_thinking(jiff::Timestamp::now());
    session.core.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When appending a thinking token.
    let before = jiff::Timestamp::now();
    session.append_thinking_token("reasoning").expect("ok");

    // Then the activity timestamp advanced to ~now.
    assert!(session.core.last_history_activity_at >= before);
}

#[rstest::rstest]
fn begin_sending_seeds_last_history_activity_at() {
    // Given a session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.core.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When beginning sending.
    let before = jiff::Timestamp::now();
    session.begin_sending();

    // Then the activity timestamp was seeded to ~now.
    assert!(session.core.last_history_activity_at >= before);
}

#[rstest::rstest]
fn begin_streaming_seeds_last_history_activity_at() {
    // Given a session with a stale activity timestamp.
    let mut session = ChatSessionState::new();
    session.core.last_history_activity_at = jiff::Timestamp::UNIX_EPOCH;

    // When beginning streaming.
    let before = jiff::Timestamp::now();
    session.begin_streaming();

    // Then the activity timestamp was seeded to ~now.
    assert!(session.core.last_history_activity_at >= before);
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
    session.reset_scroll();
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
    session.reset_scroll();

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

    // Then is_at_bottom is true.
    assert!(session.is_at_bottom());
}

#[rstest::rstest]
fn is_at_bottom_false_when_scrolled_up() {
    // Given a session scrolled to offset 50.
    let mut session = ChatSessionState::new();
    session.set_scroll_offset(Some(50));

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
        &session.queue()[0],
        jinn_turn_dispatch_msg::QueueItem::UserMessage(e) if e.kind == ChatEntryKind::User {
            display: "hello".to_owned(),
            expanded: "hello".to_owned(),
            attachments: Vec::new(),
            outcome: crate::protocol::AttachmentOutcome::default(),
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
            outcome: crate::protocol::AttachmentOutcome::default(),
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
    let drained = session.drain_queue();

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
            outcome: crate::protocol::AttachmentOutcome::default(),
        }
    );
    assert_eq!(
        entries[1].kind,
        ChatEntryKind::User {
            display: "b".to_owned(),
            expanded: "b".to_owned(),
            attachments: Vec::new(),
            outcome: crate::protocol::AttachmentOutcome::default(),
        }
    );
    assert_eq!(
        entries[2].kind,
        ChatEntryKind::User {
            display: "c".to_owned(),
            expanded: "c".to_owned(),
            attachments: Vec::new(),
            outcome: crate::protocol::AttachmentOutcome::default(),
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
    let _ = session.drain_queue();

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
    let mut session = ChatSessionState::new();
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

    // Then it is idle.
    assert_eq!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn is_idle_false_when_sending() {
    // Given a session that is sending.
    let mut session = ChatSessionState::new();
    session.begin_sending();

    // Then it is not idle.
    assert_ne!(session.phase(), PhaseKind::Idle);
}

#[rstest::rstest]
fn is_idle_false_when_streaming() {
    // Given a session that is streaming.
    let mut session = ChatSessionState::new();
    session.begin_streaming();

    // Then it is not idle.
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1 - block rep
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2 - will pin
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3 - forward sub-block start
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4 - forward sub-block
    session.push_entry(ChatEntry::user("after"));

    // Expand the ignored block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));

    // Pin ignored-B (entry at idx 2).
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // The forward sub-block representative (ignored-C) should be auto-shown.
    let forward_rep = session.history()[3].id.clone();
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep),
        "forward sub-block should be auto-shown after pin inside shown block"
    );

    // Original block rep should still be shown.
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

#[rstest::rstest]
fn pin_entry_no_propagation_for_non_ignored() {
    // Pinning a non-ignored entry should never touch shown_ignored_blocks.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("normal"));
    let id = session.history()[0].id.clone();
    session.pin_entry(&id, PinPosition::Top);
    assert!(session.shown_ignored_blocks_snapshot().is_empty());
}

#[rstest::rstest]
fn pin_entry_no_propagation_for_collapsed_block() {
    // Pinning an ignored entry inside a collapsed block should NOT auto-show.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true));
    session.push_entry(ChatEntry::assistant("c").with_ignored(true));
    session.push_entry(ChatEntry::user("after"));

    // Do NOT expand the block - it stays collapsed.
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Forward sub-block should NOT be shown.
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
    // Pinning the last entry in an ignored block - no forward sub-block exists.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true));
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // pin this (last in block)
    session.push_entry(ChatEntry::user("after")); // non-ignored, breaks block

    // Expand the block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin the last ignored entry.
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // No new shown_ignored_blocks entries - forward entry is non-ignored.
    // Only the original rep should be shown.
    assert_eq!(session.shown_ignored_blocks_snapshot().len(), 1);
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

/// Regression test for: pinning an ignored entry inside an expanded block
/// should keep all entries visible. Before the fix, the forward sub-block
/// would collapse because `shown_ignored_blocks` didn't cover it.
#[rstest::rstest]
fn regression_pin_in_expanded_block_keeps_all_visible() {
    use crate::feat::ui::chat_log::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    };

    // Layout: [user] [ignored-A] [ignored-B] [ignored-C] [ignored-D] [user]
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2 - pin target
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    session.push_entry(ChatEntry::user("after")); // idx 5

    // Expand the ignored block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin ignored-B (idx 2).
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Build visual items - all 4 ignored entries should be individually visible
    // (backward sub-block + pinned entry + forward sub-block).
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );

    // There should be no CollapsedIgnoredBlock - all entries are shown.
    let collapsed = items
        .iter()
        .any(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }));
    assert!(
        !collapsed,
        "no entries should be collapsed after pinning in expanded block"
    );

    // All history entries should appear as VisualItem::Entry.
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
    // Layout: [user] [ignored-A] [ignored-B(pinned)] [ignored-C] [ignored-D] [user]
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    session.push_entry(ChatEntry::user("after")); // idx 5

    // Expand, then pin ignored-B.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Both sub-blocks should be shown.
    let forward_rep = session.history()[3].id.clone();
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
    assert!(
        session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep)
    );

    // Toggle `h` on the forward sub-block.
    session.toggle_ignored_block_visibility(&forward_rep);

    // Forward sub-block should now be collapsed.
    assert!(
        !session
            .shown_ignored_blocks_snapshot()
            .contains(&forward_rep)
    );
    // Backward sub-block should still be shown.
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep_id));
}

/// Regression test for: unpinning an entry re-merges the sub-blocks into
/// one block controlled by the original representative.
#[rstest::rstest]
fn regression_unpin_remerges_block_correctly() {
    use crate::feat::ui::chat_log::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, VisualItem, build_visual_items,
    };

    // Layout: [user] [ignored-A] [ignored-B] [ignored-C] [user]
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before"));
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::user("after")); // idx 4

    // Expand the block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin ignored-B.
    let pin_id = session.history()[2].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Unpin ignored-B.
    session.unpin_entry(&pin_id);

    // Build visual items - should re-merge into a single expanded block.
    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );

    // No collapsed block - all 5 entries visible.
    let collapsed = items
        .iter()
        .any(|item| matches!(item, VisualItem::CollapsedIgnoredBlock { .. }));
    assert!(!collapsed, "no collapsed block after unpin re-merge");

    let entry_count = items
        .iter()
        .filter(|item| matches!(item, VisualItem::Entry(_)))
        .count();
    assert_eq!(
        entry_count, 5,
        "all 5 entries should be visible after unpin"
    );

    // Toggle on the original rep should now collapse the re-merged block.
    session.toggle_ignored_block_visibility(&rep_id);
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
            outcome: crate::protocol::AttachmentOutcome::default(),
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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

    // Then it is empty.
    assert!(session.is_empty());
}

#[rstest::rstest]
fn is_empty_false_after_pushing_entry() {
    // Given a new session with one entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));

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
    session.core.last_history_activity_at = jiff::Timestamp::now()
        .checked_sub(jiff::Span::new().hours(1))
        .expect("past");

    // When appending streaming output.
    let before = session.core.last_history_activity_at;
    std::thread::sleep(std::time::Duration::from_millis(10));
    session.append_tool_result_output("call_1", "tick", jinn_tools_msg::ToolOutputKind::Normal);

    // Then the activity timestamp advanced past its pre-append value.
    assert!(session.core.last_history_activity_at > before);
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
fn begin_tool_result_does_not_push_in_sending_phase() {
    // Given a session in Sending phase (not Streaming).
    let mut session = ChatSessionState::new();
    session.begin_sending();

    // When beginning a tool result.
    session.begin_tool_result("call_1", "bash", jiff::Timestamp::now());

    // Then no entry is pushed.
    assert!(
        session.history().is_empty(),
        "expected no entry in Sending phase, got {} entries",
        session.history().len()
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
fn advance_after_setup_transitions_nothing_to_setup() {
    // Given NothingRan.
    let mut state = LifecycleScriptState::NothingRan;

    // When advancing after setup.
    state.advance_after_setup();

    // Then state is SetupRan.
    assert_eq!(state, LifecycleScriptState::SetupRan);
}

#[rstest::rstest]
fn advance_after_teardown_transitions_setup_to_teardown() {
    // Given SetupRan.
    let mut state = LifecycleScriptState::SetupRan;

    // When advancing after teardown.
    state.advance_after_teardown();

    // Then state is TeardownRan.
    assert_eq!(state, LifecycleScriptState::TeardownRan);
}

#[rstest::rstest]
fn advance_after_setup_is_noop_from_setup_ran() {
    // Given SetupRan.
    let mut state = LifecycleScriptState::SetupRan;

    // When advancing after setup again.
    state.advance_after_setup();

    // Then state stays SetupRan (no panic).
    assert_eq!(state, LifecycleScriptState::SetupRan);
}

#[rstest::rstest]
fn advance_after_setup_is_noop_from_teardown_ran() {
    // Given TeardownRan.
    let mut state = LifecycleScriptState::TeardownRan;

    // When advancing after setup.
    state.advance_after_setup();

    // Then state stays TeardownRan (no panic).
    assert_eq!(state, LifecycleScriptState::TeardownRan);
}

#[rstest::rstest]
fn advance_after_teardown_is_noop_from_nothing_ran() {
    // Given NothingRan.
    let mut state = LifecycleScriptState::NothingRan;

    // When advancing after teardown.
    state.advance_after_teardown();

    // Then state stays NothingRan (no panic).
    assert_eq!(state, LifecycleScriptState::NothingRan);
}

#[rstest::rstest]
fn advance_after_teardown_is_noop_from_teardown_ran() {
    // Given TeardownRan.
    let mut state = LifecycleScriptState::TeardownRan;

    // When advancing after teardown again.
    state.advance_after_teardown();

    // Then state stays TeardownRan (no panic).
    assert_eq!(state, LifecycleScriptState::TeardownRan);
}

#[rstest::rstest]
fn session_state_defaults_to_loaded() {
    // Given a new session.
    let session = ChatSessionState::new();

    // Then session state is Loaded.
    assert_eq!(session.session_state(), SessionState::Loaded);
}

#[rstest::rstest]
fn lifecycle_script_state_defaults_to_nothing_ran() {
    // Given a new session.
    let session = ChatSessionState::new();

    // Then lifecycle script state is NothingRan.
    assert_eq!(
        session.lifecycle_script_state(),
        LifecycleScriptState::NothingRan
    );
}

#[rstest::rstest]
#[case(LifecycleScriptState::NothingRan, "nothing_ran")]
#[case(LifecycleScriptState::SetupRan, "setup_ran")]
#[case(LifecycleScriptState::TeardownRan, "teardown_ran")]
fn lifecycle_script_state_displays_snake_case(
    #[case] state: LifecycleScriptState,
    #[case] expected: &str,
) {
    // Given each lifecycle script state variant.

    // When formatting it.
    let rendered = state.to_string();

    // Then it renders as the snake_case form matching serde rename.
    assert_eq!(rendered, expected);
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

    // Then no saved position exists.
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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

#[rstest::rstest]
fn toggle_ignored_block_visibility_stops_at_pinned_entry() {
    // Given: 3 non-ignored, 3 ignored-unpinned, 1 ignored-pinned, 3 ignored-unpinned, 2 non-ignored.
    let mut session = ChatSessionState::new();
    for _ in 0..3 {
        session.push_entry(ChatEntry::user("visible"));
    }
    for _ in 0..3 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    }
    {
        let mut entry = ChatEntry::user("ignored-pinned");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        entry.pin_position = Some(PinPosition::Top);
        session.push_entry(entry);
    }
    for _ in 0..3 {
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

    // history indices: 0-2 visible, 3-5 ignored, 6 ignored+pinned, 7-9 ignored, 10-11 visible
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
    // Given: 3 non-ignored, 3 ignored-unpinned, 1 ignored-pinned, 3 ignored-unpinned, 2 non-ignored.
    let mut session = ChatSessionState::new();
    for _ in 0..3 {
        session.push_entry(ChatEntry::user("visible"));
    }
    for _ in 0..3 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    }
    {
        let mut entry = ChatEntry::user("ignored-pinned");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        entry.pin_position = Some(PinPosition::Top);
        session.push_entry(entry);
    }
    for _ in 0..3 {
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

    // history indices: 0-2 visible, 3-5 ignored, 6 ignored+pinned, 7-9 ignored, 10-11 visible
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
    use crate::feat::ui::chat_log::visual_item::{
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
    use crate::feat::ui::chat_log::visual_item::{
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
    // Given a session with visual items where a collapsed block is selected.
    use crate::feat::ui::chat_log::visual_item::{
        DEFAULT_MIN_COLLAPSE_COUNT, PROXIMITY_COUNT, build_visual_items,
    };

    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a"));
    for _ in 0..15 {
        let mut entry = ChatEntry::user("ignored");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "test".into(),
            },
        );
        session.push_entry(entry);
    }
    session.push_entry(ChatEntry::user("b"));

    let items = build_visual_items(
        session.history(),
        &session.shown_ignored_blocks_snapshot(),
        PROXIMITY_COUNT,
        DEFAULT_MIN_COLLAPSE_COUNT,
    );
    session.set_visual_items(items);

    // Select the collapsed block (visual-item index 1).
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
    // But selected_entry_index() returns the visual-item index.
    assert_eq!(session.selected_entry_index(), Some(1));
    // And selected_history_index() returns None (no history index for collapsed block).
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
    session.core.lifecycle_name = Some("test-lifecycle".to_owned());

    // Then the session is persistable even without interaction.
    assert!(session.is_persistable());
    assert!(!session.has_interacted());
}

#[rstest::rstest]
#[test]
fn forked_session_is_always_persistable() {
    // Given a new session with a parent session but no interaction.
    let mut session = ChatSessionState::new();
    session.core.parent_session = Some(SessionId::new());

    // Then the session is persistable even without interaction.
    assert!(session.is_persistable());
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
        crate::protocol::ToolResultStatus::Success,
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
        crate::protocol::ToolResultStatus::Success,
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
fn force_exclude_no_tool_calls_is_noop() {
    // Given a history with no tool calls.
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
    let mut session = ChatSessionState::new();
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
        .expect("streaming")
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
        .expect("streaming")
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
        .expect("streaming")
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
        .expect("streaming")
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
        .expect("streaming")
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
    // Given an unknown string.
    let result: Result<PhaseKind, _> = "unknown_phase".parse();
    // Then it returns an error.
    assert!(result.is_err());
}

#[rstest::rstest]
fn phase_kind_from_str_rejects_empty() {
    // Given an empty string.
    let result: Result<PhaseKind, _> = "".parse();
    // Then it returns an error.
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
    session.push_entry(ChatEntry::user("a")); // idx 0, lines 0..2
    session.push_entry(ChatEntry::user("b")); // idx 1, lines 2..4
    session.push_entry(ChatEntry::user("c")); // idx 2, lines 4..6
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
    session.push_entry(ChatEntry::user("a")); // idx 0, lines 0..2
    session.push_entry(ChatEntry::user("b")); // idx 1, lines 2..4
    session.push_entry(ChatEntry::user("c")); // idx 2, lines 4..6
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
    session.push_entry(ChatEntry::user("a")); // idx 0, lines 0..2
    session.push_entry(ChatEntry::user("b")); // idx 1, lines 2..4
    session.push_entry(ChatEntry::user("c")); // idx 2, lines 4..6
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
    session.push_entry(ChatEntry::user("a")); // idx 0, lines 0..10 (tall)
    session.push_entry(ChatEntry::user("b")); // idx 1, lines 10..12
    session.set_entry_line_ranges(vec![(0, 10), (10, 12)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_last_max_offset(20);
    // Viewport showing lines 10–14.
    session.set_rendered_scroll_offset(10);
    session.set_selected_entry_index(0);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Entry is taller than viewport. abs_end(10) <= current_offset(10) → true.
    // new_offset = abs_end - viewport_height = 10 - 4 = 6.
    assert_eq!(session.scroll_offset(), Some(6));
}

#[rstest::rstest]
fn scroll_to_selected_taller_than_viewport_below() {
    // Given an entry taller than the viewport, positioned below the viewport.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0, lines 0..2
    session.push_entry(ChatEntry::user("b")); // idx 1, lines 2..12 (tall)
    session.set_entry_line_ranges(vec![(0, 2), (2, 12)]);
    session.set_viewport_height(4);
    session.set_blank_count(0);
    session.set_last_max_offset(20);
    // Viewport at top.
    session.set_rendered_scroll_offset(0);
    session.set_selected_entry_index(1);

    // When scrolling to selected.
    session.scroll_to_selected();

    // Entry is taller than viewport. abs_start(2) >= current_offset(0)+4=4 → false.
    // abs_end(12) <= current_offset(0) → false. Already overlapping → return (no change).
    // The entry starts at line 2, viewport shows 0–4 → they overlap.
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

    // Entry 0: 2>0 && 0<4 → visible.
    // Entry 1: 4>0 && 2<4 → visible.
    // Entry 2: 6>0 && 4<4 → 4<4 is false → NOT visible.
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

    // Clear selection and select next (should skip empty assistant at 0).
    session.clear_selection();
    session.select_next_entry();

    // Then selection is on the user entry (index 1), not the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_next_entry_skips_empty_assistant_in_middle() {
    // Given: [user, empty-assistant, user].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1
    session.push_entry(ChatEntry::user("b")); // idx 2

    // Select from index 0.
    session.set_selected_entry_index(0);
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

    // Clear and select prev (starts at last → should skip empty assistant).
    session.clear_selection();
    session.select_prev_entry();

    // Then selection is on the user entry (index 1), not the empty assistant.
    assert_eq!(session.selected_entry_index(), Some(1));
}

#[rstest::rstest]
fn select_prev_entry_skips_empty_assistant_in_middle() {
    // Given: [user, empty-assistant, user].
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("a")); // idx 0
    session.push_entry(ChatEntry::assistant("")); // idx 1
    session.push_entry(ChatEntry::user("b")); // idx 2

    // Select from index 2.
    session.set_selected_entry_index(2);
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
    // Given a contiguous ignored block: entries at indices 1-4 are ignored.
    // The pin_entry propagation scans backward from the pinned entry to find
    // the block start (first ignored entry with no pin and with in-context neighbor).
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before")); // idx 0
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1 - block rep
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 2
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3 - pin target
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4
    session.push_entry(ChatEntry::user("after")); // idx 5

    // Expand the block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin entry at idx 3.
    let pin_id = session.history()[3].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the block_start scan found index 1 (the first entry in the contiguous
    // ignored block). The forward sub-block starts at index 4.
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
    // Given two separate ignored blocks with a non-ignored entry between them.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 0
    session.push_entry(ChatEntry::assistant("b").with_ignored(true)); // idx 1
    session.push_entry(ChatEntry::user("boundary")); // idx 2 - in-context
    session.push_entry(ChatEntry::assistant("c").with_ignored(true)); // idx 3
    session.push_entry(ChatEntry::assistant("d").with_ignored(true)); // idx 4

    // Expand both blocks separately.
    let rep1 = session.history()[0].id.clone();
    let rep2 = session.history()[3].id.clone();
    session.toggle_ignored_block_visibility(&rep1);
    session.toggle_ignored_block_visibility(&rep2);

    // Pin entry at idx 4 (in the second block).
    let pin_id = session.history()[4].id.clone();
    session.pin_entry(&pin_id, PinPosition::Top);

    // Then the block_start scan stops at idx 3 (doesn't cross the non-ignored boundary).
    // No forward sub-block (idx 4 is the last in its block).
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep2));
    assert!(session.shown_ignored_blocks_snapshot().contains(&rep1));
}

#[rstest::rstest]
fn pin_entry_forward_start_at_history_end_is_noop() {
    // Given an ignored entry at the end of history.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("before")); // idx 0
    session.push_entry(ChatEntry::assistant("a").with_ignored(true)); // idx 1

    // Expand the block.
    let rep_id = session.history()[1].id.clone();
    session.toggle_ignored_block_visibility(&rep_id);

    // Pin the last entry (no forward sub-block possible).
    let pin_id = session.history()[1].id.clone();
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

    // Then the immutable accessor returns it.
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
    // Given a session with a set viewport height.
    let session = ChatSessionState::new();
    session.set_viewport_height(42);

    // Then viewport_height_value returns 42.
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

    // Then the discovered skill/prompt/context sets are all empty.
    assert!(session.discovered_skills().is_empty());
    assert!(session.discovered_prompt_templates().is_empty());
    assert!(session.discovered_context_files().is_empty());
}

#[rstest::rstest]
fn discovered_sets_are_independent_between_sessions() {
    // Given two sessions with different discovered skills.
    let mut a = ChatSessionState::new();
    let mut b = ChatSessionState::new();
    a.set_discovered_skills(vec![crate::feat::skills::Skill {
        name: "session-a-only".into(),
        description: String::new(),
        body: String::new(),
        file_path: PathBuf::new(),
        base_dir: PathBuf::new(),
        source: crate::feat::skills::SkillSource::Global,
    }]);
    b.set_discovered_skills(vec![crate::feat::skills::Skill {
        name: "session-b-only".into(),
        description: String::new(),
        body: String::new(),
        file_path: PathBuf::new(),
        base_dir: PathBuf::new(),
        source: crate::feat::skills::SkillSource::Global,
    }]);

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
    session.set_discovered_context_files(vec![crate::feat::context::env_context::ContextFile {
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

use crate::protocol::HistoryMutation;

fn session_with_excluded_entry(
    source: crate::protocol::ChangeSource,
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
    let mut session = ChatSessionState::new();
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
fn reset_streaming_entries_for_retry_removes_partial_streaming_entry() {
    // Given a streaming session with a partial assistant entry.
    let mut session = streaming_session();
    session
        .append_stream_token("Partial", dispatched_at())
        .expect("append token");
    let history_len_before = session.history().len();
    assert!(history_len_before >= 1, "streaming entry should exist");

    // When resetting streaming entries for retry.
    let removed = session.reset_streaming_entries_for_retry();

    // Then the partial entry is removed and the session stays in Streaming phase.
    assert_eq!(removed, 1, "exactly one streaming entry should be removed");
    assert_eq!(
        session.phase(),
        crate::feat::session::phase_machine::PhaseKind::Streaming,
        "must stay in Streaming phase so the retry can reuse it"
    );
    assert_eq!(
        session.streaming_thinking_entry_index(),
        None,
        "streaming indices cleared",
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_removes_partial_assistant_and_stays_streaming() {
    // Given a streaming session with a partial assistant entry.
    let mut session = ChatSessionState::new();
    session.push_entry(ChatEntry::user("hello"));
    session.begin_streaming();
    session
        .append_stream_token("partial", jiff::Timestamp::now())
        .expect("append token");
    let history_len_before = session.history().len();
    assert_eq!(history_len_before, 2, "user + partial assistant");

    // When resetting streaming entries for retry.
    let removed = session.reset_streaming_entries_for_retry();

    // Then the partial assistant entry was removed.
    assert_eq!(
        removed, 1,
        "only the partial assistant entry should be removed"
    );
    assert_eq!(
        session.history().len(),
        1,
        "user entry should remain, partial assistant discarded"
    );
    assert!(
        session
            .history()
            .iter()
            .all(|e| !matches!(e.kind, ChatEntryKind::Assistant(_))),
        "no assistant entry should remain after reset"
    );
    // And the session is still in the Streaming phase (no transition).
    assert_eq!(
        session.phase(),
        PhaseKind::Streaming,
        "reset must stay in Streaming phase for the retry"
    );
}

#[rstest::rstest]
fn reset_streaming_entries_for_retry_removes_partial_thinking_entry() {
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
    let removed = session.reset_streaming_entries_for_retry();

    // Then both streaming entries (assistant + thinking) are removed;
    // the committed user entry survives.
    assert_eq!(
        removed, 2,
        "partial assistant and thinking entries should be removed"
    );
    assert_eq!(
        session.history().len(),
        1,
        "only the committed user entry should remain"
    );
    assert!(
        session
            .history()
            .iter()
            .all(|e| matches!(e.kind, ChatEntryKind::User { .. })),
        "no streaming entries should remain after reset"
    );
    assert_eq!(
        session.streaming_thinking_entry_index(),
        None,
        "thinking streaming index cleared"
    );
    assert_eq!(
        session.phase(),
        PhaseKind::Streaming,
        "reset must stay in Streaming phase for the retry"
    );
}

// ---------------------------------------------------------------------------
// MCP server enablement
// ---------------------------------------------------------------------------

#[rstest::rstest]
#[test]
fn new_session_has_no_mcp_servers_enabled() {
    // Given a freshly created session.
    let session = ChatSessionState::new();

    // Then no MCP servers are enabled by default.
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
fn set_model_to_alloy_clears_endpoint_pin() {
    // Given a session with a pinned endpoint and a single model.
    let mut session = ChatSessionState::new();
    session.set_model(ModelSelection::Single(
        "openrouter/anthropic/claude".to_owned(),
    ));
    session.profile_mut().endpoint = Some(crate::feat::endpoint::Endpoint {
        tag: "anthropic".to_owned(),
        provider_name: "Anthropic".to_owned(),
    });

    // When switching the model to an alloy.
    session.set_model(ModelSelection::Alloy {
        models: vec!["openrouter/anthropic/claude".to_owned()],
        strategy: jinn_core_types::model_selection::AlloyStrategy::RoundRobin { index: 0 },
    });

    // Then the endpoint pin is cleared (an endpoint pin is model-specific and
    // incoherent across a rotating set).
    assert!(session.profile().endpoint.is_none());
}

#[rstest::rstest]
#[test]
fn default_origin_is_user() {
    // Given a newly created session.
    let session = ChatSessionState::new();

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
fn input_writes_roundtrip_without_the_cell() {
    // Given an unattached session (no slice registry handle).
    let session = ChatSessionState::new();

    // When performing input mutations through the facade.
    session.update_input(|i| {
        i.insert_text("draft text");
        i.move_cursor_to_start();
    });

    // Then writes land on the in-struct fallback and reads round-trip.
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
        crate::protocol::ToolResultStatus::Success,
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
        crate::protocol::ToolResultStatus::Success,
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
