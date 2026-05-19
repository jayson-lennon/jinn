//! Session lifecycle intent handlers — setup, close, and arg input confirmation.
//!
//! These handlers bridge the Intent-driven architecture with the session lifecycle
//! system. The IntentHandler calls these functions directly; they mutate `AppState`
//! and return `IntentResult` with commands for the actor system.

use wherror::Error;

use crate::common::app_state::AppState;
use crate::feat::preferences_actor::user_preferences::SessionLifecycle;
use crate::feat::provider_infra::NO_PROVIDER_ID;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::profile::SessionProfile;
use crate::feat::session_lifecycle::command_template::{CommandTemplate, parse_quoted_args};
use crate::feat::session_lifecycle::protocol::command::{RunSessionSetup, SaveNewLifecycleSession};
use crate::protocol::{Command, IntentResult, PromptStrategyId, SessionId};

/// Errors that can occur when validating arg input.
#[derive(Debug, Error)]
#[error(debug)]
pub enum ArgInputError {
    /// User provided fewer args than the lifecycle template expects.
    #[error("expected {expected} arguments, got {provided}")]
    NotEnoughArgs {
        /// Number of params the template has.
        expected: usize,
        /// Number of args the user entered.
        provided: usize,
    },
}

/// Validates that the arg input has enough tokens for the lifecycle template.
///
/// # Errors
///
/// Returns [`ArgInputError::NotEnoughArgs`] if there aren't enough tokens
/// to fill the template's parameters.
pub fn validate_arg_input(state: &AppState) -> Result<(), ArgInputError> {
    let arg_state = &state.frontend.arg_input;

    let param_count = state
        .frontend
        .preferences
        .session_lifecycles
        .iter()
        .find(|l| l.name == arg_state.lifecycle_name)
        .and_then(|l| l.setup_command.as_ref())
        .map_or(0, |cmd| CommandTemplate::parse(cmd).param_count());

    let arg_count = if arg_state.input.trim().is_empty() {
        0
    } else {
        parse_quoted_args(&arg_state.input).len()
    };

    if arg_count < param_count {
        return Err(ArgInputError::NotEnoughArgs {
            expected: param_count,
            provided: arg_count,
        });
    }

    Ok(())
}

/// Handle `Intent::SessionLifecycleSetup`.
///
/// Creates a new session from the named lifecycle. If the lifecycle has a
/// `setup_command`, emits `Command::RunSessionSetup` for async execution.
/// If no setup command (blank or blank-like lifecycle), creates the session
/// with the default CWD immediately.
pub fn handle_session_lifecycle_setup(
    state: &mut AppState,
    lifecycle_name: &str,
    args: &[String],
) -> IntentResult {
    // Auto-close the active session if it is empty (no history entries).
    // This prevents ghost "Untitled Session" entries from accumulating
    // when the user creates sessions in quick succession without sending
    // any messages. Teardown is skipped because an empty session has no
    // meaningful state to clean up.
    if state.active_session().is_empty() {
        let old_id = state.session.active_session_id().clone();
        state.session.sessions_mut().remove(&old_id);
        // If that was the last session, we'll create a new one below.
        // Ensure active_session points to something valid (it will be
        // overwritten immediately, but the insert below expects the map
        // to potentially be non-empty for the "switch" logic).
        if let Some(next_id) = state.session.sessions().keys().next().cloned() {
            state.session.set_active(next_id);
        }
    }

    // Extract setup command before mutating state (borrow checker).
    let setup_command = find_lifecycle(state, lifecycle_name).and_then(|l| l.setup_command.clone());

    let model = state
        .frontend
        .preferences
        .last_model
        .clone()
        .unwrap_or_else(|| NO_PROVIDER_ID.to_owned());
    let strategy = state
        .frontend
        .preferences
        .last_strategy
        .as_deref()
        .map_or_else(PromptStrategyId::passthrough, PromptStrategyId::new);
    let persona_name = state
        .context
        .active_persona
        .as_ref()
        .map_or_else(|| "coding-assistant".to_owned(), |p| p.name.clone());
    let token_budget = state.frontend.preferences.context_token_budget.budget;
    let sliding_window_size = state.frontend.preferences.context_sliding_window.size;

    let mut new_session = ChatSessionState::new_with_profile(SessionProfile::new(
        model,
        strategy,
        persona_name,
        token_budget,
        sliding_window_size,
    ));
    let new_id = new_session.session_id().clone();

    // Set lifecycle metadata on the session core.
    new_session.set_lifecycle_name(if lifecycle_name.is_empty() {
        None
    } else {
        Some(lifecycle_name.to_owned())
    });
    new_session.set_lifecycle_args(args.to_vec());

    state
        .session
        .sessions_mut()
        .insert(new_id.clone(), new_session);
    state.session.set_active(new_id.clone());
    state.frontend.scope_stack.clear_overlays();
    state
        .frontend
        .scope_stack
        .push(crate::common::app_state::FocusScope::Input);

    // If the lifecycle has a setup_command, emit it for async execution.
    if let Some(ref setup_cmd) = setup_command {
        let template = CommandTemplate::parse(setup_cmd);
        let rendered = if args.is_empty() {
            setup_cmd.to_owned()
        } else {
            template.render(args)
        };

        return IntentResult::with_commands(vec![
            Command::SaveNewLifecycleSession(SaveNewLifecycleSession {
                session_id: new_id.clone(),
            }),
            Command::RunSessionSetup(RunSessionSetup {
                session_id: new_id,
                command: rendered,
                args: args.to_vec(),
            }),
        ]);
    }

    // No setup command — use default CWD immediately.
    let default_cwd = state.session.default_cwd().clone();
    state.session_mut(&new_id).set_cwd(default_cwd);

    IntentResult::empty()
}

/// Handle `Intent::SessionClose`.
///
/// Emits a `CloseSession` command for the active session. The session actor
/// handles teardown, archival, removal, and emits `SessionClosed`.
pub fn handle_session_close(state: &mut AppState) -> IntentResult {
    let closing_id = state.session.active_session_id().clone();
    close_session_and_switch(&closing_id)
}

/// Handle `Intent::ArgInputConfirm`.
///
/// Splits the arg input by whitespace, pops the ArgInput scope,
/// and delegates to `handle_session_lifecycle_setup` with the parsed args.
/// If not enough args are provided for the lifecycle template,
/// returns without popping (user stays in the arg input popup).
pub fn handle_arg_input_confirm(state: &mut AppState) -> IntentResult {
    // Validate that enough args are provided.
    if validate_arg_input(state).is_err() {
        return IntentResult::empty();
    }

    let arg_state = &state.frontend.arg_input;
    let lifecycle_name = arg_state.lifecycle_name.clone();
    let args: Vec<String> = if arg_state.input.trim().is_empty() {
        vec![]
    } else {
        parse_quoted_args(&arg_state.input)
    };

    // Pop ArgInput scope.
    state.frontend.scope_stack.pop();
    // Clear arg input state.
    state.frontend.arg_input = crate::common::app_state::ArgInputState::default();

    handle_session_lifecycle_setup(state, &lifecycle_name, &args)
}

/// Handle character insertion in the arg input popup.
pub fn handle_arg_input_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    let arg = &mut state.frontend.arg_input;
    arg.input.insert(arg.cursor_pos, ch);
    arg.cursor_pos += ch.len_utf8();
    IntentResult::empty()
}

/// Handle grapheme deletion in the arg input popup.
pub fn handle_arg_input_delete(state: &mut AppState) -> IntentResult {
    use unicode_segmentation::UnicodeSegmentation;
    let arg = &mut state.frontend.arg_input;
    if arg.cursor_pos > 0 {
        let prev = arg.input[..arg.cursor_pos]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i);
        if let Some(prev_idx) = prev {
            arg.input.drain(prev_idx..arg.cursor_pos);
            arg.cursor_pos = prev_idx;
        }
    }
    IntentResult::empty()
}

/// Handle forward delete in the arg input popup (deletes the grapheme at/after cursor).
pub fn handle_arg_input_delete_forward(state: &mut AppState) -> IntentResult {
    use unicode_segmentation::UnicodeSegmentation;
    let arg = &mut state.frontend.arg_input;
    if arg.cursor_pos < arg.input.len() {
        let next_end = arg.input[arg.cursor_pos..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(arg.input.len(), |(i, _)| arg.cursor_pos + i);
        arg.input.drain(arg.cursor_pos..next_end);
    }
    IntentResult::empty()
}

/// Handle cursor left in the arg input popup.
pub fn handle_arg_input_cursor_left(state: &mut AppState) -> IntentResult {
    use unicode_segmentation::UnicodeSegmentation;
    let arg = &mut state.frontend.arg_input;
    if arg.cursor_pos > 0 {
        let prev = arg.input[..arg.cursor_pos]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i);
        if let Some(prev_idx) = prev {
            arg.cursor_pos = prev_idx;
        }
    }
    IntentResult::empty()
}

/// Handle cursor right in the arg input popup.
pub fn handle_arg_input_cursor_right(state: &mut AppState) -> IntentResult {
    use unicode_segmentation::UnicodeSegmentation;
    let arg = &mut state.frontend.arg_input;
    if arg.cursor_pos < arg.input.len() {
        let next = arg.input[arg.cursor_pos..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| arg.cursor_pos + i);
        match next {
            Some(next_idx) => arg.cursor_pos = next_idx,
            None => arg.cursor_pos = arg.input.len(),
        }
    }
    IntentResult::empty()
}

/// Handles `PasteText` in arg input scope — bulk inserts pasted text at the cursor.
pub fn handle_arg_input_paste(state: &mut AppState, text: &str) -> IntentResult {
    if text.is_empty() {
        return IntentResult::empty();
    }
    let arg = &mut state.frontend.arg_input;
    arg.input.insert_str(arg.cursor_pos, text);
    arg.cursor_pos += text.len();
    IntentResult::empty()
}

/// Look up a lifecycle by name in the user preferences.
fn find_lifecycle<'a>(state: &'a AppState, name: &str) -> Option<&'a SessionLifecycle> {
    state
        .frontend
        .preferences
        .session_lifecycles
        .iter()
        .find(|l| l.name == name)
}

/// Emit a `CloseSession` command to the actor system.
/// The session actor handles actual removal, active session switching, and emits
/// `SessionClosed` for the sidebar actor to clamp the cursor.
fn close_session_and_switch(closing_id: &SessionId) -> IntentResult {
    use crate::feat::session::protocol::close_session::CloseSession;
    IntentResult::with_commands(vec![Command::CloseSession(CloseSession {
        session_id: closing_id.clone(),
    })])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::feat::preferences_actor::user_preferences::SessionLifecycle;
    use crate::protocol::ChatEntry;

    #[rstest::rstest]
    fn session_lifecycle_setup_with_blank_creates_session() {
        // Given default state (no lifecycles configured).
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();

        // When handling SessionLifecycleSetup with blank lifecycle.
        let result = handle_session_lifecycle_setup(&mut state, "", &[]);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the old empty session was auto-closed.
        assert!(!state.session.sessions().contains_key(&old_id));
        // And only one session remains (the new one).
        assert_eq!(state.session.sessions().len(), 1);
        // And no commands emitted (no setup command).
        assert!(result.commands.is_empty());
        // And the session has no lifecycle name.
        assert!(state.active_session().lifecycle_name().is_none());
        // And the session has the default CWD.
        assert_eq!(state.active_session().cwd(), state.session.default_cwd());
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_lifecycle_emits_command() {
        // Given a state with a lifecycle that has a setup_command.
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("echo /tmp/workdir".to_owned()),
                teardown_command: None,
            });

        // When handling SessionLifecycleSetup.
        let result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[]);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And SaveNewLifecycleSession then RunSessionSetup are emitted.
        assert_eq!(result.commands.len(), 2);
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        assert!(matches!(
            &result.commands[1],
            Command::RunSessionSetup(RunSessionSetup {
                command,
                ..
            }) if command == "echo /tmp/workdir"
        ));
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_args_renders_command() {
        // Given a lifecycle with $1 in the setup_command.
        let mut state = AppState::default();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("script.sh $1".to_owned()),
                teardown_command: None,
            });

        // When handling SessionLifecycleSetup with args.
        let result =
            handle_session_lifecycle_setup(&mut state, "fossil branch", &["my-branch".to_owned()]);

        // Then SaveNewLifecycleSession is emitted first.
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        // And RunSessionSetup is emitted second with rendered args.
        assert!(matches!(
            &result.commands[1],
            Command::RunSessionSetup(RunSessionSetup {
                command,
                args,
                ..
            }) if command == "script.sh my-branch" && args == &["my-branch".to_owned()]
        ));
        // And the session has the args stored.
        assert_eq!(
            state.active_session().lifecycle_args(),
            &["my-branch".to_owned()]
        );
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_clears_overlays_and_pushes_input() {
        // Given a state with a picker overlay.
        let mut state = AppState::default();
        state
            .frontend
            .scope_stack
            .push(crate::common::app_state::FocusScope::Picker {
                kind: crate::protocol::PickerKind::Provider,
            });

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[]);

        // Then overlays are cleared and Input scope is pushed.
        assert!(matches!(
            state.frontend.scope_stack.current(),
            crate::common::app_state::FocusScope::Input
        ));
    }

    #[rstest::rstest]
    fn session_close_without_lifecycle_emits_close_session() {
        // Given a state with two sessions.
        let mut state = AppState::default();
        let second_session = ChatSessionState::new();
        let second_id = second_session.session_id().clone();
        state.session.insert(second_session);
        state.session.set_active(second_id.clone());

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted for the closed session.
        assert_eq!(result.commands.len(), 1);
        assert!(matches!(
            &result.commands[0],
            Command::CloseSession(cmd) if cmd.session_id == second_id
        ));
    }

    #[rstest::rstest]
    fn session_close_with_teardown_emits_close_session() {
        // Given a session with a lifecycle that has a teardown_command.
        let mut state = AppState::default();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("echo /tmp/workdir".to_owned()),
                teardown_command: Some("cleanup.sh $1".to_owned()),
            });
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(vec!["my-branch".to_owned()]);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted (actor handles teardown).
        assert!(state.session.sessions().contains_key(&session_id));
        assert_eq!(result.commands.len(), 1);
        assert!(matches!(
            &result.commands[0],
            Command::CloseSession(cmd) if cmd.session_id == session_id
        ));
    }

    #[rstest::rstest]
    fn session_close_last_session_emits_close_session() {
        // Given a state with only one session.
        let mut state = AppState::default();
        let session_id = state.session.active_session_id().clone();
        assert_eq!(state.session.sessions().len(), 1);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted.
        assert_eq!(result.commands.len(), 1);
        assert!(matches!(
            &result.commands[0],
            Command::CloseSession(cmd) if cmd.session_id == session_id
        ));
    }

    #[rstest::rstest]
    fn session_new_delegates_to_blank_lifecycle() {
        // Given default state.
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("old"));

        // When handling SessionNew (delegates to blank lifecycle setup).
        let result = crate::feat::session::intent::handle_session_new(&mut state);

        // Then a new session is created (same behavior as before).
        assert_ne!(*state.session.active_session_id(), old_id);
        assert!(state.active_session().history().is_empty());
        assert!(result.commands.is_empty());
    }

    #[rstest::rstest]
    fn arg_input_confirm_splits_input_into_args() {
        // Given an arg input state with text.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "fossil branch".to_owned();
        state.frontend.arg_input.input = "my-branch target-dir".to_owned();
        state.frontend.arg_input.cursor_pos = state.frontend.arg_input.input.len();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("script.sh $1 $2".to_owned()),
                teardown_command: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input.
        let result = handle_arg_input_confirm(&mut state);

        // Then a new session is created with the args.
        assert_ne!(*state.session.active_session_id(), old_id);
        assert_eq!(
            state.active_session().lifecycle_args(),
            &["my-branch".to_owned(), "target-dir".to_owned()]
        );
        // Then SaveNewLifecycleSession is emitted first.
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        // And RunSessionSetup is emitted second with rendered args.
        assert!(matches!(
            &result.commands[1],
            Command::RunSessionSetup(RunSessionSetup {
                command,
                args,
                ..
            }) if command == "script.sh my-branch target-dir"
                && args == &["my-branch".to_owned(), "target-dir".to_owned()]
        ));
        // And arg input state is cleared.
        assert!(state.frontend.arg_input.lifecycle_name.is_empty());
    }

    #[rstest::rstest]
    fn arg_input_confirm_rejects_empty_input_when_params_needed() {
        // Given an arg input state with empty input for a template that expects $1.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = String::new();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh $1".to_owned()),
                teardown_command: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input with empty input.
        let result = handle_arg_input_confirm(&mut state);

        // Then no command is emitted (validation rejects empty input).
        assert!(result.commands.is_empty());
        // And no session was created (state unchanged).
        assert_eq!(*state.session.active_session_id(), old_id);
        // And arg input state is NOT cleared (user stays in popup).
        assert_eq!(state.frontend.arg_input.lifecycle_name, "test");
    }

    #[rstest::rstest]
    fn arg_input_insert_char_appends_to_input() {
        let mut state = AppState::default();
        state.frontend.arg_input.input = String::new();
        state.frontend.arg_input.cursor_pos = 0;

        let _result = handle_arg_input_insert_char(&mut state, 'a');

        assert_eq!(state.frontend.arg_input.input, "a");
        assert_eq!(state.frontend.arg_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_delete_removes_last_grapheme() {
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 3;

        let _result = handle_arg_input_delete(&mut state);

        assert_eq!(state.frontend.arg_input.input, "ab");
        assert_eq!(state.frontend.arg_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_left_moves_cursor() {
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 3;

        let _result = handle_arg_input_cursor_left(&mut state);

        assert_eq!(state.frontend.arg_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_moves_cursor() {
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 0;

        let _result = handle_arg_input_cursor_right(&mut state);

        assert_eq!(state.frontend.arg_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_reaches_end_of_input() {
        // Given cursor one grapheme before end.
        let mut state = AppState::default();
        state.frontend.arg_input.input = "ab".to_owned();
        state.frontend.arg_input.cursor_pos = 1;

        // When moving right.
        let _result = handle_arg_input_cursor_right(&mut state);

        // Then cursor advances to end (input.len()).
        assert_eq!(state.frontend.arg_input.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_at_end_stays() {
        // Given cursor already at end.
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 3;

        // When moving right.
        let _result = handle_arg_input_cursor_right(&mut state);

        // Then cursor stays at end.
        assert_eq!(state.frontend.arg_input.cursor_pos, 3);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_removes_char_after_cursor() {
        // Given input "abc" with cursor at position 1 (after 'a').
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 1;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then 'b' is removed, cursor stays at 1.
        assert_eq!(state.frontend.arg_input.input, "ac");
        assert_eq!(state.frontend.arg_input.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_at_end_does_nothing() {
        // Given input "abc" with cursor at end.
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 3;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then input is unchanged.
        assert_eq!(state.frontend.arg_input.input, "abc");
        assert_eq!(state.frontend.arg_input.cursor_pos, 3);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_at_start_removes_first_char() {
        // Given input "abc" with cursor at start.
        let mut state = AppState::default();
        state.frontend.arg_input.input = "abc".to_owned();
        state.frontend.arg_input.cursor_pos = 0;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then 'a' is removed.
        assert_eq!(state.frontend.arg_input.input, "bc");
        assert_eq!(state.frontend.arg_input.cursor_pos, 0);
    }

    // --- Arg input validation ---

    #[rstest::rstest]
    fn validate_arg_input_accepts_sufficient_args() {
        // Given a state with a $1 $2 lifecycle and two args provided.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = "foo bar".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh $1 $2".to_owned()),
                teardown_command: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_rejects_insufficient_args() {
        // Given a state with a $1 $2 lifecycle and only one arg.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = "foo".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh $1 $2".to_owned()),
                teardown_command: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation fails with NotEnoughArgs.
        assert!(matches!(
            result,
            Err(ArgInputError::NotEnoughArgs {
                expected: 2,
                provided: 1
            })
        ));
    }

    #[rstest::rstest]
    fn validate_arg_input_accepts_empty_input_when_no_params() {
        // Given a state with a lifecycle that has no params.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "blank".to_owned();
        state.frontend.arg_input.input = String::new();

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes (no params to fill).
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_accepts_splat_without_numbered_params() {
        // Given a state with a $@ lifecycle and any args.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = "anything".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh $@".to_owned()),
                teardown_command: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes (splat accepts any number).
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_rejects_when_named_param_missing() {
        // Given a state with a <branch> <target> lifecycle and only one arg.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = "my-branch".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh <branch> <target>".to_owned()),
                teardown_command: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation fails.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn arg_input_confirm_accepts_sufficient_args() {
        // Given a state with a $1 $2 lifecycle and both args provided.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.input = "foo bar".to_owned();
        state.frontend.arg_input.cursor_pos = 7;
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup_command: Some("script.sh $1 $2".to_owned()),
                teardown_command: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input.
        let result = handle_arg_input_confirm(&mut state);

        // Then a command is emitted with the rendered args.
        assert!(!result.commands.is_empty(), "command should be emitted");
        // Then SaveNewLifecycleSession is emitted first.
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        // And RunSessionSetup is emitted second with rendered args.
        assert!(matches!(
            &result.commands[1],
            Command::RunSessionSetup(RunSessionSetup {
                command,
                args,
                ..
            }) if command == "script.sh foo bar" && args == &["foo".to_owned(), "bar".to_owned()]
        ));
        // And a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
    }

    #[rstest::rstest]
    fn arg_input_confirm_treats_quoted_input_as_single_arg() {
        // Given an arg input state with quoted text.
        let mut state = AppState::default();
        state.frontend.arg_input.lifecycle_name = "fossil branch".to_owned();
        state.frontend.arg_input.input = r#""my branch" target"#.to_owned();
        state.frontend.arg_input.cursor_pos = state.frontend.arg_input.input.len();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("script.sh $1 $2".to_owned()),
                teardown_command: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input.
        let result = handle_arg_input_confirm(&mut state);

        // Then "my branch" is one arg and "target" is the second.
        assert_ne!(*state.session.active_session_id(), old_id);
        assert_eq!(
            state.active_session().lifecycle_args(),
            &["my branch".to_owned(), "target".to_owned()]
        );
        // Then SaveNewLifecycleSession is emitted first.
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        // And RunSessionSetup is emitted second with rendered args.
        assert!(matches!(
            &result.commands[1],
            Command::RunSessionSetup(RunSessionSetup {
                command,
                args,
                ..
            }) if command == "script.sh 'my branch' target"
                && args == &["my branch".to_owned(), "target".to_owned()]
        ));
    }

    // --- Auto-close empty session tests ---

    #[rstest::rstest]
    fn auto_close_removes_empty_active_session_on_new_session() {
        // Given default state with a single empty session.
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();
        assert!(state.active_session().is_empty());

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[]);

        // Then the old empty session is removed.
        assert!(!state.session.sessions().contains_key(&old_id));
        // And only one session remains.
        assert_eq!(state.session.sessions().len(), 1);
    }

    #[rstest::rstest]
    fn auto_close_preserves_session_with_history() {
        // Given an active session with history.
        let mut state = AppState::default();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));

        // When creating a new session.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[]);

        // Then the old session is preserved.
        assert!(state.session.sessions().contains_key(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.sessions().len(), 2);
        // And the new session is active.
        assert_ne!(*state.session.active_session_id(), old_id);
    }

    #[rstest::rstest]
    fn auto_close_replaces_last_empty_session() {
        // Given a single empty session (app just started).
        let mut state = AppState::default();
        assert_eq!(state.session.sessions().len(), 1);

        // When creating a new session with a lifecycle.
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup_command: Some("echo /tmp/workdir".to_owned()),
                teardown_command: None,
            });
        let result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[]);

        // Then only the new session remains (old empty one was auto-closed).
        assert_eq!(state.session.sessions().len(), 1);
        // And the new session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And SaveNewLifecycleSession then RunSessionSetup are emitted.
        assert!(matches!(
            &result.commands[0],
            Command::SaveNewLifecycleSession(_)
        ));
        assert!(matches!(&result.commands[1], Command::RunSessionSetup(..)));
    }
}
