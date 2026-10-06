//! Archive-tree validation, prompting, and command flow.

use std::collections::HashMap;

use jinn_core_types::SessionId;
use jinn_kernel::common::app_state::AppState;
use jinn_kernel::protocol::IntentResult;
use jinn_session_lifecycle_msg::TeardownSessionTree;
use jinn_session_store_msg::ArchiveSessionTree;
pub use jinn_sidebar_msg::{ArchiveTreePrompt, TreePromptAction};

use jinn_session_list::descendant_closure;

use super::state::mark_in_flight;

/// Why an archive-tree request can be rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveTreeError {
    /// The sessions section is not focused.
    WrongSection,
    /// No session is selected, or the one selected is not loaded.
    NoSelection,
    /// At least one member of the subtree is busy.
    SubtreeBusy,
}

/// Resolves the selected session's descendants in breadth-first order.
///
/// The walk reads parent links rather than the rows the sidebar draws, so the
/// scope is the session's own subtree and does not change with what happens to
/// be listed. Only loaded sessions are linked here; the actors that carry out
/// the disposal re-derive their closure from the store as well, which is
/// authoritative — this count is what the prompt shows the user, not what is
/// disposed of.
///
/// # Errors
///
/// Returns [`ArchiveTreeError`] when the selection is invalid or any member
/// of the subtree is busy.
pub fn archive_tree_members(state: &AppState) -> Result<Vec<SessionId>, ArchiveTreeError> {
    if !matches!(
        state.frontend.sidebar_section(),
        Some(jinn_sidebar_msg::SidebarSectionId::Sessions)
    ) {
        return Err(ArchiveTreeError::WrongSection);
    }
    let root = state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
        .ok_or(ArchiveTreeError::NoSelection)?;
    if !state.session.contains(&root) {
        return Err(ArchiveTreeError::NoSelection);
    }
    let members = descendant_closure(&root, &parent_links(state));
    if !members.iter().all(|id| {
        state
            .session
            .get(id)
            .is_some_and(|session| matches!(session.phase(), jinn_session_msg::PhaseKind::Idle))
    }) {
        return Err(ArchiveTreeError::SubtreeBusy);
    }
    Ok(members)
}

/// Every loaded session's parent link, the input the subtree walk reads.
///
/// Over all loaded sessions rather than the rows the sidebar happens to draw,
/// so the closure is the session's own and not the display's. A root whose
/// descendants are not all loaded still walks correctly; the ones that are
/// not loaded are the store's business, and the actors that dispose of the
/// tree re-derive their closure from the store's summaries.
fn parent_links(state: &AppState) -> HashMap<SessionId, Option<SessionId>> {
    state
        .session
        .iter()
        .map(|(id, session)| (id.clone(), session.parent_session().clone()))
        .collect()
}

/// Arms the prompt on first press, or revalidates and emits the command on
/// the matching second press. Any other tree key dismisses an armed prompt.
pub fn handle_session_tree_action_arm(
    state: &mut AppState,
    action: TreePromptAction,
) -> IntentResult {
    match state.frontend.archive_tree_prompt.as_ref() {
        Some(ArchiveTreePrompt::Confirm { action: armed, .. }) if *armed == action => {
            state.frontend.archive_tree_prompt = None;
            match archive_tree_members(state) {
                Ok(members) => emit_tree_command(state, action, &members),
                Err(ArchiveTreeError::SubtreeBusy) => {
                    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);
                    IntentResult::empty()
                }
                Err(_) => IntentResult::empty(),
            }
        }
        Some(ArchiveTreePrompt::Confirm { .. }) => {
            state.frontend.archive_tree_prompt = None;
            match archive_tree_members(state) {
                Ok(members) => {
                    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Confirm {
                        count: members.len(),
                        action,
                    });
                    IntentResult::empty()
                }
                Err(ArchiveTreeError::SubtreeBusy) => {
                    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);
                    IntentResult::empty()
                }
                Err(_) => IntentResult::empty(),
            }
        }
        Some(ArchiveTreePrompt::Busy) => {
            state.frontend.archive_tree_prompt = None;
            match archive_tree_members(state) {
                Ok(members) => emit_tree_command(state, action, &members),
                Err(ArchiveTreeError::SubtreeBusy) => {
                    state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);
                    IntentResult::empty()
                }
                Err(_) => IntentResult::empty(),
            }
        }
        None => match archive_tree_members(state) {
            Ok(members) => {
                state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Confirm {
                    count: members.len(),
                    action,
                });
                IntentResult::empty()
            }
            Err(ArchiveTreeError::SubtreeBusy) => {
                state.frontend.archive_tree_prompt = Some(ArchiveTreePrompt::Busy);
                IntentResult::empty()
            }
            Err(_) => IntentResult::empty(),
        },
    }
}

/// Emits a previously validated tree command.
///
/// `members` is the closure the prompt was confirmed against, so the tint
/// covers every member and not just the root.
pub fn handle_session_tree_action_confirm(
    state: &mut AppState,
    action: TreePromptAction,
    members: &[SessionId],
) -> IntentResult {
    state.frontend.archive_tree_prompt = None;
    emit_tree_command(state, action, members)
}

/// Marks every member in flight, then builds the tree command for `action`.
///
/// Taking the members here means no dispatch path can emit a command without
/// also tinting the sessions it disposes of.
fn emit_tree_command(
    state: &AppState,
    action: TreePromptAction,
    members: &[SessionId],
) -> IntentResult {
    mark_in_flight(state, members);
    command_for(action, members[0].clone())
}

fn command_for(action: TreePromptAction, root: SessionId) -> IntentResult {
    match action {
        TreePromptAction::Archive => IntentResult::new_message(ArchiveSessionTree { root }),
        TreePromptAction::TeardownAndArchive => {
            IntentResult::new_message(TeardownSessionTree { root })
        }
    }
}
