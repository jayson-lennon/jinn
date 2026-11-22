//! Commands that trigger session-init resource discovery.

use marked_path::{Absolute, MarkedPathBuf};

/// The scan cwd a legacy blob without one resolves to: the launch directory.
#[expect(
    clippy::expect_used,
    reason = "cwd and temp-dir fallback are absolute on supported platforms"
)]
fn default_cwd() -> MarkedPathBuf<Absolute> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| tempfile::env::temp_dir());
    MarkedPathBuf::<Absolute>::new(cwd).expect("cwd and temp dir are absolute")
}

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_slices::BusMessage;

/// Scan project context files (AGENTS.md/CLAUDE.md) for a specific session.
///
/// Carries the session's cwd: the worker walks the bounded ancestor chain
/// (stopping at an exclusive `$HOME` or inclusive VCS root, whichever comes
/// first), reads the first existing candidate per walked dir, and writes the
/// result into that session's ephemeral discovered-context-files set.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Scan project context files for a session.")]
pub struct ScanContextFiles {
    /// The session whose scan this is.
    pub session_id: SessionId,
    /// The working directory driving the scan.
    #[serde(default = "default_cwd")]
    pub cwd: MarkedPathBuf<Absolute>,
}

impl BusMessage for ScanContextFiles {}
