//! The tools region's tests: the pull renders the shared vocabulary's
//! TUI-parity rows (name, description, ✓/✗ mark from the live filter),
//! the fragment is contract-complete, and the conformance gate holds
//! the region to its pull-only declaration.
//!
//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_core_types::FilterMode;
use jinn_core_types::NameFilter;

use crate::contract;
use crate::region::WebRegion as _;
use crate::server::ServerShared;

use super::ToolsRegion;

/// The tools registry slot's cell, registered into the test harness's
/// slices (the TUI picker tests' seeding shape).
fn seed_registry(state: &jinn_kernel::State, names: &[&str]) {
    let cell = state
        .read()
        .tool_registry()
        .expect("the tools registry cell is attached");
    cell.update(|registry| {
        for name in names {
            registry.global.insert(
                (*name).to_owned(),
                jinn_core_types::ToolDefinition {
                    name: (*name).to_owned(),
                    description: format!("{name} description"),
                    parameters: serde_json::json!({ "type": "object" }),
                    prompt_snippet: None,
                    prompt_guidelines: vec![],
                    server_tool_type: None,
                },
            );
        }
    });
}

/// The region-test harness: an empty app state with the cells
/// registered, and the shared handler state over an empty config.
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
    let _tools_registry = slices
        .register(
            jinn_tools_msg::tools_registry_slot(),
            jinn_tools_msg::ToolRegistry::default(),
        )
        .expect("the tools registry cell registers once per harness");
    let state = {
        let app = jinn_app_state::AppState::default();
        app.frontend.attach_slices(slices.clone());
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
        jinn_config::empty_config_layer().clone(),
        fanout,
        0,
        "tools-region-test".to_owned(),
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

/// Sets the session's tool filter (the state write a commit performs).
fn set_filter(
    state: &jinn_kernel::State,
    session: &jinn_core_types::SessionId,
    filter: Option<NameFilter>,
) {
    state.with_session(|view| {
        view.session
            .map()
            .get_or_create(session)
            .set_tool_filter(filter);
    });
}

/// One row's full fragment, sliced from the modal fragment — from the
/// row's `<div` open to its closing `</div>`. The mark rides inside
/// the row's own fragment, so assertions probe this slice (the
/// mcp-region tests' slicing shape).
fn row_fragment(html: &str, tool: &str) -> String {
    let marker = format!("{}=\"{tool}\"", contract::TOOL_NAME_DATA_ATTR);
    let (before, _after) = html
        .split_once(&marker)
        .unwrap_or_else(|| panic!("a row renders for {tool}: {html}"));
    let start = before
        .rfind("<div")
        .unwrap_or_else(|| panic!("a row div precedes the {tool} marker: {html}"));
    let end = html
        .get(start..)
        .and_then(|tail| tail.find("</div>"))
        .unwrap_or_else(|| panic!("the {tool} row's fragment closes: {html}"));
    html.get(start..start + end + "</div>".len())
        .unwrap_or_else(|| panic!("the {tool} row's fragment slices cleanly: {html}"))
        .to_owned()
}

#[rstest::rstest]
#[tokio::test]
async fn the_region_conforms_with_the_pull_route_and_the_tools_signal_declared() {
    // Given the tools region.
    let region = ToolsRegion;

    // Then the conformance gate finds nothing — a pull route plus the
    // signal-only `tools` kind is the honest declaration: the rows
    // route renders everything, and the SSE surface is one signal
    // whose re-pull swaps rows only.
    let gaps = crate::region::check_region(&region);
    assert!(gaps.is_empty(), "the tools region must conform: {gaps:?}");
    assert_eq!(region.id(), "tools");
    assert_eq!(region.container_id(), None);
    assert_eq!(region.event_kinds(), [jinn_webui_msg::SseKind::Tools]);
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_lists_the_registry_rows_in_the_shared_order() {
    // Given a live session whose registry holds three tools seeded
    // scrambled.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["Grep", "bash", "edit"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the modal overlay and rows container render, the rows come
    // in the shared vocabulary's case-insensitive order, and each
    // carries its name and description.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::TOOLS_MODAL_ID)),
        "the modal overlay is missing: {html}"
    );
    assert!(
        html.contains(&format!("id=\"{}\"", contract::TOOLS_ROWS_ID)),
        "the rows container is missing: {html}"
    );
    let bash = html
        .find(&format!("{}=\"bash\"", contract::TOOL_NAME_DATA_ATTR))
        .expect("the bash row renders");
    let edit = html
        .find(&format!("{}=\"edit\"", contract::TOOL_NAME_DATA_ATTR))
        .expect("the edit row renders");
    let grep = html
        .find(&format!("{}=\"Grep\"", contract::TOOL_NAME_DATA_ATTR))
        .expect("the Grep row renders");
    assert!(
        bash < edit && edit < grep,
        "rows render in the shared case-insensitive order: {html}"
    );
    assert!(
        html.contains("bash description"),
        "descriptions render: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_enabled_marks_read_the_session_s_live_filter() {
    // Given a session whose deny filter withholds one of three tools.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "edit", "Grep"]);
    set_filter(&state, &id, Some(NameFilter::deny(["Grep".to_owned()])));

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then exactly the withheld row renders off — the mark is the
    // live filter's verdict, not the registry's.
    let grep_row = row_fragment(&html, "Grep");
    assert!(
        grep_row.contains("tools-row-off"),
        "the withheld row renders off: {html}"
    );
    assert_eq!(
        html.matches("tools-row-off").count(),
        1,
        "exactly one row renders off: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_allow_filter_marks_its_unlisted_rows_off() {
    // Given a session permitted exactly one tool.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "edit"]);
    set_filter(
        &state,
        &id,
        Some(NameFilter {
            mode: FilterMode::Allow,
            names: ["bash".to_owned()].into_iter().collect(),
        }),
    );

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the unpermitted row renders off and the permitted one on —
    // the live filter decides, not its mode.
    assert!(
        row_fragment(&html, "edit").contains("tools-row-off"),
        "the unpermitted row renders off: {html}"
    );
    assert!(
        !row_fragment(&html, "bash").contains("tools-row-off"),
        "the permitted row renders on: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_session_without_a_filter_renders_every_row_on() {
    // Given a session with tools and no filter at all.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "edit"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then nothing renders off — absence withholds nothing.
    assert_eq!(
        html.matches("tools-row-off").count(),
        0,
        "no row renders off without a filter: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_non_bot_session_is_offered_no_bot_tool_row() {
    // Given the registry holding the bot family beside ordinary tools.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "conclude", "notify_parent"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then only the ordinary tool has a row — the shared gate, not a
    // web-private copy, decides.
    assert!(
        html.contains(&format!("{}=\"bash\"", contract::TOOL_NAME_DATA_ATTR)),
        "the ordinary row renders: {html}"
    );
    assert!(
        !html.contains(&format!("{}=\"conclude\"", contract::TOOL_NAME_DATA_ATTR)),
        "no conclude row renders for a non-bot: {html}"
    );
    assert!(
        !html.contains(&format!(
            "{}=\"notify_parent\"",
            contract::TOOL_NAME_DATA_ATTR
        )),
        "no notify_parent row renders for a non-bot: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_standalone_bot_is_offered_conclude_but_not_notify_parent() {
    // Given a parentless bot session.
    let (state, shared) = harness();
    let id = {
        let parent = jinn_session_state::ChatSessionState::new();
        let bot = jinn_session_state::ChatSessionState::new_standalone_bot(&parent, true);
        let bot_id = bot.session_id().clone();
        state.write().session.insert(bot);
        bot_id
    };
    seed_registry(&state, &["bash", "conclude", "notify_parent"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then `conclude` has a row and `notify_parent` does not — the
    // gate's parentless-bot rule, shared with the TUI.
    assert!(
        html.contains(&format!("{}=\"conclude\"", contract::TOOL_NAME_DATA_ATTR)),
        "the conclude row renders for a standalone bot: {html}"
    );
    assert!(
        !html.contains(&format!(
            "{}=\"notify_parent\"",
            contract::TOOL_NAME_DATA_ATTR
        )),
        "no notify_parent row renders for a standalone bot: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_unknown_session_and_an_empty_registry_render_zero_rows_without_error() {
    // Given a session whose registry holds nothing, beside an id no
    // live session carries.
    let (state, shared) = harness();
    let id = seed_session(&state);

    // When rendering the modal for both.
    let empty = super::modal_fragment(&shared, &id.to_string(), "").expect("empty registry renders");
    let unknown = super::modal_fragment(&shared, "00000000-0000-0000-0000-00000000dead", "")
        .expect("unknown session renders");

    // Then both render the modal shell with zero rows — the pull
    // degrades, never errors.
    for (name, html) in [("empty registry", &empty), ("unknown session", &unknown)] {
        assert!(
            html.contains(&format!("id=\"{}\"", contract::TOOLS_ROWS_ID)),
            "{name}: the rows container renders: {html}"
        );
        assert!(
            !html.contains(&format!("{}=", contract::TOOL_NAME_DATA_ATTR)),
            "{name}: no rows render: {html}"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_open_pull_stamps_the_open_time_filter_into_the_revert_stamp() {
    // Given a session holding a deny filter.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "edit"]);
    let filter = NameFilter::deny(["edit".to_owned()]);
    set_filter(&state, &id, Some(filter.clone()));

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the fragment carries the revert stamp whose init patches
    // the snapshot signal with the open-time filter's JSON — mode and
    // names included.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::TOOLS_REVERT_STAMP_ID)),
        "the revert stamp renders: {html}"
    );
    // The stamp is a quoted HTML attribute: the JSON's `"` and `'`
    // render as their entity escapes and the browser decodes them
    // before parsing the attribute, so the assertion decodes too.
    let json = serde_json::to_string(&filter).expect("the filter serializes");
    let decoded = decode_entities(&html);
    assert!(
        decoded.contains(&format!("data-init=\"$toolsRevert = '{json}'\"")),
        "the stamp carries the open-time filter's JSON: {decoded}"
    );
}

/// Decodes the entity escapes askama's attribute escaper emits, so a
/// test can assert the attribute value the browser parses.
fn decode_entities(html: &str) -> String {
    html.replace("&#39;", "'").replace("&#34;", "\"")
}

#[rstest::rstest]
#[tokio::test]
async fn an_absent_filter_stamps_null_into_the_revert_stamp() {
    // Given a session with no filter at all.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the stamp patches null — absence restored as absence.
    let stamp = html
        .split_once(&format!("id=\"{}\"", contract::TOOLS_REVERT_STAMP_ID))
        .map_or("", |(_, tail)| tail.split('>').next().unwrap_or(""));
    assert!(
        stamp.contains("= null"),
        "the stamp patches null for an absent filter: {html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn every_row_click_posts_to_the_toggle_path_naming_its_tool() {
    // Given a modal with rows.
    let (state, shared) = harness();
    let id = seed_session(&state);
    seed_registry(&state, &["bash", "edit"]);

    // When rendering the modal.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then each row's click posts the toggle endpoint with a payload
    // naming only that row's tool (the wire names what the user
    // touched; the server derives the rest).
    let row = row_fragment(&html, "bash");
    assert!(
        row.contains(&format!("@post('{}'", contract::TOOLS_TOGGLE_PATH)),
        "the row posts the toggle path: {row}"
    );
    assert!(
        row.contains("tool: 'bash'"),
        "the payload names the clicked tool: {row}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_modal_ships_hidden_and_closes_on_the_open_signal() {
    // Given the modal fragment.
    let (state, shared) = harness();
    let id = seed_session(&state);

    // When rendering it.
    let html = super::modal_fragment(&shared, &id.to_string(), "").expect("the modal renders");

    // Then the overlay is visibility-owned by the open signal and
    // ships hidden — a fresh pull never shows the modal unbidden — and
    // the dismissal handlers are guarded by the open signal (a hidden
    // modal's document escape must not re-post a stale snapshot).
    let overlay = html
        .split_once(&format!("id=\"{}\"", contract::TOOLS_MODAL_ID))
        .map_or("", |(_, tail)| tail.split('>').next().unwrap_or(""));
    assert!(
        overlay.contains("data-attr:hidden=") && overlay.contains("hidden"),
        "the overlay ships hidden: {html}"
    );
    assert!(
        overlay.contains("evt.target === evt.currentTarget"),
        "the click-away rides the overlay: {html}"
    );
    assert!(
        html.contains("$toolsOpen &&"),
        "the dismissals are guarded by the open signal: {html}"
    );
    assert!(
        html.contains(&format!("@post('{}'", contract::TOOLS_REVERT_PATH)),
        "dismissal posts the revert: {html}"
    );
}
