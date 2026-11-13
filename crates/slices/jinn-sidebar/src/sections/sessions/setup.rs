//! Re-runs a selected session's lifecycle setup command.

use jinn_kernel::IntentResult;
use jinn_kernel::common::app_state::AppState;
use jinn_preferences_config::schemas::LifecycleCommand;
use jinn_session_history_msg::PushChatEntry;
use jinn_session_lifecycle_msg::{
    LifecycleScriptState, command::RunSessionSetup, command_template::CommandTemplate,
    setup_running_msg,
};

use super::close::validate_session_close;
use jinn_preferences_config::schemas::SessionLifecycle;
use jinn_session_state::fields::LifecycleArgs;

/// Re-runs setup for the selected session when its lifecycle is still unrun.
pub fn handle_session_rerun_setup(
    state: &mut AppState,
    config: &jinn_slices::ConfigLayer,
) -> IntentResult {
    let Some(target_id) = selected_idle_session(state) else {
        return IntentResult::empty();
    };
    let (setup, args) = {
        let Some(session) = state.session.get(&target_id) else {
            return IntentResult::empty();
        };
        if session.lifecycle_script_state() != LifecycleScriptState::NothingRan {
            return IntentResult::empty();
        }
        let lifecycle_name = session.lifecycle_name().map(String::from);
        let args = session.lifecycle_args().clone();
        let setup = lifecycle_name.as_deref().and_then(|name| {
            let lifecycle = config
                .get_list::<SessionLifecycle>()
                .unwrap_or_default()
                .into_iter()
                .find(|lifecycle| lifecycle.name == name)?;
            lifecycle.setup
        });
        (setup, args)
    };
    let Some(setup) = setup else {
        return IntentResult::empty();
    };
    let rendered = render_setup(&setup, &args);
    IntentResult::empty()
        .with_message(PushChatEntry {
            session_id: target_id.clone(),
            entry: setup_running_msg(),
            pin: None,
        })
        .with_message(RunSessionSetup {
            session_id: target_id,
            command: rendered,
            args: args.values(),
            lifecycle_command: Some(setup),
        })
}

fn selected_idle_session(state: &AppState) -> Option<jinn_core_types::SessionId> {
    validate_session_close(state).ok()?;
    state
        .frontend
        .with_sections(|sections| sections.sessions.selected_id.clone(), || None)
}

fn render_setup(setup: &LifecycleCommand, args: &LifecycleArgs) -> String {
    match setup {
        LifecycleCommand::Shell(command) => CommandTemplate::parse(command)
            .render_stored(args.as_pairs())
            .unwrap_or_else(|_| command.clone()),
        LifecycleCommand::Builtin(id) => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    fn state_with_selected_session() -> AppState {
        let state = AppState::default_with_scope_focus();
        let active = state.session.active_session_id().clone();
        state
            .frontend
            .scope_push(jinn_sidebar_msg::SidebarSectionId::Sessions.focus_scope());
        state
            .frontend
            .update_sections(|sections| sections.sessions.selected_id = Some(active));
        state
    }

    #[rstest::rstest]
    fn rerun_setup_is_noop_without_lifecycle() {
        // Given a selected session without a lifecycle.
        let mut state = state_with_selected_session();

        // When rerunning setup.
        let result = handle_session_rerun_setup(&mut state, jinn_slices::empty_config_layer());

        // Then no messages are emitted.
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    fn rerun_setup_is_noop_after_setup_already_ran() {
        // Given a selected session whose setup already ran.
        let mut state = state_with_selected_session();
        state.active_session_mut().advance_lifecycle_after_setup();

        // When rerunning setup.
        let result = handle_session_rerun_setup(&mut state, jinn_slices::empty_config_layer());

        // Then no messages are emitted.
        assert!(result.messages.is_empty());
    }

    #[rstest::rstest]
    fn rerun_setup_emits_status_entry_and_lifecycle_command() {
        // Given a selected session with a configured setup command.
        let mut state = state_with_selected_session();
        let config = jinn_config::testutil::config_layer(
            "[[session_lifecycle.script]]\nname = \"release\"\nsetup_command = \"deploy\"\n",
        );
        state
            .active_session_mut()
            .set_lifecycle_name(Some("release".to_owned()));

        // When rerunning setup.
        let result = handle_session_rerun_setup(&mut state, &config);

        // Then the status entry precedes the lifecycle command.
        assert_eq!(result.message_names.len(), 2);
        assert!(result.message_names[0].ends_with("PushChatEntry"));
        assert!(result.message_names[1].ends_with("RunSessionSetup"));
    }
}
