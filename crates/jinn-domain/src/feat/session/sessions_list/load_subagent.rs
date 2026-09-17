//! Opens the child subagent session linked to the selected `task` entry.
//!
//! Normal-mode `<enter>` on a selected `task` tool call **or its result**
//! activates the child session that call spawned: switching to it when it is
//! already in memory, or requesting a disk load through the standard
//! session-load path when it is not. Anything else — no selection, a
//! non-`task` entry, a task without a link — is a no-op.

use crate::common::app_state::AppState;
use crate::feat::session::chat_entry::ChatEntryKind;
use jinn_tools_msg::TASK_TOOL_NAME;
use crate::protocol::{IntentResult, SessionId};
use wherror::Error;

/// Why a selected entry cannot be opened as a subagent session.
#[derive(Debug, Error)]
#[error(debug)]
pub enum LoadSubagentError {
    /// Nothing is selected in the chat history.
    NoSelection,
    /// The selected entry is not a `task` tool call or its result.
    NotTaskCall,
    /// The selected `task` call/result carries no child-session link.
    NoChildLink,
}

/// Resolves the child session linked to the selected `task` entry.
///
/// Accepts both halves of the task pair — the tool call and its result.
///
/// # Errors
///
/// Returns [`LoadSubagentError::NoSelection`] when nothing is selected,
/// [`LoadSubagentError::NotTaskCall`] when the selection is not a `task`
/// tool call or result, and [`LoadSubagentError::NoChildLink`] when the
/// selection predates link stamping or otherwise carries no link.
pub fn validate_load_subagent_session(state: &AppState) -> Result<SessionId, LoadSubagentError> {
    let entry = state
        .active_session()
        .selected_entry()
        .ok_or(LoadSubagentError::NoSelection)?;

    let call_id = match &entry.kind {
        ChatEntryKind::ToolCall { id, name, .. } if name == TASK_TOOL_NAME => id,
        ChatEntryKind::ToolResult { id, name, .. } if name == TASK_TOOL_NAME => id,
        _ => return Err(LoadSubagentError::NotTaskCall),
    };

    let history = state.active_session().history();
    history
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            ChatEntryKind::ToolCall {
                id, child_session, ..
            } if id == call_id => child_session.clone(),
            _ => None,
        })
        .ok_or(LoadSubagentError::NoChildLink)
}

/// Opens the selected `task` call's or result's child session.
///
/// Loads the child from disk through the standard `SessionLoadRequested`
/// path when it is not in memory (the load flow unarchives and emits
/// `ActiveSessionChanged` on completion). Error paths are silent no-ops —
/// validation failure simply leaves the selection untouched.
pub fn handle_load_subagent_session(state: &mut AppState) -> IntentResult {
    let Ok(child_id) = validate_load_subagent_session(state) else {
        return IntentResult::empty();
    };

    if state.session.get(&child_id).is_some() {
        state.session.set_active(child_id);
        return IntentResult::empty();
    }

    state.session.begin_load(child_id.clone());
    IntentResult::new_message(
        crate::feat::session::protocol::session_load_requested::SessionLoadRequested {
            session_id: child_id,
        },
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::feat::session::chat_entry::ChatEntryKind;
    use crate::protocol::{ChatEntry, SessionId};

    /// Builds a `task` tool-call entry carrying `link` (or none when `None`).
    fn task_call_entry(link: Option<SessionId>) -> ChatEntry {
        let mut entry =
            ChatEntry::tool_call("tc_load_test", TASK_TOOL_NAME, r#"{"prompt": "hello"}"#);
        if let Some(child) = link
            && let ChatEntryKind::ToolCall { child_session, .. } = &mut entry.kind
        {
            *child_session = Some(child);
        }
        entry
    }

    /// Builds an AppState whose active session has `entry` selected.
    fn state_with_selected(entry: ChatEntry) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().push_entry(entry);
        state.active_session_mut().select_prev_entry();
        state
    }

    #[rstest::rstest]
    fn validate_rejects_when_nothing_is_selected() {
        // Given an AppState with an empty history.
        let state = AppState::default_with_scope_focus();

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NoSelection.
        assert!(matches!(result, Err(LoadSubagentError::NoSelection)));
    }

    #[rstest::rstest]
    fn validate_rejects_non_task_call_selection() {
        // Given a selected user entry.
        let state = state_with_selected(ChatEntry::user("hello"));

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NotTaskCall.
        assert!(matches!(result, Err(LoadSubagentError::NotTaskCall)));
    }

    #[rstest::rstest]
    fn validate_rejects_non_task_tool_call() {
        // Given a selected `read` tool call.
        let state = state_with_selected(ChatEntry::tool_call(
            "tc_read",
            "read",
            r#"{"path": "a.rs"}"#,
        ));

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NotTaskCall.
        assert!(matches!(result, Err(LoadSubagentError::NotTaskCall)));
    }

    #[rstest::rstest]
    fn validate_rejects_task_call_without_link() {
        // Given a selected task call with no child link.
        let state = state_with_selected(task_call_entry(None));

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NoChildLink.
        assert!(matches!(result, Err(LoadSubagentError::NoChildLink)));
    }

    /// Builds the success result paired with a `task` call entry.
    fn task_result_entry(call_id: &str) -> ChatEntry {
        ChatEntry::tool_result(
            call_id,
            TASK_TOOL_NAME,
            "child output",
            crate::feat::session::tool_result_status::ToolResultStatus::Success,
        )
    }

    #[rstest::rstest]
    fn validate_rejects_task_result_without_link() {
        // Given a selected task result whose call carries no link.
        let mut state = AppState::default_with_scope_focus();
        {
            let s = state.active_session_mut();
            s.push_entry(task_call_entry(None));
            s.push_entry(task_result_entry("tc_load_test"));
            s.select_prev_entry();
        }

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NoChildLink.
        assert!(matches!(result, Err(LoadSubagentError::NoChildLink)));
    }

    #[rstest::rstest]
    fn validate_rejects_non_task_result_selection() {
        // Given a selected non-task tool result.
        let mut state = AppState::default_with_scope_focus();
        {
            let s = state.active_session_mut();
            s.push_entry(ChatEntry::tool_call("tc_read", "read", "{}"));
            s.push_entry(ChatEntry::tool_result(
                "tc_read",
                "read",
                "out",
                crate::feat::session::tool_result_status::ToolResultStatus::Success,
            ));
            s.select_prev_entry();
        }

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation fails with NotTaskCall.
        assert!(matches!(result, Err(LoadSubagentError::NotTaskCall)));
    }

    #[rstest::rstest]
    fn validate_resolves_linked_task_call_to_child_id() {
        // Given a selected task call linked to a child session.
        let child_id = SessionId::new();
        let state = state_with_selected(task_call_entry(Some(child_id.clone())));

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation resolves to the child session id.
        assert_eq!(result.expect("should resolve"), child_id);
    }

    #[rstest::rstest]
    fn validate_resolves_linked_task_result_via_paired_call() {
        // Given a selected task result whose call is linked to a child.
        let child_id = SessionId::new();
        let mut state = AppState::default_with_scope_focus();
        {
            let s = state.active_session_mut();
            s.push_entry(task_call_entry(Some(child_id.clone())));
            s.push_entry(task_result_entry("tc_load_test"));
            s.select_prev_entry();
        }

        // When validating.
        let result = validate_load_subagent_session(&state);

        // Then validation resolves to the child session id.
        assert_eq!(result.expect("should resolve"), child_id);
    }

    #[rstest::rstest]
    fn handle_activates_in_memory_child_session() {
        // Given a selected task call whose child is already in memory.
        let child_id = SessionId::new();
        let mut state = state_with_selected(task_call_entry(Some(child_id.clone())));
        state.session.get_or_create(&child_id);

        // When handling.
        let result = handle_load_subagent_session(&mut state);

        // Then the child session becomes the active session.
        assert_eq!(state.session.active_session_id(), &child_id);
        // And no messages are emitted.
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    fn handle_requests_disk_load_for_unloaded_child() {
        // Given a selected task call whose child is not in memory.
        let child_id = SessionId::new();
        let mut state = state_with_selected(task_call_entry(Some(child_id.clone())));

        // When handling.
        let result = handle_load_subagent_session(&mut state);

        // Then a SessionLoadRequested message is emitted for the child.
        assert_eq!(result.message_names.len(), 1);
        assert!(
            result.message_names[0].ends_with("SessionLoadRequested"),
            "expected SessionLoadRequested, got {:?}",
            result.message_names
        );
        // And the session map entered the loading state.
        assert!(state.session.is_loading());
    }

    #[rstest::rstest]
    fn handle_is_noop_on_validation_failure() {
        // Given a selected non-task entry.
        let mut state = state_with_selected(ChatEntry::user("hello"));

        // When handling.
        let result = handle_load_subagent_session(&mut state);

        // Then nothing is emitted and no load was started.
        assert!(result.messages.is_empty());
        assert!(!state.session.is_loading());
    }
}
