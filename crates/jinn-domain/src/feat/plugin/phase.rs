//! Lifecycle state of one configured plugin's guest.
//!
//! The type survives the plugin-infrastructure teardown: the frontend's
//! plugin picker renders phases, and the dormant plugin state cache keeps
//! its shape for re-integration. Nothing currently publishes transitions —
//! the coordinator actor that did is gone.

use serde::{Deserialize, Serialize};

/// Lifecycle state of one configured plugin's guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginPhase {
    /// The guest task spawned but the handshake has not completed.
    Starting,
    /// The handshake completed (`Hello` seen, `Welcome` sent).
    Running,
    /// The guest ended (crash, trap, or shutdown) or never came up.
    Dead,
    /// The guest lives but is flooding: the inbound channel filled and
    /// messages were dropped. Cleared back to `Running` when the channel
    /// drains.
    Unresponsive,
    /// The guest completed its work and exited cleanly after the
    /// handshake (run-to-completion plugins like the loaders). The host
    /// keeps the plugin's contributions cached after the guest ends.
    Done,
}
