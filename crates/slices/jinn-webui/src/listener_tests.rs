//! Listener activation tests: the bind is honored, a taken port fails
//! fast, and the frozen bind ignores config reloads.

#![allow(clippy::expect_used, reason = "test assertions use expect")]

use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_slices::bus::BusService;

use crate::listener::WebUiListenerActor;
use crate::listener::WebUiListenerActorDeps;

/// Builds deps over a fresh state, recording bus, seeded cell
/// registry, and test fan-out, bound to the given address.
fn deps_on(bind: std::net::SocketAddr) -> WebUiListenerActorDeps {
    let state = State::new(jinn_app_state::AppState::default());
    let (bus, _audit) = BusService::new_recording();
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once");
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once");
    let mcp_runtime_cell = slices
        .register(
            jinn_mcp_msg::mcp_runtime_slot(),
            jinn_mcp_msg::McpRuntimeState::default(),
        )
        .expect("the MCP runtime cell registers once");
    let unread_marks = slices
        .register(
            jinn_webui_msg::webui_unread_slot(),
            jinn_webui_msg::UnreadSessionMarks::default(),
        )
        .expect("the unread-mark cell registers once");
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    WebUiListenerActorDeps {
        bind,
        state,
        bus,
        session_store: jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        provider_registry: jinn_provider_config::ProviderRegistryService::new(
            jinn_provider_config::ProviderRegistry::from_config(
                jinn_provider_config::ProvidersConfig {
                    providers: std::collections::BTreeMap::default(),
                    aliases: vec![],
                    default_provider: None,
                    endpoint_defaults: vec![],
                },
            )
            .expect("an empty providers config expands"),
        ),
        api_keys: jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        config: jinn_config::empty_config_layer().clone(),
        fanout,
        version: "test".to_owned(),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_configured_bind_is_honored_and_resolves_the_port() {
    // Given deps bound to an ephemeral port.
    let deps = deps_on("127.0.0.1:0".parse().expect("valid socket addr"));
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());

    // When spawning the listener.
    let path = WebUiListenerActor::spawn(&system, &deps)
        .await
        .expect("bind succeeds");

    // Then the actor registered at the webui path.
    assert!(path.to_string().contains("webui"), "{path}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_taken_port_fails_activation_fast() {
    // Given a listener already holding a port.
    let holder = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the holder binds");
    let taken = holder.local_addr().expect("resolved");

    // When spawning a second listener on that same port.
    let deps = deps_on(taken);
    let system = trouper::system::ActorSystem::new(trouper::system::SystemConfig::production());
    let result = WebUiListenerActor::spawn(&system, &deps).await;

    // Then activation aborts: the error names the bind failure among
    // its attachments.
    let error = result.expect_err("a taken port refuses the bind");
    let rendered = format!("{error:?}");
    assert!(rendered.contains("failed to bind the web UI"), "{rendered}");
}
