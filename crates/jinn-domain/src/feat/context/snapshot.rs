//! Caller-side snapshot building for the context-assembly service.
//!
//! The service is pure; whoever dispatches a turn snapshots the session
//! state it can see (under its existing read guard) into an
//! [`AssemblyInputs`] and asks the `context-assembly` trouper actor.
//! Persona selection and tool definitions resolve through their cells.

use jinn_core_types::SessionId;

use crate::common::app_state::AppState;
use crate::feat::context::protocol::inputs::AssembleContext;
use crate::feat::context::protocol::inputs::AssembledResponse;
use crate::feat::context::protocol::inputs::AssemblyInputs;
use crate::feat::session::profile::DEFAULT_PERSONA_NAME;

/// Snapshots everything the assembly service needs for `session_id`.
///
/// Must be called while holding a state read guard — all session data
/// is cloned out of it. Cell-backed state (persona selection, tool
/// registry) resolves through `state.frontend.slices()`; a missing cell
/// degrades to empty inputs rather than failing dispatch.
#[must_use]
pub fn build_assembly_inputs(state: &AppState, session_id: &SessionId) -> AssemblyInputs {
    let session = state.session(session_id);

    let persona = state.persona_selection().and_then(|cell| {
        cell.read()
            .resolve_for(session.persona_name(), DEFAULT_PERSONA_NAME)
            .cloned()
    });

    let tools = state
        .tool_registry()
        .map(|cell| cell.read().tools_for_session(session_id))
        .unwrap_or_default();

    AssemblyInputs {
        session_id: session_id.clone(),
        cwd: session.cwd().to_path_buf(),
        persona,
        history: session.history().to_vec(),
        tools,
        disabled_tools: session.disabled_tools().clone(),
        provider_name: session.model_selection().provider_name().to_owned(),
        skills: session.discovered_skills().to_vec(),
        disabled_skills: session.disabled_skills().clone(),
        loaded_skills: session.loaded_skills(),
        context_files: session.discovered_context_files().to_vec(),
    }
}

/// Ask the `context-assembly` trouper service to assemble the prompt.
///
/// The only assembly entry point for kernel dispatch paths: builds
/// nothing itself, forwards the snapshot over the trouper boundary and
/// deserializes the reply.
///
/// # Errors
///
/// Returns the trouper `AskError` report if the service is absent or
/// the ask times out (dispatch must not proceed without a prompt).
pub async fn assemble_via_service(
    services: &crate::common::services::Services,
    inputs: AssemblyInputs,
) -> Result<jinn_slices::AssembledPrompt, error_stack::Report<trouper::context::AskError>> {
    use trouper::actor::ActorPath;

    const CONTEXT_ASSEMBLY_PATH: &str = "context-assembly";
    const ASSEMBLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    let reply = services
        .trouper_system
        .ask(
            ActorPath::new(CONTEXT_ASSEMBLY_PATH),
            AssembleContext { inputs },
            ASSEMBLE_TIMEOUT,
        )
        .await?;
    let response: AssembledResponse = serde_json::from_value(reply).map_err(|e| {
        error_stack::Report::new(trouper::context::AskError::Unresolved(
            "assembled response deserialization failed".to_owned(),
        ))
        .attach(format!("deserialization failed: {e}"))
    })?;
    Ok(response.prompt)
}

#[cfg(test)]
mod composition_ask_tests {
    use super::*;
    use crate::common::app_state::AppState;
    use crate::common::state::State;
    use crate::feat::context::protocol::inputs::AssembleContext;
    use crate::protocol::ChatEntry;

    #[rstest::rstest]
    #[tokio::test]
    async fn minimal_ask_reproduces_resolution() {
        let mut services = crate::Services::new_fake().await;
        // Composition parity: production wiring spawns this exact service
        // at boot; kernel dispatch tests rely on it for the ask path.
        jinn_context_assembly::service::spawn(&services.trouper_system);
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = state.read().session.active_session_id().clone();
        {
            let mut guard = state.write_test_no_cap();
            guard
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
        }
        let inputs = {
            let guard = state.read();
            build_assembly_inputs(&guard, &session_id)
        };
        let prompt = assemble_via_service(&services, inputs)
            .await
            .expect("ask resolves");
        assert_eq!(prompt.session_id, session_id);
    }
}
