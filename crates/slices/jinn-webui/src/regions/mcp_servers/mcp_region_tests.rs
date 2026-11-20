//! The MCP region's tests: the pull renders the shared builder's
//! TUI-parity rows (name, summary, enabled mark, status badge — the
//! singleton badge through the effective projection), the fragment is
//! contract-complete, and the conformance gate holds the region to
//! its pull+SSE declaration.
//!
//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use crate::contract;
use crate::region::WebRegion as _;
use crate::server::ServerShared;

use super::McpServersRegion;

/// The MCP section the config tests seed: one normal server, one
/// singleton, alphabetically scrambled to pin the shared builder's
/// case-insensitive sort.
const MCP_DOC: &str = r#"
[mcp.zeta]
command = "zeta-bin"
args = ["--fast"]

[mcp.alpha]
command = "alpha-bin"
singleton = true
"#;

/// The region-test harness: an empty app state with the personas and
/// MCP runtime cells registered, and the shared handler state over a
/// config layer seeded with two MCP servers.
fn harness() -> (jinn_kernel::State, ServerShared) {
    harness_with(MCP_DOC)
}

/// The harness over an arbitrary config document.
fn harness_with(config_doc: &str) -> (jinn_kernel::State, ServerShared) {
    let slices = jinn_slices::Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let personas_cell = slices
        .register(
            jinn_persona_msg::personas_slot(),
            jinn_persona_msg::Personas::default(),
        )
        .expect("the personas cell registers once per harness");
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
        .expect("the unread-mark cell registers once per harness");
    let state = {
        let app = jinn_app_state::AppState::default();
        app.session.attach_slices(slices.clone());
        jinn_kernel::State::new(app)
    };
    let (bus, _audit) = jinn_slices::bus::BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        provider_cell,
        personas_cell,
        mcp_runtime_cell,
        jinn_provider_config::ProviderRegistryService::new(
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
        jinn_provider_config::ApiKeysService::new(jinn_provider_config::ApiKeys::new()),
        jinn_config::testutil::config_layer(config_doc),
        fanout,
        0,
        "mcp-region-test".to_owned(),
    );
    (state, shared)
}

/// Inserts a fresh live session and returns its id.
fn seed_session(state: &jinn_kernel::State) -> jinn_core_types::SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    id
}

/// Enables `server` for `session` (the state write a toggle performs).
fn enable(state: &jinn_kernel::State, session: &jinn_core_types::SessionId, server: &str) {
    state.with_session(|view| {
        let session = view.session.map().get_or_create(session);
        let mut enabled = session.enabled_mcp_servers().clone();
        enabled.insert(server.to_owned());
        session.set_enabled_mcp_servers(enabled);
    });
}

/// One row's full fragment, sliced from the modal fragment: from the
/// row's `<div` open to its closing `</div>`. The enabled mark and
/// status badge ride inside the row's own fragment, so the assertions
/// probe this slice — not the whole modal, where another row's badge
/// would satisfy a naive `contains`.
fn row_fragment(html: &str, server: &str) -> String {
    let marker = format!("{}=\"{server}\"", contract::MCP_SERVER_DATA_ATTR);
    let (before, _after) = html
        .split_once(&marker)
        .unwrap_or_else(|| panic!("a row renders for {server}: {html}"));
    let start = before
        .rfind("<div")
        .unwrap_or_else(|| panic!("a row div precedes the {server} marker: {html}"));
    let end = html
        .get(start..)
        .and_then(|tail| tail.find("</div>"))
        .unwrap_or_else(|| panic!("the {server} row's fragment closes: {html}"));
    html.get(start..start + end + "</div>".len())
        .unwrap_or_else(|| panic!("the {server} row's fragment slices cleanly: {html}"))
        .to_owned()
}

#[rstest::rstest]
#[tokio::test]
async fn the_region_conforms_with_pull_route_and_sse_declared() {
    // Given the MCP region.
    let region = McpServersRegion;

    // Then the conformance gate finds nothing — a pull route plus one
    // owned SSE kind, no settle pass, and no page-shell container (the
    // modal swaps into its own host span) is the honest declaration
    // for a signal-refreshed modal.
    let gaps = crate::region::check_region(&region);
    assert!(gaps.is_empty(), "the mcp region must conform: {gaps:?}");
    assert_eq!(region.id(), "mcp-servers");
    assert_eq!(region.container_id(), None);
    assert_eq!(region.event_kinds(), &[jinn_webui_msg::SseKind::McpStatus]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_lists_every_configured_server_in_the_shared_order() {
    // Given the harness (two servers seeded, `alpha` and `zeta`) and a
    // live session.
    let (state, shared) = harness();
    let id = seed_session(&state);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then every configured server renders — unfiltered, one row per
    // server — in the shared builder's case-insensitive order, each
    // with its name and launch summary.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MCP_MODAL_ID)),
        "the modal overlay is missing: {html}"
    );
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MCP_ROWS_ID)),
        "the rows container is missing: {html}"
    );
    let alpha = html
        .find(&format!("{}=\"alpha\"", contract::MCP_SERVER_DATA_ATTR))
        .expect("the alpha row renders");
    let zeta = html
        .find(&format!("{}=\"zeta\"", contract::MCP_SERVER_DATA_ATTR))
        .expect("the zeta row renders");
    assert!(
        alpha < zeta,
        "rows render in the shared builder's case-insensitive order: {html}"
    );
    assert!(html.contains("alpha-bin"), "launch summary renders: {html}");
    assert!(html.contains("zeta-bin"), "launch summary renders: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_enabled_mark_lands_on_exactly_the_viewed_sessions_servers() {
    // Given a session with `alpha` enabled and `zeta` not.
    let (state, shared) = harness();
    let id = seed_session(&state);
    enable(&state, &id, "alpha");

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the enabled class lands on exactly the alpha row (the
    // filled mark rides the same row), and no other row wears it.
    let alpha_row = row_fragment(&html, "alpha");
    assert!(
        alpha_row.contains("mcp-row-enabled"),
        "the alpha row is the enabled one: {html}"
    );
    let enabled_count = html.matches("mcp-row-enabled").count();
    assert_eq!(enabled_count, 1, "exactly one row is enabled: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn an_enabled_server_without_a_reported_status_shows_starting() {
    // Given a session with `alpha` enabled and a runtime cell holding
    // no status for it.
    let (state, shared) = harness();
    let id = seed_session(&state);
    enable(&state, &id, "alpha");

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the alpha row's badge reads `starting` — the TUI's
    // derivation for None (no report yet), verbatim.
    let alpha_row = row_fragment(&html, "alpha");
    assert!(
        alpha_row.contains("mcp-status-starting"),
        "the alpha badge reads starting: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_singletons_badge_reads_the_effective_projection() {
    // Given a session with NO per-session status for the singleton,
    // but the runtime cell's singleton bucket holding `Running`.
    let (state, shared) = harness();
    let id = seed_session(&state);
    enable(&state, &id, "alpha");
    shared.mcp_runtime().update(|runtime| {
        runtime.set_singleton_status("alpha", jinn_mcp_msg::McpConnectionStatus::Running);
    });

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the singleton's badge reads `running` — the effective
    // projection (the per-session bucket would show starting forever,
    // the bug refresh_inspector_snapshot documents).
    let alpha_row = row_fragment(&html, "alpha");
    assert!(
        alpha_row.contains("mcp-status-running"),
        "the singleton badge reads running: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_per_session_status_overrides_the_projection_for_that_session() {
    // Given a session whose own bucket holds `Dead` for a NON-singleton
    // server while another session's bucket holds `Running`.
    let (state, shared) = harness();
    let id = seed_session(&state);
    enable(&state, &id, "zeta");
    let other = jinn_core_types::SessionId::new();
    shared.mcp_runtime().update(|runtime| {
        runtime.set_status(&other, "zeta", jinn_mcp_msg::McpConnectionStatus::Running);
        runtime.set_status(&id, "zeta", jinn_mcp_msg::McpConnectionStatus::Dead);
    });

    // When rendering the modal for `id`.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the badge reads `dead` — the viewed session's own report,
    // not another session's.
    let zeta_row = row_fragment(&html, "zeta");
    assert!(
        zeta_row.contains("mcp-status-dead"),
        "the zeta badge reads dead: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_renders_zero_rows_for_an_empty_config() {
    // Given the harness over a config with no `[mcp.*]` section.
    let (state, shared) = harness_with("");
    let id = seed_session(&state);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the overlay and rows container still render (the pull never
    // fails on an empty section) with zero row divs.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MCP_ROWS_ID)),
        "the rows container renders: {html}"
    );
    assert!(!html.contains("mcp-row\""), "no rows render: {html}");
}
