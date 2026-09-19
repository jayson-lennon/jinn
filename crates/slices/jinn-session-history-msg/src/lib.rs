//! Session-history crossing contracts.
//!
//! The conversation-history message family, consolidated from three kernel
//! protocol homes (chat_input, context, session) in the session-history
//! window. Every publisher and consumer is kameo-side; these are plain
//! `BusMessage` types — only the two events a plugin coordinator mirrors
//! carry crossing schemas, unchanged.
//!
//! Contracts:
//!
//! - [`PushChatEntry`] — add an entry to a session's history (any component).
//! - [`HistoryAppended`] — emitted after an entry lands (mid-turn compaction
//!   triggers).
//! - [`SubmitHistoryMutations`] — worker batches of `HistoryMutation`.
//! - [`PinChatEntry`] / [`UnpinChatEntry`] — pin management (sidebar pins,
//!   intent handler).
//! - [`ChatEntryPinChanged`] — emitted after a pin state change.
//! - [`CitationsReceived`] — url-citation annotations from a completed stream.
//! - [`TaskListUpdated`] — a todo-list tool mutated the task list.

use jinn_core_types::{ChatEntry, ChatEntryId, HistoryMutation, PinPosition, SessionId};
use serde::{Deserialize, Serialize};

/// Push a chat entry into the conversation history.
///
/// Any component or actor can send this to add an entry to the chat log.
/// (Formerly `jinn-domain` `chat_input` protocol.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushChatEntry {
    /// The session this entry belongs to.
    pub session_id: SessionId,
    /// The chat entry to add.
    pub entry: ChatEntry,
}

impl jinn_slices::BusMessage for PushChatEntry {}

/// Emitted when a new entry is appended to the session history.
///
/// Carries no token count - the compaction actor reads `context_size()`
/// directly from session state, which uses the tiktoken-based count
/// from the last prompt assembly. This ensures the threshold check
/// and the status bar display use the same value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryAppended {
    /// The session whose history was appended to.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for HistoryAppended {}

jinn_slices::crossing_schema!(HistoryAppended, "HistoryAppended",
trouper::schema::SchemaKind::Event,
description: "A new entry was appended to a session's history.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);

/// Submit a batch of history mutations for deferred application.
///
/// Workers produce `Vec<HistoryMutation>` batches and send them via this
/// command. The session actor queues these in `pending_mutations`. They are
/// applied at the next safe drain point (tool batch completion or stream
/// completion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitHistoryMutations {
    /// The session to apply mutations to.
    pub session_id: SessionId,
    /// The mutation batch. Empty batches are silently ignored.
    pub mutations: Vec<HistoryMutation>,
}

impl jinn_slices::BusMessage for SubmitHistoryMutations {}

/// Pin a chat entry so it survives context management strategies.
///
/// The entry will be positioned according to `position` in the assembled prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinChatEntry {
    /// The session containing the entry.
    pub session_id: SessionId,
    /// The entry to pin.
    pub entry_id: ChatEntryId,
    /// Where the pinned entry should appear in the assembled prompt.
    pub position: PinPosition,
}

impl jinn_slices::BusMessage for PinChatEntry {}

/// Remove the pin from a chat entry, allowing normal context management.
///
/// If the entry is not pinned, this is a no-op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpinChatEntry {
    /// The session containing the entry.
    pub session_id: SessionId,
    /// The entry to unpin.
    pub entry_id: ChatEntryId,
}

impl jinn_slices::BusMessage for UnpinChatEntry {}

/// A chat entry's pin state changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatEntryPinChanged {
    /// The session whose pin state changed.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for ChatEntryPinChanged {}

jinn_slices::crossing_schema!(ChatEntryPinChanged, "ChatEntryPinChanged",
trouper::schema::SchemaKind::Event,
description: "A chat entry's pin state changed.",
fields: ["session_id" => trouper::schema::FieldTy::Uuid]);

/// Emitted by the LLM actor when a completed stream accumulated one or more
/// `url_citation` annotations.
///
/// Carries the full citation list so the session actor can record a single
/// grouped `Annotation` entry for the turn. Annotations never re-enter LLM
/// context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationsReceived {
    /// The session the citations belong to.
    pub session_id: SessionId,
    /// The accumulated `url_citation` annotations for the turn.
    pub citations: Vec<jinn_core_types::UrlCitation>,
}

impl jinn_slices::BusMessage for CitationsReceived {}

/// A task list mutation was applied successfully.
///
/// Broadcast after any todo list tool modifies the task list (add phase, add task,
/// complete task, postpone task, postpone to phase, or set list).
/// The session actor subscribes to this event to persist the updated task list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskListUpdated {
    /// The session whose task list was updated.
    pub session_id: SessionId,
}

impl jinn_slices::BusMessage for TaskListUpdated {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]
    use super::*;
    use jinn_core_types::ChatEntryKind;

    #[rstest::rstest]
    fn push_chat_entry_serializes_roundtrip() {
        // Given a push command carrying a user entry.
        let cmd = PushChatEntry {
            session_id: SessionId::new(),
            entry: ChatEntry::user("hello"),
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: PushChatEntry = serde_json::from_str(&json).expect("deserialize");

        // Then the roundtrip preserves both fields.
        assert_eq!(cmd.session_id, back.session_id);
        assert_eq!(cmd.entry.kind, back.entry.kind);
    }

    #[rstest::rstest]
    fn submit_history_mutations_serializes_roundtrip() {
        // Given a mutation batch command.
        let cmd = SubmitHistoryMutations {
            session_id: SessionId::new(),
            mutations: vec![HistoryMutation::UnpinEntry {
                entry_id: ChatEntryId::new(),
            }],
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: SubmitHistoryMutations = serde_json::from_str(&json).expect("deserialize");

        // Then the roundtrip preserves the mutation.
        assert!(matches!(
            back.mutations.first(),
            Some(HistoryMutation::UnpinEntry { .. })
        ));
    }

    #[rstest::rstest]
    fn pin_commands_serialize_roundtrip() {
        // Given pin and unpin commands sharing ids.
        let session_id = SessionId::new();
        let entry_id = ChatEntryId::new();
        let pin = PinChatEntry {
            session_id: session_id.clone(),
            entry_id: entry_id.clone(),
            position: PinPosition::Top,
        };
        let unpin = UnpinChatEntry {
            session_id,
            entry_id,
        };

        // When serializing and deserializing both.
        let pin_back: PinChatEntry =
            serde_json::from_str(&serde_json::to_string(&pin).expect("pin serialize"))
                .expect("pin deserialize");
        let unpin_back: UnpinChatEntry =
            serde_json::from_str(&serde_json::to_string(&unpin).expect("unpin serialize"))
                .expect("unpin deserialize");

        // Then the roundtrips preserve the fields.
        assert_eq!(pin.position, pin_back.position);
        assert_eq!(unpin.entry_id, unpin_back.entry_id);
    }

    #[rstest::rstest]
    fn citation_and_tasklist_events_serialize_roundtrip() {
        // Given the two simple event payloads.
        let citations = CitationsReceived {
            session_id: SessionId::new(),
            citations: vec![jinn_core_types::UrlCitation {
                url: "https://example.com".to_owned(),
                title: "Source".to_owned(),
                content: None,
                start_index: None,
                end_index: None,
            }],
        };
        let tasks = TaskListUpdated {
            session_id: SessionId::new(),
        };

        // When serializing and deserializing both.
        let citations_back: CitationsReceived =
            serde_json::from_str(&serde_json::to_string(&citations).expect("citations serialize"))
                .expect("citations deserialize");
        let tasks_back: TaskListUpdated =
            serde_json::from_str(&serde_json::to_string(&tasks).expect("tasks serialize"))
                .expect("tasks deserialize");

        // Then the roundtrips preserve the fields.
        assert_eq!(citations.citations, citations_back.citations);
        assert_eq!(tasks.session_id, tasks_back.session_id);
    }

    #[rstest::rstest]
    fn history_appended_and_pin_changed_roundtrip() {
        // Given the two schema'd event payloads.
        let appended = HistoryAppended {
            session_id: SessionId::new(),
        };
        let pin_changed = ChatEntryPinChanged {
            session_id: SessionId::new(),
        };

        // When serializing and deserializing both.
        let appended_back: HistoryAppended =
            serde_json::from_str(&serde_json::to_string(&appended).expect("appended serialize"))
                .expect("appended deserialize");
        let pin_back: ChatEntryPinChanged = serde_json::from_str(
            &serde_json::to_string(&pin_changed).expect("pin-changed serialize"),
        )
        .expect("pin-changed deserialize");

        // Then the roundtrips preserve the fields.
        assert_eq!(appended.session_id, appended_back.session_id);
        assert_eq!(pin_changed.session_id, pin_back.session_id);
        // And the entry payload serializes independently.
        let _ = ChatEntryKind::System("x".to_owned());
    }
}
