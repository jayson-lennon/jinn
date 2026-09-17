//! Focus scope and scope stack — tracking what the user is focused on
//! (shared vocabulary; the kernel re-exports under `jinn_domain::common`).

use crate::mode::Mode;
use crate::picker_kind::PickerKind;

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
}

impl FocusScope {
    /// Returns the [`Mode`] corresponding to this scope.
    #[must_use]
    pub fn mode(&self) -> Mode {
        match self {
            // Input-capturing slice scopes (quake bar, popups) light up
            // input-focused UI; navigation-only slice scopes (the sidebar
            // sections) fall through to Normal like the other non-input
            // surfaces (chat, terminal, the base scope).
            Self::Dynamic(id) if id.captures_input() => Mode::Input,
            Self::Input
            | Self::ArgInput
            | Self::RenameSessionInput
            | Self::ProjectAddInput
            | Self::PrunerAccumulationInput => Mode::Input,
            Self::Picker { .. } => Mode::Picker,
            // Normal (capture-mode dynamic scopes route keystrokes to
            // their slice, not the chat input) and navigation-only
            // dynamic scopes are all non-input modes.
            _ => Mode::Normal,
        }
    }
}

impl std::fmt::Display for FocusScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "Normal"),
            Self::Input => write!(f, "Input"),
            Self::Picker { kind } => write!(f, "Picker({kind})"),
            Self::ArgInput => write!(f, "ArgInput"),
            Self::RenameSessionInput => write!(f, "RenameSessionInput"),
            Self::ProjectAddInput => write!(f, "ProjectAddInput"),
            Self::PrunerAccumulationInput => write!(f, "PrunerAccumulationInput"),
            Self::Dynamic(id) => write!(f, "Dynamic({id})"),
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
        match self.current() {
            // The resize scope is not a section.
            FocusScope::Dynamic(id) => id.slice() == "sidebar" && id.name() != "resize",
            _ => false,
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
