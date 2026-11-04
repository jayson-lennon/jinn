//! Sessions sidebar section - listing, navigation, and session lifecycle actions.
//!
//! This module groups all concerns related to the sessions list in the sidebar:
//! rendering, cursor navigation, session activation, close/archive/teardown
//! handlers, and the lifecycle picker entry point.

pub mod activate;
pub mod archive;
pub mod archive_tree;
pub mod bot_actions;
pub mod bot_properties;
pub mod close;
pub mod r#continue;
pub mod load_subagent;
pub mod navigate;
pub mod preview;
pub mod preview_load;
pub mod reconcile;
pub mod render;

pub mod setup;
pub mod state;
pub mod teardown;
pub mod yank;

#[cfg(test)]
mod bot_actions_tests;
#[cfg(test)]
mod preview_tests;

// ---------------------------------------------------------------------------
// Re-exports - preserve the public API for external consumers.
// ---------------------------------------------------------------------------

pub use activate::{handle_session_activate, handle_session_activate_insert};
pub use archive::handle_session_archive;
pub use archive_tree::{
    ArchiveTreeError, handle_session_tree_action_arm, handle_session_tree_action_confirm,
};
pub use close::{
    SessionCloseError, handle_session_close_arm, handle_session_close_with_lifecycle,
    validate_session_close,
};
pub use r#continue::handle_session_continue;
pub use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};
pub use load_subagent::{
    LoadSubagentError, handle_load_subagent_session, validate_load_subagent_session,
};

pub use navigate::{navigate, receive_cursor};
pub use preview::{
    render_session_preview, render_session_preview_for_state, session_preview_popup_rect,
};
pub use reconcile::{reconcile_after_session_removal, reconcile_split};
pub use render::SessionsSection;
pub use render::render_archive_tree_prompt_for_state;
pub use render::render_close_session_prompt_for_state;
pub use setup::handle_session_rerun_setup;
pub use state::SessionsSectionState;
#[allow(
    unused_imports,
    reason = "re-exported section API; used by kernel callers via facade paths"
)]
pub(crate) use state::sorted_open_sessions;
pub use state::{clear_visual_parents_on_load, clear_visual_parents_on_load_split};
pub use state::{update_visual_parents_on_removal, update_visual_parents_on_removal_split};
pub use teardown::handle_session_teardown;
pub use yank::{SessionYankError, handle_session_yank_id, validate_session_yank_id};

// ---------------------------------------------------------------------------
// Constants - shared across submodules.
// ---------------------------------------------------------------------------

/// Active session indicator prefix.
pub(crate) const ACTIVE_PREFIX: &str = "▸ ";
/// Inactive session prefix (two spaces to align with `ACTIVE_PREFIX`).
pub(crate) const INACTIVE_PREFIX: &str = "  ";
