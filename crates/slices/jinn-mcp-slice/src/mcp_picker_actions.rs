// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! The MCP server inspector's behavior, over its own cell.
//!
//! Everything the inspector mutates lives in [`McpPickerState`]. Where the
//! result belongs outside the cell — the session's enabled set, a restart
//! request — the function *returns* it and the route action publishes or
//! applies it, because a cell guard cannot be held across an `.await`.

use std::collections::BTreeSet;

use jinn_llm_support::token_estimator::{TiktokenCounter, toolset_context_tokens};
use jinn_mcp_msg::McpPreviewMode;
use jinn_mcp_msg::McpServerEntry;
use jinn_picker::PickerEntry;
use jinn_slices::RenderFacts;
use jinn_slices::cell::TypedCell;

/// The inspector's cell.
type McpPickerCell = TypedCell<jinn_mcp_msg::McpPickerState>;

/// The server list, as data.
type List = jinn_mcp_msg::McpPickerState;

/// Builds the inspector's rows from the configured servers and the session's
/// enabled set.
///
/// Delegates to the shared builder in `jinn-mcp-msg`
/// ([`jinn_mcp_msg::build_rows`]) — the same builder the web modal
/// consumes, so the two surfaces cannot drift in order or enabled
/// marks — and lifts the rows into theme-carrying entries. Entries are
/// marked enabled per the session's `enabled_mcp_servers` and sorted
/// case-insensitively by name (the shared builder's key, which the
/// picker tests pin). Live status, stderr, and tools start empty; the
/// render pass fills them for the highlighted server. Loading never
/// touches the filesystem.
#[must_use]
pub fn build_entries(
    servers: &[(String, String)],
    enabled: &BTreeSet<String>,
    theme: &jinn_theme::Theme,
) -> Vec<McpServerEntry> {
    jinn_mcp_msg::build_rows(servers, enabled, &std::collections::BTreeMap::new())
        .into_iter()
        .map(|row| {
            let mut entry =
                McpServerEntry::new(row.name, row.description, row.enabled, theme.clone());
            entry.status = row.status;
            entry
        })
        .collect()
}

/// Wraps entries for the selection widget, with the inspector's row, filter,
/// and preview hooks.
#[must_use]
pub fn wrap_entries(entries: Vec<McpServerEntry>) -> Vec<PickerEntry<McpServerEntry>> {
    jinn_picker::make_items_with_hooks(
        entries,
        jinn_picker::PickerItemHooks::new()
            .row(crate::mcp_picker_render::mcp_server_row)
            .search(|entry: &McpServerEntry| format!("{} {}", entry.name, entry.description))
            .preview(crate::mcp_picker_render::mcp_picker_preview),
    )
}

/// Starts a fresh, unfiltered list.
pub fn reset(state: &mut List) {
    state.selection.reset();
    state.preview_scroll = 0;
}

/// Installs freshly loaded rows, keeping the filter and highlight.
pub fn load(state: &mut List, entries: Vec<McpServerEntry>) {
    state.selection.set_items(wrap_entries(entries));
}

/// Parks the session the inspector is open for, so the render pass's live
/// refresh can read it back without app state.
pub fn set_session(state: &mut List, session_id: jinn_core_types::SessionId) {
    state.session_id = Some(session_id);
}

/// The list the navigation and filter keys operate on.
#[must_use]
pub fn list(state: &List) -> &jinn_mcp_msg::McpServerList {
    &state.selection
}

/// The list the navigation and filter keys operate on, mutably.
#[must_use]
pub fn list_mut(state: &mut List) -> &mut jinn_mcp_msg::McpServerList {
    &mut state.selection
}

/// TAB: flip the highlighted server's enabled flag, then advance the
/// highlight by the measured viewport (checklist style).
pub fn toggle_highlighted(state: &mut List) {
    state
        .selection
        .with_selected_mut(|item| item.entry_mut().enabled = !item.entry().enabled);
    let viewport = state.results_viewport;
    state.selection.move_down(viewport);
}

/// CTRL+T: flip the highlighted server's preview pane between logs and
/// tools, in place — the next render shows the other pane.
pub fn toggle_preview(state: &mut List) {
    state.selection.with_selected_mut(|item| {
        item.entry_mut().preview_mode = match item.entry().preview_mode {
            McpPreviewMode::Logs => McpPreviewMode::Tools,
            McpPreviewMode::Tools => McpPreviewMode::Logs,
        }
    });
}

/// The highlighted server's name, if a row is highlighted.
#[must_use]
pub fn highlighted_name(state: &List) -> Option<String> {
    state
        .selection
        .selected_item()
        .map(|item| item.entry().name.clone())
}

/// The names of every enabled server.
#[must_use]
pub fn enabled_names(state: &List) -> BTreeSet<String> {
    state
        .selection
        .items()
        .iter()
        .filter(|item| item.entry().enabled)
        .map(|item| item.entry().name.clone())
        .collect()
}

/// How many rows are enabled.
#[must_use]
pub fn enabled_count(state: &List) -> usize {
    state
        .selection
        .items()
        .iter()
        .filter(|item| item.entry().enabled)
        .count()
}

/// Takes the pre-open enabled set, for the `ESC` revert.
///
/// `None` when the snapshot was already consumed by a confirm, which is the
/// defensive case: a confirm makes the commit authoritative and leaves no
/// revert behind.
pub fn take_snapshot(state: &mut List) -> Option<BTreeSet<String>> {
    state.snapshot.take()
}

/// Snapshots the current enabled set, for a later `ESC` revert.
pub fn set_snapshot(state: &mut List, enabled: BTreeSet<String>) {
    state.snapshot = Some(enabled);
}

/// Clears the snapshot after a commit, so a later `ESC` cannot undo it.
pub fn clear_snapshot(state: &mut List) {
    state.snapshot = None;
}

/// Refreshes the highlighted server's live half from the MCP runtime and the
/// tool registry.
///
/// Called once per frame from the render pass, so the inspector keeps
/// ticking — status cycling, stderr arriving, tools appearing — while it is
/// open. Reads only cells, so it needs no app state.
pub fn refresh_inspector_snapshot(cell: &McpPickerCell, facts: &RenderFacts) {
    let (server_name, session_id) = {
        let state = cell.read();
        (highlighted_name(&state), state.session_id.clone())
    };
    let (Some(server_name), Some(session_id)) = (server_name, session_id) else {
        return;
    };
    let (status, stderr_tail, tools, session_defs) = {
        let runtime = facts
            .slices
            .reader::<jinn_mcp_msg::McpRuntimeState>(&jinn_mcp_msg::mcp_runtime_slot());
        let (status, stderr_tail) = runtime.map_or((None, String::new()), |runtime| {
            let runtime = runtime.read();
            // Singleton status lives outside the per-session bucket (the
            // shared actor has no owning session), so read through the
            // effective projection — otherwise the inspector shows a running
            // singleton as perpetually starting.
            (
                runtime
                    .effective_statuses(&session_id)
                    .get(&server_name)
                    .copied(),
                runtime
                    .effective_stderr(&session_id)
                    .get(&server_name)
                    .cloned()
                    .unwrap_or_default(),
            )
        });
        let defs = facts
            .slices
            .reader::<jinn_tools_msg::ToolRegistry>(&jinn_tools_msg::tools_registry_slot())
            .map_or_else(Vec::new, |registry| {
                registry.read().tools_for_session(&session_id)
            });
        let tools = tools_for(&server_name, &defs);
        (status, stderr_tail, tools, defs)
    };

    cell.update(|state| {
        state.selection.with_selected_mut(|item| {
            let entry = item.entry_mut();
            entry.status = status;
            entry.stderr_tail = stderr_tail;
            entry.tools = tools;
        });
        // The figure follows the live registry, guarded by its signature so
        // an unchanged registry never re-tokenizes.
        refresh_cost_figure(state, &session_defs);
    });
}

/// The tools a server advertises, namespaced and stripped to
/// `(local_name, description)` pairs.
#[must_use]
pub fn tools_for(
    server_name: &str,
    defs: &[jinn_core_types::ToolDefinition],
) -> Vec<(String, String)> {
    let prefix = provider_prefix(server_name);
    defs.iter()
        .filter(|definition| definition.name.starts_with(&prefix))
        .map(|definition| {
            (
                definition
                    .name
                    .strip_prefix(prefix.as_str())
                    .unwrap_or(&definition.name)
                    .to_owned(),
                definition.description.clone(),
            )
        })
        .collect()
}

/// The tool-name prefix a server's tools carry.
#[must_use]
pub fn provider_prefix(server_name: &str) -> String {
    jinn_mcp::provider_prefix(server_name)
}

/// The registered tools of the enabled servers: every `mcp__`-prefixed
/// definition in the session's registry.
///
/// Registration is per-session and tracks enablement — a disabled server's
/// tools are not registered, and a still-connecting server has registered
/// none yet — so the prefix filter over the session's definitions *is* the
/// enabled set; no second enablement check is applied here.
#[must_use]
pub fn registered_mcp_defs(
    defs: &[jinn_core_types::ToolDefinition],
) -> Vec<jinn_core_types::ToolDefinition> {
    defs.iter()
        .filter(|definition| definition.name.starts_with("mcp__"))
        .cloned()
        .collect()
}

/// Byte-length change signature over a definition set:
/// `(count, Σ name + description + schema byte lengths)`.
fn registry_signature(defs: &[jinn_core_types::ToolDefinition]) -> (usize, usize) {
    let total: usize = defs
        .iter()
        .map(|definition| {
            definition.name.len()
                + definition.description.len()
                + definition.parameters.to_string().len()
        })
        .sum();
    (defs.len(), total)
}

/// Recomputes the enabled-servers cost figure when the registry has moved.
///
/// The refresh path calls this every frame; the signature guard keeps the
/// tokenizer idle on any frame where nothing registered or unregistered.
/// MCP-mapped tools carry no prompt snippet or guidelines, so the figure
/// degrades to the schema sum — exactly this picker's contribution to the
/// request. Toggling a row never calls this: tools register and unregister
/// only when a confirm reconciles the connection set, so consumption has not
/// changed until it has.
pub fn refresh_cost_figure(state: &mut List, session_defs: &[jinn_core_types::ToolDefinition]) {
    let mcp_defs = registered_mcp_defs(session_defs);
    let sig = registry_signature(&mcp_defs);
    if state.registry_sig == Some(sig) {
        return;
    }
    let counter = TiktokenCounter::o200k_base();
    state.context_tokens = toolset_context_tokens(&mcp_defs, &counter);
    state.context_definitions = mcp_defs;
    state.registry_sig = Some(sig);
}
