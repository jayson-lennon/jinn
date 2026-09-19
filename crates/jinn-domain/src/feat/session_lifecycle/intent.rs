//! Session lifecycle intent handlers - setup, close, and arg input confirmation.
//!
//! These handlers bridge the Intent-driven architecture with the session lifecycle
//! system. The IntentHandler calls these functions directly; they mutate `AppState`
//! and return `IntentResult` with commands for the actor system.

use wherror::Error;

use crate::common::app_state::AppState;
use jinn_session_history_msg::PushChatEntry;
use crate::feat::session::chat_session::ChatSessionState;
use crate::feat::session::chat_session::LifecycleScriptState;
use crate::feat::session::profile::{DEFAULT_PERSONA_NAME, SessionProfile};
use crate::feat::session::session_actor::setup_running_msg;
use crate::feat::session::sessions_list::close::validate_session_close;
use crate::feat::session::sessions_list::state::sorted_open_sessions;
use crate::feat::session_lifecycle::command_template::{CommandTemplate, parse_quoted_args};
use crate::feat::session_lifecycle::protocol::command::{
    PersistSession, RunSessionSetup, RunSessionTeardown,
};
use crate::feat::session_lifecycle::protocol::event::SessionCreated;
use crate::protocol::{IntentResult, SessionId};
use jinn_preferences_config::schemas::SessionLifecycle;

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
        .and_then(|l| l.setup.as_ref())
        .map_or(0, |cmd| match cmd {
            jinn_preferences_config::schemas::LifecycleCommand::Shell(s) => {
                CommandTemplate::parse(s).param_count()
            }
            jinn_preferences_config::schemas::LifecycleCommand::Builtin(_) => 0,
        });

    let arg_count = if arg_state.text.input.trim().is_empty() {
        0
    } else {
        parse_quoted_args(&arg_state.text.input).len()
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
    cwd: Option<&std::path::Path>,
) -> IntentResult {
    // Extract setup command before mutating state (borrow checker).
    let setup_command = find_lifecycle(state, lifecycle_name).and_then(|l| l.setup.clone());

    let model = state
        .frontend
        .app_state
        .last_model
        .clone()
        .unwrap_or_default();

    let persona_name = state
        .persona_selection()
        .and_then(|cell| cell.read().active.clone())
        .unwrap_or_else(|| DEFAULT_PERSONA_NAME.to_owned());

    let reasoning_effort = state.frontend.app_state.reasoning_effort;

    // Seed per-session defaults from jinn.toml (disablement sets +
    // auto-enabled MCP servers), matching every other session-creation path.
    let seed =
        crate::feat::session::profile::SessionSeed::from_preferences(&state.frontend.preferences);

    let mut new_session = ChatSessionState::new_with_profile(SessionProfile::new(
        model,
        persona_name,
        seed.disabled_tools.clone(),
        seed.disabled_skills.clone(),
        reasoning_effort,
        None,
    ));
    new_session.set_enabled_mcp_servers(seed.enabled_mcp.clone());
    let new_id = new_session.session_id().clone();

    // Set lifecycle metadata on the session core.
    new_session.set_lifecycle_name(if lifecycle_name.is_empty() {
        None
    } else {
        Some(lifecycle_name.to_owned())
    });
    new_session.set_lifecycle_args(args.to_vec());

    // Resolve the new session's starting CWD and project stamp. The CWD
    // precedence is:
    //   1. explicit `cwd` override (e.g. scripted callers),
    //   2. the stashed creation's starting CWD
    //      (set by the project picker then consumed here),
    //   3. inherit the active session's CWD (legacy behavior).
    //
    // The project stamp comes only from the stashed creation - the projects UI
    // is the sole source of a project association. The two are independent on
    // purpose: setup scripts may re-cwd the session later, the project never
    // moves.
    //
    // CWD is resolved BEFORE insert/set_active — once set_active(new_id)
    // runs below, active_session() points at this new session. The stash is
    // always cleared here so it never leaks into the next creation, even when
    // an explicit `cwd` was supplied.
    //
    // A scripted lifecycle's stdout output still wins as the final CWD via the
    // session actor; this only sets the starting value.
    let (stamped_project, starting_cwd) = {
        let pending = state.frontend.pending_creation.take();
        let cwd_override = cwd
            .map(std::path::Path::to_path_buf)
            .or_else(|| pending.as_ref().map(|p| p.starting_cwd.clone()));
        (
            pending.map(|p| p.project_dir),
            cwd_override.unwrap_or_else(|| state.active_session().cwd().to_path_buf()),
        )
    };
    // Defensively clear any residual stash so it can never leak into the next
    // creation (the `.take()` above is skipped only when an explicit cwd
    // overrides, so clear unconditionally here).
    state.frontend.pending_creation = None;
    new_session.set_cwd(starting_cwd.clone());
    new_session.set_project(stamped_project);

    state.session.insert(new_session);
    state.session.set_active(new_id.clone());
    state.frontend.scope_clear_overlays();
    state
        .frontend
        .scope_push(crate::common::app_state::FocusScope::Input);

    // Build the session-created event.
    let created_event = SessionCreated {
        session_id: new_id.clone(),
        cwd: starting_cwd,
    };

    // If the lifecycle has a setup command, emit it for async execution.
    if let Some(ref setup_cmd) = setup_command {
        let rendered = match setup_cmd {
            jinn_preferences_config::schemas::LifecycleCommand::Shell(cmd) => {
                let template = CommandTemplate::parse(cmd);
                if args.is_empty() {
                    cmd.clone()
                } else {
                    template.render(args)
                }
            }
            jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => id.to_string(),
        };

        let mut result = IntentResult::empty()
            .with_message(PersistSession {
                session_id: new_id.clone(),
            })
            .with_message(PushChatEntry {
                session_id: new_id.clone(),
                entry: crate::feat::session::session_actor::setup_running_msg(),
            })
            .with_message(RunSessionSetup {
                session_id: new_id.clone(),
                command: rendered,
                args: args.to_vec(),
                lifecycle_command: Some(setup_cmd.clone()),
            })
            .with_message(created_event);

        // Notify the MCP coordinator of any config-seeded enablement so the
        // new session's servers spawn without a picker visit. When nothing is
        // auto-enabled, the message is skipped (nothing to reconcile).
        if seed.has_auto_enabled_mcp() {
            result = result.with_message(jinn_mcp_msg::McpEnablementChanged {
                session_id: new_id,
                enabled: seed.enabled_mcp,
            });
        }

        return result;
    }

    // No setup command — the starting CWD was already set on the new session
    // before insert (above), so there's nothing more to do here.
    if !seed.has_auto_enabled_mcp() {
        return IntentResult::new_message(created_event);
    }
    IntentResult::new_message(created_event).with_message(jinn_mcp_msg::McpEnablementChanged {
        session_id: new_id,
        enabled: seed.enabled_mcp,
    })
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
    let args: Vec<String> = if arg_state.text.input.trim().is_empty() {
        vec![]
    } else {
        parse_quoted_args(&arg_state.text.input)
    };

    // Pop ArgInput scope.
    state.frontend.scope_pop();
    // Clear arg input state.
    state.frontend.arg_input = crate::common::app_state::ArgInputState::default();

    handle_session_lifecycle_setup(state, &lifecycle_name, &args, None)
}

/// Handle character insertion in the arg input popup.
pub fn handle_arg_input_insert_char(state: &mut AppState, ch: char) -> IntentResult {
    state.frontend.arg_input.text.insert_char(ch);
    IntentResult::empty()
}

/// Handle grapheme deletion in the arg input popup.
pub fn handle_arg_input_delete(state: &mut AppState) -> IntentResult {
    state.frontend.arg_input.text.delete();
    IntentResult::empty()
}

/// Handle forward delete in the arg input popup (deletes the grapheme at/after cursor).
pub fn handle_arg_input_delete_forward(state: &mut AppState) -> IntentResult {
    state.frontend.arg_input.text.delete_forward();
    IntentResult::empty()
}

/// Handle cursor left in the arg input popup.
pub fn handle_arg_input_cursor_left(state: &mut AppState) -> IntentResult {
    state.frontend.arg_input.text.cursor_left();
    IntentResult::empty()
}

/// Handle cursor right in the arg input popup.
pub fn handle_arg_input_cursor_right(state: &mut AppState) -> IntentResult {
    state.frontend.arg_input.text.cursor_right();
    IntentResult::empty()
}

/// Handles `PasteText` in arg input scope - bulk inserts pasted text at the cursor.
pub fn handle_arg_input_paste(state: &mut AppState, text: &str) -> IntentResult {
    state.frontend.arg_input.text.paste(text);
    IntentResult::empty()
}

/// Handle `Intent::SidebarSessionRerunSetup`.
///
/// Re-runs the lifecycle setup command for the sidebar-selected session.
/// Only valid when the session's `lifecycle_script_state` is `NothingRan`.
/// If the session has no lifecycle, no setup command, or is not in `NothingRan`,
/// this is a no-op.
///
/// # Panics
///
/// Panics if the sidebar session entry at the selected index is missing
/// from the session map (indicates a corrupt UI state).
pub fn handle_session_rerun_setup(state: &mut AppState) -> IntentResult {
    if validate_session_close(state).is_err() {
        return IntentResult::empty();
    }

    let index = state
        .frontend
        .with_sections(|s| s.sessions.selected_index, || None)
        .unwrap();
    let sessions = sorted_open_sessions(state);
    let Some(target_session) = sessions.get(index) else {
        return IntentResult::empty();
    };
    let target_id = target_session.id.clone();
    let (setup_command, lifecycle_args) = {
        let session = state.session.get(&target_id);
        let Some(session) = session else {
            return IntentResult::empty();
        };

        if session.lifecycle_script_state() != LifecycleScriptState::NothingRan {
            return IntentResult::empty();
        }

        let lifecycle_name = session.lifecycle_name().map(String::from);
        let args = session.lifecycle_args().to_vec();
        let setup = lifecycle_name.as_deref().and_then(|name| {
            state
                .frontend
                .preferences
                .session_lifecycles
                .iter()
                .find(|l| l.name == name)
                .and_then(|l| l.setup.clone())
        });
        (setup, args)
    };

    let Some(ref setup_cmd) = setup_command else {
        return IntentResult::empty();
    };

    let rendered = match setup_cmd {
        jinn_preferences_config::schemas::LifecycleCommand::Shell(cmd) => {
            let template = CommandTemplate::parse(cmd);
            if lifecycle_args.is_empty() {
                cmd.clone()
            } else {
                template.render(&lifecycle_args)
            }
        }
        jinn_preferences_config::schemas::LifecycleCommand::Builtin(id) => id.to_string(),
    };

    IntentResult::empty()
        .with_message(PushChatEntry {
            session_id: target_id.clone(),
            entry: setup_running_msg(),
        })
        .with_message(RunSessionSetup {
            session_id: target_id,
            command: rendered,
            args: lifecycle_args,
            lifecycle_command: Some(setup_cmd.clone()),
        })
}

/// Resolve and render the teardown command for a session by ID.
///
/// Reads the session's `lifecycle_name` + `lifecycle_args`, looks up the named
/// lifecycle in `frontend.preferences`, takes its `teardown` command, and renders
/// it (replaying the stored args). Returns `None` when the session doesn't exist,
/// has no lifecycle name, or the named lifecycle has no teardown command.
///
/// This is the session-ID-keyed counterpart of the sidebar teardown handler —
/// UI-coupled callers resolve the selected session's ID, then delegate here.
pub fn build_run_session_teardown(
    state: &AppState,
    session_id: &SessionId,
) -> Option<RunSessionTeardown> {
    use jinn_preferences_config::schemas::LifecycleCommand;

    let (teardown_command, lifecycle_args) = {
        let session = state.session.get(session_id)?;
        let lifecycle_name = session.lifecycle_name().map(String::from);
        let args = session.lifecycle_args().to_vec();
        let teardown = lifecycle_name.as_deref().and_then(|name| {
            state
                .frontend
                .preferences
                .session_lifecycles
                .iter()
                .find(|l| l.name == name)
                .and_then(|l| l.teardown.clone())
        });
        (teardown, args)
    };

    let teardown_cmd = teardown_command.as_ref()?;
    let rendered = match teardown_cmd {
        LifecycleCommand::Shell(cmd) => {
            let template = CommandTemplate::parse(cmd);
            if lifecycle_args.is_empty() {
                cmd.clone()
            } else {
                template.render(&lifecycle_args)
            }
        }
        LifecycleCommand::Builtin(id) => id.to_string(),
    };

    Some(RunSessionTeardown {
        session_id: session_id.clone(),
        command: rendered,
        args: lifecycle_args,
    })
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
    IntentResult::new_message(CloseSession {
        session_id: closing_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::common::app_state::AppState;
    use crate::protocol::ChatEntry;
    use jinn_preferences_config::schemas::SessionLifecycle;

    #[rstest::rstest]
    fn session_lifecycle_setup_with_blank_creates_session() {
        // Given default state (no lifecycles configured).
        let mut state = AppState::default_with_scope_focus();
        // Set the active session's cwd to a distinct value so inheritance is
        // distinguishable from default_cwd() (the app launch dir).
        let inherited_cwd = std::path::PathBuf::from("/tmp/inherited-project");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        let old_id = state.session.active_session_id().clone();

        // When handling SessionLifecycleSetup with blank lifecycle.
        let result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the old empty session is preserved (no auto-close).
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
        // And one message emitted (SessionCreated).
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("SessionCreated"));
        // And the session has no lifecycle name.
        assert!(state.active_session().lifecycle_name().is_none());
        // And the new session inherited the active session's CWD, not the app
        // launch dir.
        assert_eq!(state.active_session().cwd(), inherited_cwd);
    }

    #[rstest::rstest]
    fn explicit_cwd_override_overrides_inherited_cwd() {
        // Given a state whose active session has a distinct CWD and a
        // pending creation stashed on the frontend (as the project picker does).
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/active-project"));
        state.frontend.pending_creation =
            Some(crate::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: std::path::PathBuf::from("/tmp/override-project"),
                starting_cwd: std::path::PathBuf::from("/tmp/override-project"),
            });

        // When handling SessionLifecycleSetup with an explicit cwd override.
        let _result = handle_session_lifecycle_setup(
            &mut state,
            "",
            &[],
            Some(std::path::Path::new("/tmp/explicit-dir")),
        );

        // Then the new session's CWD is the explicit override, not the active
        // session's CWD and not the stashed starting CWD.
        assert_eq!(
            state.active_session().cwd(),
            std::path::Path::new("/tmp/explicit-dir"),
        );
        // And the stash is cleared (never leaks to the next creation).
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn pending_creation_cwd_is_used_when_no_explicit_cwd_given() {
        // Given a state whose active session has a distinct CWD and a
        // pending creation stashed on the frontend.
        let mut state = AppState::default_with_scope_focus();
        state
            .active_session_mut()
            .set_cwd(std::path::PathBuf::from("/tmp/active-project"));
        state.frontend.pending_creation =
            Some(crate::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: std::path::PathBuf::from("/tmp/override-project"),
                starting_cwd: std::path::PathBuf::from("/tmp/override-project"),
            });

        // When handling SessionLifecycleSetup with no explicit cwd.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new session's CWD is the stashed starting CWD, not the
        // active session's CWD.
        assert_eq!(
            state.active_session().cwd(),
            std::path::Path::new("/tmp/override-project"),
        );
        // And the stash is cleared after consumption.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn setup_stamps_project_from_pending_creation() {
        // Given a state with a pending creation stashed from the projects UI.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pending_creation =
            Some(crate::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: std::path::PathBuf::from("/home/user/projects/jinn"),
                starting_cwd: std::path::PathBuf::from("/home/user/projects/jinn"),
            });

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new active session is stamped with the stashed project.
        assert_eq!(
            state.active_session().project(),
            Some(std::path::Path::new("/home/user/projects/jinn")),
        );
    }

    #[rstest::rstest]
    fn setup_without_pending_creation_leaves_project_none() {
        // Given a default state with no pending creation stash.
        let mut state = AppState::default_with_scope_focus();

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new active session has no project association.
        assert_eq!(state.active_session().project(), None);
    }

    #[rstest::rstest]
    fn setup_consumes_stash_exactly_once() {
        // Given a state that already consumed a pending creation.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.pending_creation =
            Some(crate::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: std::path::PathBuf::from("/tmp/first-project"),
                starting_cwd: std::path::PathBuf::from("/tmp/first-project"),
            });
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // When creating a second session (the stash is now None).
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the second session has no project association (no leak from
        // the first creation).
        assert_eq!(state.active_session().project(), None);
        // And the stash is still clear.
        assert!(state.frontend.pending_creation.is_none());
    }

    #[rstest::rstest]
    fn scripted_lifecycle_setup_pre_seeds_inherited_cwd_in_memory() {
        // Given a state whose active session has a distinct CWD, and a
        // lifecycle with a setup_command (so the script path is taken).
        let mut state = AppState::default_with_scope_focus();
        let inherited_cwd = std::path::PathBuf::from("/tmp/inherited-project");
        state.active_session_mut().set_cwd(inherited_cwd.clone());
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "echo /tmp/workdir".to_owned(),
                )),
                teardown: None,
            });

        // When handling SessionLifecycleSetup with the scripted lifecycle.
        let _result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None);

        // Then the new session's in-memory CWD is the inherited value
        // (pre-seeded before the actor runs the script). The actor may
        // overwrite it with the script's stdout later, but at creation
        // time the inherited CWD is present.
        assert_eq!(state.active_session().cwd(), inherited_cwd);
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_lifecycle_emits_command() {
        // Given a state with a lifecycle that has a setup_command.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "echo /tmp/workdir".to_owned(),
                )),
                teardown: None,
            });

        // When handling SessionLifecycleSetup.
        let result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None);

        // Then a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
        // And the session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And PersistSession, PushChatEntry, RunSessionSetup, SessionCreated are emitted.
        assert_eq!(result.message_names.len(), 4);
        assert!(result.message_names[0].contains("PersistSession"));
        assert!(result.message_names[1].contains("PushChatEntry"));
        assert!(result.message_names[2].contains("RunSessionSetup"));
        assert!(result.message_names[3].contains("SessionCreated"));
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_with_args_renders_command() {
        // Given a lifecycle with $1 in the setup_command.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1".to_owned(),
                )),
                teardown: None,
            });

        // When handling SessionLifecycleSetup with args.
        let result = handle_session_lifecycle_setup(
            &mut state,
            "fossil branch",
            &["my-branch".to_owned()],
            None,
        );

        // Then PersistSession is emitted first.
        assert!(result.message_names[0].contains("PersistSession"));
        // And RunSessionSetup is emitted third with rendered args.
        assert!(result.message_names[2].contains("RunSessionSetup"));
        // And the session has the args stored.
        assert_eq!(
            state.active_session().lifecycle_args(),
            &["my-branch".to_owned()]
        );
    }

    #[rstest::rstest]
    fn session_lifecycle_setup_clears_overlays_and_pushes_input() {
        // Given a state with a picker overlay.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(crate::common::app_state::FocusScope::Picker {
                kind: crate::protocol::PickerKind::Provider,
            });

        // When handling SessionLifecycleSetup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then overlays are cleared and Input scope is pushed.
        assert!(matches!(
            state.frontend.scope(),
            crate::common::app_state::FocusScope::Input
        ));
    }

    #[rstest::rstest]
    fn session_close_without_lifecycle_emits_close_session() {
        // Given a state with two sessions.
        let mut state = AppState::default_with_scope_focus();
        let second_session = ChatSessionState::new();
        let second_id = second_session.session_id().clone();
        state.session.insert(second_session);
        state.session.set_active(second_id);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted for the closed session.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_close_with_teardown_emits_close_session() {
        // Given a session with a lifecycle that has a teardown_command.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "echo /tmp/workdir".to_owned(),
                )),
                teardown: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "cleanup.sh $1".to_owned(),
                )),
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
        assert!(state.session.contains(&session_id));
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_close_last_session_emits_close_session() {
        // Given a state with only one session.
        let mut state = AppState::default_with_scope_focus();
        let _session_id = state.session.active_session_id().clone();
        assert_eq!(state.session.session_count(), 1);

        // When handling SessionClose.
        let result = handle_session_close(&mut state);

        // Then a CloseSession command is emitted.
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("CloseSession"));
    }

    #[rstest::rstest]
    fn session_new_delegates_to_blank_lifecycle() {
        // Given default state.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("old"));

        // When handling SessionNew (delegates to blank lifecycle setup).
        let result = crate::feat::session::intent::handle_session_new(&mut state);

        // Then a new session is created (same behavior as before).
        assert_ne!(*state.session.active_session_id(), old_id);
        // And one message emitted (SessionCreated).
        assert_eq!(result.message_names.len(), 1);
        assert!(result.message_names[0].contains("SessionCreated"));
    }

    #[rstest::rstest]
    fn arg_input_confirm_splits_input_into_args() {
        // Given an arg input state with text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "fossil branch".to_owned();
        state.frontend.arg_input.text.input = "my-branch target-dir".to_owned();
        state.frontend.arg_input.text.cursor_pos = state.frontend.arg_input.text.input.len();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1 $2".to_owned(),
                )),
                teardown: None,
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
        // Then PersistSession is emitted first.
        assert!(result.message_names[0].contains("PersistSession"));
        // And RunSessionSetup is emitted third.
        assert!(result.message_names[2].contains("RunSessionSetup"));
        // And arg input state is cleared.
        assert!(state.frontend.arg_input.lifecycle_name.is_empty());
    }

    #[rstest::rstest]
    fn arg_input_confirm_rejects_empty_input_when_params_needed() {
        // Given an arg input state with empty input for a template that expects $1.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = String::new();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1".to_owned(),
                )),
                teardown: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input with empty input.
        let result = handle_arg_input_confirm(&mut state);

        // Then no command is emitted (validation rejects empty input).
        assert!(result.message_names.is_empty());
        // And no session was created (state unchanged).
        assert_eq!(*state.session.active_session_id(), old_id);
        // And arg input state is NOT cleared (user stays in popup).
        assert_eq!(state.frontend.arg_input.lifecycle_name, "test");
    }

    #[rstest::rstest]
    fn arg_input_insert_char_appends_to_input() {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = String::new();
        state.frontend.arg_input.text.cursor_pos = 0;

        let _result = handle_arg_input_insert_char(&mut state, 'a');

        assert_eq!(state.frontend.arg_input.text.input, "a");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_delete_removes_last_grapheme() {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 3;

        let _result = handle_arg_input_delete(&mut state);

        assert_eq!(state.frontend.arg_input.text.input, "ab");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_left_moves_cursor() {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 3;

        let _result = handle_arg_input_cursor_left(&mut state);

        assert_eq!(state.frontend.arg_input.text.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_moves_cursor() {
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 0;

        let _result = handle_arg_input_cursor_right(&mut state);

        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_reaches_end_of_input() {
        // Given cursor one grapheme before end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "ab".to_owned();
        state.frontend.arg_input.text.cursor_pos = 1;

        // When moving right.
        let _result = handle_arg_input_cursor_right(&mut state);

        // Then cursor advances to end (input.len()).
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_at_end_stays() {
        // Given cursor already at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 3;

        // When moving right.
        let _result = handle_arg_input_cursor_right(&mut state);

        // Then cursor stays at end.
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 3);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_removes_char_after_cursor() {
        // Given input "abc" with cursor at position 1 (after 'a').
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 1;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then 'b' is removed, cursor stays at 1.
        assert_eq!(state.frontend.arg_input.text.input, "ac");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_at_end_does_nothing() {
        // Given input "abc" with cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 3;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then input is unchanged.
        assert_eq!(state.frontend.arg_input.text.input, "abc");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 3);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_at_start_removes_first_char() {
        // Given input "abc" with cursor at start.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 0;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then 'a' is removed.
        assert_eq!(state.frontend.arg_input.text.input, "bc");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn validate_arg_input_accepts_sufficient_args() {
        // Given a state with a $1 $2 lifecycle and two args provided.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = "foo bar".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1 $2".to_owned(),
                )),
                teardown: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_rejects_insufficient_args() {
        // Given a state with a $1 $2 lifecycle and only one arg.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = "foo".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1 $2".to_owned(),
                )),
                teardown: None,
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
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "blank".to_owned();
        state.frontend.arg_input.text.input = String::new();

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes (no params to fill).
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_accepts_splat_without_numbered_params() {
        // Given a state with a $@ lifecycle and any args.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = "anything".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $@".to_owned(),
                )),
                teardown: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation passes (splat accepts any number).
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    fn validate_arg_input_rejects_when_named_param_missing() {
        // Given a state with a <branch> <target> lifecycle and only one arg.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = "my-branch".to_owned();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh <branch> <target>".to_owned(),
                )),
                teardown: None,
            });

        // When validating.
        let result = validate_arg_input(&state);

        // Then validation fails.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    fn arg_input_confirm_accepts_sufficient_args() {
        // Given a state with a $1 $2 lifecycle and both args provided.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "test".to_owned();
        state.frontend.arg_input.text.input = "foo bar".to_owned();
        state.frontend.arg_input.text.cursor_pos = 7;
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1 $2".to_owned(),
                )),
                teardown: None,
            });
        let old_id = state.session.active_session_id().clone();

        // When confirming arg input.
        let result = handle_arg_input_confirm(&mut state);

        // Then a command is emitted with the rendered args.
        assert!(
            !result.message_names.is_empty(),
            "command should be emitted"
        );
        // Then PersistSession is emitted first.
        assert!(result.message_names[0].contains("PersistSession"));
        // And RunSessionSetup is emitted third.
        assert!(result.message_names[2].contains("RunSessionSetup"));
        // And a new session is created.
        assert_ne!(*state.session.active_session_id(), old_id);
    }

    #[rstest::rstest]
    fn arg_input_confirm_treats_quoted_input_as_single_arg() {
        // Given an arg input state with quoted text.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.lifecycle_name = "fossil branch".to_owned();
        state.frontend.arg_input.text.input = r#""my branch" target"#.to_owned();
        state.frontend.arg_input.text.cursor_pos = state.frontend.arg_input.text.input.len();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "script.sh $1 $2".to_owned(),
                )),
                teardown: None,
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
        // Then PersistSession is emitted first.
        assert!(result.message_names[0].contains("PersistSession"));
        // And RunSessionSetup is emitted third.
        assert!(result.message_names[2].contains("RunSessionSetup"));
    }

    #[rstest::rstest]
    fn empty_session_is_preserved_on_new_session() {
        // Given default state with a single empty session.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        assert!(state.active_session().is_empty());

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the old empty session is preserved (no auto-close).
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
    }

    #[rstest::rstest]
    fn session_with_history_is_preserved_on_new_session() {
        // Given an active session with history.
        let mut state = AppState::default_with_scope_focus();
        let old_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .push_entry(ChatEntry::user("hello"));

        // When creating a new session.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the old session is preserved.
        assert!(state.session.contains(&old_id));
        // And two sessions exist.
        assert_eq!(state.session.session_count(), 2);
        // And the new session is active.
        assert_ne!(*state.session.active_session_id(), old_id);
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_reasoning_effort_from_global_default() {
        // Given a global default effort of High.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.app_state.reasoning_effort = Some(crate::ReasoningEffort::High);

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new session owns the seeded effort (a copy, not a live reference).
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            Some(crate::ReasoningEffort::High),
            "new session should be seeded from the global default"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_none_reasoning_effort_when_global_unset() {
        // Given no global default effort.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.app_state.reasoning_effort = None;

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new session's effort is None (provider decides).
        assert_eq!(
            state.active_session().profile().reasoning_effort,
            None,
            "new session should be seeded as None when global is unset"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_seeds_disabled_tools_and_skills_from_preferences() {
        // Given preferences disabling a tool and a skill.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.preferences.disabled_tools =
            ["bash"].iter().map(|s| (*s).to_owned()).collect();
        state.frontend.preferences.disabled_skills = ["phased-task-loop"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();

        // When creating a new session via lifecycle setup.
        let _result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new session carries both disablement sets.
        assert!(state.active_session().disabled_tools().contains("bash"));
        // And the skill too.
        assert!(
            state
                .active_session()
                .disabled_skills()
                .contains("phased-task-loop")
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_with_auto_enabled_mcp_returns_enablement_message() {
        // Given preferences with one auto-enabled MCP server.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.preferences.mcp_server = [(
            "excalimate".to_owned(),
            jinn_mcp_msg::McpServerConfig {
                command: Some("npx".to_owned()),
                auto_enable: true,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();

        // When creating a new session (blank lifecycle).
        let result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the new session has the server enabled.
        assert!(state.active_session().is_mcp_server_enabled("excalimate"));
        // And an McpEnablementChanged message is emitted so the coordinator
        // spawns the connection.
        assert!(
            result
                .message_names
                .iter()
                .any(|n| n.contains("McpEnablementChanged"))
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_without_auto_enable_emits_no_enablement_message() {
        // Given preferences with a server that is NOT auto-enabled.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.preferences.mcp_server = [(
            "manual".to_owned(),
            jinn_mcp_msg::McpServerConfig {
                command: Some("npx".to_owned()),
                auto_enable: false,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();

        // When creating a new session.
        let result = handle_session_lifecycle_setup(&mut state, "", &[], None);

        // Then the server is not enabled on the new session.
        assert!(!state.active_session().is_mcp_server_enabled("manual"));
        // And no enablement message is emitted.
        assert!(
            !result
                .message_names
                .iter()
                .any(|n| n.contains("McpEnablementChanged"))
        );
    }

    #[rstest::rstest]
    fn scripted_lifecycle_setup_with_auto_enable_attaches_enablement_message() {
        // Given a scripted lifecycle and one auto-enabled server.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "echo /tmp/workdir".to_owned(),
                )),
                teardown: None,
            });
        state.frontend.preferences.mcp_server = [(
            "excalimate".to_owned(),
            jinn_mcp_msg::McpServerConfig {
                command: Some("npx".to_owned()),
                auto_enable: true,
                ..Default::default()
            },
        )]
        .into_iter()
        .collect();

        // When creating a session with the scripted lifecycle.
        let result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None);

        // Then the enablement message follows SessionCreated in the chain.
        let created_idx = result
            .message_names
            .iter()
            .position(|n| n.contains("SessionCreated"))
            .expect("SessionCreated emitted");
        let enablement_idx = result
            .message_names
            .iter()
            .position(|n| n.contains("McpEnablementChanged"))
            .expect("enablement emitted for scripted path");
        assert!(
            enablement_idx > created_idx,
            "enablement must trail SessionCreated"
        );
    }

    #[rstest::rstest]
    fn lifecycle_setup_preserves_empty_session_when_creating_lifecycle_session() {
        // Given a single empty session (app just started).
        let mut state = AppState::default_with_scope_focus();
        assert_eq!(state.session.session_count(), 1);

        // When creating a new session with a lifecycle.
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "echo /tmp/workdir".to_owned(),
                )),
                teardown: None,
            });
        let result = handle_session_lifecycle_setup(&mut state, "fossil branch", &[], None);

        // Then both sessions exist (old empty one is preserved).
        assert_eq!(state.session.session_count(), 2);
        // And the new session has the lifecycle name.
        assert_eq!(
            state.active_session().lifecycle_name(),
            Some("fossil branch")
        );
        // And PersistSession, PushChatEntry, then RunSessionSetup are emitted.
        assert!(result.message_names[0].contains("PersistSession"));
        assert!(result.message_names[1].contains("PushChatEntry"));
        assert!(result.message_names[2].contains("RunSessionSetup"));
    }

    #[rstest::rstest]
    fn arg_input_delete_multi_byte_grapheme_at_boundary() {
        // Given input with a multi-byte emoji at the start and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "ab\u{1F600}".to_owned(); // "ab😀"
        state.frontend.arg_input.text.cursor_pos = state.frontend.arg_input.text.input.len();

        // When deleting backward.
        let _result = handle_arg_input_delete(&mut state);

        // Then the emoji (4 bytes) is removed and cursor moves back by 4.
        assert_eq!(state.frontend.arg_input.text.input, "ab");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 2);
    }

    #[rstest::rstest]
    fn arg_input_delete_at_position_zero_does_nothing() {
        // Given input with cursor at position 0.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 0;

        // When deleting backward.
        let _result = handle_arg_input_delete(&mut state);

        // Then nothing changes.
        assert_eq!(state.frontend.arg_input.text.input, "abc");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn arg_input_delete_forward_multi_byte_grapheme() {
        // Given input with a multi-byte emoji after cursor.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "\u{1F600}bc".to_owned(); // "😀bc"
        state.frontend.arg_input.text.cursor_pos = 0;

        // When forward deleting.
        let _result = handle_arg_input_delete_forward(&mut state);

        // Then the emoji (4 bytes) is removed.
        assert_eq!(state.frontend.arg_input.text.input, "bc");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 0);
    }

    #[rstest::rstest]
    fn arg_input_cursor_left_multi_byte_grapheme() {
        // Given input with a multi-byte emoji at the end and cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "a\u{1F600}".to_owned(); // "a😀"
        state.frontend.arg_input.text.cursor_pos = state.frontend.arg_input.text.input.len(); // 5

        // When moving left.
        let _result = handle_arg_input_cursor_left(&mut state);

        // Then cursor moves to the start of the emoji (position 1).
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 1);
    }

    #[rstest::rstest]
    fn arg_input_cursor_right_multi_byte_grapheme() {
        // Given input with a multi-byte emoji at position 1 and cursor at 1.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "a\u{1F600}b".to_owned(); // "a😀b"
        state.frontend.arg_input.text.cursor_pos = 1;

        // When moving right.
        let _result = handle_arg_input_cursor_right(&mut state);

        // Then cursor moves past the emoji (4 bytes) to position 5.
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 5);
    }

    #[rstest::rstest]
    fn arg_input_paste_updates_cursor_position() {
        // Given input with existing text and cursor at position 2.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abcd".to_owned();
        state.frontend.arg_input.text.cursor_pos = 2;

        // When pasting text at cursor.
        let _result = handle_arg_input_paste(&mut state, "XY");

        // Then text is inserted at cursor and cursor advances by pasted length.
        assert_eq!(state.frontend.arg_input.text.input, "abXYcd");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 4); // 2 + 2
    }

    #[rstest::rstest]
    fn arg_input_paste_at_end_appends() {
        // Given input with cursor at end.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abc".to_owned();
        state.frontend.arg_input.text.cursor_pos = 3;

        // When pasting.
        let _result = handle_arg_input_paste(&mut state, "XYZ");

        // Then text is appended.
        assert_eq!(state.frontend.arg_input.text.input, "abcXYZ");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 6);
    }

    #[rstest::rstest]
    fn arg_input_paste_empty_does_nothing() {
        // Given input with cursor at position 2.
        let mut state = AppState::default_with_scope_focus();
        state.frontend.arg_input.text.input = "abcd".to_owned();
        state.frontend.arg_input.text.cursor_pos = 2;

        // When pasting empty text.
        let _result = handle_arg_input_paste(&mut state, "");

        // Then nothing changes.
        assert_eq!(state.frontend.arg_input.text.input, "abcd");
        assert_eq!(state.frontend.arg_input.text.cursor_pos, 2);
    }

    // -----------------------------------------------------------------------
    // Re-run setup tests
    // -----------------------------------------------------------------------

    fn setup_rerun_state() -> AppState {
        use jinn_preferences_config::schemas::LifecycleCommand;

        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test-lifecycle".to_owned(),
                description: None,
                setup: Some(LifecycleCommand::Shell("echo /tmp/workdir".to_owned())),
                teardown: None,
            });
        state
            .active_session_mut()
            .set_lifecycle_name(Some("test-lifecycle".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(vec!["arg1".to_owned()]);
        state
    }

    #[rstest::rstest]
    #[test]
    fn rerun_setup_noop_when_no_lifecycle() {
        // Given a session with no lifecycle name in NothingRan state.

        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));

        // When handling rerun setup.
        let result = handle_session_rerun_setup(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn rerun_setup_noop_when_no_setup_command() {
        // Given a session with a lifecycle that has no setup command.

        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .update_sections(|s| s.sessions.selected_index = Some(0));
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "test-lifecycle".to_owned(),
                description: None,
                setup: None,
                teardown: None,
            });
        state
            .active_session_mut()
            .set_lifecycle_name(Some("test-lifecycle".to_owned()));

        // When handling rerun setup.
        let result = handle_session_rerun_setup(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn rerun_setup_noop_when_not_nothing_ran() {
        // Given a session that has already run setup (SetupRan state).
        let mut state = setup_rerun_state();
        state.active_session_mut().advance_lifecycle_after_setup();

        // When handling rerun setup.
        let result = handle_session_rerun_setup(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn rerun_setup_noop_when_no_selection() {
        // Given a sidebar sessions view with no selected index.

        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        // No selected_index set.

        // When handling rerun setup.
        let result = handle_session_rerun_setup(&mut state);

        // Then no commands are emitted.
        assert!(result.message_names.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn rerun_setup_emits_commands_when_valid() {
        // Given a session in NothingRan with a setup command.
        let mut state = setup_rerun_state();
        let _session_id = state.session.active_session_id().clone();

        // When handling rerun setup.
        let result = handle_session_rerun_setup(&mut state);

        // Then two commands are emitted.
        assert_eq!(result.message_names.len(), 2);
        // And the first is PushChatEntry.
        assert!(result.message_names[0].contains("PushChatEntry"));
        // And the second is RunSessionSetup.
        assert!(result.message_names[1].contains("RunSessionSetup"));
    }

    #[rstest::rstest]
    #[test]
    fn abandon_via_enter_normal_mode_clears_pending_creation() {
        // Given a state with a pending session creation stashed from a
        // project-picker confirm (midway through the lifecycle/args chain).
        let mut state = AppState::default_with_scope_focus();
        let active_cwd = state.active_session().cwd().to_path_buf();
        state.frontend.pending_creation =
            Some(crate::feat::ui::frontend_state::PendingSessionCreation {
                project_dir: std::path::PathBuf::from("/tmp/project-a"),
                starting_cwd: std::path::PathBuf::from("/tmp/project-a"),
            });

        // When abandoning the chain via ESC (EnterNormalMode).
        let _result = crate::feat::chat_input::intent::handle_enter_normal_mode(&mut state);

        // Then the stash is cleared so it never leaks into a future
        // `n`/`N`.
        assert!(state.frontend.pending_creation.is_none());
        // And the active session's CWD is unchanged (no side-channel mutation).
        assert_eq!(state.active_session().cwd(), active_cwd);
    }

    #[rstest::rstest]
    fn build_run_session_teardown_renders_command_with_args() {
        // Given a session with a lifecycle that has a teardown command.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: None,
                teardown: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "cleanup.sh $1".to_owned(),
                )),
            });
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(vec!["my-branch".to_owned()]);

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id);

        // Then a rendered RunSessionTeardown is returned.
        let msg = msg.expect("teardown command should be built");
        assert_eq!(msg.session_id, session_id);
        assert_eq!(msg.command, "cleanup.sh my-branch");
        assert_eq!(msg.args, vec!["my-branch".to_owned()]);
    }

    #[rstest::rstest]
    fn build_run_session_teardown_returns_none_without_teardown_command() {
        // Given a session whose lifecycle has no teardown command.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "blank".to_owned(),
                description: None,
                setup: None,
                teardown: None,
            });
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("blank".to_owned()));

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id);

        // Then None is returned.
        assert!(msg.is_none());
    }

    #[rstest::rstest]
    fn build_run_session_teardown_returns_none_without_lifecycle_name() {
        // Given a session with no lifecycle name.
        let state = AppState::default_with_scope_focus();
        let session_id = state.session.active_session_id().clone();

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id);

        // Then None is returned.
        assert!(msg.is_none());
    }

    #[rstest::rstest]
    fn build_run_session_teardown_renders_positional_arg_from_stored_args() {
        // Given a lifecycle teardown with $1 and a session storing the arg.
        let mut state = AppState::default_with_scope_focus();
        state
            .frontend
            .preferences
            .session_lifecycles
            .push(SessionLifecycle {
                name: "fossil branch".to_owned(),
                description: None,
                setup: None,
                teardown: Some(jinn_preferences_config::schemas::LifecycleCommand::Shell(
                    "cleanup.sh $1".to_owned(),
                )),
            });
        let session_id = state.session.active_session_id().clone();
        state
            .active_session_mut()
            .set_lifecycle_name(Some("fossil branch".to_owned()));
        state
            .active_session_mut()
            .set_lifecycle_args(vec!["feature-x".to_owned()]);

        // When building the teardown command.
        let msg = build_run_session_teardown(&state, &session_id);

        // Then the $1 positional is rendered with the stored arg.
        let msg = msg.expect("teardown command should be built");
        assert_eq!(msg.command, "cleanup.sh feature-x");
    }
}
