//! Tests for the `HistoryEditor` (owned by the `jinn-session-history`
//! slice; exercised here through the kernel `ChatSessionState`).

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]
use crate::feat::session::chat_session::ChatSessionState;
use crate::protocol::{ChangeSource, ChatEntry, ChatEntryId, ContextOverride};
use crate::protocol::{PinPosition, ToolResultStatus};
use jinn_core_types::llm_message::LlmMessage;
use jinn_core_types::{ChatEntryKind, HistoryMutation};

/// A complete loop: empty assistant, one call, one result.
fn simple_loop() -> Vec<ChatEntry> {
    vec![
        ChatEntry::user("run it"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
    ]
}

fn worker_source() -> ChangeSource {
    ChangeSource::Worker {
        name: "test-worker".to_owned(),
    }
}

fn session_with(entries: Vec<ChatEntry>) -> ChatSessionState {
    let mut session = ChatSessionState::new();
    for entry in entries {
        session.edit_history().append(entry);
    }
    session
}

fn entry_ids(session: &ChatSessionState) -> Vec<ChatEntryId> {
    session.history().iter().map(|e| e.id.clone()).collect()
}

#[rstest::rstest]
#[test]
fn set_context_on_tool_call_excludes_whole_loop() {
    // Given a complete loop in history.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);
    let call_id = ids[2].clone();

    // When excluding the call as a worker.
    let changed = session.edit_history().set_context(
        &call_id,
        ContextOverride::ForcedExclude,
        &ChangeSource::Worker {
            name: "test".into(),
        },
    );

    // Then every loop member (assistant, call, result) changed; the user
    // entry did not.
    assert_eq!(
        changed.len(),
        3,
        "assistant+call+result excluded: {changed:?}"
    );
    assert!(!changed.contains(&ids[0]));
    assert!(
        session.history()[1..4]
            .iter()
            .all(|e| e.context_override() == ContextOverride::ForcedExclude)
    );
}

#[rstest::rstest]
#[test]
fn worker_exclude_refused_for_pinned_member() {
    // Given a loop whose result is pinned (skill-load shape).
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);
    session.core.history[3].pin_position = Some(PinPosition::Relative);

    // When a worker excludes the call.
    let changed = session.edit_history().set_context(
        &ids[2],
        ContextOverride::ForcedExclude,
        &ChangeSource::Worker {
            name: "test".into(),
        },
    );

    // Then nothing changed (pin wins).
    assert!(changed.is_empty());
    assert!(
        session.history()[1..4]
            .iter()
            .all(|e| e.context_override() == ContextOverride::Default)
    );
}

#[rstest::rstest]
#[test]
fn worker_include_refused_for_user_excluded_chunk() {
    // Given a loop the user excluded.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);
    session.edit_history().set_context(
        &ids[2],
        ContextOverride::ForcedExclude,
        &ChangeSource::User,
    );

    // When a worker tries to re-include it.
    let changed = session.edit_history().set_context(
        &ids[2],
        ContextOverride::ForcedInclude,
        &ChangeSource::Worker {
            name: "test".into(),
        },
    );

    // Then nothing changed (user exclude wins).
    assert!(changed.is_empty());
    assert_eq!(
        session.history()[1].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn user_exclude_refused_for_pinned_chunk() {
    // Given a pinned loop.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);
    session.core.history[1].pin_position = Some(PinPosition::Relative);

    // When the user excludes a member.
    let changed = session.edit_history().set_context(
        &ids[2],
        ContextOverride::ForcedExclude,
        &ChangeSource::User,
    );

    // Then nothing changed (pin beats user exclude).
    assert!(changed.is_empty());
    assert!(
        session.history()[1..4]
            .iter()
            .all(|e| e.context_override() == ContextOverride::Default)
    );
}

#[rstest::rstest]
#[test]
fn internal_exclude_bypasses_pin_guard() {
    // Given a pinned but incomplete loop (hard-cancel shape).
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("dangling", "bash", "{}"),
    ]);
    session.core.history[1].pin_position = Some(PinPosition::Relative);

    // When the internal dangling sweep runs.
    let changed = session.edit_history().exclude_incomplete_trailing_loops();

    // Then the incomplete loop is excluded despite the pin.
    assert_eq!(changed.len(), 2, "assistant+call excluded: {changed:?}");
    assert_eq!(
        session.history()[1].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        session.history()[2].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn pin_on_result_pins_whole_loop() {
    // Given a complete loop.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);

    // When pinning the result.
    let changed = session.edit_history().pin(&ids[3], PinPosition::Relative);

    // Then all three loop members are pinned.
    assert_eq!(changed.len(), 3);
    assert!(
        session.history()[1..4]
            .iter()
            .all(|e| e.pin_position == Some(PinPosition::Relative))
    );
}

#[rstest::rstest]
#[test]
fn unpin_clears_kind_level_pin_mirror() {
    // Given a loop pinned via the result.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);
    session.edit_history().pin(&ids[3], PinPosition::Relative);

    // When unpinning.
    session.edit_history().unpin(&ids[3]);

    // Then both the entry-level and kind-level pins are cleared.
    assert!(
        session.history()[1..4]
            .iter()
            .all(|e| e.pin_position.is_none())
    );
    assert!(matches!(
        &session.history()[3].kind,
        ChatEntryKind::ToolResult { pin_position, .. } if pin_position.is_none()
    ));
}

#[rstest::rstest]
#[test]
fn insert_standalone_after_advances_past_loop() {
    // Given a complete loop followed by a user entry.
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ChatEntry::user("done"),
    ]);
    let ids = entry_ids(&session);

    // When inserting after the assistant that opened the loop.
    let steer = ChatEntry::user("steer");
    session
        .edit_history()
        .insert_standalone_after(Some(&ids[1]), steer);

    // Then the insertion landed after the loop's last result, not
    // between the call and its result.
    let kinds: Vec<&str> = session
        .history()
        .iter()
        .map(|e| match &e.kind {
            ChatEntryKind::User { .. } => "user",
            ChatEntryKind::Assistant(_) => "assistant",
            ChatEntryKind::ToolCall { .. } => "call",
            ChatEntryKind::ToolResult { .. } => "result",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "call", "result", "user", "user"]
    );
}

#[rstest::rstest]
#[test]
fn insert_standalone_after_plain_entry_inserts_directly() {
    // Given two user entries.
    let mut session = session_with(vec![ChatEntry::user("a"), ChatEntry::user("b")]);
    let ids = entry_ids(&session);

    // When inserting after the first.
    session
        .edit_history()
        .insert_standalone_after(Some(&ids[0]), ChatEntry::user("between"));

    // Then the order is a, between, b.
    let texts: Vec<&str> = session
        .history()
        .iter()
        .filter_map(|e| match &e.kind {
            ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["a", "between", "b"]);
}

#[rstest::rstest]
#[test]
fn normalize_relocates_interior_interstitials_after_loop() {
    // Given a loop with a system entry between call and result.
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::system("status mid-loop"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ChatEntry::user("done"),
    ]);

    // When normalizing.
    session.edit_history().normalize_loop_layout();

    // Then the system entry moved after the result.
    let kinds: Vec<&str> = session
        .history()
        .iter()
        .map(|e| match &e.kind {
            ChatEntryKind::User { .. } => "user",
            ChatEntryKind::Assistant(_) => "assistant",
            ChatEntryKind::ToolCall { .. } => "call",
            ChatEntryKind::ToolResult { .. } => "result",
            ChatEntryKind::System(_) => "system",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "call", "result", "system", "user"]
    );
}

#[rstest::rstest]
#[test]
fn normalize_is_idempotent() {
    // Given an already-normalized history with an interstitial after a loop.
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ChatEntry::system("already outside"),
        ChatEntry::user("done"),
    ]);
    session.edit_history().normalize_loop_layout();
    let before: Vec<ChatEntryId> = entry_ids(&session);

    // When normalizing again.
    session.edit_history().normalize_loop_layout();

    // Then the order is unchanged.
    assert_eq!(before, entry_ids(&session));
}

#[rstest::rstest]
#[test]
fn remove_trailing_removes_descending() {
    // Given a session with five entries.
    let mut session = session_with(vec![
        ChatEntry::user("a"),
        ChatEntry::user("b"),
        ChatEntry::user("c"),
        ChatEntry::user("d"),
        ChatEntry::user("e"),
    ]);

    // When removing indices 2 and 4.
    let removed = session.edit_history().remove_trailing(&[2, 4]);

    // Then both are gone, order otherwise preserved.
    assert_eq!(removed, 2);
    let texts: Vec<&str> = session
        .history()
        .iter()
        .filter_map(|e| match &e.kind {
            ChatEntryKind::User { expanded, .. } => Some(expanded.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["a", "b", "d"]);
}

#[rstest::rstest]
#[test]
fn exclude_incomplete_loops_leaves_complete_loops() {
    // Given one complete loop and one incomplete loop.
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-2", "bash", "{}"),
    ]);

    // When the dangling sweep runs.
    let changed = session.edit_history().exclude_incomplete_trailing_loops();

    // Then only the incomplete loop's members changed.
    assert_eq!(changed.len(), 2);
    assert_eq!(
        session.history()[2].context_override(),
        ContextOverride::Default
    );
    assert_eq!(
        session.history()[4].context_override(),
        ContextOverride::ForcedExclude
    );
    assert_eq!(
        session.history()[5].context_override(),
        ContextOverride::ForcedExclude
    );
}

#[rstest::rstest]
#[test]
fn apply_executes_mutation_batch() {
    // Given a complete loop.
    let mut session = session_with(simple_loop());
    let ids = entry_ids(&session);

    // When applying a batch: worker exclude on the call.
    let changed = session
        .edit_history()
        .apply(vec![HistoryMutation::SetContextOverride {
            entry_id: ids[2].clone(),
            value: ContextOverride::ForcedExclude,
            source: ChangeSource::Worker { name: "w".into() },
        }]);

    // Then the whole loop changed.
    assert_eq!(changed.len(), 3);
}

#[rstest::rstest]
#[test]
fn normalize_preserves_relative_order_of_multiple_interstitials() {
    // Given a loop with two interior interstitials.
    let mut session = session_with(vec![
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::system("first status"),
        ChatEntry::thinking("second thought"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
        ChatEntry::user("done"),
    ]);

    // When normalizing.
    session.edit_history().normalize_loop_layout();

    // Then both interstitials follow the result, in original order.
    let kinds: Vec<&str> = session
        .history()
        .iter()
        .map(|e| match &e.kind {
            ChatEntryKind::User { .. } => "user",
            ChatEntryKind::Assistant(_) => "assistant",
            ChatEntryKind::ToolCall { .. } => "call",
            ChatEntryKind::ToolResult { .. } => "result",
            ChatEntryKind::System(_) => "system",
            ChatEntryKind::Thinking(_) => "thinking",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["assistant", "call", "result", "system", "thinking", "user"],
        "both interstitials must move after the loop, order preserved"
    );
}

#[rstest::rstest]
#[test]
fn normalize_noop_when_no_loops() {
    // Given a history with no tool loops at all.
    let mut session = session_with(vec![
        ChatEntry::user("a"),
        ChatEntry::assistant("b"),
        ChatEntry::system("c"),
        ChatEntry::user("d"),
    ]);
    let before = entry_ids(&session);

    // When normalizing.
    session.edit_history().normalize_loop_layout();

    // Then nothing moved.
    assert_eq!(before, entry_ids(&session));
}

#[rstest::rstest]
#[test]
fn normalize_continues_past_leading_non_loop_entries() {
    // Given a user entry before the loop and an interstitial inside it.
    let mut session = session_with(vec![
        ChatEntry::user("go"),
        ChatEntry::assistant(""),
        ChatEntry::tool_call("call-1", "bash", "{}"),
        ChatEntry::system("status mid-loop"),
        ChatEntry::tool_result("call-1", "bash", "ok", ToolResultStatus::Success),
    ]);

    // When normalizing.
    session.edit_history().normalize_loop_layout();

    // Then the interstitial moved despite the leading user entry.
    let kinds: Vec<&str> = session
        .history()
        .iter()
        .map(|e| match &e.kind {
            ChatEntryKind::User { .. } => "user",
            ChatEntryKind::Assistant(_) => "assistant",
            ChatEntryKind::ToolCall { .. } => "call",
            ChatEntryKind::ToolResult { .. } => "result",
            ChatEntryKind::System(_) => "system",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "call", "result", "system"],
        "scan must not stop at the leading user entry"
    );
}

#[rstest::rstest]
#[test]
fn pinned_skill_result_keeps_whole_loop_through_worker_exclude() {
    // Given a complete loop whose result carries a tool-requested pin
    // (the skill/save_plan shape, as finalize_tool_result now pins it).
    let mut session = session_with(simple_loop());
    let result_id = session.history()[3].id.clone();
    session
        .edit_history()
        .pin(&result_id, PinPosition::Relative);

    // When a prune worker tries to exclude the call half.
    let call_id = session.history()[2].id.clone();
    let changed = session.edit_history().set_context(
        &call_id,
        ContextOverride::ForcedExclude,
        &worker_source(),
    );

    // Then the exclusion is refused and every loop member stays in context.
    assert!(changed.is_empty(), "pin must win: {changed:?}");
    assert!(
        session.history()[1..=3]
            .iter()
            .all(ChatEntry::is_in_context),
        "whole loop remains in context"
    );
}

/// Whether a converted message list satisfies the provider-neutral tool
/// sequence contract (mirrors the tripwire's invariant).
fn sequence_is_valid(messages: &[LlmMessage]) -> bool {
    use std::collections::HashSet;
    let mut open: Option<HashSet<String>> = None;
    for message in messages {
        match message {
            LlmMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                if open.is_some() {
                    return false;
                }
                let ids: HashSet<String> = calls.iter().map(|c| c.id.clone()).collect();
                if ids.len() != calls.len() {
                    return false;
                }
                open = Some(ids);
            }
            LlmMessage::Tool { tool_call_id, .. } => match open.as_mut() {
                Some(remaining) => {
                    if !remaining.remove(tool_call_id) {
                        return false;
                    }
                    if remaining.is_empty() {
                        open = None;
                    }
                }
                None => return false,
            },
            LlmMessage::Assistant {
                tool_calls: None, ..
            }
            | LlmMessage::User { .. } => {
                if open.is_some_and(|remaining| !remaining.is_empty()) {
                    return false;
                }
                open = None;
            }
        }
    }
    open.is_none_or(|remaining| remaining.is_empty())
}

#[rstest::rstest]
#[test]
fn randomized_editor_ops_always_assemble_valid_sequences() {
    use rand::Rng;
    use std::collections::HashSet;

    // Given a seeded generator and a session built from editor ops only.
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(0x5EED);
    let mut session = ChatSessionState::new();
    let mut call_counter = 0usize;
    let mut pinned: HashSet<ChatEntryId> = HashSet::new();

    for step in 0..2000 {
        let history_len = session.history().len();
        if history_len == 0 {
            session.edit_history().append(ChatEntry::user("start"));
            continue;
        }
        let pick = rng.random_range(0..8);
        let random_index = rng.random_range(0..history_len);
        let id = session.history()[random_index].id.clone();
        match pick {
            0 | 1 => {
                session
                    .edit_history()
                    .append(ChatEntry::user(format!("u{step}")));
            }
            2 => {
                session
                    .edit_history()
                    .append(ChatEntry::assistant(format!("a{step}")));
            }
            3 => {
                call_counter += 1;
                session.edit_history().append(ChatEntry::tool_call(
                    format!("c{call_counter}"),
                    "bash",
                    "{}",
                ));
            }
            4 => {
                let call_id = format!("c{}", rng.random_range(1..=(call_counter.max(1))));
                session.edit_history().append(ChatEntry::tool_result(
                    call_id,
                    "bash",
                    "ok",
                    ToolResultStatus::Success,
                ));
            }
            5 => {
                let changed = session.edit_history().set_context(
                    &id,
                    ContextOverride::ForcedExclude,
                    &ChangeSource::Worker {
                        name: "rand".to_owned(),
                    },
                );
                let _ = changed;
            }
            6 => {
                if !pinned.contains(&id) {
                    session.edit_history().pin(&id, PinPosition::Relative);
                    pinned.clear();
                    // After chunk pinning, re-derive which ids are pinned.
                    pinned.extend(
                        session
                            .history()
                            .iter()
                            .filter(|e| e.is_pinned())
                            .map(|e| e.id.clone()),
                    );
                }
            }
            _ => {
                session.edit_history().normalize_loop_layout();
            }
        }

        // Then the assembled message list is always sequence-valid.
        let messages =
            crate::feat::provider::entries_to_messages::entries_to_messages(session.history());
        assert!(
            sequence_is_valid(&messages),
            "step {step} produced an invalid sequence: {messages:?}"
        );
    }
}
