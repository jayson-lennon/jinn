//! MCP server picker intent handlers.
//!
//! The MCP picker lists every server declared in `jinn.toml` under
//! `[[mcp_server]]` and lets the user toggle which ones are enabled for the
//! active session. Enablement is an opt-in set persisted in `SessionCore`;
//! only the toggled-on servers spawn an `McpActor` (Phase 4 wiring).
//!
//! Like the tool/skill pickers, toggles mutate the picker entries in memory and
//! a snapshot of the pre-edit enabled set is taken on open so ESC can revert.
//! [`handle_picker_confirm`](crate::feat::picker::intent::handle_picker_confirm)
//! commits the final set via [`confirm_mcp`].

use std::collections::BTreeSet;

use crate::common::app_state::AppState;
use crate::feat::intent::IntentResult;
use crate::feat::mcp::picker_entry::{McpPreviewMode, McpServerEntry};
use crate::feat::picker::geometry::active_viewport;
use crate::feat::ui::picker_states::PickerExt;
use crate::{ChatEntry, PushChatEntry};

/// Populates the MCP server picker entries from configured servers.
///
/// Reads `state.frontend.preferences.mcp_server` and marks each entry enabled
/// according to the active session's `enabled_mcp_servers` set.
pub(crate) fn load_mcp_picker_entries(state: &mut AppState) {
    let enabled = state.active_session().enabled_mcp_servers().clone();
    let theme = state.frontend.theme.clone();

    let mut entries: Vec<McpServerEntry> = state
        .frontend
        .preferences
        .mcp_server
        .iter()
        .map(|(name, server)| {
            let name = name.clone();
            let description = server.description_for_picker();
            McpServerEntry {
                name: name.clone(),
                description,
                search_text: name.clone(),
                enabled: enabled.contains(&name),
                theme: theme.clone(),
                status: None,
                stderr_tail: String::new(),
                tools: Vec::new(),
                preview_mode: McpPreviewMode::default(),
            }
        })
        .collect();

    entries.sort_by_key(|e| e.name.to_lowercase());

    state.frontend.mcp_server_picker_mut().set_items(entries);
}

/// Toggles the `enabled` state of the currently selected MCP server entry.
///
/// Mutates the picker entry only; the session set is written on confirm.
pub fn handle_mcp_toggle(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .mcp_server_picker_mut()
        .with_selected_mut(|entry| {
            entry.enabled = !entry.enabled;
        });
    let viewport = active_viewport(state);
    state.frontend.mcp_server_picker_mut().move_down(viewport);
    IntentResult::empty()
}

/// Confirms the MCP server picker: collects enabled server names from the
/// picker entries and writes them to the active session's enabled set.
///
/// The session set is the source of truth for which `McpActor`s should be
/// running. Spawning/killing the actors is wired in Phase 4 (lifecycle
/// wiring); this handler only persists the enablement decision and pops the
/// picker scope.
pub(crate) fn confirm_mcp(state: &mut AppState) -> IntentResult {
    let session_id = state.active_session().session_id().clone();
    let enabled: BTreeSet<String> = state
        .frontend
        .mcp_server_picker()
        .items()
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| entry.name.clone())
        .collect();

    state
        .active_session_mut()
        .set_enabled_mcp_servers(enabled.clone());
    *state.frontend.mcp_server_picker_snapshot_mut() = None;
    state.frontend.scope_pop();

    // Signal the MCP lifecycle actor to spawn/kill `McpActor`s for the diff
    // between this desired set and the currently-running ones.
    IntentResult::new_message(
        crate::feat::mcp_coordinator_actor::protocol::McpEnablementChanged {
            session_id,
            enabled,
        },
    )
}

/// Restarts the selected MCP server's connection by signaling the
/// coordinator to kill and respawn its `McpActor`.
///
/// Does not pop the picker; the inspector stays open so the user can watch
/// the status cycle. Only acts on the currently selected entry.
pub fn handle_mcp_restart_selected(state: &mut AppState) -> IntentResult {
    let Some(server) = state
        .frontend
        .mcp_server_picker()
        .selected_item()
        .map(|e| e.name.clone())
    else {
        return IntentResult::empty();
    };
    let session_id = state.active_session().session_id().clone();

    IntentResult::empty()
        .with_message(
            crate::feat::mcp_coordinator_actor::protocol::RestartMcpServer {
                session_id: session_id.clone(),
                server,
            },
        )
        .with_message(PushChatEntry {
            session_id,
            entry: ChatEntry::transient("Restarting MCP server"),
        })
}

/// Toggles the MCP inspector preview pane between logs and tools.
///
/// Flips the selected entry's `preview_mode` in place; the next render shows
/// the other pane.
pub fn handle_mcp_toggle_preview(state: &mut AppState) -> IntentResult {
    state
        .frontend
        .mcp_server_picker_mut()
        .with_selected_mut(|entry| {
            entry.preview_mode = match entry.preview_mode {
                McpPreviewMode::Logs => McpPreviewMode::Tools,
                McpPreviewMode::Tools => McpPreviewMode::Logs,
            };
        });
    IntentResult::empty()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::feat::theme::default_theme;

    fn state_with_selected_mcp_entry(name: &str) -> AppState {
        let mut state = AppState::default_with_scope_focus();
        let entry =
            McpServerEntry::new(name.to_owned(), "npx ...".to_owned(), true, default_theme());
        state
            .frontend
            .mcp_server_picker_mut()
            .set_items(vec![entry]);
        state
    }

    #[rstest::rstest]
    fn restart_selected_emits_restart_command_for_selected_server() {
        // Given the MCP inspector open with "excalimate" selected.
        let mut state = state_with_selected_mcp_entry("excalimate");

        // When restarting the selected server.
        let result = handle_mcp_restart_selected(&mut state);

        // Then a RestartMcpServer message is emitted for the active session.
        assert!(result.message_names[0].contains("RestartMcpServer"));
    }

    #[rstest::rstest]
    fn restart_selected_with_no_selection_emits_nothing() {
        // Given the MCP inspector open with no items.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.mcp_server_picker_mut().set_items(vec![]);

        // When restarting.
        let result = handle_mcp_restart_selected(&mut state);

        // Then nothing is emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn restart_selected_keeps_picker_open() {
        // Given the MCP inspector open.
        let mut state = state_with_selected_mcp_entry("excalimate");
        state.frontend.scope_push(crate::FocusScope::Picker {
            kind: crate::PickerKind::McpServer,
        });

        // When restarting.
        let _ = handle_mcp_restart_selected(&mut state);

        // Then the picker scope is still on the stack.
        assert!(matches!(
            state.frontend.scope(),
            crate::FocusScope::Picker {
                kind: crate::PickerKind::McpServer
            }
        ));
    }

    #[rstest::rstest]
    fn toggle_preview_flips_logs_to_tools() {
        // Given the MCP inspector with the selected entry defaulting to Logs mode.
        let mut state = state_with_selected_mcp_entry("excalimate");
        assert_eq!(
            state
                .frontend
                .mcp_server_picker()
                .selected_item()
                .expect("entry")
                .preview_mode,
            McpPreviewMode::Logs
        );

        // When toggling preview.
        let _ = handle_mcp_toggle_preview(&mut state);

        // Then the selected entry is now in Tools mode.
        assert_eq!(
            state
                .frontend
                .mcp_server_picker()
                .selected_item()
                .expect("entry")
                .preview_mode,
            McpPreviewMode::Tools
        );
    }

    #[rstest::rstest]
    fn toggle_preview_flips_tools_back_to_logs() {
        // Given the MCP inspector with the selected entry already in Tools mode.
        let mut state = state_with_selected_mcp_entry("excalimate");
        state
            .frontend
            .mcp_server_picker_mut()
            .with_selected_mut(|e| e.preview_mode = McpPreviewMode::Tools);

        // When toggling preview.
        let _ = handle_mcp_toggle_preview(&mut state);

        // Then the selected entry is back in Logs mode.
        assert_eq!(
            state
                .frontend
                .mcp_server_picker()
                .selected_item()
                .expect("entry")
                .preview_mode,
            McpPreviewMode::Logs
        );
    }

    #[rstest::rstest]
    fn toggle_preview_emits_no_commands() {
        // Given the MCP inspector open.
        let mut state = state_with_selected_mcp_entry("excalimate");

        // When toggling preview.
        let result = handle_mcp_toggle_preview(&mut state);

        // Then no messages are emitted (pure state flip).
        assert!(result.message_names.is_empty());
    }
}
