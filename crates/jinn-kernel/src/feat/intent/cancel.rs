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

//! The cancel-stream cascade — stopping a session and everything beneath it.
//!
//! A confirmed cancel is not local. The session's own stream is cancelled
//! inline, and every subagent or attendant below it is stopped too, recursively.
//! Forks are hard boundaries: their descendants are independent threads and
//! out of the cancel's scope.
//!
//! Descendant stops are messages rather than synchronous writes, because each
//! child's phase is owned by its session actor. A caller that owns its own
//! session's phase must drive that phase itself, or a user message it dispatches
//! immediately afterwards will be queued rather than sent.
//!
//! Split out of [`super::handler`] because the cascade walk has more than one
//! caller — `Esc` here, and the attendant slice's manual re-run — and its
//! fork-boundary rule deserves exactly one owner.

use std::collections::HashSet;

use crate::AppState;
use crate::IntentResult;
use crate::protocol::KernelIntent;
use jinn_core_types::SessionId;

/// Whether `session_id`, or any descendant a cancel would reach, has running
/// work — the one condition that raises the cancel-stream prompt, keeps it
/// standing, and decides whether the renderer draws the prompt bar.
///
/// "Cancellable" is the load-bearing word: the answer is scoped to the same
/// subtree [`cascade_descendants`] would walk. A prompt therefore never
/// advertises a cancel the kernel would decline to perform.
///
/// The session itself counts when it is busy or out of `Idle`; a descendant
/// counts when a cancel would reach it *and* it is actively running. A
/// subagent counts on presence in the task-spawn registry, which is
/// authoritative — the guard's `Drop` unregisters a finished call. An attendant
/// has no registry, so it counts on its own phase or busy counter. An idle,
/// finished attendant does not arm the prompt.
///
/// This function answers only. It emits nothing and mutates nothing: a
/// predicate that cancelled would be a second writer of the cascade's
/// decisions, free to drift from the walk.
#[must_use]
pub fn subtree_has_running_work(state: &AppState, session_id: &SessionId) -> bool {
    if let Some(session) = state.try_session(session_id)
        && (session.is_busy() || !matches!(session.phase(), jinn_session_msg::PhaseKind::Idle))
    {
        return true;
    }

    let mut visited = HashSet::new();
    visited.insert(session_id.clone());
    descendant_has_running_work(state, session_id, &mut visited)
}

/// Whether any reachable descendant below `session_id` is actively running.
///
/// A child is followed on its origin, exactly as the cascade follows it:
/// `Subagent` and `Attendant` recurse, `Fork` is a hard boundary, `User` is
/// skipped. The `visited` set terminates the walk on a cyclic parent link.
fn descendant_has_running_work(
    state: &AppState,
    session_id: &SessionId,
    visited: &mut HashSet<SessionId>,
) -> bool {
    cancellable_children(state, session_id)
        .into_iter()
        .any(|child_id| child_is_running(state, &child_id, visited))
}

/// Whether one child reached through [`cancellable_children`] is running, or
/// has something running beneath it.
///
/// A subagent reached here came out of the in-flight registry, so its presence
/// is already the answer — no phase read, and no walk below it. An attendant
/// is reached through the live session map, so its own liveness must be read,
/// and only an idle one is worth walking into.
fn child_is_running(
    state: &AppState,
    child_id: &SessionId,
    visited: &mut HashSet<SessionId>,
) -> bool {
    if !visited.insert(child_id.clone()) {
        return false;
    }
    let Some(session) = state.try_session(child_id) else {
        return false;
    };
    match session.origin() {
        jinn_session_msg::SessionOrigin::Subagent => true,
        jinn_session_msg::SessionOrigin::Attendant => {
            session.is_busy()
                || !matches!(session.phase(), jinn_session_msg::PhaseKind::Idle)
                || descendant_has_running_work(state, child_id, visited)
        }
        jinn_session_msg::SessionOrigin::Fork | jinn_session_msg::SessionOrigin::User => false,
    }
}

/// Every child a cancel of `session_id` would consider, from both sources: the
/// in-flight task-spawn registry (subagents) and the live session map
/// (attendants). Sorted and deduplicated so the walk visits a child once even
/// when both sources know it.
fn cancellable_children(state: &AppState, session_id: &SessionId) -> Vec<SessionId> {
    let mut child_ids = state.task_spawns.children_of(session_id);
    for (id, session) in state.session.iter() {
        if session.parent_session().as_ref() == Some(session_id) && session.is_attendant() {
            child_ids.push(id.clone());
        }
    }
    child_ids.sort();
    child_ids.dedup();
    child_ids
}

/// Disarms the cancel prompt when nothing a cancel would reach is running.
///
/// The prompt is armed by a keystroke, but the work it refers to ends on its
/// own — a turn finishes, an attendant returns — and no keystroke is involved.
/// Left standing, the flag would advertise a confirmation the user cannot
/// act on. Keystroke-driven dismissal cannot catch that: it runs only when a
/// key arrives, so a prompt whose work ended in silence would sit armed
/// indefinitely.
///
/// Call this from whatever polls the session while the app is idle. It is a
/// no-op unless the flag is armed, and it disarms on exactly the condition the
/// renderer gates the bar on — so the flag and the bar can never disagree.
///
/// The predicate is scoped to the subtree the cascade walks, so work the
/// cascade would decline to cancel (a fork's own descendants) never armed the
/// prompt in the first place and cannot disarm it here.
pub fn disarm_stale_cancel_prompt(state: &mut AppState) {
    if state.frontend.cancel_stream_prompt
        && !subtree_has_running_work(state, state.session.active_session_id())
    {
        state.frontend.cancel_stream_prompt = false;
    }
}

pub(crate) fn try_handle_cancel_stream_prompt(
    intent: &KernelIntent,
    state: &mut AppState,
) -> Option<IntentResult> {
    if !state.frontend.cancel_stream_prompt {
        return None;
    }

    // Dismiss the prompt regardless of which intent triggered it.
    state.frontend.cancel_stream_prompt = false;

    if !matches!(intent, KernelIntent::NormalEscape) {
        // Any other key — dismiss prompt, fall through to normal processing.
        return None;
    }

    let session_id = state.session.active_session_id().clone();

    // Check busy state before resetting. Busy and phase are separate: a
    // lifecycle command in flight sets the counter while the phase is still
    // `Idle`, and either one means there is the session's own work to stop.
    let was_busy = state.active_session().is_busy();
    let has_own_turn = was_busy
        || !matches!(
            state.active_session().phase(),
            jinn_session_msg::PhaseKind::Idle
        );

    // An idle session's own turn already produced nothing to salvage, so the
    // cascade reaches down into its running descendants and leaves the parent
    // alone. Sending it a `CancelTurn` would be actively harmful: the
    // inference actor tombstones a session id before it checks for a live
    // stream, which would drop this session's `ToolContinuation` sends until
    // its next user message. Cancelling it inline would drain its steering
    // fragments and queue over a draft the user is typing.
    let result = if has_own_turn {
        // Cancel busy background operations (lifecycle, etc.).
        if was_busy {
            state.active_session_mut().cancel_busy();
        }

        // Cancel stream.
        state.active_session_mut().cancel_stream_and_drain();
        let mut result = IntentResult::empty().with_message(jinn_inference_msg::CancelTurn {
            session_id: session_id.clone(),
            cause: jinn_inference_msg::CancelCause::Turn,
        });

        // Also cancel any running lifecycle command.
        if was_busy {
            result = result.with_message(jinn_session_lifecycle_msg::CancelLifecycleCommand {
                session_id: session_id.clone(),
            });
        }
        result
    } else {
        IntentResult::empty()
    };

    // The cascade: every subagent or attendant beneath this session stops
    // with it, recursively. Forks are boundaries — their descendants are
    // independent threads, out of the cancel's scope.
    let mut visited = HashSet::new();
    visited.insert(session_id.clone());
    Some(cascade_descendants(state, &session_id, &mut visited).merge(result))
}

/// Collects the cancel messages for every running descendant of `session_id`.
///
/// Immediate children come from two sources: the in-flight task-spawn
/// registry (subagents) and the live session map (attendants). A child is
/// followed on its origin — `Subagent` and `Attendant` recurse, `Fork` is a
/// hard boundary, `User` is skipped. The `visited` set terminates the walk
/// on a cyclic parent link (the same defence the visible session tree uses).
///
/// Descendant cancels are messages, not synchronous state writes: the
/// session actor owns each child's phase. A caller that also owns its own
/// session's phase must drive it to `Idle` itself, or a user message it
/// dispatches immediately after will be queued rather than sent.
///
/// The walk is shared by every caller that stops a subtree — `Esc` on the
/// active session, and the attendant slice's manual re-run.
#[must_use]
pub fn cascade_descendants(
    state: &AppState,
    session_id: &jinn_core_types::SessionId,
    visited: &mut std::collections::HashSet<jinn_core_types::SessionId>,
) -> IntentResult {
    let mut result = IntentResult::empty();

    for child_id in cancellable_children(state, session_id) {
        if !visited.insert(child_id.clone()) {
            continue;
        }
        let child_origin = state
            .try_session(&child_id)
            .map(jinn_session_state::ChatSessionState::origin);
        match child_origin {
            // The child's result is only valid in the context of the parent
            // turn that asked the question — stop it and follow its own
            // subtree.
            Some(
                jinn_session_msg::SessionOrigin::Subagent
                | jinn_session_msg::SessionOrigin::Attendant,
            ) => {
                result = result
                    .with_message(jinn_inference_msg::CancelTurn {
                        session_id: child_id.clone(),
                        cause: jinn_inference_msg::CancelCause::Turn,
                    })
                    .merge(cascade_descendants(state, &child_id, visited));
            }
            // A fork is an independent thread: its own descendants are out
            // of scope. The walk stops here, deliberately. A user-created
            // child is not the cancel's to stop either.
            None
            | Some(jinn_session_msg::SessionOrigin::Fork | jinn_session_msg::SessionOrigin::User) =>
                {}
        }
    }

    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::missing_docs_in_private_items, reason = "test code")]
    use super::*;

    use crate::feat::intent::handler::IntentHandler;
    use jinn_core_types::SessionId;
    use jinn_session_msg::SessionOrigin;
    use jinn_session_state::ChatSessionState;

    /// The slice registry with nothing registered: enough for dispatch tests
    /// that are not exercising composition.
    fn empty_slices() -> jinn_slices::Slices {
        jinn_slices::Slices::new()
    }

    fn empty_routes() -> jinn_slices::route::KeyRoutes {
        jinn_slices::route::KeyRoutes::new()
    }

    /// A registry with every slice cell registered — the same catalog
    /// production boot uses, for the one assertion that reads a cell.
    fn cell_backed_slices() -> jinn_slices::Slices {
        let slices = jinn_slices::Slices::new();
        jinn_cell_catalog::register_all_cells(&slices);
        slices
    }

    fn confirmed_cancel(state: &mut AppState) -> IntentResult {
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;
        IntentHandler::handle(
            &KernelIntent::NormalEscape,
            state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        )
    }

    fn cancel_count(result: &IntentResult) -> usize {
        result
            .message_names
            .iter()
            .filter(|name| name.contains("CancelTurn"))
            .count()
    }

    /// Whether the result includes a lifecycle-command cancel.
    fn names_lifecycle_cancel(result: &IntentResult) -> bool {
        result
            .message_names
            .iter()
            .any(|name| name.contains("CancelLifecycleCommand"))
    }

    fn link_child(state: &mut AppState, parent_id: &SessionId, origin: SessionOrigin) -> SessionId {
        let parent = state.session.get(parent_id).expect("parent").clone();
        let child = match origin {
            SessionOrigin::Attendant => ChatSessionState::new_attendant(&parent, false),
            _ => {
                let mut child = ChatSessionState::new_child(parent_id, false);
                child.set_origin(origin);
                child
            }
        };
        let child_id = child.session_id().clone();
        state.session.insert(child);
        child_id
    }

    #[rstest::rstest]
    fn confirmed_cancel_stops_immediate_subagents_and_attendants() {
        // Given a parent with a running subagent and a running attendant.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let subagent = link_child(&mut state, &parent_id, SessionOrigin::Subagent);
        let _attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .task_spawns
            .register(parent_id.clone(), subagent.clone());

        // When the confirmed cancel runs.
        let result = confirmed_cancel(&mut state);

        // Then both children receive a cancel (plus the parent's own).
        assert_eq!(cancel_count(&result), 3, "parent + subagent + attendant");
        // And the registry is untouched by the walk itself: it empties when
        // the task future's guard drops, not when the cancel publishes (the
        // registry-layer assertion lives in task_tests).
        assert!(state.task_spawns.has_in_flight(&parent_id));
    }

    #[rstest::rstest]
    fn confirmed_cancel_recurses_through_nested_subagents() {
        // Given a depth-2 subagent tree.
        let mut state = AppState::default_with_scope_focus();
        let root_id = state.session.active_session_id().clone();
        let mid = link_child(&mut state, &root_id, SessionOrigin::Subagent);
        let leaf = link_child(&mut state, &mid, SessionOrigin::Subagent);
        state.task_spawns.register(root_id.clone(), mid.clone());
        state.task_spawns.register(mid.clone(), leaf);

        // When the confirmed cancel runs at the root.
        let result = confirmed_cancel(&mut state);

        // Then every level cancelled — the walk is real recursion.
        assert_eq!(cancel_count(&result), 3, "root + mid + leaf");
    }

    #[rstest::rstest]
    fn confirmed_cancel_stops_at_fork_boundary() {
        // Given a parent with a fork child and a fork grandchild under it.
        let mut state = AppState::default_with_scope_focus();
        let root_id = state.session.active_session_id().clone();
        let fork = link_child(&mut state, &root_id, SessionOrigin::Fork);
        let fork_child = link_child(&mut state, &fork, SessionOrigin::Subagent);
        state.task_spawns.register(fork.clone(), fork_child.clone());

        // When the confirmed cancel runs at the root.
        let result = confirmed_cancel(&mut state);

        // Then only the parent cancelled — the fork subtree is untouched.
        assert_eq!(cancel_count(&result), 1, "parent only; fork is a boundary");
    }

    #[rstest::rstest]
    fn fork_child_subagents_survive_cancelling_grandparent() {
        // Given a fork whose own subagent is running, under a busy root.
        let mut state = AppState::default_with_scope_focus();
        let root_id = state.session.active_session_id().clone();
        let fork = link_child(&mut state, &root_id, SessionOrigin::Fork);
        let fork_subagent = link_child(&mut state, &fork, SessionOrigin::Subagent);
        state
            .task_spawns
            .register(fork.clone(), fork_subagent.clone());

        // When the confirmed cancel runs at the root.
        let _result = confirmed_cancel(&mut state);

        // Then the fork's subagent is still registered as in-flight.
        assert!(
            state.task_spawns.has_in_flight(&fork),
            "the fork's own subagent must survive a grandparent cancel"
        );
    }

    #[rstest::rstest]
    fn single_escape_does_not_cascade() {
        // Given a parent with a running subagent and the prompt NOT armed.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let subagent = link_child(&mut state, &parent_id, SessionOrigin::Subagent);
        state
            .task_spawns
            .register(parent_id.clone(), subagent.clone());

        // When a single (unconfirmed) escape arrives — over a turn in
        // flight, so arming is allowed.
        state.active_session_mut().begin_streaming();
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then nothing cancelled — the prompt merely armed.
        assert!(state.frontend.cancel_stream_prompt);
        assert_eq!(cancel_count(&result), 0);
    }

    #[rstest::rstest]
    fn cancel_walk_terminates_on_cyclic_parent_links() {
        // Given two sessions whose parent links form a cycle, every edge
        // reachable through the registry.
        let mut state = AppState::default_with_scope_focus();
        let root_id = state.session.active_session_id().clone();
        let a = link_child(&mut state, &root_id, SessionOrigin::Subagent);
        let b = link_child(&mut state, &a, SessionOrigin::Subagent);
        // Close the cycle: a's parent becomes b — and register both edges so
        // the walk would loop without the visited guard.
        state
            .session
            .get_mut(&a)
            .expect("a")
            .set_parent_session(b.clone());
        state.task_spawns.register(root_id.clone(), a.clone());
        state.task_spawns.register(a.clone(), b.clone());
        state.task_spawns.register(b.clone(), a.clone());

        // When the confirmed cancel runs at the root.
        let result = confirmed_cancel(&mut state);

        // Then the walk terminated (root + a + b, no repeat) — reaching here
        // at all proves termination; the count proves no double-cancel.
        assert_eq!(cancel_count(&result), 3);
    }

    // ── What the cancel prompt is offered over ──
    //
    // One predicate answers "is there anything a cancel would reach that is
    // running". It gates arming, dismissal, and the renderer's bar, so a
    // prompt cannot appear over a session the confirming half would decline
    // to cancel.

    #[rstest::rstest]
    fn idle_session_with_running_attendant_has_cancellable_work() {
        // Given an idle session with an attendant that is still working.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then the subtree is cancellable even though the session itself is idle.
        assert!(cancellable);
        assert!(
            matches!(
                state.active_session().phase(),
                jinn_session_msg::PhaseKind::Idle
            ),
            "the fixture must really be idle, else this test proves nothing"
        );
    }

    #[rstest::rstest]
    fn idle_session_with_finished_attendant_has_no_cancellable_work() {
        // Given an idle session with an attendant that has already finished.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        link_child(&mut state, &parent_id, SessionOrigin::Attendant);

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then nothing is running, so there is nothing to offer a cancel for.
        assert!(!cancellable);
    }

    #[rstest::rstest]
    fn idle_session_with_running_subagent_has_cancellable_work() {
        // Given an idle session blocked on nothing, but with a subagent in
        // flight under it.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let subagent = link_child(&mut state, &parent_id, SessionOrigin::Subagent);
        state.task_spawns.register(parent_id.clone(), subagent);

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then the in-flight subagent makes the subtree cancellable.
        assert!(cancellable);
    }

    #[rstest::rstest]
    fn running_work_under_a_fork_is_not_cancellable() {
        // Given a subagent running under a fork of the active session.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let fork = link_child(&mut state, &parent_id, SessionOrigin::Fork);
        let fork_child = link_child(&mut state, &fork, SessionOrigin::Subagent);
        state.task_spawns.register(fork.clone(), fork_child);

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then the fork's subagent is out of scope, so nothing is cancellable.
        assert!(!cancellable);
    }

    #[rstest::rstest]
    fn idle_attendant_with_running_subagent_below_it_is_cancellable() {
        // Given an idle attendant whose own subagent is in flight.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        let subagent = link_child(&mut state, &attendant, SessionOrigin::Subagent);
        state.task_spawns.register(attendant.clone(), subagent);

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then the walk recursed through the idle attendant to its subagent.
        assert!(cancellable);
    }

    #[rstest::rstest]
    fn running_work_check_terminates_on_cyclic_parent_links() {
        // Given two descendants whose parent links form a cycle.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let a = link_child(&mut state, &parent_id, SessionOrigin::Subagent);
        let b = link_child(&mut state, &a, SessionOrigin::Subagent);
        state
            .session
            .get_mut(&a)
            .expect("a")
            .set_parent_session(b.clone());

        // When the predicate is asked about the active session.
        let cancellable = subtree_has_running_work(&state, &parent_id);

        // Then the walk terminated rather than looping; reaching here proves it.
        assert!(!cancellable, "no child in the cycle is in flight");
    }

    #[rstest::rstest]
    fn escape_arms_the_prompt_over_an_idle_session_with_running_attendant() {
        // Given an idle session with a running attendant and no armed prompt.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();

        // When a single escape arrives.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt is armed, and nothing was cancelled yet.
        assert!(state.frontend.cancel_stream_prompt);
        assert_eq!(cancel_count(&result), 0);
    }

    #[rstest::rstest]
    fn armed_prompt_survives_the_session_turn_finishing() {
        // Given the prompt armed over a running attendant, with the session's
        // own turn finishing while it is up.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When that session's own turn finishes and the escape confirms.
        state
            .active_session_mut()
            .finish_streaming(false, jiff::Timestamp::now());
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the prompt was not dismissed as stale, and the attendant's work
        // was still cancellable — the only thing that stood it down.
        assert_eq!(cancel_count(&result), 1, "the attendant is still running");
    }

    // ── A prompt whose work ends in silence must not stay armed ──

    #[rstest::rstest]
    fn armed_prompt_disarms_when_the_turn_finishes_without_a_keystroke() {
        // Given the prompt armed over a live turn.
        let mut state = AppState::default_with_scope_focus();
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the turn finishes and the app polls.
        state
            .active_session_mut()
            .finish_streaming(false, jiff::Timestamp::now());
        disarm_stale_cancel_prompt(&mut state);

        // Then the flag is cleared, not merely hidden.
        assert!(!state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    fn armed_prompt_stays_armed_while_a_descendant_is_still_running() {
        // Given the prompt armed over an idle session with a running attendant.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the app polls.
        disarm_stale_cancel_prompt(&mut state);

        // Then the flag stands — the attendant is still cancellable.
        assert!(state.frontend.cancel_stream_prompt);
    }

    #[rstest::rstest]
    fn armed_prompt_stays_armed_over_work_the_cascade_would_not_reach() {
        // Given the prompt armed over a fork whose own subagent is running.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let fork = link_child(&mut state, &parent_id, SessionOrigin::Fork);
        let fork_child = link_child(&mut state, &fork, SessionOrigin::Subagent);
        state.task_spawns.register(fork.clone(), fork_child);
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the app polls.
        disarm_stale_cancel_prompt(&mut state);

        // Then the prompt stands — the session's own turn is still live, and
        // the fork's work is not something this sweep may judge.
        assert!(state.frontend.cancel_stream_prompt);
    }

    // ── What a confirmed cancel sends when the session itself is idle ──

    #[rstest::rstest]
    fn confirmed_cancel_on_idle_session_targets_only_its_running_descendants() {
        // Given an idle session with a running attendant and no subagents.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then exactly one cancel is sent — the attendant's.
        assert_eq!(
            cancel_count(&result),
            1,
            "the attendant only, not the parent"
        );
    }

    #[rstest::rstest]
    fn confirmed_cancel_on_idle_session_sends_no_lifecycle_cancel() {
        // Given the same idle session with a running attendant.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then no lifecycle command is cancelled — the idle parent ran none.
        assert!(!names_lifecycle_cancel(&result));
    }

    #[rstest::rstest]
    fn confirmed_cancel_on_idle_session_leaves_its_phase_untouched() {
        // Given an idle session with a running attendant.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let _result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the parent's phase is unchanged — its own turn was never ours to stop.
        assert!(matches!(
            state.active_session().phase(),
            jinn_session_msg::PhaseKind::Idle
        ));
    }

    #[rstest::rstest]
    fn confirmed_cancel_on_idle_session_leaves_its_draft_untouched() {
        // Given an idle session with a typed draft and a running attendant.
        let mut state = AppState::default_with_scope_focus();
        // The draft lives in the chat-input cell, which a session only reaches
        // through an attached registry — the same handle composition hands it.
        let slices = cell_backed_slices();
        state.session.attach_slices(slices);
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state
            .active_session_mut()
            .update_input(|input| input.insert_text("a draft the user is typing"));
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let _result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the draft is still the user's — a cancel with nothing to drain
        // must not overwrite it.
        let draft = state
            .active_session()
            .with_input(|input| input.text().to_owned(), String::new);
        assert_eq!(draft, "a draft the user is typing");
    }

    #[rstest::rstest]
    fn confirmed_cancel_on_busy_session_still_cancels_the_session_itself() {
        // Given a busy session with a running attendant beneath it.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.active_session_mut().begin_busy();
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the parent is cancelled alongside the attendant.
        assert_eq!(cancel_count(&result), 2, "parent + attendant");
    }

    #[rstest::rstest]
    fn confirmed_cancel_on_busy_session_still_cancels_its_lifecycle_command() {
        // Given a busy session with a running attendant beneath it.
        let mut state = AppState::default_with_scope_focus();
        let parent_id = state.session.active_session_id().clone();
        let attendant = link_child(&mut state, &parent_id, SessionOrigin::Attendant);
        state
            .session
            .get_mut(&attendant)
            .expect("attendant")
            .begin_streaming();
        state.active_session_mut().begin_busy();
        state.active_session_mut().begin_streaming();
        state.frontend.cancel_stream_prompt = true;

        // When the confirmed cancel runs.
        let result = IntentHandler::handle(
            &KernelIntent::NormalEscape,
            &mut state,
            &empty_slices(),
            &empty_routes(),
            jinn_slices::empty_config_layer(),
        );

        // Then the busy session's lifecycle command is cancelled too.
        assert!(names_lifecycle_cancel(&result));
    }
}
