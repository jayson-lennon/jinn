//! Frontend capsule: cap + Ops newtypes + view + extension traits, colocated.
//!
//! Write access to [`FrontendState`] and its sub-fields is gated by an
//! unforgeable ZST token ([`FrontendCap`]). The projection methods
//! ([`State::with_*`]) hand the cap-holder narrow borrowed views scoped to the
//! exact concern they write (preferences, dashboard, quake bar, token cache,
//! skills picker, persona picker, app state).
//!
//! Frontend is owned by IntentHandler (God-mode via [`State::write`]); the
//! actors that also write here (preferences, dashboard, token-count, skills,
//! quake-bar, status, directory-lister) receive [`FrontendCap`] at wiring for
//! their narrow slice.

use std::collections::HashSet;

use crate::common::state::State;
use crate::feat::file_lister::FilePickerState;
use crate::feat::persona::PersonaEntry;
use crate::feat::preferences_actor::app_state_file::AppStateFile;
use crate::feat::skills::Skill;
use crate::feat::theme::Theme;
use crate::feat::ui::frontend_state::FrontendState;
use crate::feat::ui::picker_states::PickerExt;

// ── The cap ──────────────────────────────────────────────────────────────────

/// Proof of authority to write [`FrontendState`]. Minted only via
/// [`crate::common::tcaps::mint`].
#[derive(Clone, Copy, Debug)]
pub struct FrontendCap(());

impl FrontendCap {
    /// Private constructor scoped to the `tcaps/` subtree.
    pub(in crate::common::tcaps) fn new() -> Self {
        Self(())
    }
}

// ── Per-struct narrow newtypes ───────────────────────────────────────────────

/// Narrow write-handle to all of `FrontendState` for the preferences actor.
/// The tuple field is PRIVATE. The `frontend()` accessor returns the whole
/// `FrontendState` (its public field API is the capsule wall).
pub struct PreferencesOps<'a>(&'a mut FrontendState);

/// Narrow write-handle to the skills picker + preview cache for the skills
/// actor.
pub struct SkillPickerOps<'a>(&'a mut FrontendState);

/// Narrow write-handle to the persona picker for the session-actor context handler.
pub struct PersonaPickerOps<'a>(&'a mut FrontendState);

/// Narrow write-handle to `frontend.file_picker` for the directory-lister actor.
pub struct FilePickerOps<'a>(&'a mut FilePickerState);

/// Narrow write-handle to `frontend.terminal` for the interactive-term actor.
/// Exposes the [`TerminalMirrorWrite`] trait.
pub struct TerminalOps<'a>(
    &'a mut crate::feat::interactive_term::terminal_tab_state::TerminalTabState,
);

/// Narrow write-handle to `frontend.app_state` for the session-actor startup handler.
pub struct AppStateOps<'a>(&'a mut AppStateFile);

// ── Extension traits (the opt-in method menu) ───────────────────────────────

/// Mirror terminal screen/control updates into the frontend.
pub trait TerminalMirrorWrite {
    /// Replaces one chat session's mirrored screen and cursor.
    fn apply_screen(
        &mut self,
        chat_session_id: &crate::protocol::SessionId,
        term_session_id: &str,
        screen: String,
        cells: crate::feat::interactive_term::emulator::ScreenCells,
        cursor: (u16, u16),
        cursor_hidden: bool,
    );
    /// Sets who holds control.
    fn set_control(
        &mut self,
        holder: crate::feat::interactive_term::terminal_tab_state::TermControlHolder,
    );
    /// Marks (or clears) a chat session's live-terminal flag.
    fn set_live(&mut self, chat_session_id: &crate::protocol::SessionId, live: bool);
}

// ── Inherent accessors on the Ops newtypes ──────────────────────────────────

impl PreferencesOps<'_> {
    /// Mutable access to the whole frontend (preferences, sidebar, theme, ...).
    pub fn frontend(&mut self) -> &mut FrontendState {
        self.0
    }
}

impl SkillPickerOps<'_> {
    /// Reload the skills picker entries from the discovered/disabled sets.
    pub fn reload_picker(
        &mut self,
        discovered: &[Skill],
        disabled: &HashSet<String>,
        theme: &Theme,
    ) {
        crate::feat::skills::reload::reload_skill_picker_entries(
            self.0, discovered, disabled, theme,
        );
    }
}

impl PersonaPickerOps<'_> {
    /// Replace the persona picker items, wrapped through the persona
    /// spec's render/search hooks (the storage holds `PickerEntry`s).
    pub fn set_items(&mut self, items: Vec<PersonaEntry>) {
        let wrapped = {
            let registry = crate::feat::picker::registry::build_picker_registry();
            registry
                .make_items(crate::feat::picker::registry::PERSONA_ID, items)
                .unwrap_or_default()
        };
        self.0.persona_picker_mut().set_items(wrapped);
    }
}

impl TerminalMirrorWrite for TerminalOps<'_> {
    fn apply_screen(
        &mut self,
        chat_session_id: &crate::protocol::SessionId,
        term_session_id: &str,
        screen: String,
        cells: crate::feat::interactive_term::emulator::ScreenCells,
        cursor: (u16, u16),
        cursor_hidden: bool,
    ) {
        self.0.apply_screen(
            chat_session_id,
            term_session_id,
            screen,
            cells,
            cursor,
            cursor_hidden,
        );
    }

    fn set_control(
        &mut self,
        holder: crate::feat::interactive_term::terminal_tab_state::TermControlHolder,
    ) {
        self.0.set_control(holder);
    }

    fn set_live(&mut self, chat_session_id: &crate::protocol::SessionId, live: bool) {
        if live {
            self.0.live_terms.insert(chat_session_id.clone());
        } else {
            self.0.live_terms.remove(chat_session_id);
        }
    }
}

impl FilePickerOps<'_> {
    /// Mutable access to the file-picker state.
    pub fn file_picker(&mut self) -> &mut FilePickerState {
        self.0
    }
}

impl AppStateOps<'_> {
    /// Replace the whole app-state file.
    pub fn set(&mut self, app_state: AppStateFile) {
        *self.0 = app_state;
    }
}

// ── Trait impls ─────────────────────────────────────────────────────────────

// ── Projection methods ──────────────────────────────────────────────────────

impl State {
    /// Write access to the whole frontend (preferences/sidebar/theme), scoped via
    /// [`PreferencesOps`].
    pub fn with_preferences<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut PreferencesOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut PreferencesOps(&mut app.frontend))
    }

    /// Write access to the terminal-tab mirror, scoped via [`TerminalOps`].
    pub fn with_terminal<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut TerminalOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut TerminalOps(&mut app.frontend.terminal))
    }

    /// Write access to the skills picker, scoped via [`SkillPickerOps`].
    pub fn with_skills_frontend<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut SkillPickerOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut SkillPickerOps(&mut app.frontend))
    }

    /// Write access to the persona picker, scoped via [`PersonaPickerOps`].
    pub fn with_persona_picker<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut PersonaPickerOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut PersonaPickerOps(&mut app.frontend))
    }

    /// Write access to the app-state file, scoped via [`AppStateOps`].
    pub fn with_frontend_app_state<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut AppStateOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut AppStateOps(&mut app.frontend.app_state))
    }

    /// Write access to `frontend.file_picker`, scoped via [`FilePickerOps`].
    pub fn with_file_picker<R, F>(&self, _cap: &FrontendCap, f: F) -> R
    where
        F: FnOnce(&mut FilePickerOps<'_>) -> R,
    {
        let mut guard = self.write_lock();
        let app = &mut *guard;
        f(&mut FilePickerOps(&mut app.frontend.file_picker))
    }
}
