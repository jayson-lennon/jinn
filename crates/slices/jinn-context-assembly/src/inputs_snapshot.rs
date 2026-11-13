//! Caller-side snapshot building for the context-assembly service.
//!
//! The service is pure; whoever dispatches a turn snapshots the session
//! state it can see (under its existing read guard) into an
//! [`AssemblyInputs`] and asks the `context-assembly` trouper actor.
//! Persona selection and tool definitions resolve through their cells.

use jinn_core_types::SessionId;

use jinn_context_assembly_msg::{AssembleContext, AssemblyInputs};
use jinn_core_types::DEFAULT_PERSONA_NAME;
use jinn_kernel::common::app_state::AppState;

/// Snapshots everything the assembly service needs for `session_id`.
///
/// Must be called while holding a state read guard — all session data
/// is cloned out of it. Cell-backed state (persona selection, tool
/// registry) resolves through `state.frontend.slices()`; a missing cell
/// degrades to empty inputs rather than failing dispatch.
/// `handoff_enabled` is not resolved here: kernel-side callers pass the
/// config layer to [`crate::inputs::build_assembly_inputs`]; this
/// snapshot builder defaults the flag off and callers that need the
/// live value set it on the returned inputs.
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
        .map_or_default(|cell| cell.read().tools_for_session(session_id));

    AssemblyInputs {
        session_id: session_id.clone(),
        cwd: session.cwd().clone(),
        persona,
        history: session.history().to_vec(),
        tools,
        tool_filter: session.tool_filter().cloned(),
        provider_name: session.model_selection().provider_name().to_owned(),
        skills: session.discovered_skills().to_vec(),
        skill_filter: session.skill_filter().cloned(),
        loaded_skills: session.loaded_skills(),
        context_files: session.discovered_context_files().to_vec(),
        handoff_enabled: false,
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
    services: &jinn_kernel::common::services::Services,
    inputs: AssemblyInputs,
) -> Result<jinn_slices::AssembledPrompt, error_stack::Report<trouper::context::AskError>> {
    assemble_response_via_service(services, inputs)
        .await
        .map(|response| response.prompt)
}

/// Like [`assemble_via_service`], but returns the full reply envelope —
/// the context-size actor reads the prompt and lets its caller pair the
/// write with a generation check.
///
/// # Errors
///
/// Returns the trouper `AskError` report if the service is absent or
/// the ask times out.
pub async fn assemble_response_via_service(
    services: &jinn_kernel::common::services::Services,
    inputs: AssemblyInputs,
) -> Result<
    jinn_context_assembly_msg::AssembledResponse,
    error_stack::Report<trouper::context::AskError>,
> {
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
    reply.decode().map_err(|e| {
        error_stack::Report::new(trouper::context::AskError::Unresolved(
            "assembled response deserialization failed".to_owned(),
        ))
        .attach(format!("deserialization failed: {e}"))
    })
}

#[cfg(test)]
mod composition_ask_tests {
    use super::*;
    use jinn_kernel::common::app_state::AppState;
    use jinn_kernel::common::state::State;

    use jinn_kernel::protocol::ChatEntry;

    #[rstest::rstest]
    #[tokio::test]
    #[expect(
        clippy::expect_used,
        reason = "test helper: a failed ask fails the test"
    )]
    async fn minimal_ask_reproduces_resolution() {
        // Given a service spawned at the production path, and a session
        // holding one user entry.
        // Composition parity: production wiring spawns the slice service at
        // this exact path at boot; unit tests spawn the test-crate stub at the
        // same path so the live-value ask crosses no compilation boundary (see
        // assembly_test_bridge docs).
        let services = jinn_kernel::Services::new_fake().await;
        let _ = crate::assembly_test_bridge::spawn(&services.trouper_system);
        let state = State::new(AppState::default_with_scope_focus());
        let session_id = state.read().session.active_session_id().clone();
        {
            let mut guard = state.write();
            guard
                .active_session_mut()
                .push_entry(ChatEntry::user("hello"));
        }
        let inputs = {
            let guard = state.read();
            build_assembly_inputs(&guard, &session_id)
        };
        // When assembling through the service.
        let prompt = assemble_via_service(&services, inputs)
            .await
            .expect("ask resolves");

        // Then the prompt comes back for the same session.
        assert_eq!(prompt.session_id, session_id);
    }
}
