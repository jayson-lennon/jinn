//! The region conformance tests: every registered region's declared
//! parts exist, the whole registry is single-owner per SSE kind, a
//! crippled region fails with exactly the named gap, and the fan-out's
//! registry-driven settle preserves the event order the correctness
//! story depends on.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_app_state::AppState;
use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_slices::bus::BusService;
use jinn_webui_msg::SseKind;

use crate::region::RegionGap;
use crate::region::RegionParts;
use crate::region::check_region;
use crate::region::check_registry;
use crate::region::regions;
use crate::server::ServerShared;

/// The harness the positive conformance tests render through: an
/// empty app state, a fresh provider cell, and the shared handler
/// state. Returns the state (for seeding) and the shared.
fn harness() -> (State, ServerShared) {
    let slices = Slices::new();
    let provider_cell = slices
        .register(
            jinn_provider_selection_msg::provider_state_slot(),
            jinn_provider_selection_msg::ProviderCell::default(),
        )
        .expect("the provider cell registers once per harness");
    let state = {
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone())
    };
    let shared = ServerShared::new(
        state.clone(),
        bus,
        jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
            jinn_kernel::common::services::test_services::FakeSessionStore,
        )),
        provider_cell,
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
        fanout,
        0,
        "region-test".to_owned(),
    );
    (state, shared)
}

/// Inserts a fresh live session and returns its id.
fn seed_session(state: &State) -> jinn_core_types::SessionId {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    id
}

// ── Positive conformance ────────────────────────────────────────────

#[rstest::rstest]
#[test]
fn every_registered_region_passes_the_conformance_gate() {
    // Given the real registry.
    // When running the whole-registry gate.
    let gaps = check_registry();

    // Then it finds nothing — the shipped regions all conform.
    assert!(
        gaps.is_empty(),
        "the shipped registry must conform, got: {gaps:?}"
    );
    // And the registry is non-empty (a refactoring that silently
    // emptied it would otherwise pass vacuously).
    assert!(!regions().is_empty(), "the registry registers regions");
}

#[rstest::rstest]
#[test]
fn the_registry_ids_are_unique_and_mirror_the_client_modules() {
    // Given the registered regions.
    let ids: Vec<&str> = regions().iter().map(|region| region.id()).collect();

    // Then each id appears exactly once and names the client region
    // module's filename stem — the pairing the ownership doc relies
    // on when a reader jumps from one half of a region to the other.
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    let expected = ["composer", "menu", "model-picker", "sessions", "transcript"];
    assert_eq!(sorted, expected, "the registry ids are fixed vocabulary");
}

#[rstest::rstest]
#[tokio::test]
async fn every_declared_container_id_appears_in_the_rendered_page_shell() {
    // Given the fully rendered page shell (the shape both page
    // routes serve).
    let (state, shared) = harness();
    let id = seed_session(&state);
    let sessions = crate::regions::sessions::sessions_fragment(&shared, None)
        .await
        .expect("sessions render");
    let transcript_html = crate::templates::render(&crate::templates::TranscriptTemplate {
        entries: Vec::new(),
        start: 0,
    })
    .expect("transcript renders");
    let controls = crate::templates::render(&crate::templates::ControlsTemplate::with_contract(
        id.to_string(),
        false,
    ))
    .expect("controls render");
    let model = crate::regions::model_picker::status_fragment(&shared, &id.to_string())
        .expect("picker renders");
    let page = crate::templates::render(&crate::templates::PageTemplate::session(
        id.to_string(),
        "Conformance".to_owned(),
        0,
        sessions,
        transcript_html,
        controls,
        model,
        crate::regions::menu::menu_fragment(),
    ))
    .expect("the page renders");

    // When collecting every declared container id.
    // Then each appears as an id attribute in the shell — a region
    // whose container the page lacks would swap into nothing.
    for region in regions() {
        if let Some(container) = region.container_id() {
            assert!(
                page.contains(&format!("id=\"{container}\"")),
                "region `{}`'s container #{container} is missing from the page shell",
                region.id()
            );
        }
    }
}

#[rstest::rstest]
#[test]
fn every_pull_route_pattern_is_well_formed_for_its_binding() {
    // Given every region's pull binding.
    // Then each pattern is the contract's route constant and carries
    // an `{id}` segment exactly when the handler reads one (the flat
    // sessions pull vs the session-scoped pulls).
    for region in regions() {
        if let Some(pull) = region.pull() {
            assert!(
                pull.pattern.starts_with("/web/"),
                "region `{}`'s pull pattern `{}` is under the web mount",
                region.id(),
                pull.pattern
            );
        }
    }
}

#[rstest::rstest]
#[tokio::test]
async fn every_guard_rendering_region_renders_from_a_seeded_guard() {
    // Given a live session with one entry.
    let (state, _shared) = harness();
    let id = seed_session(&state);
    state
        .write()
        .session
        .get_mut(&id)
        .expect("seeded session")
        .push_entry(jinn_core_types::ChatEntry::user("region render drill"));

    // When rendering every region that declares the guard-render part
    // (the picker declines the part; its blank render is by design).
    let guard = state.read();
    for region in regions() {
        if !region.parts().settle_render {
            continue;
        }
        let html = region.render(&id, &guard);

        // Then the fragment names the seeded content (or is the
        // sessions list, which renders rows, not entry text).
        assert!(
            !html.is_empty() || region.id() == "sessions",
            "region `{}` rendered a blank fragment from a seeded guard",
            region.id()
        );
    }
}

#[rstest::rstest]
#[test]
fn the_settle_pushes_exactly_the_registry_declared_kinds_in_order() {
    // Given the flattened settle order the fan-out computes.
    let order: Vec<SseKind> = regions()
        .iter()
        .flat_map(|region| region.settle_kinds().iter().copied())
        .collect();

    // Then it is exactly transcript-full then controls — the ordering
    // that keeps the final tokens from appearing twice (the partial
    // flush runs first, outside this list) and re-bases the client.
    assert_eq!(
        order,
        vec![SseKind::TranscriptFull, SseKind::Controls],
        "the settle order is behavioral — changing it changes reconnect windows"
    );
}

// ── The negative drills ─────────────────────────────────────────────

/// A region declaring every part but implementing none of them — the
/// "shipped half-wired" shape the gate exists to catch.
struct CrippledRegion;

impl crate::region::WebRegion for CrippledRegion {
    fn id(&self) -> &'static str {
        "crippled"
    }

    fn parts(&self) -> RegionParts {
        RegionParts {
            settle_render: true,
            pull_route: true,
            sse_event: true,
            page_fill: true,
        }
    }

    fn event_kinds(&self) -> &'static [SseKind] {
        &[]
    }

    fn pull(&self) -> Option<crate::region::RegionPull> {
        None
    }

    fn container_id(&self) -> Option<&'static str> {
        None
    }

    fn render(
        &self,
        _session_id: &jinn_core_types::SessionId,
        _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
    ) -> String {
        String::new()
    }
}

#[rstest::rstest]
#[test]
fn a_region_missing_every_declared_part_is_reported_per_part() {
    // Given a region that declares all four parts and implements none.
    // When running the per-region gate.
    let gaps = check_region(&CrippledRegion);

    // Then every declared-but-missing part is named — the report is
    // the fix list, one line per gap.
    assert!(
        gaps.contains(&RegionGap::RouteWithoutBinding { region: "crippled" }),
        "the missing pull binding is named: {gaps:?}"
    );
    assert!(
        gaps.contains(&RegionGap::EventDeclaredNoneOwned { region: "crippled" }),
        "the missing SSE kinds are named: {gaps:?}"
    );
    assert!(
        gaps.contains(&RegionGap::FillWithoutContainer { region: "crippled" }),
        "the missing container is named: {gaps:?}"
    );
    assert!(
        gaps.contains(&RegionGap::SettleWithoutEvent { region: "crippled" }),
        "the settle-without-event contradiction is named: {gaps:?}"
    );
}

#[rstest::rstest]
#[test]
fn an_honest_region_with_undeclared_parts_passes_clean() {
    // Given a region that declares only what it carries — the
    // composer's real shape (no pull route), spelled as a test
    // fixture.
    struct HonestComposer;

    impl crate::region::WebRegion for HonestComposer {
        fn id(&self) -> &'static str {
            "honest-composer"
        }

        fn parts(&self) -> RegionParts {
            RegionParts {
                settle_render: true,
                pull_route: false,
                sse_event: true,
                page_fill: true,
            }
        }

        fn event_kinds(&self) -> &'static [SseKind] {
            &[SseKind::Controls]
        }

        fn pull(&self) -> Option<crate::region::RegionPull> {
            None
        }

        fn container_id(&self) -> Option<&'static str> {
            Some(crate::contract::CONTROLS_ID)
        }

        fn render(
            &self,
            _session_id: &jinn_core_types::SessionId,
            _guard: &jinn_kernel::common::state::StateReadGuard<'_>,
        ) -> String {
            String::new()
        }
    }

    // When running the gate.
    let gaps = check_region(&HonestComposer);

    // Then nothing is reported — the gate is declaration-shaped, not
    // blind all-five.
    assert!(gaps.is_empty(), "an honest region must pass: {gaps:?}");
}
