//! Sessions sidebar section - listing, navigation, and session lifecycle actions.
//!
//! This module groups all concerns related to the sessions list in the sidebar:
//! rendering, cursor navigation, session activation, close/archive/teardown
//! handlers, and the lifecycle picker entry point.

pub mod activate;
pub mod archive;
pub mod r#continue;
pub mod load_subagent;
pub mod navigate;
pub mod preview;
pub mod reconcile;
pub mod render;

pub mod state;
pub mod teardown;

#[cfg(test)]
mod preview_tests;

use std::time::Duration;

use jinn_domain::common::app_state::AppState;

// ---------------------------------------------------------------------------
// Re-exports - preserve the public API for external consumers.
// ---------------------------------------------------------------------------

pub use activate::{handle_session_activate, handle_session_activate_insert};
pub use archive::handle_session_archive;
pub use r#continue::handle_session_continue;
pub use jinn_domain::feat::session::sessions_list::archive_tree::{
    handle_session_tree_action_arm, handle_session_tree_action_confirm,
};
pub use jinn_domain::feat::session::sessions_list::close::{
    SessionCloseError, handle_session_close, handle_session_close_with_lifecycle,
    validate_session_close,
};
pub use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};
pub use load_subagent::{
    LoadSubagentError, handle_load_subagent_session, validate_load_subagent_session,
};

pub use navigate::{navigate, receive_cursor, scroll_to_cursor, scroll_to_cursor_split};
pub use preview::{
    render_session_preview, render_session_preview_for_state, session_preview_popup_rect,
    sessions_section_content_height,
};
pub use reconcile::{reconcile_after_session_removal, reconcile_split};
pub use render::SessionsSection;
pub use render::render_archive_tree_prompt_for_state;
pub use render::render_close_session_prompt_for_state;
pub use state::SessionsSectionState;
#[allow(
    unused_imports,
    reason = "re-exported section API; used by kernel callers via facade paths"
)]
pub(crate) use state::sorted_open_sessions;
pub use state::{clear_visual_parents_on_load, clear_visual_parents_on_load_split};
pub use state::{update_visual_parents_on_removal, update_visual_parents_on_removal_split};
pub use teardown::handle_session_teardown;

// ---------------------------------------------------------------------------
// Constants - shared across submodules.
// ---------------------------------------------------------------------------

/// Active session indicator prefix.
pub(crate) const ACTIVE_PREFIX: &str = "▸ ";
/// Inactive session prefix (two spaces to align with `ACTIVE_PREFIX`).
pub(crate) const INACTIVE_PREFIX: &str = "  ";
/// Maximum number of session entries visible at once.
pub const MAX_VISIBLE_SESSIONS: usize = 15;
/// Minimum time between animation frame advances.
pub(crate) const ANIMATION_INTERVAL: Duration = Duration::from_millis(80);

/// Handles the close key (`x`) in the sessions section.
///
/// First press arms the confirmation prompt; the press that arrives
/// while the prompt is showing re-validates and emits `CloseSession`.
/// Any other intent dismisses the prompt (the kernel's dismiss guard).
pub fn handle_session_close_arm(state: &mut AppState) -> jinn_domain::protocol::IntentResult {
    if state.frontend.close_session_prompt {
        // Second x press — perform the close (re-validates in case the
        // session became busy between taps).
        state.frontend.close_session_prompt = false;
        return handle_session_close_with_lifecycle(state);
    }
    // First press - show confirmation prompt.
    // The interceptor (the kernel's dismiss guard + this arm) handles the second press.
    state.frontend.close_session_prompt = true;
    jinn_domain::protocol::IntentResult::empty()
}
