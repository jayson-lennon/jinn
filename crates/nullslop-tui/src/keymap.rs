//! Keymap configuration and initialization.
//!
//! Defines the key categories and builds the keymap with all scope bindings.
//! Binds keys to [`Intent`] variants. Parameterized on
//! [`KeyEvent`] so the keymap works in both TUI and headless modes.

use crossterm::event::{self, MouseEventKind};
use derive_more::Display;
use nullslop_domain::Intent;
use nullslop_domain::PickerKind;
use nullslop_domain::feat::theme::Theme;
use nullslop_domain::{Key, KeyEvent};
use ratatui_which_key::CrosstermKeymapExt as _;
use ratatui_which_key::Key as WhichKeyKey;
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
}

/// Builds and returns the full keymap with all scope bindings.
/// Adds shared sidebar keybindings common to all sidebar section scopes.
///
/// Includes: quit, help, navigation (j/k/J/K), escape, tab switching,
/// pane navigation, sidebar resize, and input mode entry.
fn add_sidebar_base(b: &mut ratatui_which_key::ScopeBuilder<KeyEvent, Scope, Intent, KeyCategory>) {
    b
        // General — app control
        .bind("q", Intent::Quit, KeyCategory::General)
        .bind("<c-c>", Intent::Quit, KeyCategory::General)
        .bind("?", Intent::ToggleWhichkey, KeyCategory::General)
        // Navigation — within section and between sections
        .bind("j", Intent::SidebarMoveDown, KeyCategory::Navigation)
        .bind("k", Intent::SidebarMoveUp, KeyCategory::Navigation)
        .bind("J", Intent::SidebarSectionNext, KeyCategory::Navigation)
        .bind("K", Intent::SidebarSectionPrev, KeyCategory::Navigation)
        .bind("<esc>", Intent::SidebarLeave, KeyCategory::General)
        // Pane navigation — focus back to chat
        .bind("<c-h>", Intent::SidebarLeave, KeyCategory::Navigation)
        // Sidebar resize
        .bind("<c-w>", Intent::SidebarResizeEnter, KeyCategory::Navigation)
        // Input — external editor
        .bind("<c-e>", Intent::EditInput, KeyCategory::Input)
        // Input — enter input mode
        .bind("i", Intent::EnterInsertMode, KeyCategory::Input);
}

/// Builds and returns the full keymap with all scope bindings.
#[must_use]
#[rustfmt::skip]
#[expect(clippy::too_many_lines, reason = "exhaustive keymap bindings grow with each scope")]
pub fn init() -> Keymap<KeyEvent, Scope, Intent, KeyCategory> {
    let mut keymap = Keymap::new();

    keymap
        // Normal scope: navigation and commands
        .scope(Scope::Normal, |b| {
            b
            // General — app control
            .bind("q", Intent::Quit, KeyCategory::General)
            .bind("<c-c>", Intent::Quit, KeyCategory::General)
            .bind("?", Intent::ToggleWhichkey, KeyCategory::General)
            .bind("<c-p>", Intent::OpenPicker { kind: PickerKind::Keymap }, KeyCategory::General)
            .bind("<leader>sk", Intent::OpenPicker { kind: PickerKind::Keymap }, KeyCategory::General)
            .describe_group_with_category("<leader>s", "search", KeyCategory::General)
            .bind("<leader>sm", Intent::OpenPicker { kind: PickerKind::Provider }, KeyCategory::General)
            .bind("<leader>ss", Intent::OpenPicker { kind: PickerKind::Session }, KeyCategory::General)
            .bind("<leader>sp", Intent::OpenPicker { kind: PickerKind::Persona }, KeyCategory::General)
            .bind("<leader>st", Intent::OpenPicker { kind: PickerKind::Theme }, KeyCategory::General)
            // Input — enter input mode
            .bind("i", Intent::EnterInsertMode, KeyCategory::Input)
            .bind("<c-j>", Intent::EnterInsertMode, KeyCategory::Input)
            // Navigation — scrolling and tab switching
            .bind("k", Intent::ChatEntrySelectPrev, KeyCategory::Navigation)
            .bind("j", Intent::ChatEntrySelectNext, KeyCategory::Navigation)
            .bind("<c-u>", Intent::ScrollUp, KeyCategory::Navigation)
            .bind("<c-d>", Intent::ScrollDown, KeyCategory::Navigation)
            // Input — external editor
            .bind("<c-e>", Intent::EditInput, KeyCategory::Input)
            // g prefix — general commands and model management
            .describe_group_with_category("g", "general", KeyCategory::General)
            .describe_group_with_category("gm", "model", KeyCategory::Model)
            .describe_group_with_category("gc", "context", KeyCategory::Context)
            .bind("<leader>sf", Intent::OpenPicker { kind: PickerKind::SessionFork }, KeyCategory::General)
            .bind("<leader>sl", Intent::OpenPicker { kind: PickerKind::SessionLifecycle }, KeyCategory::General)
            .bind("gg", Intent::ScrollToTop, KeyCategory::Navigation)
            .bind("G", Intent::ScrollToBottom, KeyCategory::Navigation)
            .bind("gmr", Intent::RefreshModels, KeyCategory::Model)
            .bind("<leader>sc", Intent::OpenPicker { kind: PickerKind::ContextAssembly }, KeyCategory::General)
            .bind("gcr", Intent::RescanPromptTemplates, KeyCategory::Context)
            .bind("gb", Intent::TokenBudgetInputEnter, KeyCategory::Context)
            .bind("<c-l>", Intent::SidebarFocus, KeyCategory::Navigation)
            // Sidebar resize
            .bind("<c-w>", Intent::SidebarResizeEnter, KeyCategory::Navigation)
            // Pin selected entry
            .bind("p", Intent::ChatEntryPinSelected, KeyCategory::Context)
            // Expand/collapse tool entry
            .bind("e", Intent::ExpandToolEntry, KeyCategory::Navigation)
            // Escape: cancel selection
            .bind("<esc>", Intent::NormalEscape, KeyCategory::General);
        })
        // Sidebar — Persona section
        .scope(Scope::SidebarPersona, |b| {
            add_sidebar_base(b);
            b
            // Persona-specific actions
            .bind("c", Intent::SidebarPersonaEdit, KeyCategory::Context);
        })
        // Sidebar — Pins section
        .scope(Scope::SidebarPins, |b| {
            add_sidebar_base(b);
            b
            // Pin management actions
            .bind("u", Intent::PinsUnpin, KeyCategory::Context)
            .bind("t", Intent::PinsPinTop, KeyCategory::Context)
            .bind("b", Intent::PinsPinBottom, KeyCategory::Context)
            .bind("r", Intent::PinsPinRelative, KeyCategory::Context)
            .bind("m", Intent::PinsPinCycle, KeyCategory::Context);
        })
        // Sidebar — Sessions section
        .scope(Scope::SidebarSessions, |b| {
            add_sidebar_base(b);
            b
            // Session management actions
            .bind("x", Intent::SidebarSessionClose, KeyCategory::General)
            .bind("t", Intent::SidebarSessionTeardown, KeyCategory::General)
            .bind("<enter>", Intent::SidebarConfirm, KeyCategory::General)
            .bind("n", Intent::SessionNew, KeyCategory::General)
            .bind("N", Intent::SidebarSessionNewWithLifecycle, KeyCategory::General)
            .bind("r", Intent::SidebarRenameSession, KeyCategory::General);
        })
        // Input scope: typing into the input buffer
        .scope(Scope::Input, |b| {
            b.bind("<enter>", Intent::SubmitMessage, KeyCategory::Input)
            .bind("<s-enter>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<c-enter>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
            .bind("<c-k>", Intent::EnterNormalMode, KeyCategory::General)
            .bind("<c-c>", Intent::Interrupt { session_id: None }, KeyCategory::General)
            .bind("<c-e>", Intent::EditInput, KeyCategory::Input)
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
            .bind("<c-p>", Intent::OpenPicker { kind: PickerKind::Keymap }, KeyCategory::General)
            .bind("<c-l>", Intent::SidebarFocus, KeyCategory::Navigation)
            .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
            .catch_all(|key: KeyEvent| {
                if let Key::Char(c) = key.key {
                    Some(Intent::InsertChar { ch: c })
                } else {
                    None
                }
            });
        });

    keymap
        .scope(Scope::Picker, |b| {
            b.bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
            .bind("<enter>", Intent::PickerConfirm, KeyCategory::Model)
            .bind("<up>", Intent::PickerMoveUp, KeyCategory::Navigation)
            .bind("<down>", Intent::PickerMoveDown, KeyCategory::Navigation)
            .bind("<left>", Intent::PickerMoveCursorLeft, KeyCategory::Input)
            .bind("<right>", Intent::PickerMoveCursorRight, KeyCategory::Input)
            .bind("<backspace>", Intent::PickerBackspace, KeyCategory::Input)
            .bind("<c-r>", Intent::RefreshModels, KeyCategory::Model)
            .bind("<c-p>", Intent::OpenPicker { kind: PickerKind::Keymap }, KeyCategory::General)
            .bind("<c-a>", Intent::ToggleForkAssistantFilter, KeyCategory::General)
            .bind("<c-n>", Intent::SessionNew, KeyCategory::General)
            .bind("<c-u>", Intent::ToggleForkUserFilter, KeyCategory::General)
            .catch_all(|key: KeyEvent| {
                if let Key::Char(c) = key.key {
                    Some(Intent::PickerInsertChar { ch: c })
                } else {
                    None
                }
            });
        });

    // ArgInput scope — typing positional args for a lifecycle command.
    keymap.scope(Scope::ArgInput, |b| {
        b.bind("<esc>", Intent::EnterNormalMode, KeyCategory::General)
        .bind("<enter>", Intent::ArgInputConfirm, KeyCategory::Input)
        .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
        .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::InsertChar { ch: c })
            } else {
                None
            }
        });
    });

    // SidebarResize scope — adjusting sidebar width.
    keymap.scope(Scope::SidebarResize, |b| {
        b
        .bind("h", Intent::SidebarResizeExpand, KeyCategory::Navigation)
        .bind("l", Intent::SidebarResizeContract, KeyCategory::Navigation)
        .bind("<esc>", Intent::SidebarResizeLeave, KeyCategory::General)
        .bind("<c-c>", Intent::Quit, KeyCategory::General);
    });

    // TokenBudgetInput scope — typing a numeric budget value.
    keymap.scope(Scope::TokenBudgetInput, |b| {
        b
        .bind("<esc>", Intent::TokenBudgetInputLeave, KeyCategory::General)
        .bind("<enter>", Intent::TokenBudgetInputConfirm, KeyCategory::Input)
        .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
        .bind("0", Intent::InsertChar { ch: '0' }, KeyCategory::Input)
        .bind("1", Intent::InsertChar { ch: '1' }, KeyCategory::Input)
        .bind("2", Intent::InsertChar { ch: '2' }, KeyCategory::Input)
        .bind("3", Intent::InsertChar { ch: '3' }, KeyCategory::Input)
        .bind("4", Intent::InsertChar { ch: '4' }, KeyCategory::Input)
        .bind("5", Intent::InsertChar { ch: '5' }, KeyCategory::Input)
        .bind("6", Intent::InsertChar { ch: '6' }, KeyCategory::Input)
        .bind("7", Intent::InsertChar { ch: '7' }, KeyCategory::Input)
        .bind("8", Intent::InsertChar { ch: '8' }, KeyCategory::Input)
        .bind("9", Intent::InsertChar { ch: '9' }, KeyCategory::Input);
    });

    // SlidingWindowInput scope — typing a numeric window size.
    keymap.scope(Scope::SlidingWindowInput, |b| {
        b
        .bind("<esc>", Intent::SlidingWindowInputLeave, KeyCategory::General)
        .bind("<enter>", Intent::SlidingWindowInputConfirm, KeyCategory::Input)
        .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
        .bind("0", Intent::InsertChar { ch: '0' }, KeyCategory::Input)
        .bind("1", Intent::InsertChar { ch: '1' }, KeyCategory::Input)
        .bind("2", Intent::InsertChar { ch: '2' }, KeyCategory::Input)
        .bind("3", Intent::InsertChar { ch: '3' }, KeyCategory::Input)
        .bind("4", Intent::InsertChar { ch: '4' }, KeyCategory::Input)
        .bind("5", Intent::InsertChar { ch: '5' }, KeyCategory::Input)
        .bind("6", Intent::InsertChar { ch: '6' }, KeyCategory::Input)
        .bind("7", Intent::InsertChar { ch: '7' }, KeyCategory::Input)
        .bind("8", Intent::InsertChar { ch: '8' }, KeyCategory::Input)
        .bind("9", Intent::InsertChar { ch: '9' }, KeyCategory::Input);
    });

    // RenameSessionInput scope — editing a session title.
    keymap.scope(Scope::RenameSessionInput, |b| {
        b
        .bind("<esc>", Intent::RenameSessionLeave, KeyCategory::General)
        .bind("<enter>", Intent::RenameSessionConfirm, KeyCategory::Input)
        .bind("<left>", Intent::MoveCursorLeft, KeyCategory::Input)
        .bind("<right>", Intent::MoveCursorRight, KeyCategory::Input)
        .bind("<backspace>", Intent::DeleteGrapheme, KeyCategory::Input)
        .bind("<delete>", Intent::DeleteGraphemeForward, KeyCategory::Input)
        .bind("<c-j>", Intent::InsertChar { ch: '\n' }, KeyCategory::Input)
        .catch_all(|key: KeyEvent| {
            if let Key::Char(c) = key.key {
                Some(Intent::InsertChar { ch: c })
            } else {
                None
            }
        });
    });

    keymap.on_mouse(|mouse: event::MouseEvent, _scope: &Scope| {
        match mouse.kind {
            MouseEventKind::ScrollUp => Some(Intent::MouseScrollUp),
            MouseEventKind::ScrollDown => Some(Intent::MouseScrollDown),
            _ => None,
        }
    })
}

/// Collects all fully-resolved leaf bindings from the keymap for a given scope.
///
/// Walks the keymap tree recursively, collecting only leaf entries (no prefix-only
/// branch nodes). Each entry includes the full key sequence, description, scope,
/// category, and the command it triggers.
pub fn collect_bindings_for_scope(
    keymap: &Keymap<KeyEvent, Scope, Intent, KeyCategory>,
    scope: &Scope,
    theme: &Theme,
) -> Vec<nullslop_domain::KeymapEntry> {
    let mut entries = Vec::new();
    collect_leaf_bindings(keymap.bindings(), *scope, "", &mut entries, theme);
    entries
}

/// Collects fully-resolved leaf bindings from all scopes.
///
/// Iterates over all known scopes and collects entries from each one.
pub fn collect_all_bindings(
    keymap: &Keymap<KeyEvent, Scope, Intent, KeyCategory>,
    theme: &Theme,
) -> Vec<nullslop_domain::KeymapEntry> {
    let mut entries = Vec::new();
    for scope in &[
        Scope::Normal,
        Scope::SidebarPersona,
        Scope::SidebarPins,
        Scope::SidebarSessions,
        Scope::SidebarMinimap,
        Scope::Picker,
        Scope::Input,
        Scope::ArgInput,
        Scope::TokenBudgetInput,
        Scope::SidebarResize,
    ] {
        collect_leaf_bindings(keymap.bindings(), *scope, "", &mut entries, theme);
    }
    entries
}

/// Recursively walks the keybinding tree, collecting fully-resolved leaf entries.
///
/// Only `KeyNode::Leaf` entries are collected — prefix-only branch nodes like `g`
/// (which lead to sub-menus) are not included since they are not actionable.
/// Branch nodes that also have `leaf_entries` for the given scope are included
/// (those represent keys that are both a prefix and a terminal in different scopes).
fn collect_leaf_bindings(
    children: &[ratatui_which_key::KeyChild<KeyEvent, Scope, Intent, KeyCategory>],
    scope: Scope,
    prefix: &str,
    out: &mut Vec<nullslop_domain::KeymapEntry>,
    theme: &Theme,
) {
    for child in children {
        let key_display = WhichKeyKey::display(&child.key);
        let full_sequence = if prefix.is_empty() {
            key_display.clone()
        } else {
            format!("{prefix}{key_display}")
        };

        match &child.node {
            ratatui_which_key::KeyNode::Leaf(entries) => {
                for entry in entries {
                    if entry.scope == scope {
                        out.push(nullslop_domain::KeymapEntry {
                            key_sequence: full_sequence.clone(),
                            description: entry.description.clone(),
                            scope: entry.scope.to_string(),
                            category: entry.category.to_string(),
                            command: entry.action.clone(),
                            search_text: format!("{} {}", full_sequence, entry.description),
                            theme: theme.clone(),
                        });
                    }
                }
            }
            ratatui_which_key::KeyNode::Branch {
                children: branch_children,
                leaf_entries,
                category: branch_category,
                ..
            } => {
                // Collect leaf entries attached to this branch for the given scope.
                // These represent keys that act as both a prefix and a terminal action
                // in different scopes.
                for entry in leaf_entries {
                    if entry.scope == scope {
                        let cat = (*branch_category).unwrap_or(entry.category);
                        out.push(nullslop_domain::KeymapEntry {
                            key_sequence: full_sequence.clone(),
                            description: entry.description.clone(),
                            scope: entry.scope.to_string(),
                            category: cat.to_string(),
                            command: entry.action.clone(),
                            search_text: format!("{} {}", full_sequence, entry.description),
                            theme: theme.clone(),
                        });
                    }
                }

                // Recurse into children.
                collect_leaf_bindings(branch_children, scope, &full_sequence, out, theme);
            }
        }
    }
}
