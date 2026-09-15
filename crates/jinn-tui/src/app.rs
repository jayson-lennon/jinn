//! Main application state and per-frame rendering.

mod builder;
mod signals;

use std::mem;

use crossterm::event::{MouseButton, MouseEventKind};
use derive_more::Debug;
use jinn_domain::AppCore;
use jinn_domain::AppUiRegistry;
use jinn_domain::IntentHandler;
use jinn_domain::feat::ui::sidebar::Sidebar;
use jinn_domain::{FocusScope, Intent, PickerKind};
use ratatui::Frame;
use ratatui_which_key::{CrosstermKeymapExt as _, WhichKeyState};

use crate::config::TuiConfig;
use crate::msg::Msg;
use crate::render;
use crate::scope::Scope;
use crate::selection::{SelectableRects, SelectionState};
use crate::suspend::{Suspend, SuspendAction};
use crate::{AppStatus, MsgHandler};

pub use builder::TuiAppBuilder;

/// Type alias for the which-key state parameterized for jinn.
pub type WhichKeyInstance =
    WhichKeyState<jinn_domain::KeyEvent, Scope, Intent, crate::keymap::KeyCategory>;

/// Top-level application state and event loop.
#[derive(Debug)]
pub struct TuiApp {
    /// Application core (state, message channel).
    pub core: AppCore,
    /// Runtime services.
    #[debug(skip)]
    pub services: jinn_domain::Services,
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
    /// Capability for God-mode `State::write()` — held by the platform layer,
    /// passed to IntentHandler::handle and used for render-time frontend mutations.
    pub intent_handler_cap: jinn_domain::common::tcaps::IntentHandlerCap,
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
                    let session_id = {
                        let state = self.core.state.read();
                        state.session.active_session_id().clone()
                    };
                    let mut state = self.core.state.write(&self.intent_handler_cap);
                    state.session.clear_load();
                    let closure = jinn_domain::common::bridge::Bridge::publish_closure(
                        jinn_domain::feat::chat_input::protocol::command::PushChatEntry {
                            session_id,
                            entry: jinn_domain::ChatEntry::system(
                                "Failed to load session: timed out",
                            ),
                        },
                    );
                    let _ = self.core.bridge.send(closure);
                }
            }
            Msg::Input(event) => {
                // Sync scope from state before processing key.
                // This ensures the which-key scope matches the actual scope stack,
                // which is important when the initial scope differs from Normal
                // (e.g., app starts in Input mode).
                {
                    let state = self.core.state.read();
                    let scope = scope_for_focus(&state.frontend.scope());
                    drop(state);
                    self.which_key.set_scope(scope);
                }
                match event {
                    crossterm::event::Event::Key(key) => {
                        if key.kind != crossterm::event::KeyEventKind::Press {
                            return;
                        }
                        let Some(protocol_key) = crate::convert::from_crossterm(key) else {
                            tracing::debug!(
                                crossterm_code = ?key.code,
                                crossterm_mods = ?key.modifiers,
                                "key converted to None"
                            );
                            return;
                        };
                        tracing::trace!(
                            key = ?protocol_key.key,
                            mods = ?protocol_key.modifiers,
                            scope = ?self.which_key.scope(),
                            "key event received"
                        );

                        let intent_opt = self.which_key.handle_key(protocol_key);
                        let Some(intent) = intent_opt else {
                            return;
                        };

                        self.route_intent(intent);
                    }
                    crossterm::event::Event::Mouse(mouse) => {
                        // Selection handling - intercept before keymap
                        // (only when mouse capture is enabled).
                        if self.config.mouse_selection && self.handle_selection_mouse(mouse) {
                            return; // consumed by selection
                        }
                        // Fall through to keymap for scroll, etc.
                        let scope = self.which_key.scope().clone();
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
                        self.route_intent(jinn_domain::Intent::PasteText { text });
                    }
                    _ => {}
                }
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
        reason = "public entry point consumed inside via IntentHandler::handle"
    )]
    pub fn route_intent(&mut self, intent: Intent) {
        // Step 1-3: Handle intent, collect results, release lock.
        let (messages, signals) = {
            let mut state = self.core.state.write(&self.intent_handler_cap);

            let result = IntentHandler::handle(
                &intent,
                &mut state,
                &self.services.slices,
                &self.services.key_routes,
                &self.services.picker_registry,
            );

            // Cancel selection when mode changes away from Picker.
            if matches!(intent, Intent::EnterNormalMode | Intent::NormalEscape) {
                self.selection = mem::take(&mut self.selection).cancel();
            }

            // Collect signals before releasing lock.
            let signals = signals::TuiSignalsSnapshot::from_state(&state);
            (result.messages, signals)
        };

        // Send bus closures via bridge.
        for closure in messages {
            let _ = self.core.bridge.send(closure);
        }

        // Step 6: Handle TUI signals.
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
        if let Some(root) = signals.change_cwd_requested {
            let search_root = match root {
                jinn_domain::protocol::CwdRoot::Session => {
                    self.core.state.read().active_session().cwd().to_owned()
                }
                jinn_domain::protocol::CwdRoot::Home => dirs::home_dir().unwrap_or_default(),
            };
            self.suspend
                .request(SuspendAction::ChangeCwd { search_root });
        }
        if let Some(text) = signals.yank_text {
            std::thread::spawn(move || {
                let mut cb = match arboard::Clipboard::new() {
                    Ok(cb) => cb,
                    Err(e) => {
                        tracing::warn!(err = %e, "failed to create clipboard");
                        return;
                    }
                };
                if let Err(e) = cb.set_text(&text) {
                    tracing::warn!(err = %e, "failed to yank entry to clipboard");
                    return;
                }
                tracing::debug!(len = text.len(), "yanked entry to clipboard");
                std::thread::sleep(std::time::Duration::from_secs(2));
            });
        }

        // Step 6: Update scope based on new focus.
        let state_read = self.core.state.read();
        let new_scope = scope_for_focus(&state_read.frontend.scope());
        drop(state_read);
        self.which_key.set_scope(new_scope.clone());
    }

    /// Renders the application for a single frame.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        render::render(self, frame);
    }
}

/// Returns the keymap scope corresponding to the given focus scope.
pub fn scope_for_focus(focus: &jinn_domain::FocusScope) -> Scope {
    match focus {
        FocusScope::Picker { kind } => match kind {
            PickerKind::Provider => Scope::PickerProvider,
            PickerKind::Session => Scope::PickerSession,
            PickerKind::Persona => Scope::PickerPersona,
            PickerKind::Theme => Scope::PickerTheme,
            PickerKind::SessionLifecycle => Scope::PickerLifecycle,
            PickerKind::ReasoningEffort => Scope::PickerReasoningEffort,
            PickerKind::Endpoint => Scope::PickerEndpoint,
            PickerKind::Tool => Scope::PickerTool,
            PickerKind::Skill => Scope::PickerSkill,
            PickerKind::TaskList => Scope::PickerTaskList,
            PickerKind::Project => Scope::PickerProject,
            PickerKind::McpServer => Scope::PickerMcpServer,
            PickerKind::Plugin => Scope::PickerPlugin,
            // CompactionModel has no picker state (the kind is retired); it
            // is never pushed as a scope.
            PickerKind::CompactionModel => Scope::PickerProvider,
        },
        FocusScope::Input => Scope::Input,
        FocusScope::SidebarPersona => Scope::SidebarPersona,
        FocusScope::SidebarPins => Scope::SidebarPins,
        FocusScope::SidebarSessions => Scope::SidebarSessions,
        FocusScope::SidebarTaskList => Scope::SidebarTaskList,
        FocusScope::SidebarMcpServers => Scope::SidebarMcpServers,
        FocusScope::ArgInput => Scope::ArgInput,
        FocusScope::RenameSessionInput => Scope::RenameSessionInput,
        FocusScope::CwdInput => Scope::CwdInput,
        FocusScope::ProjectAddInput => Scope::ProjectAddInput,
        FocusScope::PrunerAccumulationInput => Scope::PrunerAccumulationInput,
        // Dynamic slice scopes pass their identity through unchanged.
        FocusScope::Dynamic(id) => Scope::Dynamic(id.clone()),
        FocusScope::SidebarResize => Scope::SidebarResize,

        FocusScope::Normal => Scope::Normal,
        FocusScope::TerminalView => Scope::TerminalView,
        FocusScope::TerminalControl => Scope::TerminalControl,
    }
}
