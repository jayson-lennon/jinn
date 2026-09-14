//! Picker intent handlers - navigation, filtering, confirmation, and scope toggling.
//!
//! Handles all picker intents: open, insert char, backspace, confirm, move up/down,
//! cursor movement, and keymap scope filter toggle. The `handle_picker_confirm`
//! function returns `(IntentResult, Option<Intent>)` to allow the caller
//! (`jinn-intent`) to re-dispatch keymap intents without creating a circular
//! dependency.

use crate::common::app_state::AppState;
use crate::common::app_state::FocusScope;
use crate::feat::preferences_actor::protocol::app_state_command::{AppStateUpdate, UpdateAppState};
use crate::feat::preferences_actor::protocol::command::{PreferenceUpdate, UpdatePreferences};
use crate::feat::provider::ProviderState;
use crate::feat::provider::picker_entry::PickerEntry;
use crate::feat::provider::protocol::command::{LoadProviderPickerEntries, ProviderSwitch};
use crate::feat::session::model_selection::{AlloyStrategy, ModelSelection};
use crate::feat::session::protocol::load_session_picker_entries::LoadSessionPickerEntries;
use crate::feat::session::protocol::mark_session_interacted::MarkSessionInteracted;
use crate::feat::session::protocol::session_load_requested::SessionLoadRequested;
use crate::feat::tools_actor::tool_entry::ToolEntry;

use crate::feat::ui::picker_states::PickerExt;
use crate::protocol::{Intent, IntentResult, PickerKind};

use super::geometry::active_viewport;
use super::validator;

/// Opens a picker of the given kind. Sets mode to Picker and optionally
/// requests picker entries from the actor system.
pub fn handle_open_picker(
    state: &mut AppState,
    kind: PickerKind,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    if validator::validate_open_picker(state, &kind).is_err() {
        return IntentResult::empty();
    }

    // Endpoint picker is only reachable for a Single (non-alloy) model. The
    // backend gate (OpenRouter vs direct) runs later in the discovery actor,
    // which owns `Services`; here we only reject the model-shape mismatch.
    if matches!(kind, PickerKind::Endpoint)
        && matches!(
            state.active_session().profile().model,
            ModelSelection::Alloy { .. }
        )
    {
        return IntentResult::empty();
    }

    state.frontend.scope_stack.push(FocusScope::Picker { kind });

    // Spec-driven pickers own their open-time preparation entirely —
    // but only when the registry actually holds their spec; otherwise the
    // legacy open path still runs.
    if crate::feat::picker::registry::spec_id_for_kind(&kind)
        .is_some_and(|id| pickers.get(id).is_some())
    {
        return crate::feat::picker::action::run_active_hook(
            state,
            pickers,
            crate::feat::picker::action::Hook::Open,
        );
    }

    reset_picker_for_open(state, kind);

    match kind {
        PickerKind::Provider => IntentResult::new_message(LoadProviderPickerEntries),
        PickerKind::Session => IntentResult::new_message(LoadSessionPickerEntries),
        PickerKind::Persona
        | PickerKind::Skill
        | PickerKind::Theme
        | PickerKind::Tool
        | PickerKind::TaskList
        | PickerKind::Project
        | PickerKind::McpServer
        | PickerKind::Plugin => IntentResult::empty(),
        PickerKind::SessionLifecycle => {
            // Populate from user preferences + implicit blank lifecycle.
            load_lifecycle_picker_entries(state);
            IntentResult::empty()
        }
        PickerKind::CompactionModel => IntentResult::new_message(
            crate::feat::provider::protocol::command::LoadCompactionModelPickerEntries,
        ),

        PickerKind::ReasoningEffort => IntentResult::new_message(
            crate::feat::provider::protocol::command::LoadReasoningEffortPickerEntries,
        ),

        PickerKind::Endpoint => IntentResult::new_message(
            crate::feat::provider::protocol::command::LoadEndpointPickerEntries,
        ),
    }
}

/// Resets and repopulates the target picker's state before it is shown.
///
/// Each arm owns its picker's open-time preparation: reset, snapshot for
/// ESC revert (where applicable), and synchronous entry loading.
fn reset_picker_for_open(state: &mut AppState, kind: PickerKind) {
    match kind {
        PickerKind::Provider => {
            state.provider.provider_picker.reset();
            // Derive alloy mode from the active session's model selection:
            // an existing Alloy opens in alloy mode (with members pre-checked
            // by the loader), anything else opens in single mode.
            state.provider.set_alloy_mode(matches!(
                state.active_session().profile().model,
                ModelSelection::Alloy { .. }
            ));
        }
        PickerKind::Session => {
            state.frontend.session_picker_mut().reset();
        }
        PickerKind::Persona | PickerKind::Skill | PickerKind::Theme => {
            // Spec-driven when the registry holds their spec; nothing to
            // prepare in the legacy path.
        }
        PickerKind::SessionLifecycle => {
            state.frontend.session_lifecycle_picker_mut().reset();
        }
        PickerKind::CompactionModel => {
            state.frontend.compaction_model_picker_mut().reset();
        }
        PickerKind::ReasoningEffort => {
            state.frontend.reasoning_effort_picker_mut().reset();
        }
        PickerKind::Tool => {
            state.frontend.tool_picker_mut().reset();
            // Snapshot current disabled tools for ESC revert.
            *state.frontend.tool_picker_snapshot_mut() =
                Some(state.active_session().disabled_tools().clone());
            load_tool_picker_entries(state);
        }
        PickerKind::TaskList => {
            state.frontend.task_list_picker_mut().reset();
            load_task_list_picker_entries(state);
        }
        PickerKind::Project => {
            // Defensive: a stale stash from an abandoned previous flow must
            // never leak into a new project-picker session.
            state.frontend.pending_creation = None;
            state.frontend.project_picker_mut().reset();
            load_project_picker_entries(&mut state.frontend);
        }
        PickerKind::McpServer => {
            state.frontend.mcp_server_picker_mut().reset();
            // Snapshot current enabled set for ESC revert.
            *state.frontend.mcp_server_picker_snapshot_mut() =
                Some(state.active_session().enabled_mcp_servers().clone());
            crate::feat::mcp::intent::load_mcp_picker_entries(state);
        }
        PickerKind::Plugin => {
            state.frontend.plugin_picker_mut().reset();
            load_plugin_picker_entries(state);
        }
        PickerKind::Endpoint => {
            state.frontend.endpoint_picker_mut().reset();
            state.frontend.pickers.endpoint_loading = true;
        }
    }
}

/// Force-refresh the OpenRouter endpoint picker for the active model, bypassing
/// the in-memory cache (the `<c-r>` keybind).
///
/// Mirrors the open gate: an alloy model is a no-op (the picker does not apply
/// to alloys). Unlike model refresh this does not push a chat entry — it is a
/// picker-local action.
pub fn handle_refresh_endpoints(state: &mut AppState) -> IntentResult {
    // Given the active model is an alloy, the endpoint picker does not apply.
    if matches!(
        state.active_session().profile().model,
        ModelSelection::Alloy { .. }
    ) {
        return IntentResult::empty();
    }

    // Set loading synchronously so the indicator appears this frame, reset the
    // picker, and publish the forced-refresh command.
    state.frontend.pickers.endpoint_loading = true;
    state.frontend.endpoint_picker_mut().reset();
    IntentResult::new_message(
        crate::feat::provider::protocol::command::RefreshEndpointPickerEntries,
    )
}

/// Loads plugins into the plugin picker from the plugin contribution cache.
///
/// One read-only entry per known plugin (name + latest phase), in name
/// order (BTreeMap iteration). Opening the picker never touches the plugin
/// coordinator — it reads whatever phases were last mirrored into the cache.
fn load_plugin_picker_entries(state: &mut AppState) {
    use crate::feat::plugin::PluginPickerEntry;

    let entries: Vec<PluginPickerEntry> = state
        .plugins
        .phases()
        .map(|(name, phase)| {
            PluginPickerEntry::new(name.to_owned(), phase, state.frontend.theme.clone())
        })
        .collect();

    state.frontend.plugin_picker_mut().set_items(entries);
}

/// Resets the preview scroll offset when the active picker's spec opts in
/// (`PreviewSpec::reset_scroll_on_selection_change`).
fn reset_preview_scroll(state: &mut AppState, registry: &jinn_picker::PickerRegistry) {
    let Some(kind) = state.frontend.scope_stack.picker_kind().copied() else {
        return;
    };
    let Some(spec) =
        crate::feat::picker::registry::spec_id_for_kind(&kind).and_then(|id| registry.get(id))
    else {
        return;
    };
    if spec.resets_scroll_on_selection_change() {
        state
            .frontend
            .pickers
            .pickers_scrolls
            .reset(jinn_picker::PickerId::new(spec.id().as_str()));
    }
}

/// Inserts a character into the active picker's filter.
pub fn handle_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    validator::validate_picker_insert_char(state, ch);
    if let Some(picker) = state.active_picker_ops() {
        picker.insert_char(ch);
    }
    IntentResult::empty()
}

/// Handles `PasteText` in picker scope - bulk inserts pasted text into the filter.
///
/// Newlines are stripped by the picker's `insert_text` method since the filter
/// is a single-line input.
pub fn handle_picker_paste(state: &mut AppState, text: &str) -> IntentResult {
    if let Some(picker) = state.active_picker_ops() {
        picker.insert_text(text);
    }
    IntentResult::empty()
}

/// Removes the last character from the active picker's filter.
pub fn handle_backspace(state: &mut AppState) -> IntentResult {
    validator::validate_picker_backspace(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.backspace();
    }
    IntentResult::empty()
}

/// Confirms the active picker selection.
///
/// Returns `(IntentResult, Option<Intent>)`. For Provider and
/// Session pickers, the second element is `None`. For Keymap picker, returns
/// `(IntentResult::empty(), Some(selected_intent))` so the caller can re-dispatch.
pub fn handle_picker_confirm(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> (IntentResult, Option<Intent>) {
    if validator::validate_picker_confirm(state).is_err() {
        return (IntentResult::empty(), None);
    }

    // Spec-driven pickers own their confirm behavior entirely — but only
    // when the registry actually holds their spec; otherwise the legacy
    // confirm path still runs.
    if state
        .frontend
        .scope_stack
        .picker_kind()
        .and_then(crate::feat::picker::registry::spec_id_for_kind)
        .is_some_and(|id| pickers.get(id).is_some())
    {
        return (
            crate::feat::picker::action::run_active_hook(
                state,
                pickers,
                crate::feat::picker::action::Hook::Confirm,
            ),
            None,
        );
    }

    match state.frontend.scope_stack.picker_kind().copied() {
        // Persona, Skill, and Theme are fully spec-driven; the registry
        // guard above runs their confirm hook. Reaching the match means the
        // registry is empty (test seams) — nothing to do.
        Some(PickerKind::Provider) => (confirm_provider(state), None),
        Some(PickerKind::Session) => (confirm_session(state), None),
        Some(PickerKind::SessionLifecycle) => (confirm_session_lifecycle(state), None),
        Some(PickerKind::Project) => (confirm_project(state), None),
        Some(PickerKind::ReasoningEffort) => (confirm_reasoning_effort(state), None),
        Some(PickerKind::McpServer) => (crate::feat::mcp::intent::confirm_mcp(state), None),
        Some(PickerKind::Endpoint) => (confirm_endpoint(state), None),

        Some(
            PickerKind::CompactionModel
            | PickerKind::Persona
            | PickerKind::TaskList
            | PickerKind::Plugin
            | PickerKind::Skill
            | PickerKind::Theme,
        )
        | None => (IntentResult::empty(), None),
        Some(PickerKind::Tool) => (confirm_tool(state), None),
    }
}

/// Moves the selection up in the active picker.
pub fn handle_move_up(state: &mut AppState, pickers: &jinn_picker::PickerRegistry) -> IntentResult {
    validator::validate_picker_move_up(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_up(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Moves the selection down in the active picker.
pub fn handle_move_down(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    validator::validate_picker_move_down(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_down(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Pages the selection up by half the visible window in the active picker.
pub fn handle_page_up(state: &mut AppState, pickers: &jinn_picker::PickerRegistry) -> IntentResult {
    validator::validate_picker_page_up(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.page_up(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Pages the selection down by half the visible window in the active picker.
pub fn handle_page_down(
    state: &mut AppState,
    pickers: &jinn_picker::PickerRegistry,
) -> IntentResult {
    validator::validate_picker_page_down(state);
    let viewport = active_viewport(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.page_down(viewport);
    }
    reset_preview_scroll(state, pickers);
    crate::feat::picker::action::run_selection_change(state, pickers);
    IntentResult::empty()
}

/// Moves the filter cursor left in the active picker.
pub fn handle_move_cursor_left(state: &mut AppState) -> IntentResult {
    validator::validate_picker_move_cursor_left(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_cursor_left();
    }
    IntentResult::empty()
}

/// Moves the filter cursor right in the active picker.
pub fn handle_move_cursor_right(state: &mut AppState) -> IntentResult {
    validator::validate_picker_move_cursor_right(state);
    if let Some(picker) = state.active_picker_ops() {
        picker.move_cursor_right();
    }
    IntentResult::empty()
}

/// Confirms the selected provider and dispatches a switch command.
///
/// In single mode, ENTER commits the highlighted entry as a single model.
/// In alloy mode, ENTER force-includes the highlighted entry (deduped against the
/// checked set) and commits: one model -> `Single`, two or more -> anonymous
/// `Alloy`. The highlighted entry must be available or nothing is committed.
fn confirm_provider(state: &mut AppState) -> IntentResult {
    // Resolve the highlighted entry. It is the foundation of both modes, and
    // its availability gates the entire confirm.
    let Some(highlight) = state.provider.provider_picker.selected_item() else {
        return IntentResult::empty();
    };
    if !highlight.is_available {
        return IntentResult::empty();
    }
    let highlight_id = highlight.provider_id.clone();

    let model_selection = resolve_provider_selection(&state.provider, highlight_id);

    let last_model = Some(model_selection.clone());
    let session_id = state.session.active_session_id().clone();

    state.frontend.scope_stack.pop();
    IntentResult::empty()
        .with_message(ProviderSwitch {
            session_id,
            provider_id: model_selection,
        })
        .with_message(UpdateAppState {
            updates: vec![AppStateUpdate::SetLastModel(last_model)],
        })
}

/// Resolves the provider confirmation decision for the given highlighted entry.
///
/// Single mode: the highlighted entry becomes `ModelSelection::Single`.
/// Alloy mode: the checked set union the highlight (deduped); one model -> `Single`,
/// two or more -> `Alloy`.
fn resolve_provider_selection(provider: &ProviderState, highlighted: String) -> ModelSelection {
    if !provider.is_alloy_mode() {
        return ModelSelection::Single(highlighted);
    }
    let mut models: Vec<String> = provider
        .provider_picker
        .items()
        .iter()
        .filter(|e| e.selected && e.is_available)
        .map(|e| e.provider_id.clone())
        .collect();

    // Force-include the highlighted entry (ENTER adds it before committing).
    if !models.contains(&highlighted) {
        models.push(highlighted);
    }

    if models.len() <= 1 {
        ModelSelection::Single(models.into_iter().next().unwrap_or_default())
    } else {
        ModelSelection::Alloy {
            models,
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        }
    }
}

/// Confirms the selected reasoning effort and applies it.
///
/// Dual-write mirroring [`confirm_persona`]: sets the active session's
/// `reasoning_effort` override in-memory, persists it immediately via
/// [`MarkSessionInteracted`], and updates the last-used seed via
/// [`UpdateAppState`] so new sessions inherit the choice.
fn confirm_reasoning_effort(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.reasoning_effort_picker().selected_item() else {
        return IntentResult::empty();
    };
    let effort = entry.effort;
    let session_id = state.session.active_session_id().clone();

    state.active_session_mut().profile_mut().reasoning_effort = Some(effort);

    state.frontend.scope_stack.pop();

    IntentResult::empty()
        .with_message(MarkSessionInteracted { session_id })
        .with_message(UpdateAppState {
            updates: vec![AppStateUpdate::SetReasoningEffort(Some(effort))],
        })
}

/// Confirms the selected OpenRouter endpoint and pins it on the session profile.
///
/// Writes `profile.endpoint = Some(...)` for a real upstream, or `None` when the
/// "Default (auto-route)" sentinel is chosen (its `tag` is empty). Persists the
/// session immediately via [`MarkSessionInteracted`] so the pin survives reload.
fn confirm_endpoint(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.endpoint_picker().selected_item().cloned() else {
        return IntentResult::empty();
    };
    let endpoint = if entry.tag.is_empty() {
        None
    } else {
        Some(crate::feat::endpoint::Endpoint {
            tag: entry.tag.clone(),
            provider_name: entry.provider_name.clone(),
        })
    };
    let session_id = state.session.active_session_id().clone();

    state.active_session_mut().profile_mut().endpoint = endpoint;

    state.frontend.scope_stack.pop();

    IntentResult::empty().with_message(MarkSessionInteracted { session_id })
}

/// Confirms the selected session and dispatches a switch command.
fn confirm_session(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.session_picker().selected_item() else {
        return IntentResult::empty();
    };
    let session_id = entry.session_id.clone();

    state.session.begin_load(session_id.clone());
    state.frontend.scope_stack.pop();

    IntentResult::new_message(SessionLoadRequested { session_id })
}

/// Populates the lifecycle picker entries from user preferences.
///
/// Always includes the implicit blank lifecycle (no commands, uses default CWD)
/// as the first entry, followed by all lifecycles defined in `jinn.toml`.
fn load_lifecycle_picker_entries(state: &mut AppState) {
    use crate::feat::session_lifecycle::command_template::CommandTemplate;
    use crate::feat::session_lifecycle::picker_entry::SessionLifecycleEntry;

    let mut entries = Vec::new();

    let theme = state.frontend.theme.clone();

    // Always include the implicit blank lifecycle.
    entries.push(SessionLifecycleEntry {
        name: "blank".to_owned(),
        description: Some("New empty session".to_owned()),
        has_args: false,
        theme: theme.clone(),
    });

    // Add lifecycles from preferences.
    for lifecycle in &state.frontend.preferences.session_lifecycles {
        let has_args = lifecycle
            .setup
            .as_ref()
            .and_then(|cmd| match cmd {
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(s) => {
                    Some(s.as_str())
                }
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Builtin(_) => None,
            })
            .is_some_and(|cmd| CommandTemplate::parse(cmd).has_params());
        entries.push(SessionLifecycleEntry {
            name: lifecycle.name.clone(),
            description: lifecycle.description.clone(),
            has_args,
            theme: theme.clone(),
        });
    }

    state
        .frontend
        .session_lifecycle_picker_mut()
        .set_items(entries);
}

/// Confirms the selected session lifecycle.
///
/// If the lifecycle has args, the arg input popup would open (Phase 6).
/// For now, directly triggers setup with empty args (lifecycles without args)
/// or with empty args as a placeholder.
fn confirm_session_lifecycle(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.session_lifecycle_picker().selected_item() else {
        return IntentResult::empty();
    };

    let lifecycle_name = entry.name.clone();
    let has_args = entry.has_args;
    state.frontend.scope_stack.pop();

    if has_args {
        // Save context and open the arg input popup.
        let template_display = state
            .frontend
            .preferences
            .session_lifecycles
            .iter()
            .find(|l| l.name == lifecycle_name)
            .and_then(|l| l.setup.as_ref())
            .and_then(|cmd| match cmd {
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(s) => {
                    Some(s.as_str())
                }
                crate::feat::session_lifecycle::builtin::LifecycleCommand::Builtin(_) => None,
            })
            .map(|cmd| {
                crate::feat::session_lifecycle::command_template::CommandTemplate::parse(cmd)
                    .display()
            })
            .unwrap_or_default();

        state.frontend.arg_input = crate::common::app_state::ArgInputState {
            lifecycle_name,
            template_display,
            text: crate::common::line_input::LineInput::new(),
        };
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::ArgInput);
        return IntentResult::empty();
    }

    // No args - proceed directly.
    crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(
        state,
        &lifecycle_name,
        &[],
        None,
    )
}
/// Marks each entry as enabled/disabled based on the session's `disabled_tools` set.
fn load_tool_picker_entries(state: &mut AppState) {
    let active_session = state.active_session();
    let disabled = active_session.disabled_tools();
    let provider_name = active_session.model_selection().provider_name().to_owned();
    let theme = state.frontend.theme.clone();

    let active_id = state.session.active_session_id().clone();
    let mut entries: Vec<ToolEntry> = state
        .context
        .tools_for_session(&active_id)
        .into_iter()
        .filter(|def| def.available_for_provider(&provider_name))
        .map(|def| {
            let name = def.name.clone();
            let description = def.description.clone();
            ToolEntry {
                name: name.clone(),
                description: description.clone(),
                search_text: format!("{name} {description}"),
                enabled: !disabled.contains(&def.name),
                theme: theme.clone(),
            }
        })
        .collect();

    entries.sort_by_key(|e| e.name.to_lowercase());

    state.frontend.tool_picker_mut().set_items(entries);
}

/// Confirms the tool picker: collects disabled tool names from picker entries
/// and writes them to the active session's profile.
fn confirm_tool(state: &mut AppState) -> IntentResult {
    let disabled: std::collections::HashSet<String> = state
        .frontend
        .tool_picker()
        .items()
        .iter()
        .filter(|entry| !entry.enabled)
        .map(|entry| entry.name.clone())
        .collect();

    state.active_session_mut().set_disabled_tools(disabled);
    *state.frontend.tool_picker_snapshot_mut() = None;
    state.frontend.scope_stack.pop();
    IntentResult::empty()
}

/// Toggles the `enabled` state of the currently selected tool entry.
pub fn handle_tool_toggle(state: &mut AppState) -> IntentResult {
    state.frontend.tool_picker_mut().with_selected_mut(|entry| {
        entry.enabled = !entry.enabled;
    });
    let viewport = active_viewport(state);
    state.frontend.tool_picker_mut().move_down(viewport);
    IntentResult::empty()
}

/// Populates the skill picker entries from discovered skills.
///
/// Populates the task list picker entries from the active session's task list.
///
/// Phases are emitted as tree roots; tasks are emitted as children of their owning
/// phase (parent_id = the phase's id string). Postponed tasks are filtered out,
/// matching the sidebar's `render_text` behavior.
///
/// Empty task lists produce an empty picker - no panic.
fn load_task_list_picker_entries(state: &mut AppState) {
    use crate::feat::theme::default_theme;
    use crate::feat::todo_list::TaskStatus;
    use crate::feat::todo_list::picker_entry::TaskListTreeEntry;

    let theme = default_theme();
    let entries: Vec<TaskListTreeEntry> = state
        .active_session()
        .task_list()
        .phases()
        .iter()
        .flat_map(|phase| {
            let phase_id_str = format!("phase:{}", phase.id());
            let phase_entry = TaskListTreeEntry::new_phase(
                phase_id_str.clone(),
                phase.description().to_owned(),
                theme.clone(),
            );
            let task_entries: Vec<TaskListTreeEntry> = phase
                .tasks()
                .iter()
                .filter(|task| task.status() != TaskStatus::Postponed)
                .map(|task| {
                    TaskListTreeEntry::new_task(
                        format!("task:{}", task.id()),
                        Some(phase_id_str.clone()),
                        task.description().to_owned(),
                        task.status(),
                        theme.clone(),
                    )
                })
                .collect();
            std::iter::once(phase_entry).chain(task_entries)
        })
        .collect();

    state.frontend.task_list_picker_mut().set_items(entries);
}

/// Populates the project picker entries from `UserPreferences.projects`.
///
/// Entries are pre-computed display strings (tilde-compressed) so the picker
/// never has to call `shorten_path` per-render.
pub(crate) fn load_project_picker_entries(
    frontend: &mut crate::feat::ui::frontend_state::FrontendState,
) {
    use crate::feat::project::picker_entry::{ProjectEntry, project_entries};

    let theme = frontend.theme.clone();
    let entries: Vec<ProjectEntry> = project_entries(&frontend.preferences.projects, &theme);
    frontend.project_picker_mut().set_items(entries);
}

/// Confirms the highlighted project: stashes its dir as the pending session
/// creation (project stamp + starting CWD), pops the picker, and delegates to
/// a plain new session.
///
/// `Enter` path: the new session inherits nothing from the active session's
/// CWD - it is created at the chosen project dir via the stash.
fn confirm_project(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item() else {
        return IntentResult::empty();
    };

    // Stash the chosen dir and pop the picker before delegating, so the new
    // session is created in Normal scope at the right CWD. The project stamp
    // and the starting CWD start equal here but are semantically distinct:
    // lifecycle scripts may re-cwd the session, the project never moves.
    state.frontend.pending_creation =
        Some(crate::feat::ui::frontend_state::PendingSessionCreation {
            project_dir: entry.path.clone(),
            starting_cwd: entry.path.clone(),
        });
    state.frontend.scope_stack.pop();

    crate::feat::session_lifecycle::intent::handle_session_lifecycle_setup(state, "", &[], None)
}

/// Confirms the highlighted project and chains into the lifecycle picker.
///
/// `<c-enter>` path: same stash + pop as `confirm_project`, but then opens
/// the session lifecycle picker so the user picks a recipe (and optional args).
/// The stash survives the lifecycle -> arg-input chain because every
/// confirmation path in that chain consumes `pending_creation`.
pub fn handle_project_lifecycle_confirm(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item() else {
        return IntentResult::empty();
    };

    state.frontend.pending_creation =
        Some(crate::feat::ui::frontend_state::PendingSessionCreation {
            project_dir: entry.path.clone(),
            starting_cwd: entry.path.clone(),
        });
    state.frontend.scope_stack.pop();

    // Re-enter the lifecycle picker. `handle_open_picker` pushes a fresh
    // `Picker { SessionLifecycle }` scope.
    handle_open_picker(
        state,
        PickerKind::SessionLifecycle,
        &jinn_picker::PickerRegistry::new(),
    )
}

/// Removes the highlighted project from the curated list (`d`).
///
/// Applies the diff optimistically to `frontend.preferences.projects`,
/// reloads the picker items so the list updates immediately, and emits
/// `UpdatePreferences` so the `PreferencesActor` persists the change.
/// The `PreferencesActor` will also write `frontend.preferences` inline
/// after persisting, reconciling any divergence.
pub fn handle_project_remove_highlighted(state: &mut AppState) -> IntentResult {
    let Some(entry) = state.frontend.project_picker().selected_item().cloned() else {
        return IntentResult::empty();
    };

    state
        .frontend
        .preferences
        .projects
        .retain(|p| p.path != entry.path);
    load_project_picker_entries(&mut state.frontend);

    IntentResult::new_message(UpdatePreferences {
        updates: vec![PreferenceUpdate::RemoveProject(entry.path)],
    })
}

/// Toggles the selected model's `selected` state in the provider picker.
///
/// Used for multi-select alloy building. When toggled on, the entry gets a
/// checkmark and is sorted to the top of the list. The cursor stays put so the
/// user can toggle several adjacent entries without losing their place.
pub fn handle_model_toggle(state: &mut AppState) -> IntentResult {
    // No-op outside alloy mode: single mode never builds checkmarks.
    if !state.provider.is_alloy_mode() {
        return IntentResult::empty();
    }
    state.provider.provider_picker.with_selected_mut(|entry| {
        entry.selected = !entry.selected;
    });

    // Re-sort: selected entries to top, then alphabetical.
    resort_provider_picker(&mut state.provider.provider_picker);

    IntentResult::empty()
}
/// Toggles the provider picker between single-model and alloy-selection modes.
///
/// No-op unless the provider picker is active. When entering alloy mode, the
/// current session model's entries are pre-checked (so editing an existing alloy
/// only requires swapping the desired members). When leaving, all checks are
/// cleared. Either way the list is re-sorted so checked entries float to the top.
pub fn handle_toggle_alloy_mode(state: &mut AppState) -> IntentResult {
    // Flip first, then branch on the resulting (target) state.
    let now_alloy = state.provider.toggle_alloy_mode();

    if now_alloy {
        // Entered alloy mode: pre-check the current session model's entries,
        // so editing an existing alloy only requires swapping members.
        let model_selection = state.active_session().profile().model.clone();
        let mut entries = state.provider.provider_picker.items().to_vec();
        crate::feat::provider::loader::pre_check_active_models(&mut entries, &model_selection);
        state.provider.provider_picker.set_items(entries);
    } else {
        // Left alloy mode: clear every check.
        let mut entries = state.provider.provider_picker.items().to_vec();
        for entry in &mut entries {
            entry.selected = false;
        }
        state.provider.provider_picker.set_items(entries);
    }

    resort_provider_picker(&mut state.provider.provider_picker);
    IntentResult::empty()
}

/// Re-sorts provider picker entries: selected first (alphabetical), then unselected (alphabetical).
fn resort_provider_picker(picker: &mut jinn_selection_widget::SelectionState<PickerEntry>) {
    let entries: Vec<PickerEntry> = picker.items().to_vec();
    let (selected, unselected): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| e.selected);
    let mut sorted: Vec<PickerEntry> = selected;
    sorted.extend(unselected);
    picker.set_items(sorted);
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

    /// Wraps persona entries through the persona spec's hooks for storage.
    fn wrap_persona_entries(
        entries: Vec<crate::feat::persona::PersonaEntry>,
    ) -> Vec<jinn_picker::PickerEntry<crate::feat::persona::PersonaEntry>> {
        crate::feat::picker::registry::build_picker_registry()
            .make_items(crate::feat::picker::registry::PERSONA_ID, entries)
            .expect("persona spec is registered")
    }

    fn empty_pickers() -> jinn_picker::PickerRegistry {
        jinn_picker::PickerRegistry::new()
    }
    use crate::feat::session::ChatSessionState;
    use crate::feat::todo_list::TaskStatus;
    use crate::feat::todo_list::picker_entry::RowStatus;
    use jinn_selection_widget::TreeItem;

    #[rstest::rstest]
    fn confirm_provider_rejects_unavailable() {
        // If the ! were deleted, unavailable providers could be confirmed.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        // Add an unavailable provider entry to the picker and select it.
        let entry = crate::protocol::PickerEntry {
            provider_id: "openrouter/gpt-4".to_owned(),
            name: "openrouter".to_owned(),
            provider_name: "openrouter".to_owned(),
            backend: "openrouter".to_owned(),
            model: "gpt-4".to_owned(),
            search_text: "gpt-4 openrouter".to_owned(),
            is_alias: false,
            alias_target: None,
            is_available: false, // Unavailable!
            is_remote: false,
            is_active: false,
            selected: false,
            theme: crate::feat::theme::default_theme(),
        };
        state.provider.provider_picker.set_items(vec![entry]);
        state.provider.provider_picker.move_down(1); // Select first entry.

        let result = confirm_provider(&mut state);

        // Then no commands are emitted (the unavailable provider was rejected).
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_provider_accepts_available() {
        // Counter-test: confirms that available providers ARE accepted.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        let entry = crate::protocol::PickerEntry {
            provider_id: "ollama/llama3".to_owned(),
            name: "ollama".to_owned(),
            provider_name: "ollama".to_owned(),
            backend: "ollama".to_owned(),
            model: "llama3".to_owned(),
            search_text: "llama3 ollama".to_owned(),
            is_alias: false,
            alias_target: None,
            is_available: true, // Available!
            is_remote: false,
            is_active: false,
            selected: false,
            theme: crate::feat::theme::default_theme(),
        };
        state.provider.provider_picker.set_items(vec![entry]);
        state.provider.provider_picker.move_down(1);

        let result = confirm_provider(&mut state);

        // Then commands are emitted.
        assert!(!result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn handle_model_toggle_flips_selected() {
        // Given a picker with two available entries.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });

        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(true);
        // Cursor starts on the first entry (selection 0) after reset.

        // When toggling model selection.
        handle_model_toggle(&mut state);

        // Then the first entry is now selected.
        let first = state.provider.provider_picker.items()[0].selected;
        assert!(first, "first entry should be selected after toggle");
    }

    #[rstest::rstest]
    fn handle_model_toggle_keeps_cursor_in_place() {
        // Given a picker with two entries, cursor on the first, in alloy mode.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });

        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(true);
        // Cursor starts on the first entry (selection 0) after reset.
        assert_eq!(state.provider.provider_picker.selection(), 0);
        // When toggling model selection.
        handle_model_toggle(&mut state);

        // Then the cursor stays on the first entry.
        assert_eq!(
            state.provider.provider_picker.selection(),
            0,
            "cursor should not advance after toggle"
        );
    }

    #[rstest::rstest]
    fn handle_model_toggle_toggles_off() {
        // Given a picker with one entry already selected.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });

        let entries = vec![crate::protocol::PickerEntry {
            provider_id: "ollama/llama3".to_owned(),
            name: "ollama".to_owned(),
            provider_name: "ollama".to_owned(),
            backend: "ollama".to_owned(),
            model: "llama3".to_owned(),
            search_text: "llama3".to_owned(),
            is_alias: false,
            alias_target: None,
            is_available: true,
            is_remote: false,
            is_active: false,
            selected: true, // Already selected
            theme: crate::feat::theme::default_theme(),
        }];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(true);
        state.provider.provider_picker.move_down(1);

        // When toggling model selection again.
        handle_model_toggle(&mut state);

        // Then the entry is now deselected.
        let first = state.provider.provider_picker.items()[0].selected;
        assert!(!first, "entry should be deselected after second toggle");
    }

    #[rstest::rstest]
    fn open_picker_sets_single_mode_for_single_model_session() {
        // Given a session on a single model.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama/llama3".to_owned()));

        // When opening the provider picker.
        handle_open_picker(&mut state, PickerKind::Provider, &empty_pickers());

        // Then alloy_mode is false (single mode).
        assert!(
            !state.provider.is_alloy_mode(),
            "picker should open in single mode for a single-model session"
        );
    }

    #[rstest::rstest]
    fn open_picker_sets_alloy_mode_for_alloy_session() {
        // Given a session on an alloy of two models.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "openrouter/gpt-4".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        // When opening the provider picker.
        handle_open_picker(&mut state, PickerKind::Provider, &empty_pickers());

        // Then alloy_mode is true (alloy mode).
        assert!(
            state.provider.is_alloy_mode(),
            "picker should open in alloy mode for an alloy session"
        );
    }

    #[rstest::rstest]
    fn toggle_alloy_mode_flips_false_to_true() {
        // Given a provider picker in single mode.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        state.provider.set_alloy_mode(false);

        // When toggling alloy mode.
        handle_toggle_alloy_mode(&mut state);

        // Then alloy_mode is now true.
        assert!(state.provider.is_alloy_mode(), "mode should flip to alloy");
    }

    #[rstest::rstest]
    fn toggle_alloy_mode_flips_true_to_false() {
        // Given a provider picker in alloy mode.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        state.provider.set_alloy_mode(true);

        // When toggling alloy mode.
        handle_toggle_alloy_mode(&mut state);

        // Then alloy_mode is now false.
        assert!(
            !state.provider.is_alloy_mode(),
            "mode should flip to single"
        );
    }

    #[rstest::rstest]
    fn toggle_into_alloy_mode_pre_checks_current_single_model() {
        // Given a provider picker in single mode with the session on a single model.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(false);

        // When toggling into alloy mode.
        handle_toggle_alloy_mode(&mut state);

        // Then the entry matching the current model is pre-checked.
        let llama = state
            .provider
            .provider_picker
            .items()
            .iter()
            .find(|e| e.provider_id == "ollama/llama3")
            .expect("llama entry");
        assert!(
            llama.selected,
            "current model should be pre-checked on entering alloy mode"
        );
    }

    #[rstest::rstest]
    fn toggle_into_alloy_mode_pre_checks_all_alloy_members() {
        // Given a provider picker in single mode with the session on an alloy.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "openrouter/gpt-4".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(false);

        // When toggling into alloy mode.
        handle_toggle_alloy_mode(&mut state);

        // Then both alloy members are pre-checked.
        let checked: Vec<&str> = state
            .provider
            .provider_picker
            .items()
            .iter()
            .filter(|e| e.selected)
            .map(|e| e.provider_id.as_str())
            .collect();
        assert_eq!(checked.len(), 2, "both alloy members should be pre-checked");
    }

    #[rstest::rstest]
    fn toggle_out_of_alloy_mode_clears_all_checks() {
        // Given a provider picker in alloy mode with two entries checked.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: true,
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: true,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(true);

        // When toggling out of alloy mode.
        handle_toggle_alloy_mode(&mut state);

        // Then no entries remain checked.
        let any_checked = state
            .provider
            .provider_picker
            .items()
            .iter()
            .any(|e| e.selected);
        assert!(
            !any_checked,
            "all checks should be cleared on leaving alloy mode"
        );
    }

    #[rstest::rstest]
    fn confirm_provider_with_multiple_selected_creates_alloy() {
        // Given a picker with two available entries, both selected.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        let entries = vec![
            crate::protocol::PickerEntry {
                provider_id: "ollama/llama3".to_owned(),
                name: "ollama".to_owned(),
                provider_name: "ollama".to_owned(),
                backend: "ollama".to_owned(),
                model: "llama3".to_owned(),
                search_text: "llama3".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: true, // Selected!
                theme: crate::feat::theme::default_theme(),
            },
            crate::protocol::PickerEntry {
                provider_id: "openrouter/gpt-4".to_owned(),
                name: "openrouter".to_owned(),
                provider_name: "openrouter".to_owned(),
                backend: "openrouter".to_owned(),
                model: "gpt-4".to_owned(),
                search_text: "gpt-4".to_owned(),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: true, // Selected!
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state.provider.provider_picker.set_items(entries);
        state.provider.set_alloy_mode(true);

        let result = confirm_provider(&mut state);

        // Then a ProviderSwitch message is emitted for alloy.
        assert!(!result.message_names.is_empty());
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("ProviderSwitch")),
            "messages should contain ProviderSwitch: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn confirm_persona_sets_correct_persona() {
        // If the match were inverted, the wrong persona would be set.
        use crate::feat::persona::PersonaEntry;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        // Add two personas to context.
        state.context.set_personas(vec![
            crate::feat::persona::Persona {
                name: "coder".to_owned(),
                description: String::new(),
                body: "You are a coder.".to_owned(),
            },
            crate::feat::persona::Persona {
                name: "writer".to_owned(),
                description: String::new(),
                body: "You are a writer.".to_owned(),
            },
        ]);

        // Set picker entries with "writer" as the selected item.
        let entries = vec![
            PersonaEntry {
                name: "coder".to_owned(),
                description: String::new(),
                is_active: false,
                theme: crate::feat::theme::default_theme(),
            },
            PersonaEntry {
                name: "writer".to_owned(),
                description: String::new(),
                is_active: false,
                theme: crate::feat::theme::default_theme(),
            },
        ];
        state
            .frontend
            .persona_picker_mut()
            .set_items(wrap_persona_entries(entries));
        state.frontend.persona_picker_mut().move_down(1); // coder
        state.frontend.persona_picker_mut().move_down(1); // writer

        // Persona confirm runs through its spec (registry dispatch).
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Persona,
            });
        let (result, _redispatch) = handle_picker_confirm(
            &mut state,
            &crate::feat::picker::registry::build_picker_registry(),
        );

        // Then the active persona is "writer", not "coder".
        assert_eq!(
            state.context.active_persona().map(|p| p.name.as_str()),
            Some("writer"),
            "confirm_persona should set the correct persona"
        );
        assert!(!result.message_names.is_empty());
    }

    #[rstest::rstest]
    fn confirm_reasoning_effort_sets_session_override() {
        // If the session override were never set, the profile would stay None.
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        // Populate the picker with a single entry; select it.
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::High,
            name: "high".to_owned(),
            description: "High effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);
        state.frontend.reasoning_effort_picker_mut().move_down(1);

        // When confirming.
        let _ = confirm_reasoning_effort(&mut state);

        // Then the active session's override is set to High.
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(ReasoningEffort::High),
            "confirm should set the session reasoning_effort override"
        );
    }

    #[rstest::rstest]
    fn confirm_endpoint_pins_selected_endpoint_on_profile() {
        // Given a populated endpoint picker with a real upstream selected.
        use crate::feat::endpoint::picker_entry::EndpointEntry;
        use crate::feat::theme::default_theme;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        let entry = EndpointEntry {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
            uptime_30m: None,
            prompt_price: None,
            completion_price: None,
            quantization: None,
            max_completion_tokens: None,
            is_active: false,
            theme: default_theme(),
        };
        state.frontend.endpoint_picker_mut().set_items(vec![entry]);
        state.frontend.endpoint_picker_mut().move_down(1);

        // When confirming.
        let _ = confirm_endpoint(&mut state);

        // Then the session profile pins the Anthropic endpoint.
        let pinned = state.active_session().profile().endpoint.clone();
        assert_eq!(pinned.map(|e| e.tag), Some("anthropic".to_owned()));
    }

    #[rstest::rstest]
    fn confirm_endpoint_sentinel_clears_pin_to_none() {
        // Given a session that already has a pinned endpoint.
        use crate::feat::endpoint::picker_entry::EndpointEntry;
        use crate::feat::theme::default_theme;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().profile_mut().endpoint = Some(crate::feat::endpoint::Endpoint {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
        });

        // And the auto-route sentinel (index 0) is the only selected item.
        state
            .frontend
            .endpoint_picker_mut()
            .set_items(vec![EndpointEntry::auto_route(true, default_theme())]);

        // When confirming the sentinel.
        let _ = confirm_endpoint(&mut state);

        // Then the pin is cleared.
        assert!(
            state.active_session().profile().endpoint.is_none(),
            "selecting the auto-route sentinel must clear the pin"
        );
    }

    #[rstest::rstest]
    fn open_endpoint_picker_is_noop_for_alloy_model() {
        // Given a session on an alloy of two models.
        use crate::feat::session::model_selection::{AlloyStrategy, ModelSelection};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        // When opening the endpoint picker.
        handle_open_picker(&mut state, PickerKind::Endpoint, &empty_pickers());

        // Then no picker scope is pushed (the gate rejected it).
        assert!(
            !state.frontend.scope_stack.is_picker(),
            "endpoint picker must not open for an alloy model"
        );
    }

    #[rstest::rstest]
    fn refresh_endpoints_sets_loading_and_emits_refresh_command() {
        // Given a session on a single model (the picker applies to it).
        use crate::feat::session::model_selection::ModelSelection;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Single(
            "openrouter/anthropic/claude-sonnet-4".to_owned(),
        ));

        // When handling RefreshEndpoints.
        let result = handle_refresh_endpoints(&mut state);

        // Then loading is set synchronously.
        assert!(
            state.frontend.pickers.endpoint_loading,
            "refresh must set loading so the indicator appears this frame"
        );
        // And the forced-refresh command is emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("RefreshEndpointPickerEntries")),
            "refresh must emit RefreshEndpointPickerEntries; got {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn refresh_endpoints_is_noop_for_alloy_model() {
        // Given a session on an alloy of two models.
        use crate::feat::session::model_selection::{AlloyStrategy, ModelSelection};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.active_session_mut().set_model(ModelSelection::Alloy {
            models: vec!["ollama/llama3".to_owned(), "ollama/mistral".to_owned()],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        });

        // When handling RefreshEndpoints.
        let result = handle_refresh_endpoints(&mut state);

        // Then it is a no-op: no command emitted, loading never set.
        assert!(
            result.message_names.is_empty(),
            "refresh must be a no-op for an alloy model"
        );
        assert!(
            !state.frontend.pickers.endpoint_loading,
            "refresh must not set loading for an alloy model"
        );
    }

    #[rstest::rstest]
    fn confirm_reasoning_effort_in_session_a_does_not_leak_into_session_b() {
        // Regression: changing effort in one session used to leak into every other
        // override-free session because the live global was consulted at request time.
        // Now each session owns its own value; the global seeds new sessions only.
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry, resolve_effort};

        let mut state = AppState::default();

        // Session B: seeded with High (its own, frozen value).
        let mut b = ChatSessionState::new();
        b.profile_mut().reasoning_effort = Some(ReasoningEffort::High);
        let b_id = b.session_id().clone();
        state.session.insert(b);

        // Session A (active): seeded with High, then changed to Xhigh via the picker.
        let mut a = ChatSessionState::new();
        a.profile_mut().reasoning_effort = Some(ReasoningEffort::High);
        state.session.insert(a);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::Xhigh,
            name: "xhigh".to_owned(),
            description: "Extra high effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);
        state.frontend.reasoning_effort_picker_mut().move_down(1);

        // When confirming the effort change in session A.
        let result = confirm_reasoning_effort(&mut state);

        // Then session A's own effort is Xhigh.
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(ReasoningEffort::Xhigh),
            "session A should have the confirmed effort"
        );
        // And the global default was advanced to Xhigh (so future sessions inherit it).
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("UpdateAppState")),
            "confirm should still seed the global for future sessions"
        );
        // But session B's resolved effort is unchanged — the global no longer leaks.
        assert_eq!(
            resolve_effort(state.session.get(&b_id).unwrap().profile().reasoning_effort),
            Some(ReasoningEffort::High),
            "session B's own effort must be unaffected by session A's change"
        );
    }

    #[rstest::rstest]
    fn confirm_reasoning_effort_pops_picker_scope() {
        // If the scope were never popped, the picker would remain open.
        use crate::common::app_state::FocusScope;
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::Medium,
            name: "medium".to_owned(),
            description: "Medium effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);
        state.frontend.reasoning_effort_picker_mut().move_down(1);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::ReasoningEffort,
        });

        // When confirming.
        let _ = confirm_reasoning_effort(&mut state);

        // Then the picker scope has been popped (no ReasoningEffort scope remains).
        let still_open = state
            .frontend
            .scope_stack
            .picker_kind()
            .is_some_and(|k| *k == PickerKind::ReasoningEffort);
        assert!(!still_open, "picker scope should be popped after confirm");
    }

    #[rstest::rstest]
    fn confirm_reasoning_effort_emits_mark_session_interacted() {
        // If the persist message were never emitted, the profile change would
        // only be saved on a later (unrelated) event.
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::Low,
            name: "low".to_owned(),
            description: "Low effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);
        state.frontend.reasoning_effort_picker_mut().move_down(1);

        // When confirming.
        let result = confirm_reasoning_effort(&mut state);

        // Then a MarkSessionInteracted message is emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("MarkSessionInteracted")),
            "confirm should emit MarkSessionInteracted to persist the change"
        );
    }

    #[rstest::rstest]
    fn confirm_reasoning_effort_emits_update_app_state() {
        // If the global default write were never emitted, new sessions would
        // not inherit the chosen effort.
        use crate::feat::reasoning::{ReasoningEffort, ReasoningEffortEntry};

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        let entry = ReasoningEffortEntry {
            effort: ReasoningEffort::Max,
            name: "max".to_owned(),
            description: "Maximum effort".to_owned(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .reasoning_effort_picker_mut()
            .set_items(vec![entry]);
        state.frontend.reasoning_effort_picker_mut().move_down(1);

        // When confirming.
        let result = confirm_reasoning_effort(&mut state);

        // Then an UpdateAppState message is emitted (global seed write).
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("UpdateAppState")),
            "confirm should emit UpdateAppState to persist the global seed"
        );
        // And NOT UpdatePreferences (old path, now removed).
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.ends_with("UpdatePreferences")),
            "confirm should no longer emit UpdatePreferences"
        );
    }

    #[rstest::rstest]
    fn confirm_persona_emits_mark_session_interacted() {
        // added to confirm_persona.
        // If the persist message were never emitted, a pick-then-quit would lose
        // the persona change.
        use crate::feat::persona::PersonaEntry;

        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .context
            .set_personas(vec![crate::feat::persona::Persona {
                name: "coder".to_owned(),
                description: String::new(),
                body: "You are a coder.".to_owned(),
            }]);
        let entry = PersonaEntry {
            name: "coder".to_owned(),
            description: String::new(),
            is_active: false,
            theme: crate::feat::theme::default_theme(),
        };
        state
            .frontend
            .persona_picker_mut()
            .set_items(wrap_persona_entries(vec![entry]));
        state.frontend.persona_picker_mut().move_down(1);

        // When confirming through the persona spec (registry dispatch).
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: PickerKind::Persona,
            });
        let (result, _redispatch) = handle_picker_confirm(
            &mut state,
            &crate::feat::picker::registry::build_picker_registry(),
        );

        // Then a MarkSessionInteracted message is emitted.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.ends_with("MarkSessionInteracted")),
            "confirm_persona should emit MarkSessionInteracted to persist"
        );
    }

    #[rstest::rstest]
    fn confirm_session_lifecycle_finds_correct_lifecycle_for_args() {
        // If the match were inverted, find() would locate the WRONG lifecycle,
        // producing the wrong template_display in the arg_input state.
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());

        // Add two lifecycles with args ($1) to preferences.
        state.frontend.preferences.session_lifecycles = vec![
            crate::feat::preferences_actor::user_preferences::SessionLifecycle {
                name: "project-a".to_owned(),
                description: None,
                setup: Some(
                    crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                        "cd /a/$1".to_owned(),
                    ),
                ),
                teardown: None,
            },
            crate::feat::preferences_actor::user_preferences::SessionLifecycle {
                name: "project-b".to_owned(),
                description: None,
                setup: Some(
                    crate::feat::session_lifecycle::builtin::LifecycleCommand::Shell(
                        "cd /b/$1".to_owned(),
                    ),
                ),
                teardown: None,
            },
        ];

        // Load entries and select "project-b".
        load_lifecycle_picker_entries(&mut state);
        state.frontend.session_lifecycle_picker_mut().move_down(1); // blank
        state.frontend.session_lifecycle_picker_mut().move_down(1); // project-a
        state.frontend.session_lifecycle_picker_mut().move_down(1); // project-b

        let _result = confirm_session_lifecycle(&mut state);

        // Then the arg_input state references "project-b" and its template.
        assert_eq!(state.frontend.arg_input.lifecycle_name, "project-b");
        assert!(
            state.frontend.arg_input.template_display.contains("/b/"),
            "template_display should contain /b/ from project-b's setup command, got: {}",
            state.frontend.arg_input.template_display,
        );
    }

    #[rstest::rstest]
    fn load_lifecycle_picker_entries_populates_picker() {
        // If the function were a no-op, the picker would remain empty.
        let mut state = AppState::default();

        // Add lifecycle entries to preferences.
        state.frontend.preferences.session_lifecycles = vec![
            crate::feat::preferences_actor::user_preferences::SessionLifecycle {
                name: "project-a".to_owned(),
                description: Some("Project A setup".to_owned()),
                setup: None,
                teardown: None,
            },
        ];

        // When loading lifecycle picker entries.
        load_lifecycle_picker_entries(&mut state);

        // Then the picker has entries (blank + project-a = 2).
        let items = state.frontend.session_lifecycle_picker().items();
        assert_eq!(
            items.len(),
            2,
            "should have blank + 1 lifecycle = 2 entries"
        );
        assert_eq!(items[0].name, "blank");
        assert_eq!(items[1].name, "project-a");
    }

    fn setup_state_with_task_list() -> (AppState, crate::feat::todo_list::TaskId) {
        use crate::feat::todo_list::TaskPosition;

        let mut state = AppState::default();
        let mut origin = ChatSessionState::new();

        // Phase 1 with 2 tasks (one Pending, one Completed).
        let phase1 = origin.task_list_mut().add_phase("Research");
        let _ = origin
            .task_list_mut()
            .add_task(&phase1, "Read codebase", TaskPosition::End);
        let task2 = origin
            .task_list_mut()
            .add_task(&phase1, "Write notes", TaskPosition::End)
            .expect("add_task");
        origin
            .task_list_mut()
            .complete_task(&task2)
            .expect("complete");

        // Phase 2 with 3 tasks (Pending, Cancelled, plus a Postponed source).
        // postpone_task marks the source as Postponed AND inserts a new Pending
        // copy with the same description, so we surface the source ID to tests.
        let phase2 = origin.task_list_mut().add_phase("Build");
        let _ = origin
            .task_list_mut()
            .add_task(&phase2, "Implement feature", TaskPosition::End);
        let task_cancel = origin
            .task_list_mut()
            .add_task(&phase2, "Investigate alt", TaskPosition::End)
            .expect("add_task");
        origin
            .task_list_mut()
            .cancel_task(&task_cancel)
            .expect("cancel");
        let to_postpone = origin
            .task_list_mut()
            .add_task(&phase2, "Refactor later", TaskPosition::End)
            .expect("add_task");
        let postponed_id = to_postpone.clone();
        origin
            .task_list_mut()
            .postpone_task(&to_postpone, TaskPosition::After(task_cancel))
            .expect("postpone");

        let origin_id = origin.session_id().clone();
        state.session.insert(origin);
        assert!(
            state.session.set_active(origin_id),
            "origin session must be present for set_active"
        );
        (state, postponed_id)
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_skips_postponed() {
        // Given a session with one postponed task among other tasks.
        // postpone_task creates a new Pending copy with the same description, so we
        // must verify the *source* (Postponed) entry is excluded by ID, not by label.
        let (mut state, postponed_id) = setup_state_with_task_list();

        // When loading task list picker entries.
        load_task_list_picker_entries(&mut state);

        // Then no entry has the postponed task's ID.
        let excluded_id = format!("task:{postponed_id}");
        let items = state.frontend.task_list_picker().items();
        assert!(
            items.iter().all(|e| e.id() != excluded_id),
            "postponed source task should not appear in picker (id={excluded_id})"
        );
        // Sanity: the new Pending copy with the same description IS present.
        assert!(
            items.iter().any(|e| e.display_label() == "Refactor later"),
            "Pending copy of postponed task should be visible"
        );
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_produces_correct_tree_shape() {
        // Given a session with two phases and mixed-status tasks.
        let (mut state, _postponed_id) = setup_state_with_task_list();

        // When loading.
        load_task_list_picker_entries(&mut state);

        // Then there are exactly 2 phase roots.
        let items = state.frontend.task_list_picker().items();
        let roots: Vec<_> = items.iter().filter(|e| e.parent_id().is_none()).collect();
        assert_eq!(roots.len(), 2, "should have 2 phase roots");
        assert_eq!(roots[0].display_label(), "Research");
        assert_eq!(roots[1].display_label(), "Build");

        // And each task's parent_id matches its phase's id.
        let phase_ids: Vec<&str> = roots.iter().map(|e| e.id()).collect();
        for item in items.iter().filter(|e| e.parent_id().is_some()) {
            assert!(
                phase_ids.contains(&item.parent_id().expect("task parent")),
                "task {:?} should reference a known phase id",
                item.display_label()
            );
        }

        // And the counts match: Phase 1 -> 2 tasks; Phase 2 -> 3 tasks (Pending,
        // Cancelled, and the Pending copy created by postpone_task).
        let research_children: Vec<_> = items
            .iter()
            .filter(|e| e.parent_id() == Some(phase_ids[0]))
            .collect();
        let build_children: Vec<_> = items
            .iter()
            .filter(|e| e.parent_id() == Some(phase_ids[1]))
            .collect();
        assert_eq!(research_children.len(), 2);
        assert_eq!(build_children.len(), 3);
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_carries_status_through() {
        // Given a session with completed and cancelled tasks.
        let (mut state, _postponed_id) = setup_state_with_task_list();

        // When loading.
        load_task_list_picker_entries(&mut state);

        // Then task rows carry their status in row_status.

        let items = state.frontend.task_list_picker().items();
        let statuses: Vec<_> = items
            .iter()
            .filter_map(|e| match e.row_status() {
                RowStatus::Task(s) => Some((e.display_label(), s)),
                RowStatus::Phase => None,
            })
            .collect();

        let by_label: std::collections::HashMap<&str, TaskStatus> =
            statuses.iter().map(|(l, s)| (*l, *s)).collect();
        assert_eq!(
            by_label.get("Write notes").copied(),
            Some(TaskStatus::Completed),
            "'Write notes' should be Completed"
        );
        assert_eq!(
            by_label.get("Investigate alt").copied(),
            Some(TaskStatus::Cancelled),
            "'Investigate alt' should be Cancelled"
        );
        assert_eq!(
            by_label.get("Read codebase").copied(),
            Some(TaskStatus::Pending),
            "'Read codebase' should be Pending"
        );
        // The Pending copy of the postponed task should also carry its status.
        assert_eq!(
            by_label.get("Refactor later").copied(),
            Some(TaskStatus::Pending),
            "'Refactor later' (Pending copy) should be Pending"
        );
    }

    #[rstest::rstest]
    fn load_task_list_picker_entries_empty_task_list_no_panic() {
        // Given a default session with an empty task list.
        let mut state = AppState::default();

        // When loading.
        load_task_list_picker_entries(&mut state);

        // Then the picker is empty and nothing panicked.
        assert!(state.frontend.task_list_picker().items().is_empty());
    }

    #[rstest::rstest]
    fn handle_picker_confirm_task_list_is_noop_and_keeps_scope() {
        // Given state with the TaskList picker scope on the stack.
        let (mut state, _postponed_id) = setup_state_with_task_list();
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::TaskList,
        });
        let len_before = state.frontend.scope_stack.len();

        // When confirming.
        let (result, follow_up) = handle_picker_confirm(&mut state, &empty_pickers());

        // Then no commands, no follow-up, and the scope stack is unchanged.
        assert!(result.message_names.is_empty(), "no commands");
        assert!(follow_up.is_none(), "no follow-up");
        assert_eq!(
            state.frontend.scope_stack.len(),
            len_before,
            "scope stack must remain unchanged on no-op confirm"
        );
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::TaskList
            }
        ));
    }

    #[rstest::rstest]
    fn esc_from_task_list_picker_restores_sidebar_task_list_scope() {
        // Given a scope stack like: [Normal, SidebarTaskList, Picker(TaskList)].
        let mut state = AppState::default();
        state.frontend.scope_stack.push(FocusScope::SidebarTaskList);
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::TaskList,
        });

        // When Esc is pressed.
        let _ = crate::feat::chat_input::intent::handle_enter_normal_mode(&mut state);

        // Then we should return to SidebarTaskList, not Normal.
        assert!(
            matches!(
                state.frontend.scope_stack.current(),
                FocusScope::SidebarTaskList
            ),
            "Esc from TaskList picker should restore SidebarTaskList scope, got: {:?}",
            state.frontend.scope_stack.current()
        );
    }

    /// Builds a state with the provider picker open (Provider scope), `n` available
    /// single-model entries `model-0..model-n`, and the first entry highlighted.
    fn state_with_provider_picker(n: usize) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Provider,
        });
        let entries: Vec<crate::protocol::PickerEntry> = (0..n)
            .map(|i| crate::protocol::PickerEntry {
                provider_id: format!("prov/model-{i}"),
                name: "prov".to_owned(),
                provider_name: "prov".to_owned(),
                backend: "openrouter".to_owned(),
                model: format!("model-{i}"),
                search_text: format!("model-{i}"),
                is_alias: false,
                alias_target: None,
                is_available: true,
                is_remote: false,
                is_active: false,
                selected: false,
                theme: crate::feat::theme::default_theme(),
            })
            .collect();
        state.provider.provider_picker.set_items(entries);
        state.provider.provider_picker.move_down(1); // highlight first entry
        state
    }

    #[rstest::rstest]
    fn single_mode_resolve_returns_highlight_ignoring_checks() {
        // Given single mode with a stale check on model-0 and model-1 highlighted.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(false);
        // Stale check on model-0 that single mode must ignore.
        state.provider.provider_picker.with_selected_mut(|e| {
            e.selected = true;
        });
        state.provider.provider_picker.move_down(1); // highlight model-1

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-1".to_owned());

        // Then it is Single of the highlighted entry, not the checked one.
        assert_eq!(selection, ModelSelection::Single("prov/model-1".to_owned()));
    }

    #[rstest::rstest]
    fn single_mode_confirm_rejects_unavailable_highlight() {
        // Given single mode where the highlighted entry is unavailable.
        let mut state = state_with_provider_picker(1);
        state.provider.set_alloy_mode(false);
        state.provider.provider_picker.with_selected_mut(|e| {
            e.is_available = false;
        });

        // When confirming.
        let result = confirm_provider(&mut state);

        // Then no ProviderSwitch is emitted.
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("ProviderSwitch")),
            "unavailable highlight should be rejected"
        );
    }

    #[rstest::rstest]
    fn single_mode_tab_is_noop() {
        // Given single mode.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(false);

        // When toggling a model.
        handle_model_toggle(&mut state);

        // Then no entry became selected.
        let any_selected = state
            .provider
            .provider_picker
            .items()
            .iter()
            .any(|e| e.selected);
        assert!(!any_selected, "single-mode TAB must not check anything");
    }

    #[rstest::rstest]
    fn alloy_mode_resolve_includes_highlight_and_checks() {
        // Given alloy mode with model-0 checked and model-2 highlighted.
        let mut state = state_with_provider_picker(3);
        state.provider.set_alloy_mode(true);
        // Check model-0.
        state
            .provider
            .provider_picker
            .move_up(active_viewport(&state));
        state.provider.provider_picker.with_selected_mut(|e| {
            e.selected = true;
        });
        // Move to model-2 (down twice from model-0).
        state
            .provider
            .provider_picker
            .move_down(active_viewport(&state));
        state
            .provider
            .provider_picker
            .move_down(active_viewport(&state));

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-2".to_owned());

        // Then it is an Alloy containing both model-0 and model-2.
        match selection {
            ModelSelection::Alloy { models, .. } => {
                assert_eq!(models.len(), 2);
                assert!(models.contains(&"prov/model-0".to_owned()));
                assert!(models.contains(&"prov/model-2".to_owned()));
            }
            other => panic!("expected Alloy, got {other:?}"),
        }
    }

    #[rstest::rstest]
    fn alloy_mode_resolve_dedups_already_checked_highlight() {
        // Given alloy mode with the highlighted entry (model-1) already checked.
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(true);
        state.provider.provider_picker.with_selected_mut(|e| {
            e.selected = true;
        });

        // When resolving (highlight is already checked).
        let selection = resolve_provider_selection(&state.provider, "prov/model-1".to_owned());

        // Then it collapses to Single (one model, no duplication).
        assert_eq!(
            selection,
            ModelSelection::Single("prov/model-1".to_owned()),
            "already-checked highlight must not duplicate; 1-model set collapses to Single"
        );
    }

    #[rstest::rstest]
    fn alloy_mode_resolve_one_model_collapses_to_single() {
        // Given alloy mode with nothing checked and the highlighted entry (model-1).
        let mut state = state_with_provider_picker(2);
        state.provider.set_alloy_mode(true);

        // When resolving the selection for the highlighted entry.
        let selection = resolve_provider_selection(&state.provider, "prov/model-1".to_owned());

        // Then a Single selection is returned (1-model alloy collapses).
        assert_eq!(selection, ModelSelection::Single("prov/model-1".to_owned()));
    }

    /// Builds an AppState with a project picker open, the active session's CWD
    /// set to a distinct value, and `n` curated project entries loaded.
    fn state_with_project_picker(paths: &[&str]) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/active-session-cwd"));
        state.frontend.scope_stack.push(FocusScope::Picker {
            kind: PickerKind::Project,
        });
        let projects: Vec<crate::feat::project::ProjectConfig> = paths
            .iter()
            .map(|p| crate::feat::project::ProjectConfig {
                path: std::path::PathBuf::from(p),
            })
            .collect();
        state.frontend.preferences.projects = projects;
        load_project_picker_entries(&mut state.frontend);
        // index 0 is selected by default after set_items + reset.
        state
    }

    #[rstest::rstest]
    fn confirm_project_creates_new_session_at_chosen_dir() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_project_picker(&["/tmp/project-a", "/tmp/project-b"]);

        // When confirming the highlighted project (Enter).
        let result = confirm_project(&mut state);

        // Then a new session was created (a message was emitted to drive it).
        assert!(!result.message_names.is_empty());
        // And the new active session's CWD is the chosen project dir, not the
        // previously active session's CWD.
        assert_eq!(
            state.active_session().cwd(),
            std::path::Path::new("/tmp/project-a"),
        );
        // And the stash was consumed.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn confirm_project_leaves_previous_session_cwd_unchanged() {
        // Given a project picker with an existing active session.
        let mut state = state_with_project_picker(&["/tmp/project-a"]);
        let prev_id = state.session.active_session_id().clone();

        // When confirming the highlighted project.
        let _result = confirm_project(&mut state);

        // Then the previous session (now backgrounded) keeps its original CWD.
        let prev = state
            .session
            .get(&prev_id)
            .expect("previous session still exists");
        assert_eq!(prev.cwd(), std::path::Path::new("/tmp/active-session-cwd"));
    }

    #[rstest::rstest]
    fn confirm_project_with_lifecycle_sets_override_then_opens_lifecycle_picker() {
        // Given a project picker whose highlighted entry is /tmp/project-a.
        let mut state = state_with_project_picker(&["/tmp/project-a"]);

        // When confirming with lifecycle (<c-enter>).
        let _result = handle_project_lifecycle_confirm(&mut state);

        // Then the project scope was popped and the lifecycle picker opened.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            FocusScope::Picker {
                kind: PickerKind::SessionLifecycle
            }
        ));
        // And the chosen dir is stashed in a pending creation, awaiting the
        // lifecycle/args confirm chain.
        let pending = state
            .frontend
            .pending_creation
            .as_ref()
            .expect("pending creation stashed");
        assert_eq!(pending.project_dir, std::path::Path::new("/tmp/project-a"));
        assert_eq!(pending.starting_cwd, std::path::Path::new("/tmp/project-a"));
    }

    #[rstest::rstest]
    fn project_remove_highlighted_deletes_highlighted_entry() {
        // Given a project picker with two entries and the first highlighted.
        let mut state = state_with_project_picker(&["/tmp/project-a", "/tmp/project-b"]);

        // When removing the highlighted entry (d).
        let result = handle_project_remove_highlighted(&mut state);

        // Then the highlighted entry is removed from preferences.projects.
        let paths: Vec<_> = state
            .frontend
            .preferences
            .projects
            .iter()
            .map(|p| p.path.clone())
            .collect();
        assert_eq!(paths, vec![std::path::PathBuf::from("/tmp/project-b")]);
        // And an UpdatePreferences(RemoveProject) message was emitted.
        assert!(!result.message_names.is_empty());
        // And the picker now shows one entry.
        assert_eq!(state.frontend.project_picker().items().len(), 1);
    }

    fn setup_state_with_web_search_tool(model: &str) -> AppState {
        let mut state = AppState::default();
        let origin = ChatSessionState::new();
        state.session.insert(origin);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .set_model(ModelSelection::Single(model.to_owned()));

        state.context.global_tool_definitions.insert(
            "openrouter:web_search".to_owned(),
            crate::protocol::ToolDefinition {
                name: "openrouter:web_search".to_owned(),
                description: "Search the web".to_owned(),
                parameters: serde_json::json!({}),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                server_tool_type: Some(jinn_provider::ServerToolType::OpenrouterWebSearch),
            },
        );
        state
    }

    #[rstest::rstest]
    fn load_tool_picker_entries_marks_config_seeded_disabled_tools() {
        // Given state whose active session profile has the registered
        // web_search tool disabled (as seeded from jinn.toml disabled_tools
        // at session creation).
        let mut state = setup_state_with_web_search_tool("openrouter/openai/gpt-oss-120b");
        state.active_session_mut().set_disabled_tools(
            ["openrouter:web_search"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        );

        // When loading tool picker entries.
        load_tool_picker_entries(&mut state);

        // Then the seeded-disabled tool renders as disabled (crossed out) in
        // the picker.
        let entry = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .find(|e| e.name == "openrouter:web_search")
            .expect("web_search entry present");
        assert!(
            !entry.enabled,
            "config-seeded disabled tool must show as disabled"
        );
    }

    #[rstest::rstest]
    fn load_tool_picker_entries_marks_task_disabled_in_subagent_session() {
        // Given a state whose active session is a subagent (parent-linked)
        // with the task tool in its disabled set (the spawn stamp).
        let mut state = AppState::default();
        let parent_id = crate::protocol::SessionId::new();
        let child = ChatSessionState::new_child(&parent_id, true);
        state.session.insert(child);
        state
            .session
            .set_active(state.session.active_session_id().clone());
        state
            .active_session_mut()
            .profile_mut()
            .disabled_tools
            .insert(crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned());
        state.context.global_tool_definitions.insert(
            crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned(),
            crate::protocol::ToolDefinition {
                name: crate::feat::tools_actor::task::TASK_TOOL_NAME.to_owned(),
                description: "Delegate a sub-task to a subagent".to_owned(),
                parameters: serde_json::json!({}),
                prompt_snippet: None,
                prompt_guidelines: vec![],
                server_tool_type: None,
            },
        );

        // When loading tool picker entries.
        load_tool_picker_entries(&mut state);

        // Then the task tool renders as disabled in the picker.
        let entry = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .find(|e| e.name == crate::feat::tools_actor::task::TASK_TOOL_NAME)
            .expect("task entry present");
        assert!(
            !entry.enabled,
            "a subagent session's spawn stamp must show task as disabled"
        );
    }

    #[rstest::rstest]
    fn load_tool_picker_entries_hides_web_search_for_non_openrouter_model() {
        // Given state on a non-openrouter model with a web_search tool registered.
        let mut state = setup_state_with_web_search_tool("zai/glm-4.6");

        // When loading tool picker entries.
        load_tool_picker_entries(&mut state);

        // Then the web_search tool is NOT offered (it can't run on this provider).
        let names: Vec<&str> = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(
            !names.contains(&"openrouter:web_search"),
            "web_search should be hidden for non-openrouter model, got: {names:?}"
        );
    }

    #[rstest::rstest]
    fn load_tool_picker_entries_shows_web_search_for_openrouter_model() {
        // Given state on an openrouter model with a web_search tool registered.
        let mut state = setup_state_with_web_search_tool("openrouter/openai/gpt-oss-120b");

        // When loading tool picker entries.
        load_tool_picker_entries(&mut state);

        // Then the web_search tool IS offered.
        let names: Vec<&str> = state
            .frontend
            .tool_picker()
            .items()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert!(
            names.contains(&"openrouter:web_search"),
            "web_search should be visible for openrouter model, got: {names:?}"
        );
    }

    #[rstest::rstest]
    fn handle_move_down_uses_measured_viewport() {
        // Given a provider picker with 20 entries and a measured viewport of 5,
        // selection already on the last visible row (index 4).
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(5);
        state.provider.provider_picker.move_up(5); // back to selection 0
        for _ in 0..4 {
            state.provider.provider_picker.move_down(5);
        }
        assert_eq!(state.provider.provider_picker.selection(), 4);
        assert_eq!(state.provider.provider_picker.scroll_offset(), 0);

        // When moving down once more.
        handle_move_down(&mut state, &empty_pickers());

        // Then selection advances to 5 and scroll_offset advances by one
        // (measured viewport of 5, not the old hardcoded 100).
        assert_eq!(state.provider.provider_picker.selection(), 5);
        assert_eq!(state.provider.provider_picker.scroll_offset(), 1);
    }

    #[rstest::rstest]
    fn handle_move_down_uses_fallback_when_viewport_unmeasured() {
        // Given a provider picker with 30 entries and viewport left at 0
        // (before the first render writes a measurement).
        let mut state = state_with_provider_picker(30);
        assert_eq!(state.frontend.picker_results_viewport(), 0);

        // When moving down once.
        handle_move_down(&mut state, &empty_pickers());

        // Then selection advances by one without panic, using the fallback.
        assert_eq!(state.provider.provider_picker.selection(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn move_down_previews_the_theme_when_the_registry_holds_the_spec() {
        // Given an open theme picker whose second entry is a distinct theme,
        // with the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = AppState::default();
        state.plugins.set_themes(
            "theme-loader",
            vec![("other".to_owned(), None, other.clone())],
        );
        crate::feat::picker::intent::handle_open_picker(&mut state, PickerKind::Theme, &registry);

        // When moving the selection down one entry.
        handle_move_down(&mut state, &registry);

        // Then the highlighted theme is applied live (spec selection-change
        // hook ran through the move handler).
        assert_eq!(
            state.frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
    }

    #[rstest::rstest]
    #[test]
    fn move_down_skips_selection_change_when_the_spec_has_no_hook() {
        // Given an open provider picker (its kind maps to no spec) with two
        // entries and the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut state = state_with_provider_picker(2);
        let theme_before = state.frontend.theme.clone();

        // When moving the selection down.
        handle_move_down(&mut state, &registry);

        // Then the app theme is untouched (no selection-change dispatch).
        assert_eq!(state.frontend.theme.focus_accent, theme_before.focus_accent);
    }

    #[rstest::rstest]
    #[test]
    fn page_down_previews_the_theme_when_the_registry_holds_the_spec() {
        // Given an open theme picker whose second entry is a distinct theme,
        // with the domain registry in play.
        let registry = crate::feat::picker::registry::build_picker_registry();
        let mut other = crate::feat::theme::default_theme();
        other.focus_accent = ratatui::style::Color::Red;
        let mut state = AppState::default();
        state.plugins.set_themes(
            "theme-loader",
            vec![("other".to_owned(), None, other.clone())],
        );
        crate::feat::picker::intent::handle_open_picker(&mut state, PickerKind::Theme, &registry);

        // When paging down (selection jumps to the last entry).
        handle_page_down(&mut state, &registry);

        // Then the highlighted theme is applied live.
        assert_eq!(
            state.frontend.theme.focus_accent,
            ratatui::style::Color::Red
        );
    }

    #[rstest::rstest]
    fn handle_page_down_advances_selection_by_half_viewport() {
        // Given a provider picker with 20 entries, selection at 0, viewport 10.
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(10);
        state.provider.provider_picker.move_up(5); // selection back to 0

        // When handling PickerPageDown (half of 10 = 5).
        handle_page_down(&mut state, &empty_pickers());

        // Then selection advances by 5.
        assert_eq!(state.provider.provider_picker.selection(), 5);
    }

    #[rstest::rstest]
    fn handle_page_up_decrements_selection_by_half_viewport() {
        // Given a provider picker with 20 entries, selection at 10, viewport 10.
        let mut state = state_with_provider_picker(20);
        state.frontend.set_picker_results_viewport(10);
        // Advance selection to 10.
        for _ in 0..9 {
            state.provider.provider_picker.move_down(10);
        }

        // When handling PickerPageUp (half of 10 = 5).
        handle_page_up(&mut state, &empty_pickers());

        // Then selection decrements by 5.
        assert_eq!(state.provider.provider_picker.selection(), 5);
    }

    use crate::feat::plugin_coordinator_actor::protocol::PluginPhase;

    fn plugin_state_with(phases: &[(&str, PluginPhase)]) -> AppState {
        let mut state = AppState::default();
        for (name, phase) in phases {
            state.plugins.set_phase((*name).to_owned(), *phase);
        }
        state
    }

    #[rstest::rstest]
    fn open_plugin_picker_loads_one_entry_per_cached_phase() {
        // Given two plugins mirrored into the contribution cache.
        let mut state = plugin_state_with(&[
            ("theme-loader", PluginPhase::Running),
            ("url-citations", PluginPhase::Dead),
        ]);

        // When opening the plugin picker.
        handle_open_picker(&mut state, PickerKind::Plugin, &empty_pickers());

        // Then the picker holds one entry per cached plugin.
        assert_eq!(state.frontend.plugin_picker().items().len(), 2);
    }

    #[rstest::rstest]
    fn open_plugin_picker_with_empty_cache_opens_empty() {
        // Given no plugins in the contribution cache.
        let mut state = AppState::default();

        // When opening the plugin picker.
        handle_open_picker(&mut state, PickerKind::Plugin, &empty_pickers());

        // Then the picker holds zero entries.
        assert!(state.frontend.plugin_picker().items().is_empty());
    }

    #[rstest::rstest]
    fn open_plugin_picker_lists_entries_in_name_order() {
        // Given plugins cached in non-alphabetical insertion order.
        let mut state = plugin_state_with(&[
            ("zeta", PluginPhase::Running),
            ("alpha", PluginPhase::Running),
        ]);

        // When opening the plugin picker.
        handle_open_picker(&mut state, PickerKind::Plugin, &empty_pickers());

        // Then entries follow name order (BTreeMap iteration).
        let names: Vec<&str> = state
            .frontend
            .plugin_picker()
            .items()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[rstest::rstest]
    fn open_plugin_picker_carries_entry_phase() {
        // Given one plugin cached with the Unresponsive phase.
        let mut state = plugin_state_with(&[("flood", PluginPhase::Unresponsive)]);

        // When opening the plugin picker.
        handle_open_picker(&mut state, PickerKind::Plugin, &empty_pickers());

        // Then the entry carries that phase.
        assert_eq!(
            state.frontend.plugin_picker().items()[0].phase,
            PluginPhase::Unresponsive
        );
    }

    #[rstest::rstest]
    fn confirm_plugin_picker_is_noop() {
        // Given a plugin picker open with one entry.
        let mut state = plugin_state_with(&[("theme-loader", PluginPhase::Running)]);
        handle_open_picker(&mut state, PickerKind::Plugin, &empty_pickers());

        // When confirming the selection.
        let (result, redispatch) = handle_picker_confirm(&mut state, &empty_pickers());

        // Then nothing is emitted and nothing is re-dispatched.
        assert!(result.message_names.is_empty());
        assert!(redispatch.is_none());
        // And the picker is still open (read-only; Enter does not close).
        assert_eq!(
            state.frontend.scope_stack.picker_kind(),
            Some(&PickerKind::Plugin)
        );
    }
}
