//! Picker state grouping and accessor trait.
//!
//! All picker-related state (picker widgets, snapshots, scroll offsets) is grouped
//! into [`PickerStates`]. The [`PickerExt`] extension trait provides accessor methods
//! on [`FrontendState`](super::FrontendState) so consumers are decoupled from the
//! internal storage layout.

use crate::feat::endpoint::picker_entry::EndpointEntry;
use std::collections::HashSet;

use crate::feat::mcp::picker_entry::McpServerEntry;
use crate::feat::persona::PersonaEntry;
use crate::feat::plugin::PluginPickerEntry;
use crate::feat::reasoning::ReasoningEffortEntry;
use crate::feat::session::picker_entry::SessionTreeEntry;
use crate::feat::session_lifecycle::picker_entry::SessionLifecycleEntry;
use crate::feat::skills::skill_entry::SkillEntry;
use crate::feat::theme::Theme;
use crate::feat::theme::ThemeEntry;
use crate::feat::todo_list::picker_entry::TaskListTreeEntry;
use crate::feat::tools_actor::tool_entry::ToolEntry;

/// All picker state - grouped so the picker subsystem can evolve independently.
///
/// Each picker has its own selection state and optional companion fields
/// (snapshots, scroll offsets) used during the picker's open/close lifecycle.
#[derive(Debug, Default)]
pub struct PickerStates {
    /// Session picker state (items, filter text, selection index).
    /// OWNER: IntentHandler (session picker navigation).
    pub session_picker:
        jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<SessionTreeEntry>>,

    /// Persona picker state (items, filter text, selection index).
    /// OWNER: IntentHandler (persona picker navigation).
    pub persona_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PersonaEntry>>,

    /// Theme picker state (items, filter text, selection index).
    /// OWNER: IntentHandler (theme picker navigation).
    pub theme_picker: jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ThemeEntry>>,

    /// Saved theme before preview - restored on ESC.
    /// OWNER: IntentHandler (set on theme picker open, consumed on confirm/cancel).
    pub theme_preview_original: Option<Theme>,

    /// Tool picker state - shows all registered tools with toggle state.
    /// OWNER: IntentHandler (populated on tool picker open).
    pub tool_picker: jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ToolEntry>>,

    /// Snapshot of disabled tools before picker opens - restored on ESC.
    /// OWNER: IntentHandler (set on tool picker open, consumed on confirm/cancel).
    pub tool_picker_snapshot: Option<HashSet<String>>,

    /// Skill picker state - shows all discovered skills with toggle state.
    /// OWNER: IntentHandler (populated on skill picker open).
    pub skill_picker: jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SkillEntry>>,

    /// Snapshot of disabled skills before picker opens - restored on ESC.
    /// OWNER: IntentHandler (set on skill picker open, consumed on confirm/cancel).
    pub skill_picker_snapshot: Option<HashSet<String>>,

    /// Preview pane scroll offsets for spec-driven pickers, keyed by
    /// picker id.
    pub pickers_scrolls: jinn_picker::PickerScrolls,

    /// Session lifecycle picker state (items, filter text, selection index).
    /// OWNER: IntentHandler (lifecycle picker navigation).
    pub session_lifecycle_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SessionLifecycleEntry>>,

    /// Reasoning effort picker state (items, filter text, selection index).
    /// OWNER: IntentHandler (reasoning effort picker navigation).
    pub reasoning_effort_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ReasoningEffortEntry>>,

    /// Task list picker state - read-only zoom view of the active session's task list.
    /// OWNER: IntentHandler (populated on task list picker open).
    pub task_list_picker:
        jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<TaskListTreeEntry>>,

    pub project_picker:
        jinn_selection_widget::SelectionState<crate::feat::project::picker_entry::ProjectEntry>,

    /// Measured results-area row count for the currently-active picker, as
    /// written by the TUI render pre-pass each frame. Used by the picker
    /// navigation intents to keep the cursor inside the visible window.
    ///
    /// Zero before the first render of a picker; the intent layer falls back
    /// to a sane default in that case.
    /// OWNER: TUI render pre-pass (writes) / IntentHandler (reads via
    /// `active_viewport`).
    pub picker_results_viewport: u16,

    /// MCP server picker state - shows configured servers with toggle state.
    /// OWNER: IntentHandler (populated on MCP picker open).
    pub mcp_server_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<McpServerEntry>>,

    /// Plugin picker state - read-only list of loaded plugins.
    /// OWNER: IntentHandler (populated on plugin picker open).
    pub plugin_picker:
        jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PluginPickerEntry>>,

    /// Snapshot of enabled MCP servers before picker opens - restored on ESC.
    /// OWNER: IntentHandler (set on MCP picker open, consumed on confirm/cancel).
    pub mcp_server_picker_snapshot: Option<std::collections::BTreeSet<String>>,

    /// OpenRouter endpoint picker state - one row per routing upstream.
    /// OWNER: IntentHandler (populated on endpoint picker open).
    pub endpoint_picker: jinn_selection_widget::SelectionState<EndpointEntry>,

    /// True while an endpoint fetch is in flight (open or `<c-r>` refresh).
    /// Set synchronously by the open/refresh intent; cleared by `ProviderActor`
    /// when it writes items back (success or error).
    /// OWNER: IntentHandler (sets) / ProviderActor (clears).
    pub endpoint_loading: bool,

    /// When the endpoint cache for the active model was last populated.
    /// Set by `ProviderActor` on a successful fetch (and preserved on a
    /// cache-served open). Survives across picker opens so the footer can
    /// show "fetched Xs ago".
    /// OWNER: ProviderActor.
    pub endpoint_fetched_at: Option<jiff::Timestamp>,
}

/// Extension trait providing typed access to picker state on [`FrontendState`](super::FrontendState).
///
/// Import this trait to access picker fields through methods instead of direct field access.
/// This decouples consumers from the internal storage layout of `FrontendState`.
pub trait PickerExt {
    /// Read-only access to the session picker state.
    fn session_picker(
        &self,
    ) -> &jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<SessionTreeEntry>>;
    /// Mutable access to the session picker state.
    fn session_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<SessionTreeEntry>>;

    /// Read-only access to the persona picker state.
    fn persona_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PersonaEntry>>;
    /// Mutable access to the persona picker state.
    fn persona_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PersonaEntry>>;

    /// Read-only access to the theme picker state.
    fn theme_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ThemeEntry>>;
    /// Mutable access to the theme picker state.
    fn theme_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ThemeEntry>>;
    /// Read-only access to the saved theme before preview.
    fn theme_preview_original(&self) -> &Option<Theme>;
    /// Mutable access to the saved theme before preview.
    fn theme_preview_original_mut(&mut self) -> &mut Option<Theme>;

    /// Read-only access to the tool picker state.
    fn tool_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ToolEntry>>;
    /// Mutable access to the tool picker state.
    fn tool_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ToolEntry>>;
    /// Read-only access to the disabled tools snapshot.
    fn tool_picker_snapshot(&self) -> &Option<HashSet<String>>;
    /// Mutable access to the disabled tools snapshot.
    fn tool_picker_snapshot_mut(&mut self) -> &mut Option<HashSet<String>>;

    /// Read-only access to the skill picker state.
    fn skill_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SkillEntry>>;
    /// Mutable access to the skill picker state.
    fn skill_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SkillEntry>>;
    /// Read-only access to the disabled skills snapshot.
    fn skill_picker_snapshot(&self) -> &Option<HashSet<String>>;
    /// Mutable access to the disabled skills snapshot.
    fn skill_picker_snapshot_mut(&mut self) -> &mut Option<HashSet<String>>;
    /// Read-only access to the enabled MCP servers snapshot.
    fn mcp_server_picker_snapshot(&self) -> &Option<std::collections::BTreeSet<String>>;
    /// Mutable access to the enabled MCP servers snapshot.
    fn mcp_server_picker_snapshot_mut(&mut self)
    -> &mut Option<std::collections::BTreeSet<String>>;
    /// Read-only access to the session lifecycle picker state.
    fn session_lifecycle_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SessionLifecycleEntry>>;
    /// Mutable access to the session lifecycle picker state.
    fn session_lifecycle_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SessionLifecycleEntry>>;

    /// Read-only access to the reasoning effort picker state.
    fn reasoning_effort_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ReasoningEffortEntry>>;
    /// Mutable access to the reasoning effort picker state.
    fn reasoning_effort_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ReasoningEffortEntry>>;

    /// Read-only access to the task list picker state.
    fn task_list_picker(
        &self,
    ) -> &jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<TaskListTreeEntry>>;
    /// Mutable access to the task list picker state.
    fn task_list_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<TaskListTreeEntry>>;

    /// Read-only access to the project picker state.
    fn project_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<crate::feat::project::picker_entry::ProjectEntry>;
    fn project_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<crate::feat::project::picker_entry::ProjectEntry>;

    /// Read-only access to the MCP server picker state.
    fn mcp_server_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<McpServerEntry>>;
    /// Mutable access to the MCP server picker state.
    fn mcp_server_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<McpServerEntry>>;

    /// Read-only access to the plugin picker state.
    fn plugin_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PluginPickerEntry>>;
    /// Mutable access to the plugin picker state.
    fn plugin_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PluginPickerEntry>>;

    /// Read-only access to the OpenRouter endpoint picker state.
    fn endpoint_picker(&self) -> &jinn_selection_widget::SelectionState<EndpointEntry>;
    /// Mutable access to the OpenRouter endpoint picker state.
    fn endpoint_picker_mut(&mut self) -> &mut jinn_selection_widget::SelectionState<EndpointEntry>;

    fn picker_results_viewport(&self) -> u16;

    /// Updates the measured results-area row count. Called once per frame
    /// from the render pre-pass.
    fn set_picker_results_viewport(&mut self, val: u16);
}

impl PickerExt for super::frontend_state::FrontendState {
    fn session_picker(
        &self,
    ) -> &jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<SessionTreeEntry>> {
        &self.pickers.session_picker
    }

    fn session_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<SessionTreeEntry>>
    {
        &mut self.pickers.session_picker
    }

    fn persona_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PersonaEntry>> {
        &self.pickers.persona_picker
    }

    fn persona_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PersonaEntry>> {
        &mut self.pickers.persona_picker
    }

    fn theme_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ThemeEntry>> {
        &self.pickers.theme_picker
    }

    fn theme_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ThemeEntry>> {
        &mut self.pickers.theme_picker
    }

    fn theme_preview_original(&self) -> &Option<Theme> {
        &self.pickers.theme_preview_original
    }

    fn theme_preview_original_mut(&mut self) -> &mut Option<Theme> {
        &mut self.pickers.theme_preview_original
    }

    fn tool_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ToolEntry>> {
        &self.pickers.tool_picker
    }

    fn tool_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ToolEntry>> {
        &mut self.pickers.tool_picker
    }

    fn tool_picker_snapshot(&self) -> &Option<HashSet<String>> {
        &self.pickers.tool_picker_snapshot
    }

    fn tool_picker_snapshot_mut(&mut self) -> &mut Option<HashSet<String>> {
        &mut self.pickers.tool_picker_snapshot
    }

    fn skill_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SkillEntry>> {
        &self.pickers.skill_picker
    }

    fn skill_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SkillEntry>> {
        &mut self.pickers.skill_picker
    }

    fn skill_picker_snapshot(&self) -> &Option<HashSet<String>> {
        &self.pickers.skill_picker_snapshot
    }

    fn skill_picker_snapshot_mut(&mut self) -> &mut Option<HashSet<String>> {
        &mut self.pickers.skill_picker_snapshot
    }

    fn mcp_server_picker_snapshot(&self) -> &Option<std::collections::BTreeSet<String>> {
        &self.pickers.mcp_server_picker_snapshot
    }

    fn mcp_server_picker_snapshot_mut(
        &mut self,
    ) -> &mut Option<std::collections::BTreeSet<String>> {
        &mut self.pickers.mcp_server_picker_snapshot
    }

    fn session_lifecycle_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SessionLifecycleEntry>>
    {
        &self.pickers.session_lifecycle_picker
    }

    fn session_lifecycle_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<SessionLifecycleEntry>>
    {
        &mut self.pickers.session_lifecycle_picker
    }
    fn reasoning_effort_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ReasoningEffortEntry>>
    {
        &self.pickers.reasoning_effort_picker
    }

    fn reasoning_effort_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<ReasoningEffortEntry>>
    {
        &mut self.pickers.reasoning_effort_picker
    }

    fn task_list_picker(
        &self,
    ) -> &jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<TaskListTreeEntry>> {
        &self.pickers.task_list_picker
    }

    fn task_list_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::TreePickerState<jinn_picker::PickerEntry<TaskListTreeEntry>>
    {
        &mut self.pickers.task_list_picker
    }
    fn project_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<crate::feat::project::picker_entry::ProjectEntry>
    {
        &self.pickers.project_picker
    }

    fn project_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<crate::feat::project::picker_entry::ProjectEntry>
    {
        &mut self.pickers.project_picker
    }

    fn mcp_server_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<McpServerEntry>> {
        &self.pickers.mcp_server_picker
    }

    fn mcp_server_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<McpServerEntry>> {
        &mut self.pickers.mcp_server_picker
    }

    fn plugin_picker(
        &self,
    ) -> &jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PluginPickerEntry>> {
        &self.pickers.plugin_picker
    }

    fn plugin_picker_mut(
        &mut self,
    ) -> &mut jinn_selection_widget::SelectionState<jinn_picker::PickerEntry<PluginPickerEntry>>
    {
        &mut self.pickers.plugin_picker
    }

    fn endpoint_picker(&self) -> &jinn_selection_widget::SelectionState<EndpointEntry> {
        &self.pickers.endpoint_picker
    }

    fn endpoint_picker_mut(&mut self) -> &mut jinn_selection_widget::SelectionState<EndpointEntry> {
        &mut self.pickers.endpoint_picker
    }

    fn picker_results_viewport(&self) -> u16 {
        self.pickers.picker_results_viewport
    }

    fn set_picker_results_viewport(&mut self, val: u16) {
        self.pickers.picker_results_viewport = val;
    }
}
