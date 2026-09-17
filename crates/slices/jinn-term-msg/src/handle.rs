//! The kernel-facing handle to the term coordinator actor.
//!
//! The kernel (tools_actor) asks for spawns, input sends, and kills
//! through this trait; the term slice crate mints the implementation
//! at spawn and stores it in `Services`. The actor type itself stays
//! private to the slice.

use jinn_core_types::SessionId;

use crate::command::{KillTermOutcome, SendTermOutcome, SpawnTermOutcome};

/// The error surface of a failed term ask (mailbox closed, timed out).
#[derive(Debug, Clone, wherror::Error)]
#[error(debug)]
pub struct TermAskError;

/// Kernel-facing term coordinator operations.
#[async_trait::async_trait]
pub trait TermHandle: Send + Sync {
    /// Spawns (or replaces) the chat session's terminal.
    async fn spawn_term(
        &self,
        chat_session_id: SessionId,
        command: String,
        cwd: std::path::PathBuf,
        size: (u16, u16),
        max_wait: std::time::Duration,
    ) -> Result<SpawnTermOutcome, TermAskError>;

    /// Sends input to the chat session's terminal and waits for settle.
    async fn send_input(
        &self,
        chat_session_id: SessionId,
        text: Option<String>,
        keys: Vec<String>,
        enter: bool,
        max_wait: std::time::Duration,
    ) -> Result<SendTermOutcome, TermAskError>;

    /// Kills the chat session's terminal (idempotent).
    async fn kill_term(&self, chat_session_id: SessionId) -> Result<KillTermOutcome, TermAskError>;

    /// The handle's debug name.
    fn name(&self) -> &'static str;
}
