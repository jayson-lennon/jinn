// Copyright (C) 2026 Jayson Lennon
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Confirmation-prompt dismissal — clearing an armed prompt on unrelated input.
//!
//! A prompt is armed by one action and confirmed by another. If the user
//! presses something else in between, the prompt must not survive that
//! keystroke: a prompt still on screen advertises a confirmation the user never
//! made, and the next action would land on a state the user did not choose.
//!
//! This runs as the first statement of the dispatch path, ahead of every route
//! row and slice hook, both of which return early. The sidebar route actions
//! that arm and confirm a prompt are exempt: they manage their own prompt.
//!
//! Split out of [`super::handler`] because it is a cross-cutting invariant that
//! every dispatch path depends on, not part of any one intent.

use crate::AppState;
use crate::protocol::KernelIntent;

/// Dismisses armed confirmation prompts when an unrelated action arrives.
///
/// Runs as the first statement of [`IntentHandler::handle_inner`], ahead of
/// every dispatch path — including the slice route rows and the slice input
/// hooks, which both return early. A prompt cleared here cannot survive a
/// keystroke, which is the whole point: a prompt still on screen advertises a
/// confirmation the user never made.
///
/// The sidebar route actions that arm and confirm a prompt keep it intact and
/// perform their own revalidation and confirmation inside `jinn-sidebar`; the
/// cancel prompt is confirmed by the escape that raised it.
pub(crate) fn dismiss_unrelated_prompts(intent: &KernelIntent, state: &mut AppState) {
    let sidebar_action = match intent {
        KernelIntent::Dynamic(dynamic)
            if dynamic.slice == jinn_sidebar_msg::SidebarSectionId::Sessions.scope_id() =>
        {
            Some(dynamic.action.as_str())
        }
        _ => None,
    };

    if state.frontend.close_session_prompt && sidebar_action != Some("session-close") {
        state.frontend.close_session_prompt = false;
    }

    if state.frontend.archive_tree_prompt.is_some() {
        let matching_tree_action = matches!(
            sidebar_action,
            Some(jinn_sidebar_msg::TREE_ARCHIVE_ACTION | jinn_sidebar_msg::TREE_TEARDOWN_ACTION)
        );
        if !matching_tree_action {
            state.frontend.archive_tree_prompt = None;
        }
    }

    // The cancel prompt belongs to the session, not the sidebar: only the
    // confirming escape may leave it standing, and a turn that finished on
    // its own leaves nothing to cancel.
    if state.frontend.cancel_stream_prompt
        && (!matches!(intent, KernelIntent::NormalEscape) || !stream_in_flight(state))
    {
        state.frontend.cancel_stream_prompt = false;
    }
}

/// Whether the active session, or any descendant a cancel would reach, has
/// running work — the one condition that both raises the cancel-stream prompt
/// and keeps it standing.
///
/// Delegates to [`super::cancel::subtree_has_running_work`], which owns the
/// condition and the subtree boundaries it is scoped to, so the prompt can
/// never advertise a cancel the confirming half would decline to perform.
/// The renderer draws the bar from the same predicate.
pub(crate) fn stream_in_flight(state: &AppState) -> bool {
    super::cancel::subtree_has_running_work(state, state.session.active_session_id())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::missing_docs_in_private_items, reason = "test code")]
    use super::*;

    use crate::feat::intent::handler::IntentHandler;

    /// The slice registry with nothing registered: enough for dispatch tests
    /// that are not exercising composition.
    fn empty_slices() -> jinn_slices::Slices {
        jinn_slices::Slices::new()
    }

    fn empty_routes() -> jinn_slices::route::KeyRoutes {
        jinn_slices::route::KeyRoutes::new()
    }

    #[rstest::rstest]
    fn cancel_stream_prompt_esc_confirms() {
        // Given cancel_stream_prompt is showing over a turn in flight.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When handling NormalEscape.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is dismissed and a CancelTurn command is emitted.
        assert!(!state.frontend.cancel_stream_prompt);
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("CancelTurn")),
            "should emit CancelTurn: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn cancel_stream_prompt_other_intent_dismisses() {
        // Given cancel_stream_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = true;

        // When handling a different intent (NoOp).
        let _result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is dismissed but no CancelTurn command.
        assert!(!state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    fn cancel_stream_prompt_not_showing_returns_none() {
        // Given cancel_stream_prompt is NOT showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = false;

        // When handling NormalEscape.
        let _result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then no cancel command is emitted (falls through to normal escape handling).
        // The prompt remains false.
        assert!(!state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    fn close_session_prompt_other_intent_dismisses() {
        // Given close_session_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.close_session_prompt = true;

        // When handling a different intent (NoOp).
        let _result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is dismissed.
        assert!(!state.frontend.close_session_prompt);
    }

    #[rstest::rstest]
    fn cancel_stream_prompt_noop_dismisses() {
        // Given cancel_stream_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.cancel_stream_prompt = true;

        // When handling NoOp (unmapped key).
        let result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is dismissed and no CancelTurn command is emitted.
        assert!(!state.frontend.cancel_stream_prompt);
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("CancelTurn")),
            "should not emit CancelTurn: {:?}",
            result.message_names
        );
    }

    #[rstest::rstest]
    fn close_session_prompt_noop_dismisses() {
        // Given close_session_prompt is showing.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.close_session_prompt = true;

        // When handling NoOp (unmapped key).
        let _result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is dismissed.
        assert!(!state.frontend.close_session_prompt);
    }

    #[rstest::rstest]
    fn noop_is_empty_when_no_prompt() {
        // Given default state with no prompts showing.
        let mut state = AppState::default_with_scope_focus();

        // When handling NoOp.
        let result = IntentHandler::handle(
            &KernelIntent::NoOp,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then result is empty.
        assert!(result.message_names.is_empty());
    }
}
