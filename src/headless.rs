//! Headless application state.
//!
//! [`HeadlessApp`] owns the processing pipeline for non-interactive mode.
//! It receives commands, submits them to the core, and shuts down gracefully.

use error_stack::{Report, ResultExt};
use jinn_domain::EnqueueUserMessage;
use jinn_domain::IntentHandler;
use jinn_domain::common::services::Services;
use jinn_domain::{AppCore, Bridge, ChatEntry};
use std::time::Duration;
use wherror::Error;

/// Error type for headless operations.
#[derive(Debug, Error)]
#[error(debug)]
pub struct HeadlessError;

/// Headless application state.
///
/// Owns an [`AppCore`] for non-interactive command processing.
/// Commands are submitted and results can be inspected after shutdown.
pub struct HeadlessApp {
    /// Application core (state, message channel).
    core: AppCore,
    /// Services container — holds the root supervisor for shutdown.
    services: Services,
    /// Capability for God-mode `State::write()`.
    intent_handler_cap: jinn_domain::common::tcaps::IntentHandlerCap,
}

impl HeadlessApp {
    /// Creates a new headless app with the given core and services.
    #[must_use]
    pub fn new(core: AppCore, services: Services) -> Self {
        let intent_handler_cap = jinn_domain::common::tcaps::mint::mint_intent_handler_cap();
        Self {
            core,
            services,
            intent_handler_cap,
        }
    }

    /// Returns a handle to the root supervisor actor ref.
    #[must_use]
    pub fn root_supervisor(&self) -> jinn_domain::common::root_supervisor::RootSupervisorRef {
        self.services.root_supervisor.clone()
    }

    /// Sends a chat message through the core pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error if the message cannot be sent.
    pub fn send_chat(&self, message: &str) -> Result<(), Report<HeadlessError>> {
        let session_id = {
            let state = self.core.state.read();
            state.session.active_session_id().clone()
        };
        self.core
            .bridge
            .send(Bridge::publish_closure(EnqueueUserMessage {
                session_id,
                entry: ChatEntry::user(message.to_string()),
            }))
            .change_context(HeadlessError)
            .attach("failed to send chat command")
    }

    /// Runs a keystroke script through the keymap → command → bus → component pipeline.
    ///
    /// Each non-empty, non-comment line read from `reader` is parsed as a key
    /// sequence by [`parse_script`]. Keys are fed to the which-key state machine,
    /// which resolves them to commands. Commands are submitted to `AppCore`.
    ///
    /// # Errors
    ///
    /// Returns an error if the script content cannot be read or a command cannot
    /// be sent.
    pub fn run_script<R>(&mut self, mut reader: R) -> Result<(), Report<HeadlessError>>
    where
        R: std::io::Read,
    {
        let mut which_key = {
            let keymap = jinn_tui::keymap::init();
            jinn_tui::app::WhichKeyInstance::new(keymap, jinn_tui::Scope::Normal)
        };

        let lines = {
            let leader = jinn_domain::KeyEvent {
                key: jinn_domain::Key::Char('\\'),
                modifiers: jinn_domain::Modifiers::none(),
            };
            let mut content = String::new();
            reader
                .read_to_string(&mut content)
                .change_context(HeadlessError)
                .attach("failed to read script content")?;

            parse_script(&content, &leader)
        };

        for keys in lines {
            for key in keys {
                let state_read = self.core.state.read();
                let scope =
                    jinn_tui::app::scope_for_focus(state_read.frontend.scope_stack.current());
                drop(state_read);
                which_key.set_scope(scope);

                if let Some(intent) = which_key.handle_key(key) {
                    // Process the intent through the IntentHandler.
                    let mut state = self.core.state.write(&self.intent_handler_cap);
                    let result = IntentHandler::handle(
                        &intent,
                        &mut state,
                        &self.services.slices,
                        &self.services.key_routes,
                        &self.services.picker_registry,
                    );
                    drop(state);

                    // Send resulting messages to bus.
                    for closure in result.messages {
                        let _ = self.core.bridge.send(closure);
                    }
                }
            }
        }

        Ok(())
    }

    /// Prints the chat history to the log for visibility.
    pub fn print_history(&self) {
        let state = self.core.state.read();
        for entry in state.active_session().history() {
            tracing::info!("{entry:?}");
        }
    }

    /// Shuts down the actor system gracefully.
    ///
    /// Signals the root supervisor to stop, cascading to all supervised child
    /// actors, then races the shutdown barrier against a 20-second timeout.
    pub fn shutdown(&self) {
        let root = self.services.root_supervisor.clone();
        let result = self.services.handle.block_on(async {
            let _ = root.stop_gracefully().await;
            tokio::time::timeout(Duration::from_secs(20), root.wait_for_shutdown()).await
        });
        if result.is_err() {
            tracing::warn!("headless actor shutdown timed out after 20s; proceeding");
        }
    }
}

/// Parses a script's content into a list of key sequences.
///
/// Each non-empty, non-comment line is parsed into a `Vec<KeyEvent>` via
/// [`ratatui_which_key::parse_key_sequence`]. Blank lines and lines starting
/// with `#` are skipped. Returns one `Vec<KeyEvent>` per non-skipped line.
pub fn parse_script(
    content: &str,
    leader: &jinn_domain::KeyEvent,
) -> Vec<Vec<jinn_domain::KeyEvent>> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| ratatui_which_key::parse_key_sequence(line, leader))
        .collect()
}

#[cfg(test)]
mod tests {
    use jinn_domain::{Key, KeyEvent, Modifiers};

    use super::*;

    fn leader() -> KeyEvent {
        KeyEvent {
            key: Key::Char('\\'),
            modifiers: Modifiers::none(),
        }
    }

    #[rstest::rstest]
    fn parse_script_single_key() {
        let result = parse_script("a", &leader());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 1);
    }

    #[rstest::rstest]
    fn parse_script_ignores_blank_lines() {
        let result = parse_script("a\n\nb", &leader());
        assert_eq!(result.len(), 2);
    }

    #[rstest::rstest]
    fn parse_script_ignores_comments() {
        let result = parse_script("a\n# comment\nb", &leader());
        assert_eq!(result.len(), 2);
    }
}
