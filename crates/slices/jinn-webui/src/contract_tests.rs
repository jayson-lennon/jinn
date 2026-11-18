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

/// One seeded row for the sessions fragment.
fn row(session_id: jinn_core_types::SessionId, title: &str, active: bool) -> SessionListItem {
    SessionListItem {
        session_id,
        title: title.to_owned(),
        active,
        is_bot: false,
        loaded: true,
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
    let model = templates::render(&templates::ModelTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        String::new(),
        Vec::new(),
    ))
    .expect("an empty picker renders");
    templates::render(&templates::PageTemplate {
        style: templates::STYLE_CSS,
        session_id: "00000000-0000-0000-0000-000000000000".to_owned(),
        title: "Contract".to_owned(),
        model: String::new(),
        cursor: 0,
        sessions_html: sessions,
        transcript_html: transcript,
        controls_html: controls,
        model_html: model,
        vendor_prefix: contract::VENDOR_PREFIX,
        app_js: contract::VENDOR_APP_JS,
        page_root_id: contract::PAGE_ROOT_ID,
        page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
        sessions_id: contract::SESSIONS_ID,
        model_picker_id: contract::MODEL_PICKER_ID,
        transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
        transcript_id: contract::TRANSCRIPT_ID,
        cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
        controls_id: contract::CONTROLS_ID,
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
    let targets: [(&str, &str); 7] = [
        ("page root", contract::PAGE_ROOT_ID),
        ("sessions", contract::SESSIONS_ID),
        ("transcript", contract::TRANSCRIPT_ID),
        ("transcript wrap", contract::TRANSCRIPT_WRAP_ID),
        ("controls", contract::CONTROLS_ID),
        ("model picker", contract::MODEL_PICKER_ID),
        ("composer", contract::COMPOSER_ID),
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
fn the_client_script_tag_names_the_vendored_bundle() {
    // Given the fully rendered page shell.
    let page = rendered_page();

    // When finding the module script tag.
    let marker = format!(
        "src=\"{}{}\"",
        contract::VENDOR_PREFIX,
        contract::VENDOR_APP_JS
    );

    // Then it loads the contract's bundle as a module — the client's
    // entry point is reachable at the path the vendor table serves.
    assert!(
        page.contains(&marker),
        "the page must load {marker} as the client entry; got: {page}"
    );
    assert!(
        page.contains("type=\"module\""),
        "the client must load as an ES module: {page}"
    );
}

#[rstest::rstest]
#[test]
fn the_page_names_the_viewed_session_in_the_data_attribute() {
    // Given the fully rendered page shell for a session id.
    let id = jinn_core_types::SessionId::new();
    let sessions = sessions_fragment_of(&[]);
    let page = templates::render(&templates::PageTemplate {
        style: templates::STYLE_CSS,
        session_id: id.to_string(),
        title: "Named".to_owned(),
        model: String::new(),
        cursor: 3,
        sessions_html: sessions,
        transcript_html: String::new(),
        controls_html: String::new(),
        model_html: String::new(),
        vendor_prefix: contract::VENDOR_PREFIX,
        app_js: contract::VENDOR_APP_JS,
        page_root_id: contract::PAGE_ROOT_ID,
        page_session_attr: contract::PAGE_SESSION_DATA_ATTR,
        sessions_id: contract::SESSIONS_ID,
        model_picker_id: contract::MODEL_PICKER_ID,
        transcript_wrap_id: contract::TRANSCRIPT_WRAP_ID,
        transcript_id: contract::TRANSCRIPT_ID,
        cursor_attr: contract::TRANSCRIPT_CURSOR_DATA_ATTR,
        controls_id: contract::CONTROLS_ID,
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

    // Then the idle fragment names the send action and the busy one
    // names cancel — the action names the client maps to POST
    // endpoints, spelled exactly as the contract defines them.
    assert!(
        idle.contains(&format!(
            "{}=\"{}\"",
            contract::ACTION_DATA_ATTR,
            contract::ACTION_SEND
        )),
        "{idle}"
    );
    assert!(
        busy.contains(&format!(
            "{}=\"{}\"",
            contract::ACTION_DATA_ATTR,
            contract::ACTION_CANCEL
        )),
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
    // Given a model-picker fragment with one entry.
    let fragment = templates::render(&templates::ModelTemplate::with_contract(
        "00000000-0000-0000-0000-000000000000".to_owned(),
        "openai/gpt-4o".to_owned(),
        vec!["openai/gpt-4o".to_owned()],
    ))
    .expect("the picker renders");

    // Then the select names the model action via the contract's
    // attribute.
    assert!(
        fragment.contains(&format!(
            "{}=\"{}\"",
            contract::ACTION_DATA_ATTR,
            contract::ACTION_MODEL
        )),
        "{fragment}"
    );
}

// ── The contract and the emitted client agree (parity) ──────────────

#[rstest::rstest]
#[test]
fn the_contract_source_and_the_client_wire_file_agree_on_every_name() {
    // Given the contract's public names and the client's TypeScript
    // source (compiled to /vendor by `just client-check`; the source
    // rides beside this crate in the workspace tree).
    let client_src = client_wire_source();

    // When reading each name the two sides must agree on.
    let pairs: [(&str, &str); 16] = [
        ("page root id", contract::PAGE_ROOT_ID),
        ("sessions id", contract::SESSIONS_ID),
        ("transcript id", contract::TRANSCRIPT_ID),
        ("transcript wrap id", contract::TRANSCRIPT_WRAP_ID),
        ("controls id", contract::CONTROLS_ID),
        ("model picker id", contract::MODEL_PICKER_ID),
        ("composer id", contract::COMPOSER_ID),
        ("input id", contract::INPUT_ID),
        ("session row class", contract::SESSION_ROW_CLASS),
        ("session id data attr", contract::SESSION_ID_DATA_ATTR),
        ("page session data attr", contract::PAGE_SESSION_DATA_ATTR),
        ("cursor data attr", contract::TRANSCRIPT_CURSOR_DATA_ATTR),
        ("action data attr", contract::ACTION_DATA_ATTR),
        ("send action", contract::ACTION_SEND),
        ("cancel action", contract::ACTION_CANCEL),
        ("model action", contract::ACTION_MODEL),
    ];

    // Then each contract value appears verbatim in the client source —
    // one drift test, both directions (a renamed id on either side
    // fails here before any browser sees it).
    for (label, value) in pairs {
        assert!(
            client_src.contains(value),
            "contract value `{label}` ({value}) is missing from webui/client/src/wire.ts — \
             the Rust and TypeScript contracts have drifted"
        );
    }
}

/// The client's TypeScript wire source, read from the workspace tree.
///
/// The file is authored (not a build artifact), so reading it here is
/// stable; the compiled bundle is gated by `just client-check`, and
/// THIS test is what catches a source-level rename on either side.
///
/// Anchored at `CARGO_MANIFEST_DIR`, never at the process cwd: test
/// exes for this workspace run from a build-cache mirror
/// (`target/debug/...`), whose `../../../` lands in the cache, not in
/// the source tree.
fn client_wire_source() -> String {
    // <workspace>/crates/slices/jinn-webui -> <workspace>/webui/client.
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../webui/client/src/wire.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "reading the client wire contract at {}: {error} — the drift test needs the \
             TypeScript source",
            path.display()
        )
    })
}
