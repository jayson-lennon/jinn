//! The skills region's tests: the pull renders the viewed session's
//! discovered skills as TUI-parity rows (enablement mark, name,
//! description, source badge, force-load button), the fragment is
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

use super::SkillsRegion;
use jinn_skills_msg::Skill;
use jinn_skills_msg::SkillSource;

/// The region-test harness: an empty app state with the unread-marks
/// cell registered, and the shared handler state over an empty config
/// layer (skills come from session state, not config).
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
    let shared = crate::server::ServerShared::new(
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
        jinn_config::testutil::config_layer(""),
        fanout,
        0,
        "skills-region-test".to_owned(),
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

/// A scratch absolute dir for skill path fixtures.
fn scratch_dir() -> marked_path::MarkedPathBuf<marked_path::Absolute> {
    jinn_testutil::tempdir().path
}

/// A discovered skill fixture: `alpha` is project-scoped, the rest
/// global; names are scrambled alphabetically to pin the region's
/// case-insensitive sort.
fn discovered(dir: &marked_path::MarkedPathBuf<marked_path::Absolute>) -> Vec<Skill> {
    let skill = |name: &str, source: SkillSource| Skill {
        name: name.to_owned(),
        description: format!("{name} description"),
        body: format!("{name} body"),
        file_path: dir
            .join_all([name, "SKILL.md"])
            .expect("typed parent joins bare names"),
        base_dir: dir.join(name).expect("typed parent joins a bare name"),
        source,
    };
    vec![
        skill("zeta", SkillSource::Global),
        skill("alpha", SkillSource::Project { dir: dir.clone() }),
        skill("beta", SkillSource::Global),
    ]
}

/// Writes `skills` into the session's ephemeral discovered set (the
/// state write the scan actor's handling performs).
fn discover(
    state: &jinn_kernel::State,
    session: &jinn_core_types::SessionId,
    skills: Vec<Skill>,
) {
    state.with_session(|view| {
        view.session
            .map()
            .get_or_create(session)
            .set_discovered_skills(skills);
    });
}

/// One row's full fragment, sliced from the modal fragment: from the
/// row's `<div` open to its closing `</div>`. The enablement mark,
/// badge, and load button ride inside the row's own fragment, so the
/// assertions probe this slice — not the whole modal, where another
/// row's class would satisfy a naive `contains`.
fn row_fragment(html: &str, skill_name: &str) -> String {
    let marker = format!("{}=\"{skill_name}\"", contract::SKILL_NAME_DATA_ATTR);
    let (before, _after) = html
        .split_once(&marker)
        .unwrap_or_else(|| panic!("a row renders for {skill_name}: {html}"));
    let start = before
        .rfind("<div")
        .unwrap_or_else(|| panic!("a row div precedes the {skill_name} marker: {html}"));
    let end = html
        .get(start..)
        .and_then(|tail| tail.find("</div>"))
        .unwrap_or_else(|| panic!("the {skill_name} row's fragment closes: {html}"));
    html.get(start..start + end + "</div>".len())
        .unwrap_or_else(|| panic!("the {skill_name} row's fragment slices cleanly: {html}"))
        .to_owned()
}

#[rstest::rstest]
#[test]
fn the_region_conforms_with_pull_route_and_sse_declared() {
    // Given the skills region.
    let region = SkillsRegion;

    // Then the conformance gate finds nothing — a pull route plus one
    // owned SSE kind, no settle pass, and no page-shell container (the
    // modal swaps into its own host span) is the honest declaration
    // for a signal-refreshed modal.
    let gaps = crate::region::check_region(&region);
    assert!(gaps.is_empty(), "the skills region must conform: {gaps:?}");
    assert_eq!(region.id(), "skills");
    assert_eq!(region.container_id(), None);
    assert_eq!(region.event_kinds(), &[jinn_webui_msg::SseKind::Skills]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_lists_every_discovered_skill_in_case_insensitive_order() {
    // Given a live session with three discovered skills (names
    // scrambled alphabetically).
    let (state, shared) = harness();
    let id = seed_session(&state);
    let dir = scratch_dir();
    discover(&state, &id, discovered(&dir));

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the modal overlay and rows container render, and every
    // discovered skill gets a row — unfiltered — in case-insensitive
    // name order.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::SKILL_MODAL_ID)),
        "the modal overlay is missing: {html}"
    );
    assert!(
        html.contains(&format!("id=\"{}\"", contract::SKILL_ROWS_ID)),
        "the rows container is missing: {html}"
    );
    let alpha = html
        .find(&format!("{}=\"alpha\"", contract::SKILL_NAME_DATA_ATTR))
        .expect("the alpha row renders");
    let beta = html
        .find(&format!("{}=\"beta\"", contract::SKILL_NAME_DATA_ATTR))
        .expect("the beta row renders");
    let zeta = html
        .find(&format!("{}=\"zeta\"", contract::SKILL_NAME_DATA_ATTR))
        .expect("the zeta row renders");
    assert!(
        alpha < beta && beta < zeta,
        "rows render in case-insensitive name order: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn every_row_carries_mark_name_description_and_source_badge() {
    // Given a live session with a project-scoped skill.
    let (state, shared) = harness();
    let id = seed_session(&state);
    let dir = scratch_dir();
    let skill = Skill {
        name: "solo".to_owned(),
        description: "solo description".to_owned(),
        body: "solo body".to_owned(),
        file_path: dir
            .join_all(["solo", "SKILL.md"])
            .expect("typed parent joins bare names"),
        base_dir: dir.join("solo").expect("typed parent joins a bare name"),
        source: SkillSource::Project { dir: dir.clone() },
    };
    discover(&state, &id, vec![skill]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the row carries the description text and the project
    // badge (the TUI badge rule), and the force-load button with its
    // class hook.
    let row = row_fragment(&html, "solo");
    assert!(row.contains("solo description"), "description renders: {row}");
    assert!(
        row.contains("skill-source-project"),
        "the project badge renders: {row}"
    );
    assert!(
        row.contains(contract::SKILL_LOAD_CLASS),
        "the force-load button renders: {row}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_global_skill_renders_no_source_badge() {
    // Given a live session with one global skill.
    let (state, shared) = harness();
    let id = seed_session(&state);
    let dir = scratch_dir();
    let skill = Skill {
        name: "global".to_owned(),
        description: "global description".to_owned(),
        body: "global body".to_owned(),
        file_path: dir
            .join_all(["global", "SKILL.md"])
            .expect("typed parent joins bare names"),
        base_dir: dir
            .join("global")
            .expect("typed parent joins a bare name"),
        source: SkillSource::Global,
    };
    discover(&state, &id, vec![skill]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the row carries no source badge — the TUI badge rule
    // (project skills badge, global skills do not).
    let row = row_fragment(&html, "global");
    assert!(
        !row.contains("skill-row-source"),
        "a global skill renders no badge: {row}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_enabled_mark_lands_on_exactly_the_permitted_skills() {
    // Given a session withholding `beta` (a deny filter, the default
    // mode) with the three-skill fixture discovered.
    let (state, shared) = harness();
    let id = seed_session(&state);
    let dir = scratch_dir();
    discover(&state, &id, discovered(&dir));
    state.with_session(|view| {
        view.session
            .map()
            .get_or_create(&id)
            .set_skill_filter(Some(jinn_core_types::NameFilter::deny(
                ["beta".to_owned()],
            )));
    });

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then the enabled class lands on exactly the permitted rows
    // (alpha and zeta), and the withheld row wears the hollow mark.
    let beta_row = row_fragment(&html, "beta");
    assert!(
        !beta_row.contains("skill-row-enabled"),
        "the withheld row is not enabled: {html}"
    );
    assert_eq!(
        html.matches("skill-row-enabled").count(),
        2,
        "exactly the permitted rows are enabled: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_absent_filter_permits_every_row() {
    // Given a session whose filter was never set (the boot state).
    let (state, shared) = harness();
    let id = seed_session(&state);
    let dir = scratch_dir();
    discover(&state, &id, discovered(&dir));

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string()).expect("the modal renders");

    // Then every row is enabled — an absent filter inherits permit-
    // all, so the e2e's fresh-boot skill renders enabled.
    assert_eq!(
        html.matches("skill-row-enabled").count(),
        3,
        "an absent filter permits every row: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_that_died_mid_view_renders_an_empty_modal() {
    // Given no live session carries the id the pull names.
    let (_state, shared) = harness();

    // When rendering the modal for that id.
    let html = super::modal_fragment(
        &shared,
        "00000000-0000-0000-0000-000000000000",
    )
    .expect("the modal renders");

    // Then the overlay and empty rows container render, with no rows
    // — an empty modal, not an error (the pull is the renderer; a
    // session that died mid-view is a legal read).
    assert!(
        html.contains(&format!("id=\"{}\"", contract::SKILL_MODAL_ID)),
        "the modal overlay renders: {html}"
    );
    assert!(
        !html.contains(contract::SKILL_NAME_DATA_ATTR),
        "no rows render for a dead session: {html}"
    );
}
