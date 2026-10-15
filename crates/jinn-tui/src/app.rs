//! Main application state and per-frame rendering.

mod builder;
mod signals;

use std::mem;

use crossterm::event::{MouseButton, MouseEventKind};
use derive_more::Debug;
use jinn_kernel::AppCore;
use jinn_kernel::AppUiRegistry;
use jinn_kernel::IntentHandler;
use jinn_kernel::{FocusScope, KernelIntent};
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
    WhichKeyState<jinn_kernel::KeyEvent, Scope, KernelIntent, crate::keymap::KeyCategory>;

/// Top-level application state and event loop.
#[derive(Debug)]
pub struct TuiApp {
    /// Application core (state, message channel).
    pub core: AppCore,
    /// Runtime services.
    #[debug(skip)]
    pub services: jinn_kernel::Services,
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
}

impl TuiApp {
    /// Create a test builder with sensible defaults.
    pub fn test_builder() -> builder::TuiAppBuilder {
        builder::TuiAppBuilder::default()
    }

    /// Processes a single message.
    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            // The tick no longer times out a session load. A large load is
            // now bounded by the chat log layout supervisor's own deadline,
            // which releases the loading indication and — unlike the watchdog
            // this replaces — writes nothing into the conversation, because a
            // measurement that could not be taken is not a conversation event.
            //
            // It does decay the cancel prompt. A prompt is armed by a
            // keystroke, but the work it refers to ends on its own; keystroke
            // dismissal cannot catch that, since it only runs when a key
            // arrives. The kernel owns the condition, and this is the tick
            // that notices — keeping the app layer out of the decision.
            Msg::Tick => jinn_kernel::feat::intent::cancel::disarm_stale_cancel_prompt(
                &mut self.core.state.write(),
            ),
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

                        let intent_opt = crate::keymap_gen::handle_key_with_shift_fallback(
                            &mut self.which_key,
                            protocol_key,
                        );
                        let Some(intent) = intent_opt else {
                            // The key fed the which-key sequence instead of
                            // resolving to an intent, so the handler never
                            // saw it and could not dismiss the prompt itself.
                            self.dismiss_prompt_for_unresolved_key();
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
                        let intent_opt = self
                            .which_key
                            .keymap()
                            .mouse_handler()
                            .and_then(|h| h(mouse, &scope));
                        let Some(intent) = intent_opt else {
                            // The event resolved to no intent — the same gap a
                            // key that feeds the which-key sequence leaves.
                            self.dismiss_prompt_for_unresolved_key();
                            return;
                        };
                        self.route_intent(intent);
                    }
                    crossterm::event::Event::Paste(text) => {
                        // A bracketed paste carries bytes and nothing else:
                        // which surface claims it is the kernel's call, made
                        // against the focused scope. The scope named here is
                        // a placeholder the kernel's paste branch replaces,
                        // so this platform layer names no slice.
                        self.route_intent(jinn_kernel::KernelIntent::Dynamic(
                            jinn_slices::DynamicIntent::with_bytes(
                                jinn_chat_input_msg::chat_input_scope(),
                                jinn_chat_input_msg::PASTE_TEXT_ACTION,
                                "paste text",
                                text.into_bytes(),
                            ),
                        ));
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

    /// Dismisses a confirmation prompt that a key consumed before it became
    /// an intent.
    ///
    /// Most keystrokes resolve to an intent, and [`IntentHandler::handle`]
    /// dismisses every armed prompt on its own. These mint none: opening a
    /// which-key sequence returns a branch, continuing one matches a nested
    /// branch, and a key that matches nothing pending closes the popup — all
    /// of which return `None`. The event loop returns on that `None`, so the
    /// handler never runs and a prompt armed beforehand would sit on screen
    /// advertising a confirmation the user has moved on from.
    ///
    /// Every one of those is a keypress, so this dismisses unconditionally.
    pub(super) fn dismiss_prompt_for_unresolved_key(&self) {
        self.core.state.write().frontend.cancel_stream_prompt = false;
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
    pub fn route_intent(&mut self, intent: KernelIntent) {
        // Step 1-3: Handle intent, collect results, release lock.
        let (messages, signals) = {
            let mut state = self.core.state.write();

            let result = IntentHandler::handle(
                &intent,
                &mut state,
                &self.services.slices,
                &self.services.key_routes,
                &self.services.config,
            );

            // Cancel selection when mode changes away from Picker.
            if matches!(intent, KernelIntent::NormalEscape) {
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
            let initial_content = self
                .core
                .state
                .read()
                .active_session()
                .with_input(|i| i.text().to_owned(), String::new);
            self.suspend.request(SuspendAction::Edit {
                initial_content,
                on_result: Box::new(|result| result),
            });
        }
        if let Some(root) = signals.change_cwd_requested {
            let search_root = match root {
                jinn_kernel::protocol::CwdRoot::Session => {
                    self.core.state.read().active_session().cwd().to_owned()
                }
                jinn_kernel::protocol::CwdRoot::Home => {
                    jinn_kernel::AppPaths::default().home_dir().to_path_buf()
                }
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
pub fn scope_for_focus(focus: &jinn_slices::FocusScope) -> Scope {
    match focus {
        // A `Picker` focus scope is a legacy name that no longer resolves:
        // every picker pushes a dynamic slice scope instead. Normal is the
        // honest fallback for a saved scope that predates the migration.
        // A saved scope predating the picker migration. Kept as an explicit
        // fallback rather than deleted: persisted sessions still name it.
        #[expect(
            clippy::match_same_arms,
            reason = "the Input arm below has the same body by design; see the comment"
        )]
        FocusScope::Picker { .. } => Scope::Normal,
        FocusScope::Input => Scope::Input,
        // Dynamic slice scopes pass their identity through unchanged.
        FocusScope::Dynamic(id) => Scope::Dynamic(id.clone()),

        FocusScope::Normal => Scope::Normal,
    }
}
