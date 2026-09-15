//! Shared application state.
//!
//! [`AppState`] is the single source of truth for what the user sees and how the
//! application is currently behaving. Every component reads from and writes to this
//! shared state.
//!
//! Fields are grouped into owner-named structs (`Session`, `Context`, `Provider`,
//! `Shutdown`, `Frontend`) to make cross-boundary writes visually obvious during
//! code review. Each group struct carries `/// OWNER:` documentation on the struct
//! and on each field.

pub use crate::common::focus::{FocusScope, ScopeStack};
pub use crate::common::session_map::SessionLoadGuard;
pub use crate::feat::context::assembly_state::ContextAssemblyState;
pub use crate::feat::provider::ProviderState;
pub use crate::feat::pruner_accumulation_input::state::PrunerAccumulationInputState;
pub use crate::feat::rename_session_input::state::RenameSessionInputState;

pub use crate::feat::session_lifecycle::arg_input_state::ArgInputState;
pub use crate::feat::ui::frontend_state::{FrontendCaches, FrontendState};

use crate::protocol::{ChatEntryId, PickerKind, PinPosition, SessionId};

use crate::common::session_map::SessionMap;
pub use crate::feat::chat_input::ChatInputBoxState;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::ui::picker_states::PickerExt;

/// Written to exclusively by `SessionPersistenceActor` and `IntentHandler`.
/// No other actor should mutate these fields.
///
/// See [`SessionMap`] for the full API.
pub type SessionState = SessionMap;

/// A snapshot of everything the application is doing right now.
#[derive(Debug, Default)]
pub struct AppState {
    /// Session lifecycle state - owned by session-actor.
    pub session: SessionState,
    /// Context assembly state - owned by context-actor.
    pub context: ContextAssemblyState,
    /// Provider selection state - owned by provider-actor.
    pub provider: ProviderState,
    /// Frontend / UI state - owned by IntentHandler.
    pub frontend: FrontendState,
    /// Plugin contributions - owned by plugin-coordinator-actor.
    pub plugins: crate::feat::plugin::PluginContributions,
}

impl AppState {
    /// Returns a mutable reference to the active picker's navigation interface.
    ///
    /// Returns `None` if no picker is currently active.
    /// Use for operations that work the same way on all picker types
    /// (insert char, backspace, move up/down, cursor left/right).
    pub fn active_picker_ops(&mut self) -> Option<&mut dyn jinn_selection_widget::PickerOps> {
        let kind = self.frontend.scope_stack.picker_kind().copied()?;
        match kind {
            PickerKind::Provider => Some(&mut self.provider.provider_picker),
            PickerKind::Session => Some(self.frontend.session_picker_mut()),
            PickerKind::Persona => Some(self.frontend.persona_picker_mut()),
            PickerKind::Theme => Some(self.frontend.theme_picker_mut()),

            PickerKind::SessionLifecycle => Some(self.frontend.session_lifecycle_picker_mut()),
            PickerKind::ReasoningEffort => Some(self.frontend.reasoning_effort_picker_mut()),
            PickerKind::Tool => Some(self.frontend.tool_picker_mut()),
            PickerKind::Skill => Some(self.frontend.skill_picker_mut()),
            PickerKind::TaskList => Some(self.frontend.task_list_picker_mut()),
            PickerKind::Project => Some(self.frontend.project_picker_mut()),
            PickerKind::McpServer => Some(self.frontend.mcp_server_picker_mut()),
            PickerKind::Plugin => Some(self.frontend.plugin_picker_mut()),
            PickerKind::Endpoint => Some(self.frontend.endpoint_picker_mut()),
        }
    }
    /// Read-only access to the active picker's navigation interface.
    ///
    /// Returns `None` if no picker is currently active.
    /// Companion to [`AppState::active_picker_ops`] for the read-only
    /// `is_filter_empty` check used by the `CtrlClear` intent.
    pub fn active_picker_ops_ref(&self) -> Option<&dyn jinn_selection_widget::PickerOps> {
        let kind = self.frontend.scope_stack.picker_kind().copied()?;
        match kind {
            PickerKind::Provider => Some(&self.provider.provider_picker),
            PickerKind::Session => Some(self.frontend.session_picker()),
            PickerKind::Persona => Some(self.frontend.persona_picker()),
            PickerKind::Theme => Some(self.frontend.theme_picker()),

            PickerKind::SessionLifecycle => Some(self.frontend.session_lifecycle_picker()),
            PickerKind::ReasoningEffort => Some(self.frontend.reasoning_effort_picker()),
            PickerKind::Tool => Some(self.frontend.tool_picker()),
            PickerKind::Skill => Some(self.frontend.skill_picker()),
            PickerKind::TaskList => Some(self.frontend.task_list_picker()),
            PickerKind::Project => Some(self.frontend.project_picker()),
            PickerKind::McpServer => Some(self.frontend.mcp_server_picker()),
            PickerKind::Plugin => Some(self.frontend.plugin_picker()),
            PickerKind::Endpoint => Some(self.frontend.endpoint_picker()),
        }
    }

    /// Read-only access to the active chat session.
    ///
    /// Infallible - `SessionMap` guarantees the active session exists.
    pub fn active_session(&self) -> &ChatSessionState {
        self.session.active_session()
    }

    /// Mutable access to the active chat session.
    ///
    /// Infallible - `SessionMap` guarantees the active session exists.
    pub fn active_session_mut(&mut self) -> &mut ChatSessionState {
        self.session.active_session_mut()
    }

    /// Read-only access to a session by ID.
    ///
    /// # Panics
    ///
    /// Panics if the given session ID does not exist.
    pub fn session(&self, id: &SessionId) -> &ChatSessionState {
        self.session.get_unchecked(id)
    }

    /// Mutable access to a session by ID.
    ///
    /// # Panics
    ///
    /// Panics if the given session ID does not exist.
    pub fn session_mut(&mut self, id: &SessionId) -> &mut ChatSessionState {
        self.session.get_unchecked_mut(id)
    }

    /// Fallible read-only access to a session by ID.
    ///
    /// Returns `None` if the session does not exist. Use this from paths
    /// where the session may have been closed concurrently (e.g. scan actors
    /// that received a command for an ID that is no longer present).
    #[must_use]
    pub fn try_session(&self, id: &SessionId) -> Option<&ChatSessionState> {
        self.session.get(id)
    }

    /// Fallible mutable access to a session by ID.
    ///
    /// Returns `None` if the session does not exist.
    #[must_use]
    pub fn try_session_mut(&mut self, id: &SessionId) -> Option<&mut ChatSessionState> {
        self.session.get_mut(id)
    }

    /// Returns mutable access to a session by ID, creating it if missing.
    ///
    /// Used by streaming handlers that receive tokens from actors
    /// which may create new session IDs not yet present in the
    /// sessions map.
    pub fn session_mut_or_create(&mut self, id: &SessionId) -> &mut ChatSessionState {
        self.session.get_or_create(id)
    }

    /// Read-only access to the active session's input box.
    ///
    /// Delegates to [`ChatSessionState::chat_input`] on the active session.
    ///
    /// # Panics
    ///
    /// Panics if the active session does not exist in the sessions map.
    pub fn active_chat_input(&self) -> &ChatInputBoxState {
        self.active_session().chat_input()
    }

    /// Mutable access to the active session's input box.
    ///
    /// Delegates to [`ChatSessionState::chat_input_mut`] on the active session.
    ///
    /// # Panics
    ///
    /// Panics if the active session does not exist in the sessions map.
    pub fn active_chat_input_mut(&mut self) -> &mut ChatInputBoxState {
        self.active_session_mut().chat_input_mut()
    }

    /// Returns pinned entry IDs sorted by position for the active session.
    ///
    /// Order: TOP entries first, then RELATIVE, then BOTTOM.
    /// Within each group, entries maintain their original history order (stable sort).
    #[must_use]
    pub fn sorted_pinned_ids(&self) -> Vec<ChatEntryId> {
        let mut pinned = self.active_session().pinned_entries();
        pinned.sort_by_key(|entry| pin_sort_key(entry.pin_position));
        pinned.iter().map(|e| e.id.clone()).collect()
    }

    /// Invalidate all theme-sensitive caches. Called when the active theme changes.
    pub fn invalidate_theme_caches(&self) {
        self.frontend.caches.invalidate_all();
    }
}

/// Returns the sort key for a pin position.
///
/// TOP = 0, RELATIVE (or None) = 1, BOTTOM = 2.
/// Used to sort pinned entries in display order.
#[must_use]
pub fn pin_sort_key(position: Option<PinPosition>) -> u8 {
    match position {
        Some(PinPosition::Top) => 0,
        Some(PinPosition::Relative) | None => 1,
        Some(PinPosition::Bottom) => 2,
    }
}
