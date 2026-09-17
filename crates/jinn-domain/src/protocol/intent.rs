//! The [`Intent`] enum - one variant per user-initiated action.
use crate::protocol::{PickerKind, SessionId};

/// The search root for the directory picker (shared vocabulary from
/// `jinn-slices`; the scope-focus cell carries it in `TuiSignals`).
pub use jinn_slices::cwd_root::CwdRoot;

/// A user-initiated action.
///
/// Every keymap binding and mouse event produces exactly one [`Intent`] variant.
/// The keymap decides the intent; the `IntentHandler` decides what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
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
    /// Toggle the input submission mode between Queue and Steer.
    ToggleInputMode,

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
    /// Universal ctrl-c clear/leave: clears the active text input; if the input
    /// is empty, leaves the active popup scope (equivalent to `<esc>` for popups).
    CtrlClear,
    /// Enter Insert (Input) mode - the chat input box is active.
    EnterInsertMode,
    /// Enter Normal mode - cancel streams, clear picker, return to neutral.
    EnterNormalMode,
    /// Toggle the which-key popup.
    ToggleWhichkey,
    /// Escape key in Normal mode: cancel selection.
    NormalEscape,
    /// No-op intent produced by unmapped keys in scopes with confirmation prompts.
    /// Dismisses any active confirmation prompt via the pre-match interceptors.
    NoOp,

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
    /// Run a spec-driven picker's declared bind action.
    ///
    /// One data-carried intent replaces per-picker variants as pickers
    /// migrate: `picker` is the spec's registry id, `action` the bind
    /// row's notation. Resolved through the picker's own bind table.
    PickerAction {
        /// The picker spec's registry id (e.g. `"skill"`).
        picker: String,
        /// The bind row's action (e.g. `"<tab>"`).
        action: String,
    },
    /// Move the picker selection up.
    PickerMoveUp,
    /// Move the picker selection down.
    PickerMoveDown,
    /// Page the picker selection up by half the visible window.
    PickerPageUp,
    /// Page the picker selection down by half the visible window.
    PickerPageDown,
    /// Move the picker filter cursor left.
    PickerMoveCursorLeft,
    /// Move the picker filter cursor right.
    PickerMoveCursorRight,
    /// Create a new session.
    SessionNew,
    /// Refresh the model list from all providers.
    RefreshModels,
    /// Rescan the prompt templates directory.
    RescanPromptTemplates,
    /// Open the session lifecycle picker from the sidebar sessions section.
    SessionNewWithLifecycle,

    /// Select the next chat entry.
    ChatEntrySelectNext,
    /// Select the previous chat entry.
    ChatEntrySelectPrev,
    /// Jump the cursor to the next (newer) compaction summary entry.
    ChatEntryJumpNextCompaction,
    /// Jump the cursor to the previous (older) compaction summary entry.
    ChatEntryJumpPrevCompaction,
    /// Jump the cursor to the next (newer) user message.
    ChatEntryJumpNextUserEntry,
    /// Jump the cursor to the previous (older) user message.
    ChatEntryJumpPrevUserEntry,
    /// Jump the cursor to the next (newer) pinned entry.
    ChatEntryJumpNextPinned,
    /// Jump the cursor to the previous (older) pinned entry.
    ChatEntryJumpPrevPinned,
    /// Jump the cursor to the next (newer) Sources (annotation) entry.
    ChatEntryJumpNextSources,
    /// Jump the cursor to the previous (older) Sources (annotation) entry.
    ChatEntryJumpPrevSources,
    /// Pin the currently selected chat entry.
    /// Open the selected task call's subagent session.
    LoadSubagentSession,
    /// Close the selected sidebar session (arms a confirmation prompt).
    SidebarSessionClose,
    /// Archive the selected session and its visible subtree (arms prompt).
    SidebarSessionArchiveTree,
    /// Tear down the selected session root and archive its subtree (arms prompt).
    SidebarSessionTeardownTree,
    ChatEntryPinSelected,
    /// Toggle expand/collapse of the selected tool entry (tool call, tool result, or annotation).
    ExpandToolEntry,
    /// Toggle visibility of the audit popup for the currently selected chat entry.
    ToggleAuditPopup,
    /// Toggle visibility of the ignored entry block at the cursor.
    ToggleIgnoredBlockVisibility,
    /// Fork the session at the currently selected chat entry.
    ForkFromEntry,
    /// Create a new empty session seeded with the selected entry's text.
    ///
    /// Unlike [`ForkFromEntry`], the new session carries no inherited history;
    /// only the selected entry is copied in (kind preserved) as the sole
    /// history entry. Restricted to User and Assistant entries.
    NewSessionFromEntry,
    /// Yank (copy) the currently selected chat entry to the system clipboard.
    YankSelectedEntry,
    /// Toggle the `ignored` flag on the currently selected chat entry.
    ChatEntryIgnoreSelected,
    /// Reset the currently selected chat entry's context override to `Default`.
    ChatEntryResetSelected,
    /// Isolate the selected chat entry in context: force-include it and
    /// force-exclude all other non-pinned entries.
    ChatEntryIsolateSelected,

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

    /// Open the pruner accumulation threshold input popup.
    OpenPrunerAccumulationInput,
    /// Confirm the pruner accumulation input and persist.
    PrunerAccumulationConfirm,
    /// Cancel the pruner accumulation input popup.
    PrunerAccumulationLeave,
    /// Insert a character into the pruner accumulation input.
    PrunerAccumulationInsertChar {
        /// The character to insert.
        ch: char,
    },
    /// Move cursor left in the pruner accumulation input.
    PrunerAccumulationCursorLeft,
    /// Move cursor right in the pruner accumulation input.
    PrunerAccumulationCursorRight,
    /// Delete the grapheme before the cursor in pruner accumulation input.
    PrunerAccumulationDeleteGrapheme,
    /// Delete the grapheme after the cursor in pruner accumulation input.
    PrunerAccumulationDeleteForward,

    /// Confirm the project-add input - resolve, validate, and register.
    ProjectAddInputConfirm,
    /// Cancel the project-add input popup.
    ProjectAddInputLeave,

    /// Change the session's working directory via an external picker.
    ChangeCwd {
        /// Where to search from.
        root: CwdRoot,
    },

    /// A dynamically-registered slice's action.
    ///
    /// Dispatched exclusively through the feature route table
    /// ([`KeyRoutes`](crate::common::slices::key_routes::KeyRoutes)):
    /// a slice that never registered a row for this intent is inert by
    /// construction. Carries its identity as data, so slices never edit
    /// this enum.
    Dynamic(jinn_slices::DynamicIntent),

    /// Switch between Chat and the registered dynamic tabs.
    SwitchTab,

}

impl std::fmt::Display for Intent {
    #[expect(
        clippy::too_many_lines,
        clippy::match_same_arms,
        reason = "handler reads best as a single unit"
    )]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::InsertChar { ch } => write!(f, "insert '{ch}'"),
            Intent::DeleteGrapheme => write!(f, "delete"),
            Intent::DeleteGraphemeForward => write!(f, "forward delete"),
            Intent::SubmitMessage => write!(f, "submit message"),
            Intent::ToggleInputMode => write!(f, "toggle input mode"),
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
            Intent::CtrlClear => write!(f, "ctrl-c clear"),
            Intent::EnterInsertMode => write!(f, "enter insert mode"),
            Intent::EnterNormalMode => write!(f, "enter normal mode"),
            Intent::ToggleWhichkey => write!(f, "toggle which-key"),
            Intent::NormalEscape => write!(f, "escape"),
            Intent::NoOp => write!(f, "no-op"),
            Intent::OpenPicker { kind } => write!(f, "search {kind}"),
            Intent::PickerInsertChar { ch } => write!(f, "picker insert '{ch}'"),
            Intent::PickerBackspace => write!(f, "picker backspace"),
            Intent::PickerConfirm => write!(f, "picker confirm"),
            Intent::PickerAction { picker, action } => {
                write!(f, "picker action {action} ({picker})")
            }
            Intent::PickerMoveUp => write!(f, "picker move up"),
            Intent::PickerMoveDown => write!(f, "picker move down"),
            Intent::PickerPageUp => write!(f, "picker page up"),
            Intent::PickerPageDown => write!(f, "picker page down"),
            Intent::PickerMoveCursorLeft => write!(f, "picker cursor left"),
            Intent::PickerMoveCursorRight => write!(f, "picker cursor right"),
            Intent::SessionNew => write!(f, "new session"),
            Intent::RefreshModels => write!(f, "refresh models"),
            Intent::RescanPromptTemplates => write!(f, "rescan prompt templates"),
            Intent::SessionNewWithLifecycle => write!(f, "new session with lifecycle"),

            Intent::ChatEntrySelectNext => write!(f, "select next entry"),
            Intent::ChatEntrySelectPrev => write!(f, "select prev entry"),
            Intent::ChatEntryJumpNextCompaction => write!(f, "next compaction"),
            Intent::ChatEntryJumpPrevCompaction => write!(f, "previous compaction"),
            Intent::ChatEntryJumpNextUserEntry => write!(f, "next user message"),
            Intent::ChatEntryJumpPrevUserEntry => write!(f, "previous user message"),
            Intent::ChatEntryJumpNextPinned => write!(f, "next pinned entry"),
            Intent::ChatEntryJumpPrevPinned => write!(f, "previous pinned entry"),
            Intent::ChatEntryJumpNextSources => write!(f, "next sources entry"),
            Intent::ChatEntryJumpPrevSources => write!(f, "previous sources entry"),
            Intent::LoadSubagentSession => write!(f, "open subagent session"),
            Intent::SidebarSessionClose => write!(f, "close session"),
            Intent::SidebarSessionArchiveTree => write!(f, "archive session tree"),
            Intent::SidebarSessionTeardownTree => write!(f, "teardown session tree"),
            Intent::ChatEntryPinSelected => write!(f, "pin entry"),
            Intent::ExpandToolEntry => write!(f, "expand tool entry"),
            Intent::ToggleAuditPopup => write!(f, "toggle audit popup"),
            Intent::ToggleIgnoredBlockVisibility => write!(f, "toggle ignored block visibility"),
            Intent::ForkFromEntry => write!(f, "fork from entry"),
            Intent::NewSessionFromEntry => write!(f, "new session from entry"),
            Intent::YankSelectedEntry => write!(f, "yank entry"),
            Intent::ChatEntryIgnoreSelected => write!(f, "toggle entry in/out of context"),
            Intent::ChatEntryResetSelected => write!(f, "reset entry to default context"),
            Intent::ChatEntryIsolateSelected => write!(f, "isolate selected entry in context"),

            Intent::SessionLifecycleSetup { lifecycle_name, .. } => {
                write!(f, "session lifecycle setup: {lifecycle_name}")
            }
            Intent::SessionClose => write!(f, "session close"),
            Intent::ArgInputConfirm => write!(f, "arg input confirm"),
            Intent::OpenPrunerAccumulationInput => write!(f, "set pruner accumulation threshold"),
            Intent::PrunerAccumulationConfirm => write!(f, "pruner accumulation confirm"),
            Intent::PrunerAccumulationLeave => write!(f, "pruner accumulation leave"),
            Intent::PrunerAccumulationInsertChar { ch } => {
                write!(f, "pruner accumulation insert '{ch}'")
            }
            Intent::PrunerAccumulationCursorLeft => write!(f, "pruner accumulation cursor left"),
            Intent::PrunerAccumulationCursorRight => write!(f, "pruner accumulation cursor right"),
            Intent::PrunerAccumulationDeleteGrapheme => write!(f, "pruner accumulation delete"),
            Intent::PrunerAccumulationDeleteForward => {
                write!(f, "pruner accumulation forward delete")
            }
            Intent::ProjectAddInputConfirm => write!(f, "project-add input confirm"),
            Intent::ProjectAddInputLeave => write!(f, "project-add input leave"),

            Intent::ChangeCwd { root } => write!(f, "change cwd from '{root}'"),

            Intent::Dynamic(dynamic) => write!(f, "{dynamic}"),
            Intent::SwitchTab => write!(f, "switch tab"),
        }
    }
}

/// What an intent handler returns after processing an intent.
///
/// A type alias for the slice-level [`RouteResult`]: the route
/// mechanics (and this result type) live in `jinn-slices` so slice
/// crates can produce outcomes without depending on the kernel. The
/// publish closures are identical — `RouteResult::new_message` and
/// `Bridge::publish_closure` spawn the same `bus.tell(Publish(..))` —
/// so behavior is unchanged; only the definition's home moved.
///
/// Carries typed message closures to be dispatched to the actor system
/// via the kameo message bus, plus an optional scope transition. The
/// scope signal is applied by the handler (an exempt scope-stack
/// writer) *before* the messages publish, so a slice that opens itself
/// pushes its scope before any bus message a subscriber could observe.
pub use jinn_slices::RouteResult as IntentResult;

/// A scope-stack transition requested by a route action.
///
/// Slices declare their transitions as data; the composition-side
/// handler applies them. Ownership stays single-writer: only the
/// handler mutates the scope stack, and it does so only on these signals.
pub use jinn_slices::ScopeSignal;
