//! Frontend / UI state.
//!
//! The frontend half of [`AppState`](crate::app_state::AppState): what the
//! user sees, which overlays are open, and which theme is in force. It sits
//! beside [`AppState`](crate::app_state::AppState) rather than under any one
//! feature so that a consumer of the state never has to load a feature's code
//! to name a type.

use jinn_chat_input_msg::FilePickerState;
use jinn_sidebar_msg::SidebarScopeExt;

use jinn_preferences_config::app_state_file::AppStateFile;
use jinn_sidebar_msg::SidebarSectionId;
use jinn_slices::FocusScope;
use jinn_slices::TuiSignals;
use jinn_slices::cell::TypedCell;

pub use jinn_sidebar_msg::McpServersSectionState;
pub use jinn_sidebar_msg::PersonaSectionState;
pub use jinn_sidebar_msg::PinsState;
pub use jinn_sidebar_msg::SessionsSectionState;
pub use jinn_sidebar_msg::SidebarSections;
pub use jinn_sidebar_msg::TaskListSectionState;
use jinn_theme::Theme;
use marked_path::{Absolute, MarkedPathBuf};

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
    pub project_dir: MarkedPathBuf<Absolute>,
    /// The new session's starting cwd (scripts may later override it).
    pub starting_cwd: MarkedPathBuf<Absolute>,
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
    /// Cached copy of app state from `state.toml`.
    /// Updated by `AppStateActor` inline after persisting to `state.toml` (authoritative),
    /// and by the `IntentHandler` for immediate UI feedback (exempt).
    pub app_state: AppStateFile,

    /// The current resolved theme (colors for the render pipeline).
    /// OWNER: IntentHandler (theme picker preview, exempt), AppStateActor (authoritative, on state.toml change).
    pub theme: Theme,

    /// Whether the "Press ESC again to cancel" prompt is showing.
    /// OWNER: IntentHandler (set on first ESC in Normal/Sidebar with active stream,
    ///         consumed on second ESC or dismissed on any other key).
    pub cancel_stream_prompt: bool,

    /// Whether the "Press x again to teardown and archive 1 session" prompt is showing.
    /// OWNER: sidebar route action (set on first close, consumed on matching
    /// second close or dismissed by IntentHandler for unrelated actions).
    pub close_session_prompt: bool,

    /// State of the "Press A again to archive N sessions" prompt.
    /// OWNER: sidebar route action; IntentHandler dismisses it for unrelated actions.
    pub archive_tree_prompt: Option<jinn_sidebar_msg::ArchiveTreePrompt>,

    /// All picker state - grouped for independent evolution.

    /// Creation stash for the next session from the projects UI.
    ///
    /// Set by the project picker (`<enter>`/`<c-enter>`) so a new session can
    /// be rooted at a chosen project directory and stamped with that project,
    /// without mutating the active session's CWD. Consumed (and cleared) by
    /// `handle_session_lifecycle_setup` when the new session is created.
    /// OWNER: IntentHandler (set by project picker, consumed by session creation).
    pub pending_creation: Option<PendingSessionCreation>,

    pub sidebar_width: u16,

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
            app_state: AppStateFile::default(),
            theme: jinn_theme::default_theme(),
            cancel_stream_prompt: false,
            close_session_prompt: false,
            archive_tree_prompt: None,
            pending_creation: None,

            sidebar_width: 30,
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

    /// Resolves the chat log's line-cache cell, if the handle is attached and
    /// the cell catalog registered it.
    ///
    /// The cache is a `jinn-chat-log-view` cell rather than a field on this
    /// struct: it is written only by that slice's render and layout paths, and
    /// a theme change has to clear it — which is a fact about the slice, not
    /// about every piece of frontend state.
    pub fn line_cache_cell(&self) -> Option<TypedCell<jinn_chat_log_view_msg::EntryLineCache>> {
        let slices = self.scope_focus.get()?;
        slices.reader::<jinn_chat_log_view_msg::EntryLineCache>(
            &jinn_chat_log_view_msg::entry_line_cache_slot(),
        )
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

    /// Runs `f` against the five sidebar sections' state, returning its result.
    ///
    /// A no-op when the cell is absent (slice not activated) — writes are
    /// silently dropped and `None` is returned, matching the no-slice
    /// configuration. The value comes back because a transition's *outcome* is
    /// often the point: an abandon reports whether it had anything to abandon.
    pub fn update_sections<F, R>(&self, f: F) -> Option<R>
    where
        R: Sized,
        F: FnOnce(&mut jinn_sidebar_msg::SidebarSections) -> R,
    {
        let cell = self.sections_cell()?;
        Some(cell.update(f))
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

    /// Resolves the `@path` file popup's cell, if the handle is attached
    /// and the chat-input slice's `activate()` minted it.
    fn file_picker_cell(&self) -> Option<jinn_slices::cell::TypedCell<FilePickerState>> {
        let slices = self.scope_focus.get()?;
        slices.reader::<FilePickerState>(&jinn_chat_input_msg::file_picker_slot())
    }

    /// Runs `f` against the `@path` file popup's state, returning its result.
    ///
    /// A no-op returning `None` when the cell is absent (the slice was
    /// never activated) — writes are silently dropped, matching the
    /// no-slice configuration.
    ///
    /// OWNER: DirectoryListerActor (entries, loading); IntentHandler
    /// stamps `expected_request_id` and `loading` when it emits a
    /// listing request (exempt as the synchronous frontend mutator).
    pub fn update_file_picker<F, R>(&self, f: F) -> Option<R>
    where
        R: Sized,
        F: FnOnce(&mut FilePickerState) -> R,
    {
        let cell = self.file_picker_cell()?;
        Some(cell.update(f))
    }

    /// Reads the `@path` file popup's state through `f`, returning
    /// `None` when the cell is absent (the slice was never activated).
    ///
    /// A caller that needs a value either way passes a fallback, as
    /// [`Self::with_sections`] does; this pair reports absence so a
    /// caller that seeds the cell can tell an empty popup from an
    /// unactivated slice.
    #[must_use]
    pub fn with_file_picker<R, F>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&FilePickerState) -> R,
    {
        let cell = self.file_picker_cell()?;
        let guard = cell.read();
        Some(f(&guard))
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
