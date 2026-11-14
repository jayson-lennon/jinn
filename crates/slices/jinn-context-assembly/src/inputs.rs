//! State capture for the stateless context-assembly service.

use jinn_bot_msg::is_bot_tool_definition;
use jinn_context_assembly_msg::AssemblyInputs;
use jinn_core_types::{DEFAULT_PERSONA_NAME, SessionId};
use jinn_kernel::common::app_state::AppState;
use jinn_session_state::AssemblySessionProjection;

/// Whether `session` is offered the tool definition `def`.
///
/// A non-bot sees none of the bot-only tools. A bot sees `conclude`
/// always — recording a conclusion needs no parent — and `notify_parent`
/// only when it has one to notify.
fn bot_sees_tool(
    session: &AssemblySessionProjection,
    def: &jinn_core_types::tool_types::ToolDefinition,
) -> bool {
    if !session.is_bot {
        return !is_bot_tool_definition(def);
    }
    if !is_bot_tool_definition(def) {
        return true;
    }
    session.bot_has_parent || def.name == "conclude"
}

/// Builds a coherent assembly request from the current application state.
///
/// The caller must hold the `State` read guard while invoking this function.
/// Session-owned values are captured together through
/// [`AssemblySessionProjection`]; persona and tool definitions are resolved
/// from their attached cells while the same guard is still held.
/// `handoff_enabled` comes from the configuration layer, read live — a
/// jinn.toml edit applies at the next assembly.
///
/// Missing persona or tool cells degrade to empty assembly values rather than
/// failing turn dispatch.
#[must_use]
pub fn build_assembly_inputs(
    state: &AppState,
    config: &jinn_config::ConfigLayer,
    session_id: &SessionId,
) -> AssemblyInputs {
    let session = AssemblySessionProjection::capture(session_id, state.session(session_id));
    let handoff_enabled = config
        .get::<jinn_preferences_config::schemas::HandoffCompactionConfig>()
        .is_ok_and(|config| config.enabled);

    let persona = state.persona_selection().and_then(|cell| {
        cell.read()
            .resolve_for(&session.persona_name, DEFAULT_PERSONA_NAME)
            .cloned()
    });
    // Bot-only tools are filtered per session, the same way server
    // tools are filtered per provider in `assemble`: they are dropped
    // before the prompt is built, so a session that is not a bot
    // never sees them offered rather than being offered and refused.
    // Of the bot tools, a parentless bot sees `conclude` only —
    // `notify_parent` has no parent to wake, and offering it would be
    // offering a call that can only fail.
    let tools = state
        .tool_registry()
        .map_or_default(|cell| cell.read().tools_for_session(session_id))
        .into_iter()
        .filter(|def| bot_sees_tool(&session, def))
        .collect();

    AssemblyInputs {
        session_id: session.session_id,
        cwd: session.cwd,
        persona,
        history: session.history,
        tools,
        tool_filter: session.tool_filter,
        provider_name: session.provider_name,
        skills: session.skills,
        skill_filter: session.skill_filter,
        loaded_skills: session.loaded_skills,
        context_files: session.context_files,
        static_section: session.static_section,
        handoff_enabled,
    }
}

#[cfg(test)]
mod bot_tool_visibility_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "test module")]

    use super::*;
    use jinn_bot_msg::BOT_TOOL_NAMES;
    use jinn_core_types::tool_types::ToolDefinition;
    use jinn_core_types::{FilterMode, NameFilter};
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;
    use jinn_session_state::ChatSessionState;

    /// Ordinary built-ins every session keeps, bot or not.
    const ORDINARY_TOOL_NAMES: &[&str] = &["read", "bash", "grep"];

    #[rstest::rstest]
    #[test]
    fn an_ordinary_session_is_offered_no_bot_tool() {
        // Given a plain user session with the built-ins registered.
        let (state, session_id) = state_with_session(ChatSessionState::new());

        // When its assembly inputs are built.
        let names = tool_names(&state, &session_id);

        // Then neither bot tool appears. A user session is not a
        // judge and has no parent to wake, so offering the tools invites
        // calls that can only fail.
        for name in BOT_TOOL_NAMES {
            assert!(
                !names.iter().any(|n| n == name),
                "{name} must not be offered to a non-bot session"
            );
        }
        // And the other built-ins are untouched.
        for name in ORDINARY_TOOL_NAMES {
            assert!(names.iter().any(|n| n == name), "{name} must remain");
        }
    }

    #[rstest::rstest]
    #[test]
    fn an_attached_bot_is_offered_both_of_its_tools() {
        // Given an attached bot session.
        let parent = ChatSessionState::new();
        let (state, session_id) = state_with_session(ChatSessionState::new_bot(&parent, true));

        // When its assembly inputs are built.
        let names = tool_names(&state, &session_id);

        // Then both are present. Filtering must not starve the tools a
        // bot exists to use.
        for name in BOT_TOOL_NAMES {
            assert!(names.iter().any(|n| n == name), "{name} must reach a bot");
        }
        // And so are the ordinary ones. The bot filter subtracts from
        // a full list rather than replacing it — a bot that only
        // sees its own two tools cannot read, search or edit anything.
        for name in ORDINARY_TOOL_NAMES {
            assert!(
                names.iter().any(|n| n == name),
                "{name} must remain available to a bot"
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn a_standalone_bot_keeps_conclude_and_loses_notify_parent() {
        // Given a parentless bot session.
        let source = ChatSessionState::new();
        let (state, session_id) =
            state_with_session(ChatSessionState::new_standalone_bot(&source, true));

        // When its assembly inputs are built.
        let names = tool_names(&state, &session_id);

        // Then `conclude` is still offered — recording a conclusion needs
        // no parent — while `notify_parent` is withheld, because waking a
        // parent that does not exist is a call that can only fail.
        assert!(
            names.iter().any(|n| n == "conclude"),
            "a standalone bot must keep conclude"
        );
        assert!(
            !names.iter().any(|n| n == "notify_parent"),
            "a standalone bot must not be offered notify_parent"
        );
        // And the ordinary tools are untouched.
        for name in ORDINARY_TOOL_NAMES {
            assert!(
                names.iter().any(|n| n == name),
                "{name} must remain available to a standalone bot"
            );
        }
    }

    /// An allow filter narrows which of the session's own tools it may use.
    /// It must not be read as "these are the only tools that exist" — that
    /// reading would strip a bot of `conclude` and `notify_parent`, the
    /// two tools it exists to call.
    #[rstest::rstest]
    #[test]
    fn an_allow_filtered_bot_keeps_its_own_tools() {
        // Given a bot permitted exactly one ordinary tool.
        let parent = ChatSessionState::new();
        let mut bot = ChatSessionState::new_bot(&parent, true);
        bot.set_tool_filter(Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["read".to_owned()].into_iter().collect(),
        }));
        let (state, session_id) = state_with_session(bot);

        // When its assembly inputs are built.
        let names = tool_names(&state, &session_id);

        // Then both bot tools are still offered.
        for name in BOT_TOOL_NAMES {
            assert!(
                names.iter().any(|n| n == name),
                "{name} must survive an allow filter on a bot"
            );
        }
    }

    /// The unlisted *ordinary* tools are what the filter withholds. Without
    /// this half of the test the case above would also pass if the filter were
    /// ignored entirely.
    #[rstest::rstest]
    #[test]
    fn an_allow_filtered_bot_loses_the_tools_it_did_not_name() {
        // Given a bot permitted exactly one ordinary tool.
        let parent = ChatSessionState::new();
        let mut bot = ChatSessionState::new_bot(&parent, true);
        bot.set_tool_filter(Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["read".to_owned()].into_iter().collect(),
        }));
        let (state, session_id) = state_with_session(bot);

        // When its assembly inputs are built.
        let inputs = build_assembly_inputs(
            &state.read(),
            jinn_config::empty_config_layer(),
            &session_id,
        );

        // Then `read` is permitted and the other ordinary tools are not.
        let filter = inputs.tool_filter.expect("bot carries a filter");
        assert!(filter.permits("read"));
        assert!(!filter.permits("bash"));
        assert!(!filter.permits("grep"));
    }

    /// A state holding `session` as its active session, with the
    /// bot built-ins and one ordinary tool registered.
    fn state_with_session(session: ChatSessionState) -> (State, SessionId) {
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = session.session_id().clone();
        {
            let mut guard = state.write();
            guard.session.insert(session);
            let cell = guard.tool_registry().expect("registry cell attached");
            cell.update(|r| {
                for name in BOT_TOOL_NAMES
                    .iter()
                    .chain(ORDINARY_TOOL_NAMES.iter())
                    .copied()
                {
                    r.global.insert(name.to_owned(), tool(name));
                }
            });
        }
        (state, session_id)
    }

    /// The tool names a session's assembly inputs carry.
    fn tool_names(state: &State, session_id: &SessionId) -> Vec<String> {
        let guard = state.read();
        build_assembly_inputs(&guard, jinn_config::empty_config_layer(), session_id)
            .tools
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    /// A minimal tool definition under `name`.
    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            description: String::new(),
            parameters: serde_json::json!({ "type": "object" }),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        }
    }
}
