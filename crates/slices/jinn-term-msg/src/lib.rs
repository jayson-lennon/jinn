//! Interactive-term crossing vocabulary: the coordinator's request and
//! event contracts, the per-session terminal tab state, the takeover
//! control registry, the overlay geometry, and settle-wait defaults.
//!
//! Pure data + pure functions — the PTY machinery and the vt100
//! emulator live in the term slice crate; the kernel (tools, intent
//! rows) and jinn-tui speak only the types in this crate.

pub mod cells;
pub mod command;
pub mod event;
pub mod geometry;
pub mod handle;
pub mod overlay_facts;
pub mod prefs;
pub mod scope;
pub mod settle;
pub mod tab_state;
pub mod takeover;

pub use cells::*;
pub use command::*;
pub use event::*;
pub use geometry::*;
pub use handle::*;
pub use overlay_facts::*;
pub use prefs::*;
pub use scope::*;
pub use settle::*;
pub use tab_state::*;
pub use takeover::*;

/// Captured exit info from a terminated terminal child.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExitInfo {
    /// The process exit code (0 on success; signal deaths report 1 plus a
    /// signal name).
    pub code: u32,
    /// Signal name if the process was killed by a signal (e.g. `"Terminated"`).
    pub signal: Option<String>,
}

impl ExitInfo {
    /// One-line human summary, e.g. `exited with code 1` or `killed by SIGTERM`.
    #[must_use]
    pub fn summary(&self) -> String {
        match &self.signal {
            Some(signal) => format!("killed by {signal}"),
            None => format!("exited with code {}", self.code),
        }
    }
}
