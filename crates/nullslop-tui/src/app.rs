//! Main application state and per-frame rendering.

mod builder;
mod signals;

use std::mem;

use crossterm::event::{MouseButton, MouseEventKind};
use derive_more::Debug;
use nullslop_domain::ActorHostService;
use nullslop_domain::AppUiRegistry;
use nullslop_domain::IntentHandler;
use nullslop_domain::feat::ui::sidebar::Sidebar;
use nullslop_domain::{AppCore, AppMsg};
use nullslop_domain::{FocusScope, Intent, PickerKind};
use ratatui::Frame;
use ratatui_which_key::{CrosstermKeymapExt as _, WhichKeyState};

use crate::config::TuiConfig;
use crate::keymap;
use crate::msg::Msg;
use crate::render;
use crate::scope::Scope;
use crate::selection::{SelectableRects, SelectionState};
use crate::suspend::{Suspend, SuspendAction};
use crate::{AppStatus, MsgHandler};

pub use builder::TuiAppBuilder;

/// Type alias for the which-key state parameterized for nullslop.
pub type WhichKeyInstance =
    WhichKeyState<nullslop_domain::KeyEvent, Scope, Intent, crate::keymap::KeyCategory>;

/// Top-level application state and event loop.
#[derive(Debug)]
pub struct TuiApp {
    /// Application core (state, message channel).
    pub core: AppCore,
    /// Runtime services.
    pub services: nullslop_domain::Services,
    /// Actor host for coordinated shutdown.
    pub actor_host: ActorHostService,
    /// UI element registry.
    pub ui_registry: AppUiRegistry,
    /// Message channel for the event loop.
    pub events: MsgHandler,
    /// Which-key keybinding system state.
    #[debug(skip)]
    pub which_key: WhichKeyInstance,
    /// Deferred suspend action queue (e.g., for external editor).
    pub suspend: Suspend,
    /// Background event thread. Set by [`run`](crate::run::run).
    #[debug(skip)]
    pub event_thread: Option<crate::msg::handler::EventThreadGuard>,
    /// Current application lifecycle status.
    pub status: AppStatus,
    /// Mouse text selection state.
    pub selection: SelectionState,
    /// Selectable screen regions, rebuilt each frame during rendering.
    pub selectable_rects: SelectableRects,
    /// Set to `true` when a selection is finalized and the selected text
    /// should be copied to the system clipboard during the next render.
    pub pending_clipboard: bool,
    /// TUI configuration (mouse capture, etc.).
    pub config: TuiConfig,
    /// Sidebar container with registered sections.
    pub sidebar: Sidebar,
}

impl TuiApp {
    /// Create a test builder with sensible defaults.
    pub fn test_builder() -> builder::TuiAppBuilder {
        builder::TuiAppBuilder::default()
    }

    /// Processes a single message.
    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => {
                let load_started = {
                    let state = self.core.state.read();
                    state.session.session_load_guard().map(|g| g.started_at)
                };
                if let Some(started) = load_started
                    && started.elapsed() >= std::time::Duration::from_secs(10)
                {
                    let mut state = self.core.state.write();
                    state.session.clear_load();
                    state
                        .active_session_mut()
                        .push_entry(nullslop_domain::ChatEntry::system(
                            "Failed to load session: timed out",
                        ));
                }
                // Lazy cleanup of expired status notifications.
                self.core
                    .state
                    .write()
                    .frontend
                    .clear_expired_notification();
            }
            Msg::Input(event) => {
                match event {
                    crossterm::event::Event::Key(key) => {
                        if key.kind != crossterm::event::KeyEventKind::Press {
                            return;
                        }
                        let Some(protocol_key) = crate::convert::from_crossterm(key) else {
                            tracing::info!(
                                crossterm_code = ?key.code,
                                crossterm_mods = ?key.modifiers,
                                "key converted to None"
                            );
                            return;
                        };
                        tracing::info!(
                            key = ?protocol_key.key,
                            mods = ?protocol_key.modifiers,
                            scope = ?self.which_key.scope(),
                            "key event received"
                        );
                        let Some(intent) = self.which_key.handle_key(protocol_key) else {
                            return;
                        };
                        self.route_intent(intent);
                    }
                    crossterm::event::Event::Mouse(mouse) => {
                        // Selection handling — intercept before keymap
                        // (only when mouse capture is enabled).
                        if self.config.mouse_selection && self.handle_selection_mouse(mouse) {
                            return; // consumed by selection
                        }
                        // Fall through to keymap for scroll, etc.
                        let scope = *self.which_key.scope();
                        let Some(intent) = self
                            .which_key
                            .keymap()
                            .mouse_handler()
                            .and_then(|h| h(mouse, &scope))
                        else {
                            return;
                        };
                        self.route_intent(intent);
                    }
                    crossterm::event::Event::Paste(text) => {
                        self.route_intent(nullslop_domain::Intent::PasteText { text });
                    }
                    _ => {}
                }
            }
            Msg::Command(cmd) => {
                let _ = self.core.sender().send(AppMsg::Command {
                    command: cmd,
                    source: None,
                });
            }
        }
    }

    /// Handles mouse events for text selection. Returns `true` if the event was consumed.
    fn handle_selection_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(bounds) = self
                    .selectable_rects
                    .find_for_position(mouse.column, mouse.row)
                {
                    self.selection = SelectionState::start_drag(mouse.column, mouse.row, bounds);
                    return true;
                }
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_active() {
                    self.selection =
                        mem::take(&mut self.selection).update_focus(mouse.column, mouse.row);
                    return true;
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.selection.is_active() {
                    self.selection = mem::take(&mut self.selection).finalize();
                    self.pending_clipboard = true;
                    return true;
                }
                false
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if self.selection.is_active() {
                    self.selection = mem::take(&mut self.selection).cancel();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Routes an intent through the [`IntentHandler`] and handles TUI signals.
    ///
    /// 1. Acquires the state write lock and calls [`IntentHandler::handle`].
    /// 2. Collects TUI signals, commands, and mode from the result.
    /// 3. Drops the write lock.
    /// 4. Sends commands to the core channel.
    /// 5. Handles TUI signals (which-key toggle, editor, pinned pane, etc.).
    /// 6. Updates the keymap scope based on the new mode.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Intent is consumed by intent routing, ownership is semantic"
    )]
    pub fn route_intent(&mut self, intent: Intent) {
        // Step 1–3: Handle intent, collect results, release lock.
        let (commands, signals) = {
            let mut state = self.core.state.write();
            let result = IntentHandler::handle(&intent, &mut state);

            // Populate keymap picker entries if opening the keymap picker.
            if matches!(
                intent,
                Intent::OpenPicker {
                    kind: PickerKind::Keymap
                }
            ) {
                let scope = *self.which_key.scope();
                let intent_entries = if state.frontend.keymap_picker_show_all {
                    keymap::collect_all_bindings(self.which_key.keymap(), &state.frontend.theme)
                } else {
                    keymap::collect_bindings_for_scope(
                        self.which_key.keymap(),
                        &scope,
                        &state.frontend.theme,
                    )
                };
                // Entries now carry Intent directly — store them in AppState.
                state.frontend.keymap_picker.set_items(intent_entries);
                // Also populate all_keymap_entries for scope toggle.
                state.frontend.all_keymap_entries =
                    keymap::collect_all_bindings(self.which_key.keymap(), &state.frontend.theme);
            }

            // Cancel selection when mode changes away from Picker.
            if matches!(intent, Intent::EnterNormalMode | Intent::NormalEscape) {
                self.selection = mem::take(&mut self.selection).cancel();
            }

            // Collect signals before releasing lock.
            let signals = signals::TuiSignalsSnapshot::from_state(&state);
            let commands = result.commands;

            (commands, signals)
        };

        // Step 4: Send commands to core channel.
        for cmd in commands {
            let _ = self.core.sender().send(AppMsg::Command {
                command: cmd,
                source: None,
            });
        }

        // Step 5: Handle TUI signals.
        if signals.toggle_whichkey {
            self.which_key.toggle();
        }
        if signals.edit_requested {
            let initial_content = self.core.state.read().active_chat_input().text().to_owned();
            self.suspend.request(SuspendAction::Edit {
                initial_content,
                on_result: Box::new(|result| result),
            });
        }

        // Step 6: Update scope based on new focus.
        let state_read = self.core.state.read();
        let new_scope = scope_for_focus(state_read.frontend.scope_stack.current());
        drop(state_read);
        self.which_key.set_scope(new_scope);
    }

    /// Renders the application for a single frame.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        render::render(self, frame);
    }
}

/// Returns the keymap scope corresponding to the given focus scope.
pub fn scope_for_focus(focus: &nullslop_domain::FocusScope) -> Scope {
    match focus {
        FocusScope::Picker { .. } => Scope::Picker,
        FocusScope::Input => Scope::Input,
        FocusScope::SidebarPersona => Scope::SidebarPersona,
        FocusScope::SidebarPins => Scope::SidebarPins,
        FocusScope::SidebarSessions => Scope::SidebarSessions,
        FocusScope::SidebarMinimap => Scope::SidebarMinimap,
        FocusScope::ArgInput => Scope::ArgInput,
        FocusScope::TokenBudgetInput => Scope::TokenBudgetInput,
        FocusScope::SlidingWindowInput => Scope::SlidingWindowInput,
        FocusScope::RenameSessionInput => Scope::RenameSessionInput,
        FocusScope::SidebarResize => Scope::SidebarResize,
        FocusScope::Normal => Scope::Normal,
    }
}
