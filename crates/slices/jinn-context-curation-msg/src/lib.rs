//! Context-curation crossing contracts.
//!
//! The prune side has no crossing command — prune strategies trigger on
//! `HistoryAppended` (forwarded from `jinn-session-history-msg`) and
//! publish `SubmitHistoryMutations` (also `jinn-session-history-msg`).

pub use jinn_core_types::SessionId;

pub mod prune_report;

pub use prune_report::{PruneReport, prune_report};
