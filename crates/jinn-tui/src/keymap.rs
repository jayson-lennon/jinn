//! Keymap configuration and initialization.
//!
//! Defines the key categories and builds the keymap with all scope bindings.
//! Binds keys to [`Intent`] variants. Parameterized on
//! [`KeyEvent`] so the keymap works in both TUI and headless modes.

use crossterm::event::{self, MouseEventKind};
use derive_more::Display;
use jinn_kernel::KernelIntent;
use jinn_kernel::protocol::CwdRoot;
use jinn_kernel::{Key, KeyEvent};
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
#[must_use]
#[rustfmt::skip]
pub fn init() -> Keymap<KeyEvent, Scope, KernelIntent, KeyCategory> {
    let mut keymap = Keymap::new();

    keymap
        // Normal scope: navigation and commands
        .scope(Scope::Normal, |b| {
            b
            // General - app control
            .bind("q", KernelIntent::Quit, KeyCategory::General)
            .bind("<c-c>", KernelIntent::Quit, KeyCategory::General)
            .bind("?", KernelIntent::ToggleWhichkey, KeyCategory::General)
            .describe_group_with_category("<leader>s", "search", KeyCategory::General)
            // Every picker is slice-owned and declares its own opener row, so
            // no picker keybind belongs here. The rows in each slice's
            // `*_picker_routes.rs` carry the description text that which-key
            // shows; duplicating them here would shadow the slice's row
            // (dispatch is first-match-wins) and freeze the footer labels.
            // Input - enter input mode. The `i` and `<c-j>` keys for this
            // are the chat input slice's own row (bound in the `Normal`
            // static scope), not a kernel bind.
            // Navigation - tab switching. The chat log's own keys (cursor,
            // scroll, pin, fork, yank, jump chords) are route rows the log
            // slice attaches; they bind into this scope after these static
            // binds, so the keymap resolves them to `Intent::Dynamic`.
            .bind("<Tab>", KernelIntent::SwitchTab, KeyCategory::Navigation)
            // Change CWD - search from session CWD
            .bind("<M-c>", KernelIntent::ChangeCwd { root: CwdRoot::Session }, KeyCategory::Navigation)
            // Change CWD - search from home directory
            .bind("<M-d>", KernelIntent::ChangeCwd { root: CwdRoot::Home }, KeyCategory::Navigation)
            // g prefix - general commands and model management
            .describe_group_with_category("g", "general", KeyCategory::General)
            .describe_group_with_category("gm", "model", KeyCategory::Model)
            .describe_group_with_category("gc", "context", KeyCategory::Context)
            .describe_group_with_category("<leader>c", "change", KeyCategory::General)
            .bind("gmr", KernelIntent::RefreshModels, KeyCategory::Model)
            .bind("gcr", KernelIntent::RefreshProjectResources, KeyCategory::Context)
            // Jump chords the log binds as rows keep their group labels here;
            // the rows supply the leaves.
            .describe_group_with_category("]", "next", KeyCategory::ChatHistory)
            .describe_group_with_category("[", "previous", KeyCategory::ChatHistory)
            // Session creation
            .bind("n", KernelIntent::SessionNew, KeyCategory::General)
            // Escape: cancel selection
            .bind("<esc>", KernelIntent::NormalEscape, KeyCategory::General)
            // Unmapped character keys produce NoOp to dismiss confirmation prompts
            .catch_all(|key: KeyEvent| {
                if let Key::Char(_) = key.key {
                    Some(KernelIntent::NoOp)
                } else {
                    None
                }
            });
        })
        // Sidebar - Persona section
        
        // Sidebar - Pins section
        
        // Sidebar - Sessions section
        
        // Sidebar - Task list section
        
        // Sidebar - MCP servers section (read-only in Part 1: nav only).
        
        // Input scope: typing into the input buffer
        .scope(Scope::Input, |b| {
            // Chat input keys are the slice's own route rows, bound into
            // this scope by `bind_route_rows` after the static binds
            // (a later bind for the same key+scope replaces the earlier
            // one). Only the non-chat-input keys the box shares its scope
            // with are bound here.
            //
            // <c-c>: clear the box (or a picker filter, in its own scope).
            b.bind("<c-c>", KernelIntent::CtrlClear, KeyCategory::General)
            .bind("<c-e>", KernelIntent::EditInput, KeyCategory::Input)
            // Change CWD - search from session CWD
            .bind("<M-c>", KernelIntent::ChangeCwd { root: CwdRoot::Session }, KeyCategory::Navigation)
            // Change CWD - search from home directory
            .bind("<M-d>", KernelIntent::ChangeCwd { root: CwdRoot::Home }, KeyCategory::Navigation)
            .bind("<f1>", KernelIntent::ToggleWhichkey, KeyCategory::General);
        });

    // The chat input box's printable-character catch-all. It lives here,
    // not in `keymap_gen`, because it is part of the `Input` scope's
    // identity: any keymap that resolves typing needs it, including the
    // one a test builds to assert the scope's invariants.
    crate::keymap_gen::bind_chat_input_catch_all(&mut keymap);

    // No global bindings by design: globals survive every scope's catch-all
    // and would pierce slice capture-mode hooks (stranding the control flag
    // on User) and overlay views (popping overlays mid-composition). The
    // would-be globals are per-scope slice rows — see `keymap_gen`.
    keymap.on_mouse(|mouse: event::MouseEvent, _scope: &Scope| {
        match mouse.kind {
            MouseEventKind::ScrollUp => Some(KernelIntent::MouseScrollUp),
            MouseEventKind::ScrollDown => Some(KernelIntent::MouseScrollDown),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn the_kernel_defines_no_static_scope_for_the_reasoning_picker() {
        // Given the scope name a slice-owned picker would use.
        // The reasoning picker is slice-owned: its own scope is a
        // `Dynamic` scope its keys are attached to, not a static one the
        // kernel keymap enumerates. This guard pins the flip side of that
        // migration — the kernel must not have kept a static scope for it.
        let scope_name = "Picker(reasoning-effort)";

        // When parsing it as a static scope.
        let parsed = scope_name.parse::<Scope>();

        // Then parsing fails: the kernel defines no such static scope.
        assert!(
            parsed.is_err(),
            "the kernel must not define a static scope for the slice-owned \
             reasoning picker"
        );
    }
}

#[cfg(test)]
mod leak_check {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use crate::keymap::init;
    use crate::scope::Scope;

    /// Normal-mode <enter> opens the selected task call's subagent session.
    /// Also guards against accidental rebinding: nothing else may claim
    /// <enter> in the Normal scope.
    #[rstest::rstest]
    #[test]
    fn enter_in_normal_scope_fires_load_subagent_session() {
        use crate::app::WhichKeyInstance;
        use jinn_kernel::{Key, Modifiers};

        // Given the default keymap with a slice's route row bound: a row
        // that materializes in the Normal scope. The row is built here
        // rather than taken from a slice, because what is under test is
        // how a route row reaches a static scope, not what any one slice
        // attaches.
        let mut keymap = init();
        let routes = jinn_slices::route::KeyRoutes::new();
        routes.attach(jinn_slices::route::RouteRow {
            route_id: jinn_slices::route::RouteId::new("test:load-subagent"),
            scope: jinn_slices::slice_scope::SliceScopeId::new("test-slice", "sessions"),
            key: "<enter>",
            category: "general",
            site: jinn_slices::route::BindSite::StaticScopes(&["Normal"]),
            feature: "test",
            outcome: jinn_slices::route::RouteOutcome::Action {
                action: "load-subagent",
                display: "open subagent session",
                run: jinn_slices::route::ActionFn::new(|_| jinn_slices::RouteResult::empty()),
            },
        });
        crate::keymap_gen::bind_route_rows(&routes, &mut keymap);
        let mut wk = WhichKeyInstance::new(keymap, Scope::Normal);

        // When pressing <enter>.
        let enter = jinn_kernel::KeyEvent {
            key: Key::Enter,
            modifiers: Modifiers::none(),
        };
        let intent = wk.handle_key(enter);

        // Then it resolves to the row's load-subagent dynamic action.
        assert!(
            matches!(
            intent,
            Some(jinn_kernel::KernelIntent::Dynamic(ref dynamic))
                if dynamic.action == "load-subagent"
            ),
            "enter must open the subagent session for the selected task; got {intent:?}",
        );
    }
}
