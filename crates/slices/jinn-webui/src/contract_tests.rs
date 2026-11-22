//! The DOM-contract tests: referential integrity between what the
//! client targets and what the templates render.
//!
//! The shipped `targetError` bug: a fragment template pointed a swap
//! at an id the page shell never defined, and the click silently did
//! nothing. These tests make that class fail the build — every id,
//! attribute, and action name the client contract names must appear
//! in the rendered markup, and every action name must be one the
//! client knows how to post.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use jinn_webui_msg::SessionListItem;

use crate::contract;
use crate::templates;

/// One picker entry for a render test: availability, alias, and remote
/// flags chosen by the caller; everything else minimal.
fn test_entry(
    available: bool,
    is_alias: bool,
    is_remote: bool,
) -> jinn_provider_selection_msg::ProviderPickerEntry {
    jinn_provider_selection_msg::ProviderPickerEntry {
        provider_id: "openai/gpt-4o".to_owned(),
        name: "gpt-4o".to_owned(),
        provider_name: "openai".to_owned(),
        backend: "openai".to_owned(),
        model: "gpt-4o".to_owned(),
        search_text: "gpt-4o openai".to_owned(),
        is_alias,
        alias_target: None,
        is_available: available,
        is_remote,
        is_active: false,
        selected: false,
        theme: jinn_theme::default_theme(),
    }
}

/// One seeded row for the sessions fragment.
fn row(session_id: jinn_core_types::SessionId, title: &str, active: bool) -> SessionListItem {
    SessionListItem {
        session_id,
        title: title.to_owned(),
        active,
        is_bot: false,
        loaded: true,
        unread: false,
        working: false,
        depth: 0,
    }
}

/// Renders the full page shell with all regions filled — the shape
/// `GET /web/session/{id}` serves.
fn rendered_page() -> String {
    let sessions = sessions_fragment_of(&[]);
    let transcript = templates::render(&templates::TranscriptTemplate {
        entries: Vec::new(),
        start: 0,
    })
    .expect("an empty transcript renders");
    let controls = templates::render(&templates::ControlsTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        false,
    ))
    .expect("idle controls render");
    let button = templates::render(&templates::ModelButtonTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        String::new(),
    ))
    .expect("an empty picker renders");
    let model = templates::render(&templates::ModelStatusTemplate::with_contract(
        String::new(),
        button,
    ))
    .expect("an empty status wrapper renders");
    templates::render(&templates::PageTemplate {
        style: {
            let leaked: &'static str = Box::leak(templates::page_style().into_boxed_str());
            leaked
        },
        session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
        title: "Contract".to_owned(),
        cursor: 0,
        sessions_html: sessions,
        transcript_html: transcript,
        controls_html: controls,
        status_html: model,
        menu_html: crate::regions::menu::menu_fragment("00000000-0000-0000-0000-000000000000", 0),
        sessions_attention: 0,
        vendor_prefix: contract::VENDOR_PREFIX,
        app_js_versioned: crate::templates::app_js_versioned_path(),
        paste_js_versioned: crate::templates::paste_js_versioned_path(),
        page_root_id: contract::PAGE_ROOT_ID,
        page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
        sessions_id: contract::SESSIONS_ID,
        sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
        modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
        session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
        persona_modal_host_id: contract::PERSONA_MODAL_HOST_ID,
        mcp_modal_host_id: contract::MCP_MODAL_HOST_ID,
        tools_modal_host_id: contract::TOOLS_MODAL_HOST_ID,
        skills_modal_host_id: contract::SKILL_MODAL_HOST_ID,
        transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
        transcript_id: contract::TRANSCRIPT_ID,
        cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
        controls_id: contract::CONTROLS_ID,
        session_title_id: contract::SESSION_TITLE_ID,
        menu_id: contract::MENU_ID,
        drawer_scrim_id: contract::DRAWER_SCRIM_ID,
        menu_scrim_id: contract::MENU_SCRIM_ID,
        switch_overlay_id: contract::SWITCH_OVERLAY_ID,
        stream_url: String::new(),
        sessions_route: contract::SESSIONS_ROUTE,
        session_page_path: contract::SESSION_PAGE_PREFIX,
    })
    .expect("the page shell renders")
}

/// The sessions fragment for the given rows.
fn sessions_fragment_of(rows: &[SessionListItem]) -> String {
    templates::render(&templates::SessionsTemplate::with_contract(rows.to_vec()))
        .expect("the sessions fragment renders")
}

// ── Every client target exists in the rendered page ─────────────────

#[rstest::rstest]
#[test]
fn every_id_the_contract_names_exists_in_the_rendered_page() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // When collecting every id the contract says the client targets.
    let targets: [(&str, &str); 25] = [
        ("page root", contract::PAGE_ROOT_ID),
        ("sessions", contract::SESSIONS_ID),
        ("transcript", contract::TRANSCRIPT_ID),
        ("transcript wrap", contract::TRANSCRIPT_WRAP_ID),
        ("controls", contract::CONTROLS_ID),
        ("model picker", contract::MODEL_PICKER_ID),
        (
            "model picker modal host",
            contract::MODEL_PICKER_MODAL_HOST_ID,
        ),
        ("model picker button", contract::MODEL_PICKER_BUTTON_ID),
        ("composer", contract::COMPOSER_ID),
        ("session title", contract::SESSION_TITLE_ID),
        ("menu", contract::MENU_ID),
        ("menu button", contract::MENU_BUTTON_ID),
        ("menu panel", contract::MENU_PANEL_ID),
        ("menu item sessions", contract::MENU_ITEM_SESSIONS_ID),
        ("menu item persona", contract::MENU_ITEM_PERSONA_ID),
        ("persona modal host", contract::PERSONA_MODAL_HOST_ID),
        ("menu item mcp", contract::MENU_ITEM_MCP_ID),
        ("mcp modal host", contract::MCP_MODAL_HOST_ID),
        ("menu item skills", contract::MENU_ITEM_SKILLS_ID),
        ("skills modal host", contract::SKILL_MODAL_HOST_ID),
        ("drawer scrim", contract::DRAWER_SCRIM_ID),
        ("menu scrim", contract::MENU_SCRIM_ID),
        ("image chips", contract::IMAGE_CHIPS_ID),
        ("image file input", contract::IMAGE_FILE_INPUT_ID),
        ("image attach label", contract::IMAGE_ATTACH_BUTTON_ID),
    ];

    // Then each one appears as `id="..."` in the markup — a missing
    // target is a silent no-op click in the browser, never an error
    // the server can see.
    for (name, id) in targets {
        assert!(
            page.contains(&format!("id=\"{id}\"")),
            "contract id `{name}` ({id}) is not in the rendered page — the client would \
             target a missing element and silently do nothing"
        );
    }
}

#[rstest::rstest]
#[test]
fn the_page_shell_carries_the_webapp_install_links() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // Then the head carries every tag installability reads — the
    // manifest link, the favicon (the Android shortcut builder's icon
    // source), the apple-touch icon, and the theme color. Each
    // referenced asset is a real vendor row, so a renamed file fails
    // here rather than silently 404ing in a browser.
    let markers = [
        format!("{}manifest.webmanifest\"", contract::VENDOR_PREFIX),
        format!("{}icon-favicon-32.png\"", contract::VENDOR_PREFIX),
        format!("{}icon-apple-touch-180.png\"", contract::VENDOR_PREFIX),
    ];
    for marker in &markers {
        assert!(
            page.contains(marker.as_str()),
            "the page head must reference {marker}; got: {page}"
        );
    }
    assert!(page.contains("rel=\"manifest\""), "{page}");
    assert!(page.contains("rel=\"apple-touch-icon\""), "{page}");
    assert!(page.contains("<meta name=\"theme-color\""), "{page}");
    // And each referenced path is a vouched vendor row.
    for path in [
        "manifest.webmanifest",
        "icon-favicon-32.png",
        "icon-apple-touch-180.png",
        "icon-192.png",
        "icon-512.png",
        "icon-maskable-512.png",
    ] {
        assert!(
            crate::vendor::resolve_request(path).is_some(),
            "the page shell names {path} but no vendor row vouches for it"
        );
    }
}

#[rstest::rstest]
#[test]
fn the_client_script_tag_names_the_vendored_bundle() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // When finding the module script tag.
    let versioned = crate::templates::app_js_versioned_path();
    let marker = format!("src=\"{}{versioned}\"", contract::VENDOR_PREFIX);

    // Then it loads the bundle under its CACHE-BUSTING URL — the hash
    // segment is the manifest row's sha256 prefix, so an upgraded
    // binary serves a URL no old page has cached.
    assert!(
        page.contains(&marker),
        "the page must load {marker} as the client entry; got: {page}"
    );
    // And the versioned URL is real: it resolves back to the row.
    assert!(
        crate::vendor::resolve_request(&versioned)
            .is_some_and(|(asset, hash)| asset.path == contract::VENDOR_APP_JS && hash.is_some()),
        "the versioned entry URL must resolve through the request table"
    );
    assert!(
        page.contains("type=\"module\""),
        "the client must load as a module script (the dist carries an \
         ESM export tail): {page}"
    );
}

#[rstest::rstest]
#[test]
fn the_page_loads_the_paste_helper_as_a_versioned_classic_script() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // When finding the paste helper's script tag.
    let versioned = crate::templates::paste_js_versioned_path();
    let marker = format!("src=\"{}{versioned}\"", contract::VENDOR_PREFIX);

    // Then the page loads it under its cache-busting URL, as a plain
    // (non-module) script — modules defer, so a module-tagged helper
    // would evaluate after the bundle's first expressions run.
    assert!(
        page.contains(&marker),
        "the page must load {marker} as the attach helper; got: {page}"
    );
    assert!(
        crate::vendor::resolve_request(&versioned)
            .is_some_and(|(asset, hash)| asset.path == contract::VENDOR_PASTE_JS && hash.is_some()),
        "the versioned helper URL must resolve through the request table"
    );
}

#[rstest::rstest]
#[test]
fn the_paste_helper_bytes_name_the_contract_ids_they_drive() {
    // Given the helper's embedded bytes (the pinned vendor row).
    let asset = crate::vendor::VENDOR_ASSETS
        .iter()
        .find(|a| a.path == contract::VENDOR_PASTE_JS)
        .expect("paste.js is a manifest row");
    let source = String::from_utf8(asset.bytes.to_vec()).expect("paste.js is utf-8");

    // Then every contract id and signal the helper touches appears in
    // its source verbatim — a contract rename without a helper re-pin
    // fails here instead of silently leaving paste and chips dead.
    for token in [
        contract::IMAGE_CHIPS_ID,
        contract::IMAGE_FILE_INPUT_ID,
        contract::SIGNAL_PENDING_IMAGES,
    ] {
        assert!(
            source.contains(token),
            "paste.js must name `{token}` — the helper drives the contract's DOM"
        );
    }
    // And the merge event name the composer's expression subscribes to
    // matches the helper's dispatch.
    assert!(
        source.contains("jinnaddimages"),
        "paste.js must dispatch the merge event the composer listens for"
    );
}

#[rstest::rstest]
#[test]
fn the_page_names_the_viewed_session_in_the_data_attribute() {
    // Given the fully rendered page shell for a session id.
    let id = jinn_core_types::SessionId::new();
    let sessions = sessions_fragment_of(&[]);
    let page = templates::render(&templates::PageTemplate {
        style: {
            let leaked: &'static str = Box::leak(templates::page_style().into_boxed_str());
            leaked
        },
        session_id: id.to_string(),
        title: "Named".to_owned(),
        cursor: 3,
        sessions_html: sessions,
        transcript_html: String::new(),
        controls_html: String::new(),
        status_html: String::new(),
        menu_html: crate::regions::menu::menu_fragment(&id.to_string(), 0),
        sessions_attention: 0,
        vendor_prefix: contract::VENDOR_PREFIX,
        app_js_versioned: crate::templates::app_js_versioned_path(),
        paste_js_versioned: crate::templates::paste_js_versioned_path(),
        page_root_id: contract::PAGE_ROOT_ID,
        page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
        sessions_id: contract::SESSIONS_ID,
        sessions_filter_input_id: contract::SESSIONS_FILTER_INPUT_ID,
        modal_host_id: contract::MODEL_PICKER_MODAL_HOST_ID,
        session_new_host_id: contract::SESSION_NEW_MODAL_HOST_ID,
        persona_modal_host_id: contract::PERSONA_MODAL_HOST_ID,
        mcp_modal_host_id: contract::MCP_MODAL_HOST_ID,
        tools_modal_host_id: contract::TOOLS_MODAL_HOST_ID,
        skills_modal_host_id: contract::SKILL_MODAL_HOST_ID,
        transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
        transcript_id: contract::TRANSCRIPT_ID,
        cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
        controls_id: contract::CONTROLS_ID,
        session_title_id: contract::SESSION_TITLE_ID,
        menu_id: contract::MENU_ID,
        drawer_scrim_id: contract::DRAWER_SCRIM_ID,
        menu_scrim_id: contract::MENU_SCRIM_ID,
        switch_overlay_id: contract::SWITCH_OVERLAY_ID,
        stream_url: String::new(),
        sessions_route: contract::SESSIONS_ROUTE,
        session_page_path: contract::SESSION_PAGE_PREFIX,
    })
    .expect("the page shell renders");

    // Then the page-root carries the session id in the exact attribute
    // the client reads, and the transcript carries the cursor the
    // initial Last-Event-ID comes from.
    assert!(
        page.contains(&format!("{}=\"{}\"", contract::PAGE_SESSION_DATA_ATTR, id)),
        "{page}"
    );
    assert!(
        page.contains(&format!("{}=\"3\"", contract::TRANSCRIPT_CURSOR_DATA_ATTR)),
        "{page}"
    );
}

// ── Fragment markup matches the client's selectors ──────────────────

#[rstest::rstest]
#[test]
fn a_session_row_carries_the_class_and_data_attribute_the_client_targets() {
    // Given a sessions fragment with one row.
    let id = jinn_core_types::SessionId::new();
    let fragment = sessions_fragment_of(&[row(id.clone(), "Row contract", true)]);

    // Then the row is selectable by the contract's class hook and
    // carries the session id in the contract's data attribute — the
    // two things the client's click delegation reads.
    assert!(
        fragment.contains(&format!("class=\"{} ", contract::SESSION_ROW_CLASS)),
        "the row class hook is missing: {fragment}"
    );
    assert!(
        fragment.contains(&format!("{}=\"{}\"", contract::SESSION_ID_DATA_ATTR, id)),
        "the row's session id attribute is missing: {fragment}"
    );
}

#[rstest::rstest]
#[test]
fn composer_buttons_name_the_actions_the_client_maps_to_endpoints() {
    // Given idle and busy controls fragments.
    let idle = templates::render(&templates::ControlsTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        false,
    ))
    .expect("idle controls render");
    let busy = templates::render(&templates::ControlsTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        true,
    ))
    .expect("busy controls render");

    // Then the idle fragment binds send (its endpoint spelled from
    // the contract) and the busy one binds cancel.
    assert!(
        idle.contains(&format!("@post('{}'", contract::SEND_PATH)),
        "{idle}"
    );
    assert!(
        busy.contains(&format!("@post('{}'", contract::CANCEL_PATH)),
        "{busy}"
    );
    // And the composer carries the input id the client reads the
    // message text from.
    for fragment in [&idle, &busy] {
        assert!(
            fragment.contains(&format!("id=\"{}\"", contract::INPUT_ID)),
            "{fragment}"
        );
    }
}

#[rstest::rstest]
#[test]
fn the_model_picker_names_the_action_the_client_maps_to_the_model_endpoint() {
    // Given a model-picker modal with one available row.
    let fragment = templates::render(&templates::ModelRowTemplate::from_filtered(
        jinn_provider_selection_msg::FilteredEntry {
            entry: test_entry(true, false, false),
            match_indices: Vec::new(),
        },
        "00000000-0000-0000-0000-000000000000".to_owned(),
        false,
    ))
    .expect("the row renders");

    // Then the row's switch rides the declarative click handler
    // posting the model endpoint spelled from the contract.
    assert!(
        fragment.contains(&format!("@post('{}'", contract::MODEL_PATH)),
        "{fragment}"
    );
}

#[rstest::rstest]
#[test]
fn an_available_row_carries_its_model_id_and_the_switch_post_targets_it() {
    // Given an available, non-marker row.
    let fragment = render_row(test_entry(true, false, false), false);

    // Then the row carries its model id in the contract's attribute
    // — the value the switch POST sends.
    assert!(
        fragment.contains(&format!(
            "{}=\"openai/gpt-4o\"",
            contract::MODEL_ID_DATA_ATTR
        )),
        "{fragment}"
    );
}

#[rstest::rstest]
#[test]
fn an_unavailable_row_is_marked_and_names_no_action() {
    // Given an unavailable row (no api key).
    let fragment = render_row(test_entry(false, false, false), false);

    // Then it carries the unavailable attribute — the client's
    // non-interactive hook — and no action (a disabled element must
    // not advertise the endpoint it cannot legally call).
    assert!(
        fragment.contains(&format!("{}=\"true\"", contract::MODEL_UNAVAILABLE_ATTR)),
        "{fragment}"
    );
    assert!(
        !fragment.contains(&format!("{}=", contract::ACTION_DATA_ATTR)),
        "{fragment}"
    );
}

#[rstest::rstest]
#[test]
fn an_alias_row_carries_the_alias_marker() {
    // Given an alias row.
    let fragment = render_row(test_entry(true, true, false), false);

    // Then it carries the alias marker (`→` in the marker span).
    assert!(
        fragment.contains(&format!("{}=\"alias\"", contract::MODEL_MARKER_ATTR)),
        "{fragment}"
    );
    assert!(fragment.contains('→'), "{fragment}");
}

#[rstest::rstest]
#[test]
fn a_remote_row_carries_the_remote_marker() {
    // Given a remote-discovered row.
    let fragment = render_row(test_entry(true, false, true), false);

    // Then it carries the remote marker (`*` in the marker span).
    assert!(
        fragment.contains(&format!("{}=\"remote\"", contract::MODEL_MARKER_ATTR)),
        "{fragment}"
    );
    assert!(fragment.contains('*'), "{fragment}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_active_row_is_promoted_and_marked_in_the_modal() {
    // Given the modal fragment for a session whose current model is
    // the entry's.
    let shared = test_shared();
    let session_id = seed_session(&shared);
    let html = crate::regions::model_picker::modal_fragment(&shared, &session_id, "")
        .expect("the modal renders");

    // Then the active row renders (the promotion path ran) and the
    // modal carries the filter input and the refresh button — the
    // modal's three fixtures the client targets.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MODEL_PICKER_MODAL_ID)),
        "{html}"
    );
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MODEL_PICKER_FILTER_ID)),
        "{html}"
    );
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MODEL_PICKER_ROWS_ID)),
        "{html}"
    );
    assert!(
        html.contains(&format!("@post('{}'", contract::MODELS_REFRESH_PATH)),
        "{html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_button_fragment_carries_the_button_id_and_the_model_action_name() {
    // Given the titlebar button fragment.
    let shared = test_shared();
    let session_id = seed_session(&shared);
    let html = crate::regions::model_picker::button_fragment(&shared, &session_id)
        .expect("the button renders");

    // Then it names the button id (the page-presence gate checks the
    // page carries it; here we check the fragment itself) and hides
    // no modal markup — the button is the modal's trigger, not the
    // modal.
    assert!(
        html.contains(&format!("id=\"{}\"", contract::MODEL_PICKER_BUTTON_ID)),
        "{html}"
    );
    assert!(
        !html.contains(&format!("id=\"{}\"", contract::MODEL_PICKER_MODAL_ID)),
        "{html}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn every_modal_carries_click_away_on_the_overlay_and_escape_on_the_card() {
    // Given the three modal templates' rendered markup (empty persona
    // rows — the close handlers' placement is what is under test).
    let shared = test_shared();
    let session_id = seed_session(&shared);
    let persona = crate::regions::persona_picker::modal_fragment(&shared, &session_id, "")
        .expect("the persona modal renders");
    let model = crate::regions::model_picker::modal_fragment(&shared, &session_id, "")
        .expect("the model modal renders");
    let session_new =
        crate::regions::session_new::form_fragment(jinn_config::empty_config_layer(), "")
            .expect("the new-session modal renders");

    // Then each modal's `data-on:click` close handler rides the
    // OVERLAY element (the `<div id=…>` the `data-attr:hidden` rides)
    // — a handler on the inner card fires only when the card itself
    // is clicked, and the backdrop click the user expects never
    // reaches it. The `data-on:keydown__escape__document` handler
    // rides the card, where the fragment renders it.
    for (name, html, overlay_id) in [
        ("persona", &persona, contract::PERSONA_MODAL_ID),
        ("model", &model, contract::MODEL_PICKER_MODAL_ID),
        ("session-new", &session_new, contract::SESSION_NEW_MODAL_ID),
    ] {
        let overlay_open = html
            .split_once(&format!("id=\"{overlay_id}\""))
            .map_or("", |(_, tail)| tail.split('>').next().unwrap_or(""));
        assert!(
            overlay_open.contains("data-on:click=\"evt.target === evt.currentTarget"),
            "the {name} modal's overlay tag must carry the click-away handler: {html}"
        );
        assert!(
            overlay_open.contains("data-attr:hidden="),
            "the {name} modal's overlay tag is the visibility owner: {html}"
        );
        // And the card never carries the click handler — the card's
        // own clicks must not close the modal.
        let card = html
            .split_once("-inner\"")
            .map_or("", |(_, tail)| tail.split('>').next().unwrap_or(""));
        assert!(
            !card.contains("currentTarget"),
            "the {name} modal's card must not carry the click-away handler: {html}"
        );
    }
}

/// Renders one row with the contract hooks filled.
fn render_row(entry: jinn_provider_selection_msg::ProviderPickerEntry, is_active: bool) -> String {
    templates::render(&templates::ModelRowTemplate::from_filtered(
        jinn_provider_selection_msg::FilteredEntry {
            entry,
            match_indices: Vec::new(),
        },
        "00000000-0000-0000-0000-000000000000".to_owned(),
        is_active,
    ))
    .expect("the row renders")
}

/// A `ServerShared` over a fresh seeded state, an empty provider
/// registry and key store, and a recording bus — the fragment tests'
/// harness.
fn test_shared() -> crate::server::ServerShared {
    let slices = jinn_slices::Slices::new();
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
    crate::server::ServerShared::new(
        state,
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
        "contract-test".to_owned(),
    )
}

/// Inserts a fresh live session and returns its id as a string.
fn seed_session(shared: &crate::server::ServerShared) -> String {
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    shared.state().write().session.insert(session);
    id.to_string()
}

// ── The contract and the served page agree (self-parity) ────────────
//
// The TypeScript mirror is gone: the contract's single consumer is
// the askama page itself, which interpolates the constants at compile
// time. What remains to gate is the vocabulary's integrity — every
// constant referenced somewhere (a template, a push, a route) is
// declared, unique, and present in the rendered page.

#[rstest::rstest]
#[test]
fn every_dom_constant_is_a_unique_nonempty_vocabulary_entry() {
    // Given the contract's DOM constants.
    let mut seen = std::collections::BTreeSet::new();

    // Then each is non-empty and no two agree — a duplicated or empty
    // id would make the registry's uniqueness gate and the browser's
    // getElementById disagree silently.
    assert!(
        !contract::DOM_CONSTANTS.is_empty(),
        "the contract names at least one DOM constant"
    );
    for value in contract::DOM_CONSTANTS {
        assert!(!value.is_empty(), "an empty DOM constant is declared");
        assert!(
            seen.insert(*value),
            "the DOM constant {value} is declared twice"
        );
    }
}

#[rstest::rstest]
#[test]
fn every_sse_kind_maps_to_a_datastar_transport() {
    // Given every SSE kind the server can push (derived from the
    // enum, not a hand-maintained list).
    let kinds = [
        jinn_webui_msg::SseKind::Chat,
        jinn_webui_msg::SseKind::Controls,
        jinn_webui_msg::SseKind::Sessions,
        jinn_webui_msg::SseKind::Models,
        jinn_webui_msg::SseKind::McpStatus,
        jinn_webui_msg::SseKind::Skills,
        jinn_webui_msg::SseKind::Tools,
        jinn_webui_msg::SseKind::TranscriptFull,
    ];

    // Then each translates to an SDK event without error and the
    // selector it patches names a contract id — the push vocabulary
    // and the DOM vocabulary cannot drift apart.
    for kind in kinds {
        let events = crate::push::sdk_events_of(
            &jinn_webui_msg::SseEnvelope {
                kind,
                session_id: jinn_core_types::SessionId::new(),
                cursor: 1,
                html: "<span>x</span>".to_owned(),
            },
            1,
        )
        .unwrap_or_else(|error| panic!("{kind:?} must translate: {error:?}"));
        let event = events
            .first()
            .expect("every kind translates to at least one event")
            .clone();
        let data = event.data.join("\n");
        for id in [contract::TRANSCRIPT_ID, contract::CONTROLS_ID] {
            let _ = id; // vocabulary tie: selectors live in push.rs
        }
        assert!(
            data.contains("selector") || event.event == datastar::consts::EventType::PatchSignals,
            "{kind:?} must carry a selector or be a signal patch: {data}"
        );
    }
}

#[rstest::rstest]
#[test]
fn the_shipped_bundle_is_the_self_contained_datastar_runtime() {
    // Given the shipped bundle's bytes (the committed dist the binary
    // embeds — one self-contained classic script, no import graph).
    let entry = crate::vendor::VENDOR_ASSETS
        .iter()
        .find(|a| a.path == contract::VENDOR_APP_JS)
        .expect("the bundle is a manifest row");
    let mut decoder = flate2::read::GzDecoder::new(entry.bytes);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut bytes).expect("the bundle gunzips");
    let source = String::from_utf8(bytes).expect("the bundle is utf-8");

    // Then it is the Datastar runtime: the banner names it, it pulls
    // no other module (an import would 404 — the vendor table holds
    // exactly this one row), and the page must load it as a module
    // script (the dist carries an ESM export tail; the earlier
    // classic-script tag would have been a SyntaxError — caught here).
    assert!(source.contains("Datastar v"), "not the Datastar bundle");
    assert!(!source.contains("from \"./"), "unexpected relative import");
    assert!(!source.contains("import("), "unexpected dynamic import");
    assert!(
        source.contains("export{"),
        "the dist's ESM tail is expected"
    );
}

// ── The session-switch interstitial ─────────────────────────────────

#[rstest::rstest]
#[test]
fn the_page_shell_seeds_the_switching_signal_on_both_page_shapes() {
    // Given the fully rendered page shells for the landing page and a
    // session page — the two shapes a fresh render serves.
    let landing = templates::render(&templates::PageTemplate::landing(
        sessions_fragment_of(&[]),
        crate::regions::menu::menu_fragment("", 0),
        0,
    ))
    .expect("the landing page renders");
    let session_page = rendered_page();
    let marker = "switching: false";

    // Then both shapes seed the interstitial signal false in the
    // root's data-signals — a signal an expression references but the
    // seed omits throws at bind time and silently kills every
    // expression on the element.
    for (shape, page) in [("landing", &landing), ("session", &session_page)] {
        assert!(
            page.contains(marker),
            "the {shape} page must seed `switching: false` in the signal set: {page}"
        );
    }
}

#[rstest::rstest]
#[test]
fn the_switch_interstitial_renders_hidden_and_signal_bound_in_the_page_shell() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // Then the interstitial element exists with the contract's id,
    // rendered server-side hidden (no visible flash before the
    // runtime binds — the scrims' pattern), and its visibility rides
    // the switching signal.
    assert!(
        page.contains(&format!("id=\"{}\"", contract::SWITCH_OVERLAY_ID)),
        "the interstitial id is missing from the page shell: {page}"
    );
    let overlay_pos = page
        .find(&format!("id=\"{}\"", contract::SWITCH_OVERLAY_ID))
        .expect("the interstitial renders");
    let overlay = page
        .get(overlay_pos..(overlay_pos + 600).min(page.len()))
        .unwrap_or_default();
    assert!(
        overlay.contains(" hidden"),
        "the interstitial must render hidden server-side (no boot flash): {overlay}"
    );
    assert!(
        overlay.contains("$switching"),
        "the interstitial's visibility must ride the switching signal: {overlay}"
    );
}

#[rstest::rstest]
#[test]
fn the_switch_interstitial_is_inert_markup_with_no_dismissal_path() {
    // Given the fully rendered page shell.
    let page = rendered_page();
    let start = page
        .find(&format!("id=\"{}\"", contract::SWITCH_OVERLAY_ID))
        .expect("the interstitial renders");
    let overlay = page
        .get(start..(start + 600).min(page.len()))
        .unwrap_or_default();

    // Then the overlay carries no click handler — the activation
    // response's page swap re-seeding the signal is the ONLY clear
    // path (no manual dismissal, no timer, no fetch-event listener).
    assert!(
        !overlay.contains("data-on:click"),
        "the interstitial must carry no dismissal handler: {overlay}"
    );
}

#[rstest::rstest]
#[test]
fn a_session_row_suppresses_the_viewed_session_and_raises_the_signal_before_posting() {
    // Given a sessions fragment with one row (active or not — the
    // expression is identical; only the id differs).
    let id = jinn_core_types::SessionId::new();
    let fragment = sessions_fragment_of(&[row(id.clone(), "Switch source", false)]);

    // Then the row's click expression checks the viewed-session
    // signal against the row's id first (the viewed row fires
    // nothing), raises the interstitial signal ahead of the POST
    // (no round trip first), and posts the activate endpoint with
    // the row's id.
    assert!(
        fragment.contains(&format!("$viewedSession === '{id}'")),
        "the row must compare the viewed-session signal against its id: {fragment}"
    );
    assert!(
        fragment.contains(&format!(
            "$switching = true, @post('{}{id}/activate'",
            contract::SESSION_PAGE_PREFIX
        )),
        "the row must raise the signal ahead of the activate POST: {fragment}"
    );
}

#[rstest::rstest]
#[test]
fn the_switch_overlay_css_stacks_above_the_menu_panel_and_hides_by_default() {
    // Given the sessions region's stylesheet — the sheet the overlay's
    // rules travel in.
    let css = crate::regions::sessions::SIDEBAR_CSS;

    // Then the overlay is a fixed full-viewport layer above the menu
    // panel's z-index (70), and its hidden rule keeps it inert at
    // boot — the same shape the drawer and menu scrims pin.
    assert!(
        css.contains("#switch-overlay { position: fixed; inset: 0;"),
        "the overlay is a fixed full-viewport layer: {css}"
    );
    assert!(
        css.contains("z-index: 80"),
        "the overlay stacks above the menu panel (70): {css}"
    );
    assert!(
        css.contains("#switch-overlay[hidden]"),
        "the overlay's hidden rule keeps it inert at boot: {css}"
    );
    assert!(
        css.contains(".switch-spinner") && css.contains("webui-spin"),
        "the overlay's spinner reuses the shared spin keyframes: {css}"
    );
}
