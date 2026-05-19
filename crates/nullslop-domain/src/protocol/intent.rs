//! The [`Intent`] enum — one variant per user-initiated action.
use crate::protocol::{Command, PickerKind, SessionId};

/// A user-initiated action.
///
/// Every keymap binding and mouse event produces exactly one [`Intent`] variant.
/// The keymap decides the intent; the `IntentHandler` decides what to do with it.
#[derive(Debug, Clone)]
pub enum Intent {
    // --- Chat Input ---
    /// Insert a character at the cursor position.
    InsertChar {
        /// The character to insert.
        ch: char,
    },
    /// Delete the grapheme before the cursor.
    DeleteGrapheme,
    /// Delete the grapheme after the cursor (forward delete).
    DeleteGraphemeForward,
    /// Submit the current input as a user message.
    SubmitMessage,
    /// Move the cursor one grapheme left.
    MoveCursorLeft,
    /// Move the cursor one grapheme right.
    MoveCursorRight,
    /// Move the cursor to the beginning of the input.
    MoveCursorToStart,
    /// Move the cursor to the end of the input.
    MoveCursorToEnd,
    /// Move the cursor one word left.
    MoveCursorWordLeft,
    /// Move the cursor one word right.
    MoveCursorWordRight,
    /// Move the cursor up one visual line.
    MoveCursorUp,
    /// Move the cursor down one visual line.
    MoveCursorDown,
    /// Confirm the autocomplete selection (Tab in Input scope).
    AutocompleteConfirm,
    /// Paste text from the clipboard (bracketed paste).
    PasteText {
        /// The pasted text content.
        text: String,
    },

    // --- Navigation ---
    /// Scroll the chat log up.
    ScrollUp,
    /// Scroll the chat log down.
    ScrollDown,
    /// Mouse scroll up.
    MouseScrollUp,
    /// Mouse scroll down.
    MouseScrollDown,
    /// Scroll to the very top.
    ScrollToTop,
    /// Scroll to the very bottom.
    ScrollToBottom,
    /// Open the input in an external editor.
    EditInput,

    // --- Mode & App ---
    /// Quit the application.
    Quit,
    /// Context-sensitive interrupt: clear input or cancel stream.
    ///
    /// When `session_id` is `None`, applies to the active session (smart behavior).
    /// When `session_id` is `Some(id)`, targets a specific session for cancel only.
    Interrupt {
        /// The session to target, or `None` for the active session.
        session_id: Option<SessionId>,
    },
    /// Enter Insert (Input) mode — the chat input box is active.
    EnterInsertMode,
    /// Enter Normal mode — cancel streams, clear picker, return to neutral.
    EnterNormalMode,
    /// Toggle the which-key popup.
    ToggleWhichkey,
    /// Escape key in Normal mode: cancel selection.
    NormalEscape,

    // --- Picker ---
    /// Open a picker of the specified kind.
    OpenPicker {
        /// Which picker to open.
        kind: PickerKind,
    },
    /// Insert a character into the picker filter.
    PickerInsertChar {
        /// The character to insert.
        ch: char,
    },
    /// Delete the last character from the picker filter.
    PickerBackspace,
    /// Confirm the current picker selection.
    PickerConfirm,
    /// Move the picker selection up.
    PickerMoveUp,
    /// Move the picker selection down.
    PickerMoveDown,
    /// Move the picker filter cursor left.
    PickerMoveCursorLeft,
    /// Move the picker filter cursor right.
    PickerMoveCursorRight,
    /// Toggle the keymap picker scope filter.
    ToggleKeymapScopeFilter,
    /// Create a new session.
    SessionNew,
    /// Refresh the model list from all providers.
    RefreshModels,
    /// Rescan the prompt templates directory.
    RescanPromptTemplates,

    // --- Sidebar ---
    /// Enter the sidebar scope.
    SidebarFocus,
    /// Leave the sidebar, returning to origin scope.
    SidebarLeave,
    /// Move selection down in the sidebar.
    SidebarMoveDown,
    /// Move selection up in the sidebar.
    SidebarMoveUp,
    /// Jump to the next sidebar section.
    SidebarSectionNext,
    /// Jump to the previous sidebar section.
    SidebarSectionPrev,
    /// Activate the selected session (switch to it).
    SidebarConfirm,
    /// Unpin the selected pinned entry.
    PinsUnpin,
    /// Set the selected pinned entry's position to TOP.
    PinsPinTop,
    /// Set the selected pinned entry's position to BOTTOM.
    PinsPinBottom,
    /// Set the selected pinned entry's position to RELATIVE.
    PinsPinRelative,
    /// Cycle the selected pinned entry's pin position.
    PinsPinCycle,
    /// Close the selected open session from the sidebar.
    SidebarSessionClose,
    /// Archive the selected session without running teardown.
    SidebarSessionArchive,
    /// Re-run teardown for the selected session without closing it.
    SidebarSessionTeardown,
    /// Open the persona picker from the sidebar.
    SidebarPersonaEdit,
    /// Open the session lifecycle picker from the sidebar sessions section.
    SidebarSessionNewWithLifecycle,

    // --- Chat Entry Selection ---
    /// Select the next chat entry.
    ChatEntrySelectNext,
    /// Select the previous chat entry.
    ChatEntrySelectPrev,
    /// Pin the currently selected chat entry.
    ChatEntryPinSelected,
    /// Toggle expand/collapse of the selected tool entry (tool call or tool result).
    ExpandToolEntry,
    /// Toggle user message visibility in the fork picker.
    ToggleForkUserFilter,
    /// Toggle assistant message visibility in the fork picker.
    ToggleForkAssistantFilter,

    // --- Session Lifecycle ---
    /// Run a lifecycle setup command to create a new session.
    SessionLifecycleSetup {
        /// The lifecycle name (e.g., "fossil branch").
        lifecycle_name: String,
        /// Resolved positional arguments.
        args: Vec<String>,
    },
    /// Close the active session, running teardown if applicable.
    SessionClose,
    /// Confirm the arg input and trigger lifecycle setup.
    ArgInputConfirm,

    // --- Sidebar Resize ---
    /// Enter sidebar resize mode.
    SidebarResizeEnter,
    /// Expand the sidebar (move border left).
    SidebarResizeExpand,
    /// Contract the sidebar (move border right).
    SidebarResizeContract,
    /// Exit sidebar resize mode, returning to Normal scope.
    SidebarResizeLeave,

    // --- Token Budget Input ---
    /// Open the token budget input popup.
    TokenBudgetInputEnter,
    /// Confirm the token budget input and apply.
    TokenBudgetInputConfirm,
    /// Cancel the token budget input popup.
    TokenBudgetInputLeave,

    // --- Sliding Window Input ---
    /// Open the sliding window input popup.
    SlidingWindowInputEnter,
    /// Confirm the sliding window input and apply.
    SlidingWindowInputConfirm,
    /// Cancel the sliding window input popup.
    SlidingWindowInputLeave,

    // --- Rename Session Input ---
    /// Open the rename session input popup.
    SidebarRenameSession,
    /// Confirm the rename session input and apply.
    RenameSessionConfirm,
    /// Cancel the rename session input popup.
    RenameSessionLeave,
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::InsertChar { ch } => write!(f, "insert '{ch}'"),
            Intent::DeleteGrapheme => write!(f, "delete"),
            Intent::DeleteGraphemeForward => write!(f, "forward delete"),
            Intent::SubmitMessage => write!(f, "submit message"),
            Intent::MoveCursorLeft => write!(f, "cursor left"),
            Intent::MoveCursorRight => write!(f, "cursor right"),
            Intent::MoveCursorToStart => write!(f, "cursor home"),
            Intent::MoveCursorToEnd => write!(f, "cursor end"),
            Intent::MoveCursorWordLeft => write!(f, "cursor word left"),
            Intent::MoveCursorWordRight => write!(f, "cursor word right"),
            Intent::MoveCursorUp => write!(f, "cursor up"),
            Intent::MoveCursorDown => write!(f, "cursor down"),
            Intent::AutocompleteConfirm => write!(f, "autocomplete confirm"),
            Intent::PasteText { text } => {
                let line_count = text.lines().count();
                write!(f, "paste ({line_count} lines)")
            }
            Intent::ScrollUp => write!(f, "scroll up"),
            Intent::ScrollDown => write!(f, "scroll down"),
            Intent::MouseScrollUp => write!(f, "mouse scroll up"),
            Intent::MouseScrollDown => write!(f, "mouse scroll down"),
            Intent::ScrollToTop => write!(f, "scroll to top"),
            Intent::ScrollToBottom => write!(f, "scroll to bottom"),
            Intent::EditInput => write!(f, "edit in $EDITOR"),
            Intent::Quit => write!(f, "quit"),
            Intent::Interrupt { .. } => write!(f, "interrupt"),
            Intent::EnterInsertMode => write!(f, "enter insert mode"),
            Intent::EnterNormalMode => write!(f, "enter normal mode"),
            Intent::ToggleWhichkey => write!(f, "toggle which-key"),
            Intent::NormalEscape => write!(f, "escape"),
            Intent::OpenPicker { kind } => write!(f, "search {kind}"),
            Intent::PickerInsertChar { ch } => write!(f, "picker insert '{ch}'"),
            Intent::PickerBackspace => write!(f, "picker backspace"),
            Intent::PickerConfirm => write!(f, "picker confirm"),
            Intent::PickerMoveUp => write!(f, "picker move up"),
            Intent::PickerMoveDown => write!(f, "picker move down"),
            Intent::PickerMoveCursorLeft => write!(f, "picker cursor left"),
            Intent::PickerMoveCursorRight => write!(f, "picker cursor right"),
            Intent::ToggleKeymapScopeFilter => write!(f, "toggle keymap scope filter"),
            Intent::SessionNew => write!(f, "session new"),
            Intent::RefreshModels => write!(f, "refresh models"),
            Intent::RescanPromptTemplates => write!(f, "rescan prompt templates"),
            Intent::SidebarFocus => write!(f, "sidebar focus"),
            Intent::SidebarLeave => write!(f, "sidebar leave"),
            Intent::SidebarMoveDown => write!(f, "sidebar move down"),
            Intent::SidebarMoveUp => write!(f, "sidebar move up"),
            Intent::SidebarSectionNext => write!(f, "sidebar section next"),
            Intent::SidebarSectionPrev => write!(f, "sidebar section prev"),
            Intent::SidebarConfirm => write!(f, "sidebar confirm"),
            Intent::PinsUnpin => write!(f, "pins unpin"),
            Intent::PinsPinTop => write!(f, "pins pin top"),
            Intent::PinsPinBottom => write!(f, "pins pin bottom"),
            Intent::PinsPinRelative => write!(f, "pins pin relative"),
            Intent::PinsPinCycle => write!(f, "pins pin cycle"),
            Intent::SidebarSessionClose => write!(f, "sidebar session close"),
            Intent::SidebarSessionArchive => write!(f, "archive session"),
            Intent::SidebarSessionTeardown => write!(f, "sidebar session teardown"),
            Intent::SidebarPersonaEdit => write!(f, "edit persona"),
            Intent::SidebarSessionNewWithLifecycle => write!(f, "new session with lifecycle"),
            Intent::ChatEntrySelectNext => write!(f, "select next entry"),
            Intent::ChatEntrySelectPrev => write!(f, "select prev entry"),
            Intent::ChatEntryPinSelected => write!(f, "pin selected entry"),
            Intent::ExpandToolEntry => write!(f, "expand tool entry"),

            Intent::ToggleForkUserFilter => write!(f, "toggle fork user filter"),
            Intent::ToggleForkAssistantFilter => write!(f, "toggle fork assistant filter"),
            Intent::SessionLifecycleSetup { lifecycle_name, .. } => {
                write!(f, "session lifecycle setup: {lifecycle_name}")
            }
            Intent::SessionClose => write!(f, "session close"),
            Intent::ArgInputConfirm => write!(f, "arg input confirm"),
            Intent::SidebarResizeEnter => write!(f, "sidebar resize enter"),
            Intent::SidebarResizeExpand => write!(f, "sidebar resize expand"),
            Intent::SidebarResizeContract => write!(f, "sidebar resize contract"),
            Intent::SidebarResizeLeave => write!(f, "sidebar resize leave"),
            Intent::TokenBudgetInputEnter => write!(f, "token budget input enter"),
            Intent::TokenBudgetInputConfirm => write!(f, "token budget input confirm"),
            Intent::TokenBudgetInputLeave => write!(f, "token budget input leave"),
            Intent::SlidingWindowInputEnter => write!(f, "sliding window input enter"),
            Intent::SlidingWindowInputConfirm => write!(f, "sliding window input confirm"),
            Intent::SlidingWindowInputLeave => write!(f, "sliding window input leave"),
            Intent::SidebarRenameSession => write!(f, "rename session"),
            Intent::RenameSessionConfirm => write!(f, "rename session confirm"),
            Intent::RenameSessionLeave => write!(f, "rename session leave"),
        }
    }
}

/// What an intent handler returns after processing an intent.
///
/// Carries commands to be dispatched to the actor system.
#[derive(Debug)]
pub struct IntentResult {
    /// Commands to send to the actor system.
    pub commands: Vec<Command>,
}

impl IntentResult {
    /// An empty result with no commands.
    #[must_use]
    pub fn empty() -> Self {
        Self { commands: vec![] }
    }

    /// A result with commands.
    #[must_use]
    pub fn with_commands(commands: Vec<Command>) -> Self {
        Self { commands }
    }
}
