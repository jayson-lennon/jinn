//! Focus scope and scope stack — tracking what the user is focused on
//! (shared vocabulary; the kernel re-exports under `jinn_domain::common`).

use crate::mode::Mode;
use crate::picker_kind::PickerKind;
use crate::sidebar_section_id::SidebarSectionId;

/// A single focus context on the scope stack.
///
/// Each layer of the [`ScopeStack`] is a `FocusScope`. The top of the stack
/// determines the active mode, keymap scope, and which overlays are visible.
#[derive(Debug, Clone, PartialEq)]
pub enum FocusScope {
    /// Browsing chat entries (base scope).
    Normal,
    /// Typing into the input buffer.
    Input,
    /// Sidebar - Persona section focused.
    SidebarPersona,
    /// Sidebar - Pins section focused.
    SidebarPins,
    /// Sidebar - Sessions section focused.
    SidebarSessions,
    /// Sidebar - Task list section focused.
    SidebarTaskList,
    /// Sidebar - MCP servers section focused.
    SidebarMcpServers,
    /// Picker overlay active - kind distinguishes Provider/Session/Keymap/etc.
    Picker { kind: PickerKind },
    /// Arg input popup - collecting positional args for a lifecycle command.
    ArgInput,
    /// Rename session input popup - editing a session title.
    RenameSessionInput,
    /// Project-add input popup - typing a directory path to register a new project.
    ProjectAddInput,
    /// Pruner accumulation threshold popup - numeric input for the KV-cache gate.
    PrunerAccumulationInput,

    /// A dynamically-registered slice's scope. Carries its identity as
    /// data, so slices never edit this enum. The scope the slice's
    /// `activate()` pushed (or signaled via a route action).
    Dynamic(crate::slice_scope::SliceScopeId),

    /// Terminal tab — viewing an `interactive_term` session (watch only; keys
    /// are not forwarded to the pty).
    TerminalView,
    /// Terminal control — every key forwards to the `interactive_term` pty
    /// except the handback key (config `[interactive_term] handback_key`).
    TerminalControl,

    /// Sidebar resize mode - adjusting sidebar width with h/l keys.
    SidebarResize,
}

impl FocusScope {
    /// Returns the [`Mode`] corresponding to this scope.
    #[must_use]
    pub fn mode(&self) -> Mode {
        match self {
            Self::Normal
            | Self::SidebarPersona
            | Self::SidebarPins
            | Self::SidebarSessions
            | Self::SidebarTaskList
            | Self::SidebarMcpServers
            | Self::SidebarResize
            | Self::TerminalView
            // Capture mode routes keystrokes to the pty, not the chat input,
            // so it must not light up input-focused UI.
            | Self::TerminalControl => Mode::Normal,
            Self::Input
            | Self::ArgInput
            | Self::RenameSessionInput
            | Self::ProjectAddInput
            | Self::PrunerAccumulationInput
            // Dynamic scopes are input-capturing surfaces: a slice scope
            // captures keystrokes (its input hook serves the editing
            // intents), so it lights up input-focused UI like the quake
            // bar did.
            | Self::Dynamic(_) => Mode::Input,
            Self::Picker { .. } => Mode::Picker,
        }
    }
}

impl std::fmt::Display for FocusScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::TerminalView => write!(f, "TerminalView"),
            Self::TerminalControl => write!(f, "TerminalControl"),
            Self::Input => write!(f, "Input"),
            Self::SidebarPersona => write!(f, "SidebarPersona"),
            Self::SidebarPins => write!(f, "SidebarPins"),
            Self::SidebarSessions => write!(f, "SidebarSessions"),
            Self::SidebarTaskList => write!(f, "SidebarTaskList"),
            Self::SidebarMcpServers => write!(f, "SidebarMcpServers"),
            Self::Picker { kind } => write!(f, "Picker({kind})"),
            Self::ArgInput => write!(f, "ArgInput"),
            Self::RenameSessionInput => write!(f, "RenameSessionInput"),
            Self::ProjectAddInput => write!(f, "ProjectAddInput"),
            Self::PrunerAccumulationInput => write!(f, "PrunerAccumulationInput"),
            Self::Dynamic(id) => write!(f, "Dynamic({id})"),
            Self::SidebarResize => write!(f, "SidebarResize"),
        }
    }
}

/// A LIFO stack of [`FocusScope`] layers.
///
/// Always has at least one entry (the base scope). Entering an overlay
/// pushes a new scope; escaping pops one level, restoring the previous scope.
#[derive(Debug, Clone)]
pub struct ScopeStack {
    stack: Vec<FocusScope>,
}

impl Default for ScopeStack {
    fn default() -> Self {
        Self {
            stack: vec![FocusScope::Normal],
        }
    }
}

impl ScopeStack {
    /// Pushes a new scope onto the stack (entering an overlay).
    pub fn push(&mut self, scope: FocusScope) {
        self.stack.push(scope);
    }

    /// Pops the top scope, returning it. Returns `None` if only the base remains.
    pub fn pop(&mut self) -> Option<FocusScope> {
        if self.stack.len() <= 1 {
            None
        } else {
            self.stack.pop()
        }
    }

    /// Returns the current (top) scope.
    ///
    /// # Panics
    ///
    /// Panics if the stack is empty (should never happen as the base is always present).
    #[must_use]
    pub fn current(&self) -> &FocusScope {
        #[expect(clippy::expect_used, reason = "ScopeStack invariant: always has base")]
        self.stack.last().expect("stack always has base")
    }

    /// Returns the base (bottom) scope.
    ///
    /// Use this instead of [`current`](Self::current) when you need the
    /// underlying tab context (Chat vs Dashboard) regardless of any overlays
    /// pushed on top (e.g., the quake bar).
    ///
    /// # Panics
    ///
    /// Panics if the stack is empty (should never happen as the base is always present).
    #[must_use]
    pub fn base(&self) -> &FocusScope {
        #[expect(clippy::expect_used, reason = "ScopeStack invariant: always has base")]
        self.stack.first().expect("stack always has base")
    }

    /// Returns the scope one level below the top (the "return target").
    ///
    /// Returns `None` if only the base scope is on the stack.
    #[must_use]
    pub fn parent(&self) -> Option<&FocusScope> {
        if self.stack.len() < 2 {
            None
        } else {
            self.stack.get(self.stack.len() - 2)
        }
    }

    /// Pops all overlay scopes, returning to the base scope.
    pub fn clear_overlays(&mut self) {
        self.stack.truncate(1);
    }

    /// Replaces the base scope with `new_base` and clears all overlays.
    ///
    /// Use when transitioning between top-level contexts (e.g., Chat → Picker)
    /// where the entire scope stack should be replaced, not just pushed onto.
    pub fn swap_base(&mut self, new_base: FocusScope) {
        self.stack.clear();
        self.stack.push(new_base);
    }

    /// Returns `true` if the current scope is a Picker.
    #[must_use]
    pub fn is_picker(&self) -> bool {
        matches!(self.current(), FocusScope::Picker { .. })
    }

    /// Returns the `PickerKind` if the current scope is a Picker.
    #[must_use]
    pub fn picker_kind(&self) -> Option<&PickerKind> {
        match self.current() {
            FocusScope::Picker { kind } => Some(kind),
            _ => None,
        }
    }

    /// Returns `true` if the current scope is a sidebar section.
    #[must_use]
    pub fn is_sidebar(&self) -> bool {
        matches!(
            self.current(),
            FocusScope::SidebarPersona
                | FocusScope::SidebarPins
                | FocusScope::SidebarSessions
                | FocusScope::SidebarTaskList
                | FocusScope::SidebarMcpServers
        )
    }

    /// Returns the focused sidebar section, if a sidebar scope is active.
    #[must_use]
    pub fn sidebar_section(&self) -> Option<SidebarSectionId> {
        match self.current() {
            FocusScope::SidebarPersona => Some(SidebarSectionId::Persona),
            FocusScope::SidebarPins => Some(SidebarSectionId::Pins),
            FocusScope::SidebarSessions => Some(SidebarSectionId::Sessions),
            FocusScope::SidebarTaskList => Some(SidebarSectionId::TaskList),
            FocusScope::SidebarMcpServers => Some(SidebarSectionId::McpServers),
            _ => None,
        }
    }

    /// Swaps the top of the scope stack to a different sidebar section.
    ///
    /// No-op if the current scope is not a sidebar section.
    pub fn set_sidebar_section(&mut self, section: SidebarSectionId) {
        if self.is_sidebar() {
            let scope = match section {
                SidebarSectionId::Persona => FocusScope::SidebarPersona,
                SidebarSectionId::Pins => FocusScope::SidebarPins,
                SidebarSectionId::TaskList => FocusScope::SidebarTaskList,
                SidebarSectionId::Sessions => FocusScope::SidebarSessions,
                SidebarSectionId::McpServers => FocusScope::SidebarMcpServers,
            };
            self.stack.pop();
            self.stack.push(scope);
        }
    }

    /// Returns `true` if the stack has no scopes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Returns the number of scopes on the stack.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }
}
