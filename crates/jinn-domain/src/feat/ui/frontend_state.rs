//! Frontend / UI state.

use jinn_sidebar_msg::SidebarScopeExt;
use parking_lot::RwLock;

use crate::common::focus::FocusScope;
use crate::common::tui_signals::TuiSignals;
use crate::feat::preferences_actor::UserPreferences;
use crate::feat::preferences_actor::app_state_file::AppStateFile;
use crate::feat::project_add_input::state::ProjectAddInputState;
use crate::feat::pruner_accumulation_input::state::PrunerAccumulationInputState;
use jinn_sidebar_msg::SidebarSectionId;

use crate::feat::session_lifecycle::arg_input_state::ArgInputState;
use crate::feat::theme::Theme;
use crate::feat::ui::picker_states::PickerStates;
pub use jinn_sidebar_msg::McpServersSectionState;
pub use jinn_sidebar_msg::PersonaSectionState;
pub use jinn_sidebar_msg::PinsState;
pub use jinn_sidebar_msg::SessionsSectionState;
pub use jinn_sidebar_msg::SidebarSections;
pub use jinn_sidebar_msg::TaskListSectionState;

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
    pub skill_preview_cache:
        std::sync::Arc<crate::feat::skills::skill_preview_cache::SkillPreviewCache>,
    /// Cached rendered lines for session preview popups.
    pub session_preview_cache: RwLock<jinn_sidebar_msg::SessionPreviewCache>,
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
    /// Cached copy of user preferences from `jinn.toml`.
    /// Updated by `PreferencesActor` inline after persisting to `jinn.toml` (authoritative),
    /// and by the `IntentHandler` for immediate UI feedback (exempt).
    pub preferences: UserPreferences,

    /// Cached copy of app state from `state.toml`.
    /// Updated by `AppStateActor` inline after persisting to `state.toml` (authoritative),
    /// and by the `IntentHandler` for immediate UI feedback (exempt).
    pub app_state: AppStateFile,

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

    /// Whether the "Press x again to teardown and archive 1 session" prompt is showing.
    /// OWNER: IntentHandler (set on first SidebarSessionClose, consumed on second
    ///         SidebarSessionClose or dismissed on any other key).
    pub close_session_prompt: bool,

    /// State of the "Press A again to archive N sessions" prompt.
    /// `Confirm` arms the confirm press; `Busy` blocks it (a member of the
    /// subtree is streaming). OWNER: IntentHandler (set on first
    /// SidebarSessionArchiveTree, consumed on second SidebarSessionArchiveTree,
    /// dismissed on any other key).
    pub archive_tree_prompt: Option<jinn_sidebar_msg::ArchiveTreePrompt>,

    /// All picker state - grouped for independent evolution.
    /// Use [`PickerExt`](super::picker_states::PickerExt) to access picker fields.
    pub pickers: PickerStates,

    /// Arg input popup state - active when `FocusScope::ArgInput` is on the scope stack.
    /// OWNER: IntentHandler (arg input editing, confirmation).
    pub arg_input: ArgInputState,

    /// Pruner accumulation threshold input popup state - active when
    /// `FocusScope::PrunerAccumulationInput` is on the scope stack.
    /// OWNER: IntentHandler (threshold input editing, confirmation).
    pub pruner_accumulation_input: PrunerAccumulationInputState,

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

    pub sidebar_width: u16,

    /// `@path` file popup state.
    /// OWNER: DirectoryListerActor (entries, loading, expected_request_id).
    pub file_picker: crate::feat::file_lister::FilePickerState,

    /// Late-attached handle to the slice registry, carrying the
    /// scope-focus cell (the focus stack, TUI signals, and quit latch).
    /// Attached once at wiring, before any intent can fire; a clone of
    /// `Slices` shares its cells, so cells minted after the attach are
    /// visible here. Before attach (or without the slice's
    /// `activate()`), facade reads return defaults and writes no-op —
    /// the removability property.
    /// (Composition attaches once; direct pokes defeat the facade.)
    #[doc(hidden)]
    pub scope_focus: std::sync::OnceLock<jinn_slices::Slices>,
}

impl Default for FrontendState {
    fn default() -> Self {
        Self {
            scope_focus: std::sync::OnceLock::new(),
            preferences: UserPreferences::default(),
            app_state: AppStateFile::default(),
            theme: crate::feat::theme::default_theme(),
            caches: FrontendCaches::default(),
            cancel_stream_prompt: false,
            audit_popup_visible: false,
            close_session_prompt: false,
            archive_tree_prompt: None,
            pickers: PickerStates::default(),
            arg_input: ArgInputState::default(),
            pruner_accumulation_input: PrunerAccumulationInputState::default(),
            project_add_input: ProjectAddInputState::default(),
            pending_creation: None,

            sidebar_width: 30,
            file_picker: crate::feat::file_lister::FilePickerState::default(),
        }
    }
}

impl FrontendState {
    /// Attaches the slice registry handle carrying the scope-focus
    /// cell. Called once at wiring; later calls are ignored.
    /// The attached slice registry, if composition attached one.
    ///
    /// Cell-backed state (personas, tool registry) resolves through this;
    /// `None` before attachment or in tests that skip activation.
    #[must_use]
    pub fn slices(&self) -> Option<&jinn_slices::Slices> {
        self.scope_focus.get()
    }

    pub fn attach_slices(&self, slices: jinn_slices::Slices) {
        let _ = self.scope_focus.set(slices);
    }

    /// Resolves the scope-focus cell, if the handle is attached and the
    /// slice's `activate()` minted it.
    fn scope_cell(&self) -> Option<jinn_slices::cell::TypedCell<jinn_slices::ScopeFocusState>> {
        let slices = self.scope_focus.get()?;
        slices.reader::<jinn_slices::ScopeFocusState>(&jinn_slices::scope_focus_slot())
    }

    /// Resolves the sidebar sections cell, if the handle is attached and
    /// the sidebar slice's `activate()` minted it.
    fn sections_cell(
        &self,
    ) -> Option<jinn_slices::cell::TypedCell<jinn_sidebar_msg::SidebarSections>> {
        let slices = self.scope_focus.get()?;
        slices
            .reader::<jinn_sidebar_msg::SidebarSections>(&jinn_sidebar_msg::sidebar_sections_slot())
    }

    /// Runs `f` against the five sidebar sections' state. A no-op when the
    /// cell is absent (slice not activated) — writes are silently dropped,
    /// matching the no-slice configuration.
    pub fn update_sections<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_sidebar_msg::SidebarSections),
    {
        if let Some(cell) = self.sections_cell() {
            cell.update(f);
        }
    }

    /// Reads the sidebar sections' state through `f`, falling back to
    /// `default` when the cell is absent.
    #[must_use]
    pub fn with_sections<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_sidebar_msg::SidebarSections) -> R,
        D: FnOnce() -> R,
    {
        match self.sections_cell() {
            Some(cell) => {
                let guard = cell.read();
                f(&guard)
            }
            None => default(),
        }
    }

    /// Resolves the theme slice's entries cell, if the handle is attached
    /// and the theme slice's `activate()` minted it.
    fn theme_entries_cell(
        &self,
    ) -> Option<jinn_slices::cell::TypedCell<jinn_theme_msg::ThemeEntries>> {
        let slices = self.scope_focus.get()?;
        slices.reader::<jinn_theme_msg::ThemeEntries>(&jinn_theme_msg::theme_entries_slot())
    }

    /// Reads the theme slice's entries through `f`, falling back to
    /// `default` when the cell is absent (slice not activated).
    #[must_use]
    pub fn with_theme_entries<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_theme_msg::ThemeEntries) -> R,
        D: FnOnce() -> R,
    {
        match self.theme_entries_cell() {
            Some(cell) => {
                let guard = cell.read();
                f(&guard)
            }
            None => default(),
        }
    }

    /// Runs `f` against the theme slice's entries. A no-op when the cell
    /// is absent (slice not activated) — writes are silently dropped,
    /// matching the no-slice configuration.
    pub fn update_theme_entries<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_theme_msg::ThemeEntries),
    {
        if let Some(cell) = self.theme_entries_cell() {
            cell.update(f);
        }
    }

    /// Runs `f` against the scope-focus state (stack, signals, quit).
    /// A no-op when the cell is absent (slice not activated) — writes
    /// are silently dropped, matching the no-slice configuration.
    pub fn update_scope<F>(&self, f: F)
    where
        F: FnOnce(&mut jinn_slices::ScopeFocusState),
    {
        if let Some(cell) = self.scope_cell() {
            cell.update(f);
        }
    }

    /// Reads the scope-focus state through `f`, falling back to
    /// `default` when the cell is absent.
    #[must_use]
    pub fn with_scope<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&jinn_slices::ScopeFocusState) -> R,
        D: FnOnce() -> R,
    {
        match self.scope_cell() {
            Some(cell) => f(&cell.read()),
            None => default(),
        }
    }

    /// The current (top) focus scope; [`FocusScope::Input`] before the
    /// slice is activated.
    #[must_use]
    pub fn scope(&self) -> FocusScope {
        self.with_scope(|s| s.stack.current().clone(), || FocusScope::Input)
    }

    /// Whether a quit has been requested; `false` before the slice is
    /// activated.
    #[must_use]
    pub fn quit(&self) -> bool {
        self.with_scope(|s| s.quit, || false)
    }

    /// Latches (or clears) the quit request.
    pub fn set_quit(&self, quit: bool) {
        self.update_scope(|s| s.quit = quit);
    }

    /// A snapshot copy of the current TUI signals; all-clear before the
    /// slice is activated.
    #[must_use]
    pub fn signals_snapshot(&self) -> TuiSignals {
        self.with_scope(|s| s.signals.clone(), TuiSignals::new)
    }

    /// The picker kind of the current scope, if a picker is focused.
    #[must_use]
    pub fn picker_kind(&self) -> Option<jinn_slices::PickerKind> {
        self.with_scope(|s| s.stack.picker_kind().copied(), || None)
    }

    /// Runs `f` with the current (top) scope. Prefer [`Self::scope`]
    /// for a clone.
    pub fn with_current<R, F, D>(&self, f: F, default: D) -> R
    where
        F: FnOnce(&FocusScope) -> R,
        D: FnOnce() -> R,
    {
        self.with_scope(|s| f(s.stack.current()), default)
    }

    /// Pushes a scope (entering an overlay).
    pub fn scope_push(&self, scope: FocusScope) {
        self.update_scope(|s| s.stack.push(scope));
    }

    /// Pops the top scope (leaving an overlay). No-op at the base.
    pub fn scope_pop(&self) {
        self.update_scope(|s| {
            let _ = s.stack.pop();
        });
    }

    /// Pops all overlay scopes, returning to the base.
    pub fn scope_clear_overlays(&self) {
        self.update_scope(|s| s.stack.clear_overlays());
    }

    /// Replaces the base scope and clears overlays.
    pub fn scope_swap_base(&self, new_base: FocusScope) {
        self.update_scope(|s| s.stack.swap_base(new_base));
    }

    /// Swaps the top scope for a different sidebar section (no-op when
    /// the current scope is not a sidebar section).
    pub fn scope_set_sidebar_section(&self, section: SidebarSectionId) {
        self.update_scope(|s| s.stack.set_sidebar_section(section));
    }

    /// The focused sidebar section, if a sidebar scope is active.
    #[must_use]
    pub fn sidebar_section(&self) -> Option<SidebarSectionId> {
        self.with_scope(|s| s.stack.sidebar_section(), || None)
    }

    /// Whether the current scope is a picker.
    #[must_use]
    pub fn is_picker(&self) -> bool {
        self.with_scope(|s| s.stack.is_picker(), || false)
    }

    /// Whether the current scope is any sidebar section.
    #[must_use]
    pub fn is_sidebar(&self) -> bool {
        self.with_scope(|s| s.stack.is_sidebar(), || false)
    }

    /// The scope one level below the top.
    #[must_use]
    pub fn scope_parent(&self) -> Option<FocusScope> {
        self.with_scope(|s| s.stack.parent().cloned(), || None)
    }

    /// The base (bottom) scope.
    #[must_use]
    pub fn scope_base(&self) -> FocusScope {
        self.with_scope(|s| s.stack.base().clone(), || FocusScope::Input)
    }

    /// Whether the stack has any scopes (always `true` after default).
    #[must_use]
    pub fn scope_len(&self) -> usize {
        self.with_scope(|s| s.stack.len(), || 0)
    }

    /// Drains the TUI signals: returns the current flags and resets
    /// them to all-clear (consume-once semantics for the run loop).
    #[must_use]
    pub fn take_signals(&self) -> TuiSignals {
        let taken = self.signals_snapshot();
        self.update_scope(|s| s.signals = TuiSignals::new());
        taken
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
