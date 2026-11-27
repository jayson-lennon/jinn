//! The status bar's shared contracts: the context figure, the session
//! statistics, and the bar's transient hint cell.
//!
//! Pure data and pure functions only. The terminal status bar, the sidebar's
//! session preview, and the web UI's dash all read their figures from here,
//! so no two of them can disagree about what a session's numbers are.

pub mod context_figure;
pub mod session_stats;
pub mod status_bar_state;

pub use context_figure::*;
pub use session_stats::*;
pub use status_bar_state::*;
