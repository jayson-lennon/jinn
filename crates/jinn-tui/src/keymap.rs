//! Keymap configuration and initialization.
//!
//! Defines the key categories and builds the keymap with all scope bindings.
//! Binds keys to [`Intent`] variants. Parameterized on
//! [`KeyEvent`] so the keymap works in both TUI and headless modes.

use crossterm::event::{self, MouseEventKind};
use derive_more::Display;
use jinn_domain::Intent;
use jinn_domain::PickerKind;
use jinn_domain::protocol::CwdRoot;
use jinn_domain::{Key, KeyEvent};
use ratatui_which_key::CrosstermKeymapExt as _;
use ratatui_which_key::Keymap;

use crate::scope::Scope;

/// Categories for keybinding grouping in the which-key popup.
///
/// Each variant becomes a section header when displaying available shortcuts.
#[derive(Display, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCategory {
    /// App-level control: quit, interrupt, help.
    General,
    /// Navigation: scrolling, tab switching, picker movement.
    Navigation,
    /// Model management: model picker, model refresh.
    Model,
    /// Text editing: cursor movement, insertion, deletion, mode entry.
    Input,
    /// Context strategy and prompt template management.
    Context,
    /// Sidebar sections
    Sidebar,
    /// Chat history
    ChatHistory,
}

/// Builds and returns the full keymap with all scope bindings.
/// Adds shared sidebar keybindings common to all sidebar section scopes.
///
/// Includes: quit, help, navigation (j/k/J/K), escape, tab switching,
/// pane navigation, sidebar resize, and input mode entry.
fn add_sidebar_base(b: &mut ratatui_which_key::ScopeBuilder<KeyEvent, Scope, Intent, KeyCategory>) {
    b
        // General - app control
        .bind("q", Intent::Quit, KeyCategory::General)
        .bind("<c-c>", Intent::Quit, KeyCategory::General)
        .bind("?", Intent::ToggleWhichkey, KeyCategory::General)
        // Navigation - within section and between sections
        .bind("j", Intent::SidebarMoveDown, KeyCategory::Navigation)
        .bind("k", Intent::SidebarMoveUp, KeyCategory::Navigation)
        .bind("J", Intent::SidebarSectionNext, KeyCategory::Navigation)
        .bind("K", Intent::SidebarSectionPrev, KeyCategory::Navigation)
        .bind("<esc>", Intent::SidebarLeave, KeyCategory::General)
        // Pane navigation - focus back to chat
        .bind("<c-h>", Intent::SidebarLeave, KeyCategory::Navigation)
        // Sidebar resize
        .bind("<c-w>", Intent::SidebarResizeEnter, KeyCategory::Navigation)
        // Input - enter input mode
        .bind("i", Intent::EnterInsertMode, KeyCategory::Input)
        // Direct jump to Sessions section
        .bind(
            "<M-s>",
            Intent::SidebarFocusSessions,
            KeyCategory::Navigation,
        );
    add_terminal_toggles(b);
}

/// Adds shared picker keybindings common to all picker scopes.
///
/// Includes: escape, confirm, navigation (up/down), cursor (left/right),
/// backspace, new session, and catch-all char input.
fn add_picker_base(b: &mut ratatui_which_key::ScopeBuilder<KeyEvent, Scope, Intent, KeyCategory>) {
    add_terminal_toggles(b);
    b.bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
        .bind("<enter>", Intent::PickerConfirm, KeyCategory::Model)
        .bind("<up>", Intent::PickerMoveUp, KeyCategory::Navigation)
        .bind("<down>", Intent::PickerMoveDown, KeyCategory::Navigation)
        .bind("<pgup>", Intent::PickerPageUp, KeyCategory::Navigation)
        .bind("<pgdn>", Intent::PickerPageDown, KeyCategory::Navigation)
        .bind("<left>", Intent::PickerMoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::PickerMoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::PickerBackspace, KeyCategory::Input)
        .bind("<c-n>", Intent::SessionNew, KeyCategory::General)
        .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::PickerInsertChar { ch: c })
            } else {
                None
            }
        });
}

/// Registers the would-be-global terminal toggle on a non-terminal
/// scope. Globals pierce capture mode (globals beat catch-alls), which used
/// to strand the terminal control flag on `User`; keeping this as a scope
/// binding makes capture mode hermetic while preserving the toggle
/// everywhere else. (Slice keys are generated from route rows — see
/// `keymap_gen`.)
fn add_terminal_toggles(
    b: &mut ratatui_which_key::ScopeBuilder<KeyEvent, Scope, Intent, KeyCategory>,
) {
    b.bind(
        "<M-t>",
        Intent::ToggleTerminalOverlay { session_id: None },
        KeyCategory::General,
    );
}

/// Builds and returns the full keymap with all scope bindings.
#[must_use]
#[rustfmt::skip]
pub fn init() -> Keymap<KeyEvent, Scope, Intent, KeyCategory> {
    init_with_control_toggle(
        jinn_domain::feat::interactive_term::prefs::DEFAULT_CONTROL_TOGGLE_KEY,
    )
}

/// Builds the keymap with a configured terminal control-toggle binding.
///
/// `control_toggle` is the normalized `<c-?>` notation from
/// `[interactive_term] control_toggle_key` in `jinn.toml`; invalid notations
/// degrade to no toggle binding (the caller validates config earlier).
///
/// The terminal overlay toggle (`<M-t>`) and the quake-bar open key
/// (`<M-\`>`, a slice row binding) are deliberately **scope bindings, not
/// globals**: in `TerminalControl` a toggle would leave the control flag
/// stuck on `User`
/// (the agent locked out). Every other scope registers them locally,
/// including `TerminalView` where `<M-t>` closes the overlay; only
/// `TerminalControl` does not — capture mode is hermetic.
#[must_use]
#[rustfmt::skip]
#[expect(clippy::too_many_lines, reason = "exhaustive keymap bindings grow with each scope")]
pub fn init_with_control_toggle(control_toggle: &str) -> Keymap<KeyEvent, Scope, Intent, KeyCategory> {
    let mut keymap = Keymap::new();

    keymap
        // Normal scope: navigation and commands
        .scope(Scope::Normal, |b| {
            b
            // General - app control
            .bind("q", Intent::Quit, KeyCategory::General)
            .bind("<c-c>", Intent::Quit, KeyCategory::General)
            .bind("?", Intent::ToggleWhichkey, KeyCategory::General)
            .describe_group_with_category("<leader>s", "search", KeyCategory::General)
            .bind("<leader>sm", Intent::OpenPicker { kind: PickerKind::Provider }, KeyCategory::General)
            .bind("<leader>ss", Intent::OpenPicker { kind: PickerKind::Session }, KeyCategory::General)
            .bind("<leader>se", Intent::OpenPicker { kind: PickerKind::Persona }, KeyCategory::General)
            .bind("<leader>st", Intent::OpenPicker { kind: PickerKind::Tool }, KeyCategory::General)
            .bind("<leader>sk", Intent::OpenPicker { kind: PickerKind::Skill }, KeyCategory::General)
            .bind("<leader>sM", Intent::OpenPicker { kind: PickerKind::McpServer }, KeyCategory::General)
            .bind("<leader>sP", Intent::OpenPicker { kind: PickerKind::Plugin }, KeyCategory::General)
            .bind("<leader>sh", Intent::OpenPicker { kind: PickerKind::Theme }, KeyCategory::General)
            .bind("<leader>sr", Intent::OpenPicker { kind: PickerKind::ReasoningEffort }, KeyCategory::General)
            // OpenRouter routing endpoint pin (Single + OpenRouter models only).
            .bind("<leader>sE", Intent::OpenPicker { kind: PickerKind::Endpoint }, KeyCategory::General)
            // Projects - curated directory list for quick session creation
            .bind("<leader>so", Intent::OpenPicker { kind: PickerKind::Project }, KeyCategory::General)
            // Input - enter input mode
            .bind("i", Intent::EnterInsertMode, KeyCategory::Input)
            .bind("<c-j>", Intent::EnterInsertMode, KeyCategory::Input)
            // Navigation - scrolling and tab switching
            .bind("k", Intent::ChatEntrySelectPrev, KeyCategory::Navigation)
            .bind("j", Intent::ChatEntrySelectNext, KeyCategory::Navigation)
            .bind("<Tab>", Intent::SwitchTab, KeyCategory::Navigation)

            .bind("<c-u>", Intent::ScrollUp, KeyCategory::Navigation)
            .bind("<c-d>", Intent::ScrollDown, KeyCategory::Navigation)
            // Change CWD - search from session CWD
            .bind("<M-c>", Intent::ChangeCwd { root: CwdRoot::Session }, KeyCategory::Navigation)
            // Change CWD - search from home directory
            .bind("<M-d>", Intent::ChangeCwd { root: CwdRoot::Home }, KeyCategory::Navigation)
            // g prefix - general commands and model management
            .describe_group_with_category("g", "general", KeyCategory::General)
            .describe_group_with_category("gm", "model", KeyCategory::Model)
            .describe_group_with_category("gc", "context", KeyCategory::Context)
            .bind("<leader>sl", Intent::OpenPicker { kind: PickerKind::SessionLifecycle }, KeyCategory::General)
            .bind("<leader>sc", Intent::OpenPicker { kind: PickerKind::CompactionModel }, KeyCategory::Model)
            .describe_group_with_category("<leader>c", "change", KeyCategory::General)
            .bind("<leader>cd", Intent::OpenCwdInput, KeyCategory::General)
            .bind("gg", Intent::ScrollToTop, KeyCategory::Navigation)
            .bind("G", Intent::ScrollToBottom, KeyCategory::Navigation)
            .bind("gmr", Intent::RefreshModels, KeyCategory::Model)
            .bind("gcr", Intent::RescanPromptTemplates, KeyCategory::Context)
            .bind("gcp", Intent::OpenPrunerAccumulationInput, KeyCategory::Context)
            // Isolate selected entry: force-include its tool loop, force-exclude the rest
            .bind("gci", Intent::ChatEntryIsolateSelected, KeyCategory::Context)
            .bind("<c-l>", Intent::SidebarFocus, KeyCategory::Navigation)
            .bind("<M-s>", Intent::SidebarFocusSessions, KeyCategory::Navigation)
            // Sidebar resize
            .bind("<c-w>", Intent::SidebarResizeEnter, KeyCategory::Navigation)
            // Minimap navigation
            // Pin selected entry
            .bind("p", Intent::ChatEntryPinSelected, KeyCategory::ChatHistory)
            .bind("x", Intent::ChatEntryIgnoreSelected, KeyCategory::ChatHistory)
            // Reset selected entry to default context
            .bind("r", Intent::ChatEntryResetSelected, KeyCategory::ChatHistory)
            // Expand/collapse tool entry
            .bind("e", Intent::ExpandToolEntry, KeyCategory::ChatHistory)
            // Toggle audit popup for the selected entry
            .bind("a", Intent::ToggleAuditPopup, KeyCategory::ChatHistory)
            // Toggle ignored block visibility
            .bind("h", Intent::ToggleIgnoredBlockVisibility, KeyCategory::ChatHistory)
            // Fork session from selected entry
            .bind("f", Intent::ForkFromEntry, KeyCategory::ChatHistory)
            // New session seeded with selected entry (no inherited history)
            .bind("F", Intent::NewSessionFromEntry, KeyCategory::ChatHistory)
            // Yank (copy) selected entry to clipboard
            .bind("y", Intent::YankSelectedEntry, KeyCategory::ChatHistory)
            // Open the selected task call's subagent session
            .bind("<enter>", Intent::LoadSubagentSession, KeyCategory::ChatHistory)
            // Jump to next/previous compaction summary entry
            .describe_group_with_category("]", "next", KeyCategory::ChatHistory)
            .describe_group_with_category("[", "previous", KeyCategory::ChatHistory)
            .bind("]c", Intent::ChatEntryJumpNextCompaction, KeyCategory::ChatHistory)
            .bind("[c", Intent::ChatEntryJumpPrevCompaction, KeyCategory::ChatHistory)
            .bind("]u", Intent::ChatEntryJumpNextUserEntry, KeyCategory::ChatHistory)
            .bind("[u", Intent::ChatEntryJumpPrevUserEntry, KeyCategory::ChatHistory)
            .bind("]p", Intent::ChatEntryJumpNextPinned, KeyCategory::ChatHistory)
            .bind("[p", Intent::ChatEntryJumpPrevPinned, KeyCategory::ChatHistory)
            // Jump to next/previous Sources (annotation) entry
            .bind("]s", Intent::ChatEntryJumpNextSources, KeyCategory::ChatHistory)
            .bind("[s", Intent::ChatEntryJumpPrevSources, KeyCategory::ChatHistory)
            // Session creation
            .bind("n", Intent::SessionNew, KeyCategory::General)
            .bind("N", Intent::SessionNewWithLifecycle, KeyCategory::General)
            // Escape: cancel selection
            .bind("<esc>", Intent::NormalEscape, KeyCategory::General)
            // Unmapped character keys produce NoOp to dismiss confirmation prompts
            .catch_all(|key: KeyEvent| {
                if let Key::Char(_) = key.key {
                    Some(Intent::NoOp)
                } else {
                    None
                }
            });
            add_terminal_toggles(b);
        })
        // Sidebar - Persona section
        .scope(Scope::SidebarPersona, |b| {
            add_sidebar_base(b);
            b
            // Persona-specific actions
            .bind("c", Intent::SidebarPersonaEdit, KeyCategory::Sidebar);
        })
        // Sidebar - Pins section
        .scope(Scope::SidebarPins, |b| {
            add_sidebar_base(b);
            b
            // Pin management actions
            .bind("u", Intent::PinsUnpin, KeyCategory::Sidebar)
            .bind("t", Intent::PinsPinTop, KeyCategory::Sidebar)
            .bind("b", Intent::PinsPinBottom, KeyCategory::Sidebar)
            .bind("r", Intent::PinsPinRelative, KeyCategory::Sidebar)
            .bind("m", Intent::PinsPinCycle, KeyCategory::Sidebar)
            // Leave sidebar to Normal at the pin's position (same as <c-h>/<esc>).
            .bind("<enter>", Intent::SidebarLeave, KeyCategory::General);
        })
        // Sidebar - Sessions section
        .scope(Scope::SidebarSessions, |b| {
            add_sidebar_base(b);
            b
            // Session management actions
            .bind("x", Intent::SidebarSessionClose, KeyCategory::Sidebar)
            .bind("X", Intent::SidebarSessionTeardownTree, KeyCategory::Sidebar)
            .bind("t", Intent::SidebarSessionTeardown, KeyCategory::Sidebar)
            .describe_group_with_category("p", "sessions", KeyCategory::Sidebar)
            .bind("<enter>", Intent::SidebarSessionConfirm, KeyCategory::Sidebar)
            .bind("n", Intent::SessionNew, KeyCategory::Sidebar)
            .bind("N", Intent::SessionNewWithLifecycle, KeyCategory::Sidebar)
            .bind("r", Intent::SidebarRenameSession, KeyCategory::Sidebar)
            .bind("a", Intent::SidebarSessionArchive, KeyCategory::Sidebar)
            .bind("A", Intent::SidebarSessionArchiveTree, KeyCategory::Sidebar)
            .bind("c", Intent::SidebarSessionContinue, KeyCategory::Sidebar)
            .bind("s", Intent::SidebarSessionRerunSetup, KeyCategory::Sidebar)

            // T toggles the terminal overlay for the selected session.
            .bind("T", Intent::ToggleTerminalOverlayForSelected, KeyCategory::Sidebar)
            // i activates session and enters insert mode
            .bind("i", Intent::SidebarConfirmInsert, KeyCategory::Sidebar)
            // Unmapped character keys produce NoOp to dismiss confirmation prompts
            .catch_all(|key: KeyEvent| {
                if let Key::Char(_) = key.key {
                    Some(Intent::NoOp)
                } else {
                    None
                }
            });
        })
        // Sidebar - Task list section
        .scope(Scope::SidebarTaskList, |b| {
            add_sidebar_base(b);
            // Open full-screen task list browser
            b.bind(
                "s",
                Intent::OpenPicker { kind: jinn_domain::feat::picker::PickerKind::TaskList },
                KeyCategory::Sidebar,
            )
            // Scroll the task list preview popup (left of the sidebar).
            .bind(
                "<pgup>",
                Intent::TaskListPreviewScrollUp,
                KeyCategory::Navigation,
            )
            .bind(
                "<pgdn>",
                Intent::TaskListPreviewScrollDown,
                KeyCategory::Navigation,
            );
            b.bind(
                "s",
                Intent::OpenPicker { kind: jinn_domain::feat::picker::PickerKind::TaskList },
                KeyCategory::Sidebar,
            );
        })
        // Sidebar - MCP servers section (read-only in Part 1: nav only).
        .scope(Scope::SidebarMcpServers, |b| {
            add_sidebar_base(b);
        })
        // Input scope: typing into the input buffer
        .scope(Scope::Input, |b| {
            b.bind("<enter>", Intent::SubmitMessage, KeyCategory::Input)
                .bind("<M-q>", Intent::ToggleInputMode, KeyCategory::Input)
                .bind("<M-s>", Intent::SidebarFocusSessions, KeyCategory::Navigation)
            .bind("<s-enter>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<c-enter>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
            .bind("<c-k>", Intent::EnterNormalMode, KeyCategory::General)
            .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
            .bind("<c-e>", Intent::EditInput, KeyCategory::Input)
            // <c-g> consensus one-shot removed (workflow system deprecated)
            // Change CWD - search from session CWD
            .bind("<M-c>", Intent::ChangeCwd { root: CwdRoot::Session }, KeyCategory::Navigation)
            // Change CWD - search from home directory
            .bind("<M-d>", Intent::ChangeCwd { root: CwdRoot::Home }, KeyCategory::Navigation)
            .bind("<f1>", Intent::ToggleWhichkey, KeyCategory::General)
            .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
            .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
            .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
            .bind("<home>", Intent::MoveCursorToStart, KeyCategory::Input)
            .bind("<end>", Intent::MoveCursorToEnd, KeyCategory::Input)
            .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
            .bind("<c-left>", Intent::MoveCursorWordLeft, KeyCategory::Input)
            .bind("<c-right>", Intent::MoveCursorWordRight, KeyCategory::Input)
            .bind("<up>", Intent::MoveCursorUp, KeyCategory::Input)
            .bind("<down>", Intent::MoveCursorDown, KeyCategory::Input)
            .bind("<tab>", Intent::AutocompleteConfirm, KeyCategory::Input)
            .bind("<c-u>", Intent::ScrollUp, KeyCategory::Navigation)
            .bind("<c-d>", Intent::ScrollDown, KeyCategory::Navigation)
            .bind("<c-l>", Intent::SidebarFocus, KeyCategory::Navigation)

            .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .catch_all(|key: KeyEvent| {
                if let Key::Char(c) = key.key {
                    Some(Intent::InsertChar { ch: c })
                } else {
                    None
                }
            });
            add_terminal_toggles(b);
        });

    // Picker scopes - each picker kind has its own scope for kind-specific bindings.
    // Shared bindings (navigation, confirm, escape, char input) are in add_picker_base.
    keymap
        .scope(Scope::PickerProvider, |b| {
            add_picker_base(b);
            b.bind("<Tab>", Intent::ModelToggleSelected, KeyCategory::General);
            b.bind("<c-a>", Intent::ToggleAlloyMode, KeyCategory::Model);
            b.bind("<c-r>", Intent::RefreshModels, KeyCategory::Model);
        })
        .scope(Scope::PickerSession, |b| {
            add_picker_base(b);
        })
        .scope(Scope::PickerPersona, |b| {
            add_picker_base(b);
        })
        .scope(Scope::PickerTheme, |b| {
            add_picker_base(b);
        })
        .scope(Scope::PickerLifecycle, |b| {
            add_picker_base(b);
        })

        .scope(Scope::PickerCompactionModel, |b| {
            add_picker_base(b);
        })

        .scope(Scope::PickerReasoningEffort, |b| {
            add_picker_base(b);
        })
        .scope(Scope::PickerEndpoint, |b| {
            add_picker_base(b);
            b.bind("<c-r>", Intent::RefreshEndpoints, KeyCategory::General);
        })
        .scope(Scope::PickerTool, |b| {
            add_picker_base(b);
            b.bind("<Tab>", Intent::ToolToggleSelected, KeyCategory::General);
        })
        .scope(Scope::PickerSkill, |b| {
            // Skill spec rows (TAB toggle, CTRL+L load, CTRL+U/D preview
            // paging, CTRL+R refresh) land here via bind_picker_spec_rows.
            add_picker_base(b);
        })
        .scope(Scope::PickerTaskList, |b| {
            add_picker_base(b);
        })
        .scope(Scope::PickerProject, |b| {
            add_picker_base(b);
            b.bind("<c-enter>", Intent::ProjectNewAtHighlightedWithLifecycle, KeyCategory::General)
             .bind("<c-n>", Intent::OpenProjectAddInput, KeyCategory::General)
             .bind("<c-d>", Intent::ProjectRemoveHighlighted, KeyCategory::General);
        })
        .scope(Scope::PickerMcpServer, |b| {
            add_picker_base(b);
            b.bind("<Tab>", Intent::McpToggleSelected, KeyCategory::General)
                .bind("<c-r>", Intent::McpRestartSelected, KeyCategory::General)
                .bind("<c-t>", Intent::McpTogglePreview, KeyCategory::General);
        })
        .scope(Scope::PickerPlugin, |b| {
            add_picker_base(b);
        });

    // TerminalView scope — watching an interactive_term session. Passive:
    // nothing forwards to the pty. The configured toggle key enters control
    // mode; `<M-t>` toggles the overlay closed (view mode holds no user
    // state, so the toggle is safe here); `y` yanks the visible screen to
    // the clipboard; `I` yanks it and pushes the text to the model. `T`
    // mirrors the sidebar's toggle. Deliberately unbound here: <M-`> (would
    // pop the overlay) and `i` (capture must be a deliberate act via the
    // toggle key).
    keymap.scope(Scope::TerminalView, |b| {
        b
        .bind("<Tab>", Intent::SwitchTab, KeyCategory::General)
        .bind("T", Intent::ToggleTerminalOverlayForSelected, KeyCategory::General)
        .bind("<M-t>", Intent::ToggleTerminalOverlay { session_id: None }, KeyCategory::General)
        .bind(control_toggle, Intent::TerminalTakeControl, KeyCategory::General)
        .bind("y", Intent::TerminalYank, KeyCategory::General)
        .bind("I", Intent::TerminalPushScreen, KeyCategory::General)
        .bind("q", Intent::Quit, KeyCategory::General)
        .bind("?", Intent::ToggleWhichkey, KeyCategory::General);
    });

    // TerminalControl scope — the user holds the pty. Capture mode is
    // hermetic: every key except the configured control-toggle forwards via
    // catch_all (the toggle is bound; bindings beat catch_all) and never
    // reaches the program. Toggling exits to TerminalView and releases
    // control back to the agent.
    keymap.scope(Scope::TerminalControl, |b| {
        b
        .bind(control_toggle, Intent::TerminalHandback, KeyCategory::General)
        .catch_all(|key: KeyEvent| {
            let bytes =
                jinn_domain::feat::interactive_term::settle::encode_key_event(&key);
            if bytes.is_empty() {
                None
            } else {
                Some(Intent::TerminalSendKey {
                    bytes,
                    label: String::new(),
                })
            }
        });
    });

    // ArgInput scope - typing positional args for a lifecycle command.
    keymap.scope(Scope::ArgInput, |b| {
        // Only the toggles here, not slice openers: `<M-`>` is a shell
        // character and this scope has an InsertChar guard — unlike other
        // scopes' catch-alls, an unresolved key would mutate arg text.
        b.bind("<M-t>", Intent::ToggleTerminalOverlay { session_id: None }, KeyCategory::General);
        b.bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
        .bind("<enter>", Intent::ArgInputConfirm, KeyCategory::Input)
        .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
        .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
        .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::InsertChar { ch: c })
            } else {
                None
            }
        });
    });

    // SidebarResize scope - adjusting sidebar width.
    keymap.scope(Scope::SidebarResize, |b| {
        add_terminal_toggles(b);
        b
        .bind("h", Intent::SidebarResizeExpand, KeyCategory::Sidebar)
        .bind("l", Intent::SidebarResizeContract, KeyCategory::Sidebar)
        .bind("<esc>", Intent::SidebarResizeLeave, KeyCategory::Sidebar)
        .bind("<c-c>", Intent::Quit, KeyCategory::General);
    });

    // RenameSessionInput scope - editing a session title.
    keymap.scope(Scope::RenameSessionInput, |b| {
        add_terminal_toggles(b);
        b
        .bind("<esc>", Intent::RenameSessionLeave, KeyCategory::General)
        .bind("<enter>", Intent::RenameSessionConfirm, KeyCategory::Input)
        .bind("<left>", Intent::RenameCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::RenameCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::RenameDeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::RenameDeleteForward, KeyCategory::Input)
        .bind("<c-j>", Intent::RenameInsertChar { ch: '\n' }, KeyCategory::Input)
        .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::RenameInsertChar { ch: c })
            } else {
                None
            }
        });
    });

    // PrunerAccumulationInput scope — numeric-only threshold input.
    keymap.scope(Scope::PrunerAccumulationInput, |b| {
        add_terminal_toggles(b);
        b
        .bind("<esc>", Intent::PrunerAccumulationLeave, KeyCategory::General)
        .bind("<enter>", Intent::PrunerAccumulationConfirm, KeyCategory::Input)
        .bind("<left>", Intent::PrunerAccumulationCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::PrunerAccumulationCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::PrunerAccumulationDeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::PrunerAccumulationDeleteForward, KeyCategory::Input)
        .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::PrunerAccumulationInsertChar { ch: c })
            } else {
                None
            }
        });
    });

    // CwdInput scope - typing a directory path (mirrors ArgInput).
    keymap.scope(Scope::CwdInput, |b| {
        add_terminal_toggles(b);
        b.bind("<esc>", Intent::CwdInputLeave, KeyCategory::General)
            .bind("<enter>", Intent::CwdInputConfirm, KeyCategory::Input)
            .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
            .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
            .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
            .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
            .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
            .catch_all(|key: KeyEvent| {
                if let Key::Char(c) = key.key {
                    Some(Intent::InsertChar { ch: c })
                } else {
                    None
                }
            });
    });

    // ProjectAddInput scope - clone of CwdInput, specialized for registering
    // a new project directory from inside the project picker (<c-n>).
    keymap.scope(Scope::ProjectAddInput, |b| {
        add_terminal_toggles(b);
        b.bind("<esc>", Intent::ProjectAddInputLeave, KeyCategory::General)
            .bind("<enter>", Intent::ProjectAddInputConfirm, KeyCategory::Input)
            .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
            .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
            .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
            .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
            .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<c-c>", Intent::CtrlClear, KeyCategory::General)
            .catch_all(|key: KeyEvent| {
                if let Key::Char(c) = key.key {
                    Some(Intent::InsertChar { ch: c })
                } else {
                    None
                }
            });
    });

    // No global bindings by design: globals survive every scope's catch-all
    // and would pierce TerminalControl's forwarding catch-all (stranding the
    // control flag on User) and TerminalView (popping the overlay). The two
    // would-be globals (<M-t>, <M-`>) are per-scope via add_terminal_toggles.

    keymap.on_mouse(|mouse: event::MouseEvent, _scope: &Scope| {
        match mouse.kind {
            MouseEventKind::ScrollUp => Some(Intent::MouseScrollUp),
            MouseEventKind::ScrollDown => Some(Intent::MouseScrollDown),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    /// Drift guard: every `PickerKind` must map to a keymap scope that has
    /// at least one binding. A kind whose scope is missing from the keymap
    /// opens a picker that ignores all input (perceived freeze) — this
    /// catches a forgotten `.scope(Scope::Picker..., ...)` registration the
    /// moment a variant is added or a scope is dropped.
    #[rstest::rstest]
    fn every_picker_kind_maps_to_a_scope_with_bindings(
        #[values(
            PickerKind::Provider,
            PickerKind::Session,
            PickerKind::Persona,
            PickerKind::Theme,
            PickerKind::SessionLifecycle,
            PickerKind::CompactionModel,
            PickerKind::ReasoningEffort,
            PickerKind::Tool,
            PickerKind::Skill,
            PickerKind::TaskList,
            PickerKind::Project,
            PickerKind::McpServer,
            PickerKind::Plugin,
            PickerKind::Endpoint
        )]
        kind: PickerKind,
    ) {
        use crate::app::scope_for_focus;

        // Given the default keymap.
        let keymap = init();

        // When mapping the picker's focus scope to a keymap scope.
        let scope = scope_for_focus(&jinn_domain::FocusScope::Picker { kind });

        // Then that scope has at least one binding group with a binding.
        let groups = keymap.bindings_for_scope(scope.clone());
        let binding_count: usize = groups.iter().map(|g| g.bindings.len()).sum();
        assert!(
            binding_count > 0,
            "scope {scope:?} for picker {kind} has no bindings — the picker would ignore all input"
        );
    }

    /// The <M-t> overlay toggle resolves from every non-terminal scope —
    /// it is registered per-scope (via `add_terminal_toggles`), so the list
    /// of scopes here doubles as the drift guard: a scope added to the
    /// keymap without the toggles fails the terminal-scopes test only if it
    /// is one of the two, and this test pins the chat-tab scopes explicitly.
    #[rstest::rstest]
    #[test]
    fn alt_t_resolves_from_non_terminal_scopes() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        let alt_t = jinn_domain::KeyEvent {
            key: Key::Char('t'),
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::none()
            },
        };

        for scope in [Scope::Normal, Scope::Input, Scope::SidebarSessions] {
            // Given the default keymap starting in `scope`.
            let keymap = init();
            let mut wk = WhichKeyInstance::new(keymap, scope.clone());

            // When pressing <M-t>.
            let intent = wk.handle_key(alt_t.clone());

            // Then the terminal overlay toggle fires.
            assert!(
                matches!(
                    intent,
                    Some(Intent::ToggleTerminalOverlay { session_id: None })
                ),
                "scope {scope:?}: expected ToggleTerminalOverlay, got {intent:?}"
            );
        }
    }

    /// Capture mode is hermetic: neither would-be global resolves in the
    /// TerminalControl scope — the catch-all forwards <M-t>/<M-`> to the
    /// pty like any other key. (The old globals leaked here and could
    /// strand the control flag on User.) In TerminalView, <M-t> is bound
    /// (the toggle closes the overlay); <M-`> stays unbound.
    #[rstest::rstest]
    #[case(Scope::TerminalControl)]
    fn alt_t_and_quake_do_not_resolve_in_terminal_control(#[case] scope: Scope) {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, scope.clone());

        // When pressing <M-t>.
        let alt_t = KeyEvent {
            key: Key::Char('t'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        };
        let intent = wk.handle_key(alt_t);

        // Then it never toggles the overlay: control mode forwards it to
        // the pty.
        assert!(
            !matches!(intent, Some(Intent::ToggleTerminalOverlay { .. })),
            "{scope:?}: <M-t> must not fire an overlay intent; got {intent:?}"
        );

        // When pressing <M-`>.
        let alt_backtick = KeyEvent {
            key: Key::Char('`'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        };
        let intent = wk.handle_key(alt_backtick);

        // Then likewise no quake intent fires.
        assert!(
            !matches!(intent, Some(Intent::Dynamic(ref d)) if d.action == "open"),
            "{scope:?}: <M-`> must not open the quake bar; got {intent:?}"
        );
    }

    /// Every scope except TerminalControl (hermetic capture) and the
    /// quake-adjacent exclusions noted per-test must carry the would-be
    /// global toggles. This is the audit half of the hermetic-capture
    /// guarantee: a future scope registered without `add_terminal_toggles`
    /// fails here (toggle dead in that scope).
    #[rstest::rstest]
    #[case(Scope::Normal)]
    #[case(Scope::Input)]
    #[case(Scope::SidebarPersona)]
    #[case(Scope::SidebarPins)]
    #[case(Scope::SidebarSessions)]
    #[case(Scope::SidebarTaskList)]
    #[case(Scope::SidebarMcpServers)]
    #[case(Scope::PickerProvider)]
    #[case(Scope::PickerSession)]
    #[case(Scope::PickerPersona)]
    #[case(Scope::PickerTheme)]
    #[case(Scope::PickerLifecycle)]
    #[case(Scope::PickerCompactionModel)]
    #[case(Scope::PickerReasoningEffort)]
    #[case(Scope::PickerEndpoint)]
    #[case(Scope::PickerTool)]
    #[case(Scope::PickerSkill)]
    #[case(Scope::PickerTaskList)]
    #[case(Scope::PickerProject)]
    #[case(Scope::PickerMcpServer)]
    #[case(Scope::PickerPlugin)]
    #[case(Scope::ArgInput)]
    #[case(Scope::SidebarResize)]
    #[case(Scope::RenameSessionInput)]
    #[case(Scope::PrunerAccumulationInput)]
    #[case(Scope::CwdInput)]
    #[case(Scope::ProjectAddInput)]
    #[case(Scope::TerminalView)]
    fn alt_t_resolves_in_every_non_terminal_scope(#[case] scope: Scope) {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        // Given the default keymap queried in a non-terminal scope.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, scope.clone());

        // When pressing <M-t>.
        let alt_t = KeyEvent {
            key: Key::Char('t'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        };
        let intent = wk.handle_key(alt_t);

        // Then the overlay toggle resolves.
        assert!(
            matches!(
                intent,
                Some(Intent::ToggleTerminalOverlay { session_id: None })
            ),
            "{scope:?}: <M-t> must resolve to ToggleTerminalOverlay; got {intent:?}"
        );
    }

    /// `y` and `I` resolve in view mode only; in control mode the catch-all
    /// forwards them to the pty (yanking/sharing must be a deliberate
    /// view-mode act, never a stray keypress during capture).
    #[rstest::rstest]
    #[test]
    fn yank_and_push_resolve_only_in_view_mode() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        for ch in ['y', 'I'] {
            // Given the keymap in TerminalView scope.
            let keymap = init();
            let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);

            // When pressing the key.
            let intent = wk.handle_key(KeyEvent {
                key: Key::Char(ch),
                modifiers: Modifiers::none(),
            });

            // Then it resolves to the view-mode action.
            let expected = if ch == 'y' {
                Intent::TerminalYank
            } else {
                Intent::TerminalPushScreen
            };
            assert!(
                matches!(intent.as_ref(), Some(got) if std::mem::discriminant(got) == std::mem::discriminant(&expected)),
                "'{ch}' in TerminalView must resolve to {expected:?}; got {intent:?}"
            );

            // Given the keymap in TerminalControl scope.
            let keymap = init();
            let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

            // When pressing the same key.
            let intent = wk.handle_key(KeyEvent {
                key: Key::Char(ch),
                modifiers: Modifiers::none(),
            });

            // Then it forwards to the pty instead.
            assert!(
                matches!(intent, Some(Intent::TerminalSendKey { .. })),
                "'{ch}' in TerminalControl must forward to the pty; got {intent:?}"
            );
        }
    }

    /// Capital-I reaches the keymap already normalized by `convert.rs`
    /// (both terminal spellings — `Char('i')+SHIFT` and `Char('I')±SHIFT —
    /// become `Char('I')` with shift cleared). This pins the end-to-end
    /// path: raw crossterm shift-I in view mode resolves to
    /// TerminalPushScreen, and the same key in control mode forwards the
    /// literal `I` byte to the pty.
    #[rstest::rstest]
    #[case(
        crossterm::event::KeyCode::Char('I'),
        crossterm::event::KeyModifiers::NONE
    )]
    #[case(
        crossterm::event::KeyCode::Char('I'),
        crossterm::event::KeyModifiers::SHIFT
    )]
    #[case(
        crossterm::event::KeyCode::Char('i'),
        crossterm::event::KeyModifiers::SHIFT
    )]
    fn raw_shift_i_pushes_in_view_and_forwards_in_control(
        #[case] code: crossterm::event::KeyCode,
        #[case] cmods: crossterm::event::KeyModifiers,
    ) {
        use crate::app::WhichKeyInstance;
        use crate::convert::from_crossterm;

        // Given the raw crossterm event converted through the app adapter.
        let raw = crossterm::event::KeyEvent::new(code, cmods);
        let key = from_crossterm(raw).expect("capital-I converts");

        // When pressing it in TerminalView.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);
        let intent = wk.handle_key(key.clone());

        // Then it resolves to TerminalPushScreen.
        assert!(
            matches!(intent, Some(Intent::TerminalPushScreen)),
            "capital-I ({key:?}) must resolve to TerminalPushScreen; got {intent:?}"
        );

        // When pressing it in TerminalControl.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);
        let intent = wk.handle_key(key);

        // Then it forwards the literal byte to the pty.
        match intent {
            Some(Intent::TerminalSendKey { bytes, .. }) => assert_eq!(bytes, b"I"),
            other => panic!("capital-I in control must forward; got {other:?}"),
        }
    }

    /// The sidebar `T` key resolves to the selected-session overlay toggle.
    #[rstest::rstest]
    #[test]
    fn sidebar_upper_t_resolves_to_selected_session_overlay_toggle() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given the default keymap in the SidebarSessions scope.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::SidebarSessions);

        // When pressing 'T'.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('T'),
            modifiers: Modifiers::none(),
        });

        // Then the selected-session overlay toggle fires (not NoOp).
        assert!(matches!(
            intent,
            Some(Intent::ToggleTerminalOverlayForSelected)
        ));
    }

    /// `T` inside the overlay resolves to the toggle (same intent as the
    /// sidebar key), so the overlay closes from inside it.
    #[rstest::rstest]
    #[test]
    fn upper_t_inside_overlay_resolves_to_the_toggle() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given the default keymap in the TerminalView scope.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);

        // When pressing 'T'.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('T'),
            modifiers: Modifiers::none(),
        });

        // Then the overlay toggle fires.
        assert!(matches!(
            intent,
            Some(Intent::ToggleTerminalOverlayForSelected)
        ));
    }

    /// Regression test for ratatui-which-key v0.12.1: when a key is bound as a
    /// leaf in one scope (Normal) and used as a describe_group prefix in
    /// another scope (SidebarSessions), the leaf must survive the
    /// Leaf→Branch promotion. Before the fix, the library dropped the
    /// existing binding and the catch-all fired instead.
    #[rstest::rstest]
    #[test]
    fn p_prefix_group_in_sidebar_does_not_drop_normal_pin_binding() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given a fresh keymap with no custom bindings.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::Normal);

        // When pressing 'p' alone.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('p'),
            modifiers: Modifiers::none(),
        });

        // Then it fires ChatEntryPinSelected (not a chord prefix).
        assert!(
            matches!(intent, Some(jinn_domain::Intent::ChatEntryPinSelected)),
            "'p' in Normal scope should fire ChatEntryPinSelected; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn alt_t_in_input_scope_does_not_insert_literal_t() {
        // Given a keymap with the per-scope <M-t> binding, queried in Input scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::Input);

        // When pressing <M-t> (Alt+t).
        let alt_t = KeyEvent {
            key: Key::Char('t'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        };
        let intent = wk.handle_key(alt_t);

        // Then it resolves to the overlay toggle, NOT a literal InsertChar('t') —
        // the scope binding beats the Input catch-all.
        let intent = intent.expect(
            "<M-t> in Input scope must fire an intent; got None (scope binding missing, catch-all regression)",
        );
        assert!(
            matches!(intent, Intent::ToggleTerminalOverlay { session_id: None }),
            "<M-t> must resolve to the overlay toggle, not InsertChar; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn task_list_scope_pgup_fires_preview_scroll_up() {
        // Given a keymap queried in SidebarTaskList scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::SidebarTaskList);

        // When pressing PageUp.
        let pgup = KeyEvent {
            key: Key::PageUp,
            modifiers: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(pgup);

        // Then it resolves to TaskListPreviewScrollUp.
        let intent = intent.expect("PageUp in SidebarTaskList scope must fire an intent");
        assert!(
            matches!(intent, Intent::TaskListPreviewScrollUp),
            "PageUp must resolve to TaskListPreviewScrollUp; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn task_list_scope_pgdn_fires_preview_scroll_down() {
        // Given a keymap queried in SidebarTaskList scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::SidebarTaskList);

        // When pressing PageDown.
        let pgdn = KeyEvent {
            key: Key::PageDown,
            modifiers: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(pgdn);

        // Then it resolves to TaskListPreviewScrollDown.
        let intent = intent.expect("PageDown in SidebarTaskList scope must fire an intent");
        assert!(
            matches!(intent, Intent::TaskListPreviewScrollDown),
            "PageDown must resolve to TaskListPreviewScrollDown; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn ctrl_d_in_project_picker_removes_highlighted() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given a fresh keymap.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerProject);

        // When pressing Ctrl+D.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('d'),
            modifiers: Modifiers::ctrl(),
        });

        // Then it fires ProjectRemoveHighlighted.
        assert!(
            matches!(intent, Some(jinn_domain::Intent::ProjectRemoveHighlighted)),
            "<c-d> in PickerProject should fire ProjectRemoveHighlighted; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn bare_d_in_project_picker_types_into_filter() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given a fresh keymap.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerProject);

        // When pressing bare 'd'.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('d'),
            modifiers: Modifiers::none(),
        });

        // Then it falls through to the catch-all and types into the filter.
        assert!(
            matches!(
                intent,
                Some(jinn_domain::Intent::PickerInsertChar { ch: 'd' })
            ),
            "bare 'd' in PickerProject should type into the filter; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn bare_a_in_project_picker_types_into_filter_not_add_cwd() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given a fresh keymap.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerProject);

        // When pressing bare 'a'.
        let intent = wk.handle_key(jinn_domain::KeyEvent {
            key: Key::Char('a'),
            modifiers: Modifiers::none(),
        });

        // Then it types into the filter (the unapproved 'a' add-cwd bind is gone).
        assert!(
            matches!(
                intent,
                Some(jinn_domain::Intent::PickerInsertChar { ch: 'a' })
            ),
            "bare 'a' in PickerProject should type into the filter, not add cwd; got {intent:?}",
        );
    }

    #[rstest::rstest]
    fn leader_sr_resolves_to_reasoning_effort_picker() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();
        let leader = KeyEvent {
            key: Key::Char(' '),
            modifiers: Modifiers::none(),
        };

        // When navigating the <leader>sr sequence.
        let path = [
            leader,
            KeyEvent {
                key: Key::Char('s'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('r'),
                modifiers: Modifiers::none(),
            },
        ];
        let result = keymap.navigate(&path, &Scope::Normal).expect("path exists");

        // Then it resolves to OpenPicker{ReasoningEffort}.
        match result {
            NodeResult::Leaf { action } => assert!(
                matches!(
                    action,
                    Intent::OpenPicker {
                        kind: PickerKind::ReasoningEffort
                    }
                ),
                "<leader>sr must resolve to OpenPicker{{ReasoningEffort}}; got {action:?}",
            ),
            other => panic!("<leader>sr must be a leaf, got branch: {other:?}"),
        }
    }
    #[rstest::rstest]
    fn reasoning_effort_picker_scope_binds_base_intents() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();
        let esc = KeyEvent {
            key: Key::Esc,
            modifiers: Modifiers::none(),
        };
        let enter = KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::none(),
        };

        // When navigating the two explicit base keys within the ReasoningEffort picker scope.
        let esc_res = keymap
            .navigate(&[esc], &Scope::PickerReasoningEffort)
            .expect("esc bound");
        let enter_res = keymap
            .navigate(&[enter], &Scope::PickerReasoningEffort)
            .expect("enter bound");

        // Then each resolves to a real picker base intent (the bug: scope had no bindings).
        let NodeResult::Leaf { action: esc_action } = esc_res else {
            panic!("esc must be a leaf");
        };
        assert!(
            matches!(esc_action, Intent::EnterNormalMode),
            "esc must resolve to EnterNormalMode, got {esc_action:?}"
        );

        let NodeResult::Leaf {
            action: enter_action,
        } = enter_res
        else {
            panic!("enter must be a leaf");
        };
        assert!(
            matches!(enter_action, Intent::PickerConfirm),
            "enter must resolve to PickerConfirm, got {enter_action:?}"
        );
    }

    #[rstest::rstest]
    fn endpoint_picker_scope_binds_base_intents() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();
        let esc = KeyEvent {
            key: Key::Esc,
            modifiers: Modifiers::none(),
        };
        let enter = KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::none(),
        };

        // When navigating the two explicit base keys within the Endpoint picker scope.
        let esc_res = keymap
            .navigate(&[esc], &Scope::PickerEndpoint)
            .expect("esc bound");
        let enter_res = keymap
            .navigate(&[enter], &Scope::PickerEndpoint)
            .expect("enter bound");

        // Then each resolves to a real picker base intent (regression: scope once had no bindings, freezing the popup).
        let NodeResult::Leaf { action: esc_action } = esc_res else {
            panic!("esc must be a leaf");
        };
        assert!(
            matches!(esc_action, Intent::EnterNormalMode),
            "esc must resolve to EnterNormalMode, got {esc_action:?}"
        );

        let NodeResult::Leaf {
            action: enter_action,
        } = enter_res
        else {
            panic!("enter must be a leaf");
        };
        assert!(
            matches!(enter_action, Intent::PickerConfirm),
            "enter must resolve to PickerConfirm, got {enter_action:?}"
        );
    }

    #[rstest::rstest]
    fn endpoint_picker_scope_ctrl_r_resolves_to_refresh_endpoints() {
        // Given the default keymap.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerEndpoint);

        // When pressing Ctrl+R.
        let c_r = jinn_domain::KeyEvent {
            key: Key::Char('r'),
            modifiers: Modifiers::ctrl(),
        };
        let intent = wk.handle_key(c_r);

        // Then it resolves to RefreshEndpoints (forces a fresh endpoint fetch).
        assert!(
            matches!(intent, Some(jinn_domain::Intent::RefreshEndpoints)),
            "<c-r> in PickerEndpoint should fire RefreshEndpoints; got {intent:?}",
        );
    }

    #[rstest::rstest]
    fn leader_se_resolves_to_persona_picker() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();
        let path = [
            KeyEvent {
                key: Key::Char(' '),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('s'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('e'),
                modifiers: Modifiers::none(),
            },
        ];

        // When navigating the <leader>se sequence.
        let result = keymap.navigate(&path, &Scope::Normal).expect("path exists");

        // Then it resolves to OpenPicker{Persona} (rebound from <leader>sp).
        match result {
            NodeResult::Leaf { action } => assert!(
                matches!(
                    action,
                    Intent::OpenPicker {
                        kind: PickerKind::Persona
                    }
                ),
                "<leader>se must resolve to OpenPicker{{Persona}}; got {action:?}",
            ),
            other => panic!("<leader>se must be a leaf, got branch: {other:?}"),
        }
    }

    #[rstest::rstest]
    fn leader_sp_capital_resolves_to_plugin_picker() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();
        let path = [
            KeyEvent {
                key: Key::Char(' '),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('s'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('P'),
                modifiers: Modifiers::none(),
            },
        ];

        // When navigating the <leader>sP sequence.
        let result = keymap.navigate(&path, &Scope::Normal).expect("path exists");

        // Then it resolves to OpenPicker{Plugin}.
        match result {
            NodeResult::Leaf { action } => assert!(
                matches!(
                    action,
                    Intent::OpenPicker {
                        kind: PickerKind::Plugin
                    }
                ),
                "<leader>sP must resolve to OpenPicker{{Plugin}}; got {action:?}",
            ),
            other => panic!("<leader>sP must be a leaf, got branch: {other:?}"),
        }
    }

    #[rstest::rstest]
    fn bracket_c_chord_resolves_to_jump_compaction_intents() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();

        // When navigating ]c (next compaction) in Normal scope.
        let next_path = [
            KeyEvent {
                key: Key::Char(']'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('c'),
                modifiers: Modifiers::none(),
            },
        ];
        let next_result = keymap
            .navigate(&next_path, &Scope::Normal)
            .expect("]c path exists");

        // Then it resolves to ChatEntryJumpNextCompaction.
        match next_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpNextCompaction),
                "]c must resolve to ChatEntryJumpNextCompaction; got {action:?}",
            ),
            other => panic!("]c must be a leaf, got branch: {other:?}"),
        }

        // When navigating [c (previous compaction) in Normal scope.
        let prev_path = [
            KeyEvent {
                key: Key::Char('['),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('c'),
                modifiers: Modifiers::none(),
            },
        ];
        let prev_result = keymap
            .navigate(&prev_path, &Scope::Normal)
            .expect("[c path exists");

        // Then it resolves to ChatEntryJumpPrevCompaction.
        match prev_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpPrevCompaction),
                "[c must resolve to ChatEntryJumpPrevCompaction; got {action:?}",
            ),
            other => panic!("[c must be a leaf, got branch: {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn bracket_c_chord_does_not_resolve_in_input_scope() {
        // Given the default keymap queried in Input scope.
        // Input scope has a catch-all that turns every Char into InsertChar,
        // so the `]c` / `[c` jump chords (bound only in Normal) must never fire here.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::Input);

        let bracket = KeyEvent {
            key: Key::Char(']'),
            modifiers: Modifiers::none(),
        };

        // When pressing `]` in Input scope.
        let intent = wk.handle_key(bracket);

        // Then it resolves to a literal InsertChar(']'), not the jump chord prefix.
        // The `]c` jump intents are therefore unreachable in Input scope.
        let intent = intent.expect("] in Input scope must fire an intent (catch-all)");
        assert!(
            matches!(intent, Intent::InsertChar { ch: ']' }),
            "] in Input scope must insert a literal ], not start the jump chord; got {intent:?}",
        );
    }

    #[rstest::rstest]
    fn bracket_p_chord_resolves_to_jump_pinned_intents() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();

        // When navigating ]p (next pinned) in Normal scope.
        let next_path = [
            KeyEvent {
                key: Key::Char(']'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('p'),
                modifiers: Modifiers::none(),
            },
        ];
        let next_result = keymap
            .navigate(&next_path, &Scope::Normal)
            .expect("]p path exists");

        // Then it resolves to ChatEntryJumpNextPinned.
        match next_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpNextPinned),
                "]p must resolve to ChatEntryJumpNextPinned; got {action:?}",
            ),
            other => panic!("]p must be a leaf, got branch: {other:?}"),
        }

        // When navigating [p (previous pinned) in Normal scope.
        let prev_path = [
            KeyEvent {
                key: Key::Char('['),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('p'),
                modifiers: Modifiers::none(),
            },
        ];
        let prev_result = keymap
            .navigate(&prev_path, &Scope::Normal)
            .expect("[p path exists");

        // Then it resolves to ChatEntryJumpPrevPinned.
        match prev_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpPrevPinned),
                "[p must resolve to ChatEntryJumpPrevPinned; got {action:?}",
            ),
            other => panic!("[p must be a leaf, got branch: {other:?}"),
        }
    }

    #[rstest::rstest]
    fn bracket_s_chord_resolves_to_jump_sources_intents() {
        // Given the default keymap.
        use jinn_domain::{Key, KeyEvent, Modifiers};
        use ratatui_which_key::NodeResult;
        let keymap = init();

        // When navigating ]s (next sources) in Normal scope.
        let next_path = [
            KeyEvent {
                key: Key::Char(']'),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('s'),
                modifiers: Modifiers::none(),
            },
        ];
        let next_result = keymap
            .navigate(&next_path, &Scope::Normal)
            .expect("]s path exists");

        // Then it resolves to ChatEntryJumpNextSources.
        match next_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpNextSources),
                "]s must resolve to ChatEntryJumpNextSources; got {action:?}",
            ),
            other => panic!("]s must be a leaf, got branch: {other:?}"),
        }

        // When navigating [s (previous sources) in Normal scope.
        let prev_path = [
            KeyEvent {
                key: Key::Char('['),
                modifiers: Modifiers::none(),
            },
            KeyEvent {
                key: Key::Char('s'),
                modifiers: Modifiers::none(),
            },
        ];
        let prev_result = keymap
            .navigate(&prev_path, &Scope::Normal)
            .expect("[s path exists");

        // Then it resolves to ChatEntryJumpPrevSources.
        match prev_result {
            NodeResult::Leaf { action } => assert!(
                matches!(action, Intent::ChatEntryJumpPrevSources),
                "[s must resolve to ChatEntryJumpPrevSources; got {action:?}",
            ),
            other => panic!("[s must be a leaf, got branch: {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn terminal_view_scope_does_not_forward_keys() {
        // Given a keymap queried in TerminalView scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);

        // When pressing a printable key.
        let g = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(g);

        // Then nothing fires (view mode is passive — no pty forwarding).
        assert!(
            intent.is_none(),
            "TerminalView must not forward keys; got {intent:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn terminal_view_scope_i_is_inert() {
        // Given a keymap queried in TerminalView scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);

        // When pressing `i`.
        let i = KeyEvent {
            key: Key::Char('i'),
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(i);

        // Then nothing fires: capture is a deliberate act via the configured
        // toggle key, and `i` is a single keystroke away from accident.
        assert!(
            intent.is_none(),
            "unbound `i` in TerminalView must not fire (accidental-capture guard); got {intent:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn terminal_control_scope_printable_forwards_to_send_key() {
        // Given a keymap queried in TerminalControl scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing a printable key.
        let a = KeyEvent {
            key: Key::Char('a'),
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(a);

        // Then it resolves to TerminalSendKey carrying the encoded byte.
        let intent = intent.expect("printable key in TerminalControl must forward");
        match intent {
            Intent::TerminalSendKey { bytes, .. } => assert_eq!(bytes, b"a"),
            other => panic!("expected TerminalSendKey, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn control_toggle_key_does_not_forward_and_takes_control_in_view() {
        // Given a keymap queried in TerminalControl scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing <c-g> (the handback key).
        let ctrl_g = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_g);

        // Then it resolves to TerminalHandback, not a pty forward — the
        // toggle key is consumed by jinn in both directions.
        let intent = intent.expect("<c-g> in TerminalControl must fire an intent");
        assert!(matches!(intent, Intent::TerminalHandback));

        // Given the same keymap in TerminalView scope.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalView);

        // When pressing <c-g> (the same configured toggle key).
        let ctrl_g = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_g);

        // Then it resolves to TerminalTakeControl — the toggle works in
        // both directions.
        let intent = intent.expect("<c-g> in TerminalView must fire an intent");
        assert!(matches!(intent, Intent::TerminalTakeControl));
    }

    #[rstest::rstest]
    #[test]
    fn terminal_control_scope_ctrl_c_forwards_as_control_byte() {
        // Given a keymap queried in TerminalControl scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing Ctrl+C.
        let ctrl_c = KeyEvent {
            key: Key::Char('c'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_c);

        // Then it forwards as the C0 ETX byte (not jinn's CtrlClear).
        let intent = intent.expect("ctrl+c in TerminalControl must forward");
        match intent {
            Intent::TerminalSendKey { bytes, .. } => assert_eq!(bytes, vec![0x03]),
            other => panic!("expected TerminalSendKey, got {other:?}"),
        }
    }

    #[rstest::rstest]
    #[test]
    fn custom_control_toggle_binding_is_respected() {
        // Given a keymap built with `<c-q>` as the handback key.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init_with_control_toggle("<c-q>");
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing <c-q>.
        let ctrl_q = KeyEvent {
            key: Key::Char('q'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_q);

        // Then it resolves to TerminalHandback.
        let intent = intent.expect("configured handback key must fire an intent");
        assert!(matches!(intent, Intent::TerminalHandback));

        // When pressing <c-g> (no longer the handback key).
        let ctrl_g = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_g);

        // Then it forwards to the pty instead (TerminalSendKey).
        let intent = intent.expect("former handback key must forward");
        assert!(matches!(intent, Intent::TerminalSendKey { .. }));
    }

    /// An alt-modified control-toggle key (e.g. `<m-g>`) must bind and resolve:
    /// any keymap-parseable binding is accepted (`[interactive_term]
    /// control_toggle_key = "<m-g>"` in jinn.toml).
    #[rstest::rstest]
    #[test]
    fn alt_control_toggle_key_resolves() {
        // Given a keymap built with `<m-g>` as the handback key.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init_with_control_toggle("<m-g>");
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing alt+g.
        let alt_g = KeyEvent {
            key: Key::Char('g'),
            modifiers: Modifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        };
        let intent = wk.handle_key(alt_g);

        // Then it resolves to TerminalHandback.
        let intent = intent.expect("configured alt handback key must fire an intent");
        assert!(matches!(intent, Intent::TerminalHandback));
    }

    /// A sequence control-toggle (e.g. `zx`) binds as a prefix: the first key
    /// enters which-key pending state rather than forwarding to the pty.
    #[rstest::rstest]
    #[test]
    fn sequence_control_toggle_first_key_pends_not_forwards() {
        // Given a keymap built with a two-key handback sequence.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init_with_control_toggle("zx");
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing the sequence's first key.
        let z = KeyEvent {
            key: Key::Char('z'),
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(z);

        // Then nothing forwards to the pty yet (pending state).
        assert!(intent.is_none());
    }

    /// A punctuation control-toggle key (e.g. `<c-'>`) must bind and resolve —
    /// `normalize_control_toggle_key` permits any single character after
    /// `c-`, so the keymap must too.
    #[rstest::rstest]
    #[test]
    fn punctuation_control_toggle_key_resolves() {
        // Given a keymap built with `<c-'>` as the handback key.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init_with_control_toggle("<c-'>");
        let mut wk = WhichKeyInstance::new(keymap, Scope::TerminalControl);

        // When pressing ctrl+'.
        let ctrl_quote = KeyEvent {
            key: Key::Char('\''),
            modifiers: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(ctrl_quote);

        // Then it resolves to TerminalHandback.
        let intent = intent.expect("configured punctuation handback key must fire an intent");
        assert!(matches!(intent, Intent::TerminalHandback));
    }
}

#[cfg(test)]
mod leak_check {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use crate::keymap::init;
    use crate::scope::Scope;
    use ratatui_which_key::Keymap as WKKeymap;

    #[rstest::rstest]
    #[test]
    fn normal_scope_still_shows_chathistory_and_sidebar_groups() {
        // Regression: the library fix must not remove ChatHistory groups from
        // Normal scope where they legitimately belong. The `p` key in Normal
        // scope is a leaf (ChatEntryPinSelected → "pin entry"), not the
        // sessions branch, so we only assert the bracket groups here.
        let keymap: WKKeymap<
            jinn_domain::KeyEvent,
            Scope,
            jinn_domain::Intent,
            crate::keymap::KeyCategory,
        > = init();
        let groups = keymap.bindings_for_scope(Scope::Normal);
        let all_desc: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.bindings.iter().map(|b| b.description.as_str()))
            .collect();
        assert!(
            all_desc.iter().any(|d| d.contains("next")),
            "next group should appear in Normal scope; got {all_desc:?}"
        );
        assert!(
            all_desc.iter().any(|d| d.contains("previous")),
            "previous group should appear in Normal scope; got {all_desc:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn picker_scope_pgup_fires_picker_page_up() {
        // Given a keymap queried in a generic picker scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerPersona);

        // When pressing PageUp.
        let pgup = KeyEvent {
            key: Key::PageUp,
            modifiers: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(pgup);

        // Then it resolves to PickerPageUp.
        let intent = intent.expect("PageUp in PickerPersona must fire an intent");
        assert!(
            matches!(intent, jinn_domain::Intent::PickerPageUp),
            "PageUp must resolve to PickerPageUp; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn picker_scope_pgdn_fires_picker_page_down() {
        // Given a keymap queried in a generic picker scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerPersona);

        // When pressing PageDown.
        let pgdn = KeyEvent {
            key: Key::PageDown,
            modifiers: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(pgdn);

        // Then it resolves to PickerPageDown.
        let intent = intent.expect("PageDown in PickerPersona must fire an intent");
        assert!(
            matches!(intent, jinn_domain::Intent::PickerPageDown),
            "PageDown must resolve to PickerPageDown; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn skill_scope_pgup_fires_picker_page_up_not_preview_scroll() {
        // Given a keymap queried in the skill picker scope.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerSkill);

        // When pressing PageUp.
        let pgup = KeyEvent {
            key: Key::PageUp,
            modifiers: Modifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        };
        let intent = wk.handle_key(pgup);

        // Then it resolves to PickerPageUp (list paging), not a picker action.
        let intent = intent.expect("PageUp in PickerSkill must fire an intent");
        assert!(
            matches!(intent, jinn_domain::Intent::PickerPageUp),
            "PageUp in PickerSkill must route to list paging; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn skill_scope_ctrl_u_fires_the_spec_paging_action() {
        // Given a keymap with the domain's skill spec rows bound.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let mut keymap = init();
        crate::keymap_gen::bind_picker_spec_rows(
            &jinn_domain::feat::picker::registry::build_picker_registry(),
            &mut keymap,
        );
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerSkill);

        // When pressing Ctrl+U.
        let c_u = KeyEvent {
            key: Key::Char('u'),
            modifiers: Modifiers::ctrl(),
        };
        let intent = wk.handle_key(c_u);

        // Then it resolves to the skill spec's paging action.
        let intent = intent.expect("Ctrl+U in PickerSkill must fire an intent");
        assert!(
            matches!(
                &intent,
                jinn_domain::Intent::PickerAction { picker, action }
                    if picker == "skill" && action == "<c-u>"
            ),
            "Ctrl+U in PickerSkill must fire the spec paging action; got {intent:?}",
        );
    }

    #[rstest::rstest]
    #[test]
    fn skill_scope_ctrl_l_fires_the_spec_load_action() {
        // Given a keymap with the domain's skill spec rows bound.
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, KeyEvent, Modifiers};

        let mut keymap = init();
        crate::keymap_gen::bind_picker_spec_rows(
            &jinn_domain::feat::picker::registry::build_picker_registry(),
            &mut keymap,
        );
        let mut wk = WhichKeyInstance::new(keymap, Scope::PickerSkill);

        // When pressing Ctrl+L.
        let c_l = KeyEvent {
            key: Key::Char('l'),
            modifiers: Modifiers::ctrl(),
        };
        let intent = wk.handle_key(c_l);

        // Then it resolves to the skill spec's load action.
        assert!(
            matches!(
                &intent,
                Some(jinn_domain::Intent::PickerAction { picker, action })
                    if picker == "skill" && action == "<c-l>"
            ),
            "Ctrl+L in PickerSkill should fire the spec load action; got {intent:?}",
        );
    }

    /// Normal-mode <enter> opens the selected task call's subagent session.
    /// Also guards against accidental rebinding: nothing else may claim
    /// <enter> in the Normal scope.
    #[rstest::rstest]
    #[test]
    fn enter_in_normal_scope_fires_load_subagent_session() {
        use crate::app::WhichKeyInstance;
        use jinn_domain::{Key, Modifiers};

        // Given the default keymap queried in the Normal scope.
        let keymap = init();
        let mut wk = WhichKeyInstance::new(keymap, Scope::Normal);

        // When pressing <enter>.
        let enter = jinn_domain::KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(enter);

        // Then it resolves to LoadSubagentSession.
        assert!(
            matches!(intent, Some(jinn_domain::Intent::LoadSubagentSession)),
            "<enter> in Normal scope should fire LoadSubagentSession; got {intent:?}",
        );
    }
}
