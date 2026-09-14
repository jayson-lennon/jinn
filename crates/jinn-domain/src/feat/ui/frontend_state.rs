//! Frontend / UI state.

use parking_lot::RwLock;

use crate::common::focus::{FocusScope, ScopeStack};
use crate::common::tui_signals::TuiSignals;
use crate::feat::cwd_input::state::CwdInputState;
use crate::feat::preferences_actor::UserPreferences;
use crate::feat::preferences_actor::app_state_file::AppStateFile;
use crate::feat::project_add_input::state::ProjectAddInputState;
use crate::feat::pruner_accumulation_input::state::PrunerAccumulationInputState;
use crate::feat::rename_session_input::state::RenameSessionInputState;

use crate::feat::session_lifecycle::arg_input_state::ArgInputState;
use crate::feat::theme::Theme;
use crate::feat::ui::picker_states::PickerStates;
pub use crate::feat::ui::sidebar::mcp_servers_section::McpServersSectionState;
pub use crate::feat::ui::sidebar::persona_section::PersonaSectionState;
pub use crate::feat::ui::sidebar::pins::state::PinsState;
pub use crate::feat::ui::sidebar::sessions::SessionsSectionState;
use crate::feat::ui::sidebar::state::SidebarState;
pub use crate::feat::ui::sidebar::task_list_section::TaskListSectionState;

/// Theme-sensitive caches owned by the frontend.
///
/// All caches that store pre-rendered styled data (which embeds theme colors)
/// live here so they can be invalidated in one call when the theme changes.
///
/// Each cache is either a `RwLock` (render code borrows mutably while
/// holding shared references to the rest of `AppState`) or an `Arc` of an
/// interior-mutable cache that is also lent to spec-driven pickers through
/// the [`jinn_picker::PickerHost`](jinn_picker::PickerHost) seam.
#[derive(Debug, Default)]
pub struct FrontendCaches {
    /// Cached wrapped line counts and rendered lines per chat entry.
    pub entry_line_cache: RwLock<crate::feat::ui::chat_log::line_count_cache::EntryLineCache>,
    /// Cached rendered lines for skill-preview popups. An `Arc` handle so
    /// the skill picker's host lens can lend it to the spec's render path.
    pub skill_preview_cache: std::sync::Arc<crate::feat::skills::skill_preview_cache::SkillPreviewCache>,
    /// Cached rendered lines for session preview popups.
    pub session_preview_cache:
        RwLock<crate::feat::ui::sidebar::sessions::preview::SessionPreviewCache>,
}

impl FrontendCaches {
    /// Invalidate all caches. Called when the active theme changes.
    pub fn invalidate_all(&self) {
        self.entry_line_cache.write().clear();
        self.session_preview_cache.write().clear();
        self.skill_preview_cache.clear();
    }
}

/// Session creation in flight from the projects UI, stashed across the
/// project-picker → lifecycle-picker → arg-input chain.
///
/// The two fields are deliberately independent: lifecycle setup scripts may
/// re-cwd the session after creation, so the project association must not be
/// derived from (or conflated with) the session's cwd.
#[derive(Debug, Clone)]
pub struct PendingSessionCreation {
    /// Project directory the session is associated with. Stamped into the
    /// session's project metadata at creation; never follows later cwd changes.
    pub project_dir: std::path::PathBuf,
    /// The new session's starting cwd (scripts may later override it).
    pub starting_cwd: std::path::PathBuf,
}

/// Frontend / UI state.
///
/// Each field is owned by exactly one actor (its authoritative writer) OR by
/// the `IntentHandler` (the synchronous frontend mutator, exempt from the
/// one-writer rule). The `IntentHandler` writes fields for immediate UI feedback;
/// an actor that persists the field's underlying data is the authoritative
/// writer that reconciles it. An actor writing a frontend field it owns is
/// correct — see AGENTS.md §3 on actor state ownership and the "sync sibling"
/// anti-pattern.
#[derive(Debug)]
pub struct FrontendState {
    /// Set to `true` when the user has requested to quit.
    /// OWNER: IntentHandler (Quit intent),
    ///        shutdown-tracker (ProceedWithShutdown command).
    pub should_quit: bool,

    /// Pins sidebar section state - selection index within the pinned entries list.
    /// OWNER: IntentHandler (pins navigation).
    pub pins: PinsState,

    /// Sidebar state - focus tracking.
    /// OWNER: IntentHandler (sidebar focus/leave).
    pub sidebar: SidebarState,

    /// Persona sidebar section state - cursor tracking.
    /// OWNER: IntentHandler (sidebar navigation).
    pub persona_section: PersonaSectionState,

    /// Sessions sidebar section state - cursor tracking.
    /// OWNER: IntentHandler (sidebar navigation).
    pub sessions_section: SessionsSectionState,

    /// Task list sidebar section state - phase cursor tracking.
    /// OWNER: IntentHandler (sidebar navigation).
    pub task_list_section: TaskListSectionState,
    /// MCP servers sidebar section state - cursor tracking.
    /// OWNER: IntentHandler (sidebar navigation).
    pub mcp_servers_section: McpServersSectionState,
    /// Signals from the IntentHandler for the outer platform layer.
    /// OWNER: IntentHandler (cleared and set each handle() call).
    pub tui_signals: TuiSignals,

    /// Cached copy of user preferences from `jinn.toml`.
    /// Updated by `PreferencesActor` inline after persisting to `jinn.toml` (authoritative),
    /// and by the `IntentHandler` for immediate UI feedback (exempt).
    pub preferences: UserPreferences,

    /// Cached copy of app state from `state.toml`.
    /// Updated by `AppStateActor` inline after persisting to `state.toml` (authoritative),
    /// and by the `IntentHandler` for immediate UI feedback (exempt).
    pub app_state: AppStateFile,

    /// Focus scope stack - single source of truth for what the user is focused on.
    /// OWNER: IntentHandler (push/pop on scope transitions).
    pub scope_stack: ScopeStack,

    /// The current resolved theme (colors for the render pipeline).
    /// OWNER: IntentHandler (theme picker preview, exempt), AppStateActor (authoritative, on state.toml change).
    pub theme: Theme,

    /// Theme-sensitive caches. Invalidated when `theme` changes.
    /// OWNER: IntentHandler (cleared on theme change).
    pub caches: FrontendCaches,

    /// Whether the "Press ESC again to cancel" prompt is showing.
    /// OWNER: IntentHandler (set on first ESC in Normal/Sidebar with active stream,
    ///         consumed on second ESC or dismissed on any other key).
    pub cancel_stream_prompt: bool,

    /// Whether the audit popup is shown for the currently selected chat entry.
    /// OWNER: IntentHandler (ToggleAuditPopup intent).
    /// Global toggle (not per-session); not persisted across process restarts.
    pub audit_popup_visible: bool,

    /// Transient one-line hint shown in the status bar (e.g. "that session
    /// has no live terminal"). `None` hides it. The next intent clears it, so
    /// it never lingers past the action that raised it.
    /// OWNER: IntentHandler (raise); every handled intent dismisses it.
    pub status_hint: Option<String>,

    /// Whether the "Press x again to teardown and archive 1 session" prompt is showing.
    /// OWNER: IntentHandler (set on first SidebarSessionClose, consumed on second
    ///         SidebarSessionClose or dismissed on any other key).
    pub close_session_prompt: bool,

    /// State of the "Press A again to archive N sessions" prompt.
    /// `Confirm` arms the confirm press; `Busy` blocks it (a member of the
    /// subtree is streaming). OWNER: IntentHandler (set on first
    /// SidebarSessionArchiveTree, consumed on second SidebarSessionArchiveTree,
    /// dismissed on any other key).
    pub archive_tree_prompt:
        Option<crate::feat::ui::sidebar::sessions::archive_tree::ArchiveTreePrompt>,

    /// All picker state - grouped for independent evolution.
    /// Use [`PickerExt`](super::picker_states::PickerExt) to access picker fields.
    pub pickers: PickerStates,

    /// Path to the themes directory (`~/.config/jinn/themes/`).
    /// Set once during init from `AppPaths`. Used by the theme picker to discover themes.
    /// OWNER: Init code (set once at startup).
    pub themes_dir: std::path::PathBuf,

    /// Path to the system themes directory (`/usr/share/jinn/themes/`).
    /// Set once during init from `AppPaths`. Used as fallback for theme discovery.
    /// OWNER: Init code (set once at startup).
    pub system_themes_dir: std::path::PathBuf,

    /// Arg input popup state - active when `FocusScope::ArgInput` is on the scope stack.
    /// OWNER: IntentHandler (arg input editing, confirmation).
    pub arg_input: ArgInputState,

    /// Rename session input popup state - active when `FocusScope::RenameSessionInput` is on the scope stack.
    /// OWNER: IntentHandler (rename input editing, confirmation).
    pub rename_session_input: RenameSessionInputState,

    /// Pruner accumulation threshold input popup state - active when
    /// `FocusScope::PrunerAccumulationInput` is on the scope stack.
    /// OWNER: IntentHandler (threshold input editing, confirmation).
    pub pruner_accumulation_input: PrunerAccumulationInputState,

    /// Cwd input popup state - active when `FocusScope::CwdInput` is on the scope stack.
    /// OWNER: IntentHandler (cwd input editing, confirmation).
    pub cwd_input: CwdInputState,

    /// Project-add input popup state - active when `FocusScope::ProjectAddInput` is on
    /// the scope stack.
    /// OWNER: IntentHandler (project-add input editing, confirmation).
    pub project_add_input: ProjectAddInputState,

    /// Creation stash for the next session from the projects UI.
    ///
    /// Set by the project picker (`<enter>`/`<c-enter>`) so a new session can
    /// be rooted at a chosen project directory and stamped with that project,
    /// without mutating the active session's CWD. Consumed (and cleared) by
    /// `handle_session_lifecycle_setup` when the new session is created.
    /// OWNER: IntentHandler (set by project picker, consumed by session creation).
    pub pending_creation: Option<PendingSessionCreation>,

    /// Terminal tab state - mirror of the active `interactive_term` session.
    /// OWNER: InteractiveTermActor (screen/control events); the IntentHandler
    /// flips the control holder on takeover intents (exempt writer).
    pub terminal: crate::feat::interactive_term::terminal_tab_state::TerminalTabState,

    pub sidebar_width: u16,

    /// `@path` file popup state.
    /// OWNER: DirectoryListerActor (entries, loading, expected_request_id).
    pub file_picker: crate::feat::file_lister::FilePickerState,
}

impl Default for FrontendState {
    fn default() -> Self {
        let mut scope_stack = ScopeStack::default();
        scope_stack.push(FocusScope::Input);
        Self {
            should_quit: false,
            pins: PinsState::default(),
            sidebar: SidebarState,
            persona_section: PersonaSectionState::default(),
            sessions_section: SessionsSectionState::default(),
            task_list_section: TaskListSectionState::default(),
            mcp_servers_section: McpServersSectionState::default(),
            tui_signals: TuiSignals::new(),
            preferences: UserPreferences::default(),
            app_state: AppStateFile::default(),
            scope_stack,
            theme: crate::feat::theme::default_theme(),
            caches: FrontendCaches::default(),
            cancel_stream_prompt: false,
            audit_popup_visible: false,
            status_hint: None,
            close_session_prompt: false,
            archive_tree_prompt: None,
            pickers: PickerStates::default(),
            themes_dir: std::path::PathBuf::new(),
            system_themes_dir: std::path::PathBuf::new(),
            arg_input: ArgInputState::default(),
            rename_session_input: RenameSessionInputState::default(),
            pruner_accumulation_input: PrunerAccumulationInputState::default(),
            cwd_input: CwdInputState::default(),
            project_add_input: ProjectAddInputState::default(),
            pending_creation: None,
            terminal: crate::feat::interactive_term::terminal_tab_state::TerminalTabState::default(
            ),

            sidebar_width: 30,
            file_picker: crate::feat::file_lister::FilePickerState::default(),
        }
    }
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
    use jinn_selection_widget::PreviewCache;
    use ratatui::text::Line;

    /// `invalidate_all` (called on theme change) must clear the skill preview cache
    /// so stale theme-colored lines are never displayed after a theme switch.
    #[rstest::rstest]
    #[test]
    fn invalidate_all_clears_skill_preview_cache() {
        // Given a populated skill preview cache.
        let caches = FrontendCaches::default();
        caches.skill_preview_cache.insert(
            crate::feat::skills::skill_entry::body_hash_key("## body"),
            80,
            vec![Line::raw("old-theme")],
        );
        assert_eq!(caches.skill_preview_cache.len(), 1);

        // When the theme changes and all caches are invalidated.
        caches.invalidate_all();

        // Then the skill preview cache is empty (the AC under test).
        assert!(
            caches.skill_preview_cache.is_empty(),
            "theme change must clear skill preview cache via invalidate_all"
        );
    }

    #[rstest::rstest]
    #[test]
    fn default_includes_empty_reasoning_effort_picker() {
        // Given a default FrontendState.
        let state = FrontendState::default();

        // When accessing the reasoning effort picker.
        // Then it exists and is empty (no items).
        assert_eq!(state.pickers.reasoning_effort_picker.items().len(), 0);
    }
}
