//! Auto-prune strategies — pluggable [`HistoryWorker`] heuristics that
//! identify stale or redundant entries and produce `ForcedExclude`
//! mutations. Each strategy is config-gated at slice activation; see the
//! kernel docs' `[auto_prune.*]` schema for the per-strategy config.
//!
//! [`HistoryWorker`]: crate::worker::HistoryWorker

pub mod anchored_assistant;
pub mod broken_edit;
pub mod consecutive_reads;
pub mod double_edit;
pub mod edit_read;
pub mod min_age;
pub mod read_edit;
pub mod regex;
pub mod todo_prune;
pub mod tool_age_window;
pub mod trivial_assistant;

pub(crate) use min_age::is_within_min_age;

pub use anchored_assistant::AnchoredAssistantAutoPruneWorker;
pub use broken_edit::BrokenEditAutoPruneWorker;
pub use consecutive_reads::ConsecutiveReadsAutoPruneWorker;
pub use double_edit::DoubleEditAutoPruneWorker;
pub use edit_read::EditReadAutoPruneWorker;
pub use read_edit::ReadEditAutoPruneWorker;
pub use regex::RegexAutoPruneWorker;
pub use todo_prune::TodoAutoPruneWorker;
pub use tool_age_window::ToolAgeWindowAutoPruneWorker;
pub use trivial_assistant::TrivialAssistantAutoPruneWorker;
