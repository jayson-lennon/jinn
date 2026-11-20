//! The persona region's tests: the pull renders the TUI-parity rows
//! with the active mark and the shared filter, the fragment is
//! contract-complete, and the conformance gate holds the region to its
//! pull+page-fill declaration.
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

use super::PersonaRegion;

/// The region-test harness: an empty app state with the personas cell
/// registered, and the shared handler state over it.
fn harness() -> (jinn_kernel::State, ServerShared) {
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
        jinn_config::empty_config_layer().clone(),
        fanout,
        0,
        "persona-region-test".to_owned(),
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

/// Two personas in the cell, the first carrying a compaction block and
/// the second not — the render side does not care, but the fixture
/// mirrors the shipped catalog's shape.
fn seed_personas(shared: &ServerShared) {
    shared.personas_cell().update(|cell| {
        cell.entries = vec![
            jinn_slices::Persona {
                name: "coding-assistant".to_owned(),
                description: "writes code".to_owned(),
                body: "be terse".to_owned(),
                handoff_prompt: "<compaction>x</compaction>".to_owned(),
            },
            jinn_slices::Persona {
                name: "learning-tutor".to_owned(),
                description: "teaches".to_owned(),
                body: "be patient".to_owned(),
                handoff_prompt: String::new(),
            },
        ];
    });
}

#[rstest::rstest]
#[tokio::test]
async fn the_region_conforms_with_pull_route_and_page_fill_declared() {
    // Given the persona region.
    let region = PersonaRegion;

    // Then the conformance gate finds nothing — a pull route with a
    // container, no SSE of its own, and no settle pass is the honest
    // declaration for a request/response modal.
    let gaps = crate::region::check_region(&region);
    assert!(gaps.is_empty(), "the persona region must conform: {gaps:?}");
    assert_eq!(region.id(), "persona-picker");
    assert_eq!(region.container_id(), Some(contract::PERSONA_MODAL_HOST_ID));
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_lists_the_scanned_personas_with_the_active_marked() {
    // Given the harness with two personas and a session whose persona
    // is the second one.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_personas(&shared);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .set_persona_name("learning-tutor".to_owned());

    // When rendering the modal unfiltered.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then both rows render with their contract ids and names, and the
    // active attribute lands on exactly the viewed session's persona.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::PERSONA_MODAL_ID)),
        "the modal overlay is missing: {html}"
    );
    assert!(
        html.contains(&format!("{}=\"coding-assistant\"", contract::PERSONA_NAME_DATA_ATTR)),
        "the first persona row is missing: {html}"
    );
    assert!(
        html.contains(&format!("{}=\"learning-tutor\"", contract::PERSONA_NAME_DATA_ATTR)),
        "the second persona row is missing: {html}"
    );
    let active_count = html
        .matches(&format!("{}=\"true\"", contract::PERSONA_ACTIVE_ATTR))
        .count();
    assert_eq!(active_count, 1, "exactly one row is active: {html}");
    let tutor_tag = html
        .split_once("learning-tutor")
        .and_then(|(before, _)| {
            // The active attribute sits on the row div opening the
            // name; the nearest preceding div start carries it.
            before
                .rmatch_indices("<div")
                .next()
                .map(|(pos, _)| &before[pos..])
        })
        .unwrap_or_default();
    assert!(
        tutor_tag.contains(&format!("{}=\"true\"", contract::PERSONA_ACTIVE_ATTR)),
        "the tutor row is the active one: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_renders_no_rows_for_an_empty_catalog() {
    // Given the harness with an empty personas cell.
    let (state, shared) = harness();
    let id = seed_session(&state);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the overlay and rows container still render (the pull never
    // fails on an empty catalog) with zero row divs.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::PERSONA_ROWS_ID)),
        "the rows container renders: {html}"
    );
    assert!(
        !html.contains("persona-row\""),
        "no rows render: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_filter_applies_server_side_with_marked_runs() {
    // Given the harness with two personas.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_personas(&shared);

    // When filtering for the tutor.
    let html =
        super::modal_fragment(&shared, &id.to_string(), "teaches").expect("the modal renders");

    // Then only the matching row renders and its matched run carries a
    // mark (the shared segment split).
    assert!(
        html.contains("learning-tutor"),
        "the matching row renders: {html}"
    );
    assert!(
        !html.contains("coding-assistant"),
        "the non-matching row is absent: {html}"
    );
    assert!(html.contains("<mark>"), "matched runs are marked: {html}");
}

#[rstest::rstest]
#[tokio::test]
async fn every_dom_constant_the_region_renders_is_in_the_contract() {
    // Given the region's rendered modal and the contract's vocabulary.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_personas(&shared);
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then each of the region's rendered ids and attrs is a declared
    // constant — the vocabulary gate holds every new surface.
    for constant in [
        contract::PERSONA_MODAL_ID,
        contract::PERSONA_FILTER_ID,
        contract::PERSONA_ROWS_ID,
        contract::PERSONA_NAME_DATA_ATTR,
        contract::PERSONA_ACTIVE_ATTR,
        contract::ACTION_PERSONA,
        contract::PERSONA_PATH,
    ] {
        assert!(
            html.contains(constant),
            "the contract constant {constant} renders in the modal: {html}"
        );
    }
}
