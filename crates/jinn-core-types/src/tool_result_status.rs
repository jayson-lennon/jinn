//! Execution status for tool result entries.

use serde::{Deserialize, Serialize};

/// Execution status of a tool result entry.
///
/// Controls the background color of the rendered entry and whether
/// content is still growing (streaming).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolResultStatus {
    /// Tool is still executing - content may grow incrementally.
    Pending,
    /// Tool completed successfully.
    Success,
    /// Tool failed.
    Failure,
}
