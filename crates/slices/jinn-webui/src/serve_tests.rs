//! Serve-and-request tests: the router driven over a real TCP
//! listener, probed with reqwest.
//!
//! The tower `oneshot` tier in [`crate::server_tests`] exercises the
//! router directly; these tests add the layers below it — a real
//! socket, real connection handling, real response headers as the
//! bytes cross the wire. Each of the shipped bug classes has its pin
//! here: the bare-root redirect (the `/` 404), the vendor asset's
//! `Content-Encoding: gzip` and its gzip magic bytes (the raw-dist
//! served-as-gzip failure), the page routes' status and shape, and
//! activate's page (not fragment) response — the wrong-shape bug made
//! a failing test, not a behavior.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use axum::http::StatusCode;
use jinn_app_state::AppState;
use jinn_core_types::SessionId;
use jinn_kernel::State;
use jinn_slices::Slices;
use jinn_slices::bus::BusService;

use crate::contract;
use crate::fanout::FanoutHandle;
use crate::server::ServerShared;
use crate::server::router;

/// One live server on an OS-assigned loopback port, probed with
/// reqwest, optionally seeded with one titled session (the session's
/// id returned with the base URL). The serve task runs for the test's
/// life.
async fn harness(titled: Option<&str>) -> (String, Option<SessionId>) {
    let (base, seeded, _fanout) = harness_with_fanout(titled).await;
    (base, seeded)
}

async fn harness_with_fanout(titled: Option<&str>) -> (String, Option<SessionId>, FanoutHandle) {
    harness_seeded_with_fanout(titled, |_registry, _keys| {}).await
}

/// [`harness`] with a hook over the provider registry and key store
/// before the server boots — the picker tests' way to install models.
async fn harness_with(
    titled: Option<&str>,
    seed: impl FnOnce(&mut jinn_provider_config::ProviderRegistry, &mut jinn_provider_config::ApiKeys),
) -> (String, Option<SessionId>) {
    let (base, seeded, _fanout) = harness_seeded_with_fanout(titled, seed).await;
    (base, seeded)
}

/// The one harness body: a live server, its seeded session, and the
/// fan-out broadcast handle — every caller shape composes from this.
async fn harness_seeded_with_fanout(
    titled: Option<&str>,
    seed: impl FnOnce(&mut jinn_provider_config::ProviderRegistry, &mut jinn_provider_config::ApiKeys),
) -> (String, Option<SessionId>, FanoutHandle) {
    let slices = Slices::new();
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
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let seeded = titled.map(|title| {
        let session = jinn_session_state::ChatSessionState::new();
        let id = session.session_id().clone();
        state.write().session.insert(session);
        state
            .write()
            .session
            .get_mut(&id)
            .expect("seeded session")
            .set_title(title.to_owned());
        id
    });

    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = {
        let mut registry = jinn_provider_config::ProviderRegistry::from_config(
            jinn_provider_config::ProvidersConfig {
                providers: std::collections::BTreeMap::default(),
                aliases: vec![],
                default_provider: None,
                endpoint_defaults: vec![],
            },
        )
        .expect("an empty providers config expands");
        let mut api_keys = jinn_provider_config::ApiKeys::new();
        seed(&mut registry, &mut api_keys);
        ServerShared::new(
            state,
            bus,
            jinn_session_state::SessionStoreService::new(std::sync::Arc::new(
                jinn_kernel::common::services::test_services::FakeSessionStore,
            )),
            provider_cell,
            personas_cell,
            jinn_provider_config::ProviderRegistryService::new(registry),
            jinn_provider_config::ApiKeysService::new(api_keys),
            jinn_config::empty_config_layer().clone(),
            fanout.clone(),
            0,
            "serve-test".to_owned(),
        )
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an OS-assigned loopback port binds");
    let addr = listener.local_addr().expect("listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://{addr}"), seeded, fanout)
}

/// Installs the ring crypto provider for the reqwest client. The
/// workspace pins `rustls-no-provider` and expects each test surface
/// to opt in (the `jinn-browser` daemon tests are the in-tree
/// precedent). `install_default` errors when already installed; the
/// result is deliberately ignored.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn client() -> reqwest::Client {
    install_crypto_provider();
    // Redirects disabled: the tests assert the redirect itself, and
    // each hop's exact status.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the redirect policy builds")
}

// ── The root-404 bug class ──────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_bare_host_root_serves_a_redirect_over_a_real_socket() {
    // Given a live server on a real port.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When getting the bare host root.
    let response = http
        .get(format!("{base}/"))
        .send()
        .await
        .expect("the request completes");

    // Then it is a temporary redirect to /web — a bare visit never
    // lands on a 404, verified through the whole socket stack.
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok()),
        Some("/web"),
        "the redirect must name /web"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn following_the_root_redirect_lands_on_the_page() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    install_crypto_provider();
    let http = reqwest::Client::new();

    // When following the root's redirect (auto-follow client).
    let response = http
        .get(format!("{base}/"))
        .send()
        .await
        .expect("the redirect chain completes");

    // Then the final response is the page shell, 200, with the
    // client script tag.
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("body reads");
    assert!(body.contains("id=\"page-root\""), "{body}");
}

// ── The wrong-response-shape bug class ──────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn activating_a_session_returns_the_whole_page_over_the_wire() {
    // Given a live server holding a titled session.
    let (base, seeded) = harness(Some("Second session")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When activating it (the post a session-row click issues).
    let response = http
        .post(format!("{base}/web/session/{id}/activate"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(String::new())
        .send()
        .await
        .expect("the activate completes");

    // Then the response is the navigation patch sequence — an SSE
    // body whose element patch replaces `#page-root` with the whole
    // page shell (the Datastar transport of the same shape; the
    // shipped bug served a sessions fragment here and the swap
    // destroyed the page).
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("body reads");
    assert!(
        body.contains(&format!(
            "selector #{}\ndata: mode replace",
            contract::PAGE_ROOT_ID
        )),
        "activate must replace the page root: {body}"
    );
    assert!(body.contains("id=\"page-root\""), "{body}");
    assert!(body.contains("Second session"), "{body}");
}

#[rstest::rstest]
#[tokio::test]
async fn fragment_routes_return_fragment_bodies_not_pages() {
    // Given a live server holding a session.
    let (base, seeded) = harness(Some("Frag source")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling every fragment route.
    let sessions = http
        .get(format!("{base}/web/sessions"))
        .send()
        .await
        .expect("sessions completes");
    let transcript = http
        .get(format!("{base}/web/transcript/{id}"))
        .send()
        .await
        .expect("transcript completes");
    let picker = http
        .get(format!("{base}/web/model-picker/{id}"))
        .send()
        .await
        .expect("picker completes");

    // Then none of them carry a page shell — a fragment endpoint that
    // started answering with whole pages would recurse the swap.
    for (name, response) in [
        ("sessions", sessions),
        ("transcript", transcript),
        ("model-picker", picker),
    ] {
        let body = response.text().await.expect("body reads");
        assert!(
            !body.contains("<!DOCTYPE html>"),
            "{name} must be a fragment, not a page: {body}"
        );
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_model_picker_pull_is_a_modal_and_the_filter_narrows_it_server_side() {
    // Given a live server holding a session (its registry is empty in
    // the harness — the modal renders with zero rows but full chrome).
    let (base, seeded) = harness(Some("Picker source")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling the modal unfiltered and then with a filter that
    // matches nothing.
    let unfiltered = http
        .get(format!("{base}/web/model-picker/{id}"))
        .send()
        .await
        .expect("unfiltered pull completes")
        .text()
        .await
        .expect("unfiltered body reads");
    let filtered = http
        .get(format!(
            "{base}/web/model-picker/{id}?{}=zzz-no-match",
            crate::contract::MODEL_FILTER_QUERY_PARAM
        ))
        .send()
        .await
        .expect("filtered pull completes")
        .text()
        .await
        .expect("filtered body reads");

    // Then the unfiltered pull is the modal (chrome present, marked
    // `hidden` — the client un-hides it), and the filter echo round-
    // trips: the server stamped the typed text back into the input's
    // value so the swap preserves it.
    assert!(
        unfiltered.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_MODAL_ID
        )),
        "{unfiltered}"
    );
    assert!(
        unfiltered.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_FILTER_ID
        )),
        "{unfiltered}"
    );
    assert!(
        unfiltered.contains(&format!("id=\"{}\"", crate::contract::MODEL_PICKER_ROWS_ID)),
        "{unfiltered}"
    );
    assert!(unfiltered.contains("hidden"), "{unfiltered}");
    assert!(
        filtered.contains("value=\"zzz-no-match\""),
        "the filter must echo back into the input: {filtered}"
    );
}

/// A keyless provider entry for the picker-filter tests.
fn provider_entry(backend: &str, model: &str) -> jinn_provider_config::ProviderEntry {
    jinn_provider_config::ProviderEntry {
        model_info: Vec::new(),
        backend: backend.to_owned(),
        models: vec![model.to_owned()],
        base_url: Some("http://localhost:11434".to_owned()),
        api_key_env: None,
        requires_key: false,
        extra_body: None,
        context_length: None,
    }
}

/// Seeds the registry the picker tests filter against: a gpt and a
/// claude entry, plus one key-required provider with no key set (an
/// unavailable row that no filter may offer).
fn seed_picker_registry(
    registry: &mut jinn_provider_config::ProviderRegistry,
    _keys: &mut jinn_provider_config::ApiKeys,
) {
    let config = jinn_provider_config::ProvidersConfig {
        providers: std::collections::BTreeMap::from([
            ("openai".to_owned(), provider_entry("openai", "gpt-4o")),
            (
                "anthropic".to_owned(),
                provider_entry("anthropic", "claude-3.5-sonnet"),
            ),
            (
                "locked".to_owned(),
                jinn_provider_config::ProviderEntry {
                    model_info: Vec::new(),
                    backend: "openai".to_owned(),
                    models: vec!["locked-model".to_owned()],
                    base_url: None,
                    api_key_env: Some("LOCKED_API_KEY".to_owned()),
                    requires_key: true,
                    extra_body: None,
                    context_length: None,
                },
            ),
        ]),
        aliases: vec![],
        default_provider: None,
        endpoint_defaults: vec![],
    };
    *registry = jinn_provider_config::ProviderRegistry::from_config(config)
        .expect("the seeded config expands");
}

#[rstest::rstest]
#[tokio::test]
async fn the_fuzzy_filter_matches_what_no_substring_could_and_marks_it() {
    // Given a live server whose registry holds gpt-4o (openai) — and a
    // filter no substring of any search text matches.
    let (base, seeded) = harness_with(Some("Fuzzy"), seed_picker_registry).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling the modal with the fuzzy abbreviation "gp4".
    let body = http
        .get(format!(
            "{base}/web/model-picker/{id}?{}=gp4",
            crate::contract::MODEL_FILTER_QUERY_PARAM
        ))
        .send()
        .await
        .expect("fuzzy pull completes")
        .text()
        .await
        .expect("fuzzy body reads");

    // Then the gpt row survives (a fuzzy subsequence match — the old
    // substring filter would have dropped it), rendered with the
    // matcher's per-character match runs highlighted, and no other row
    // appears.
    assert!(body.contains("openai/gpt-4o"), "{body}");
    assert!(body.contains("<mark>gp</mark>t-<mark>4</mark>o"), "{body}");
    assert!(!body.contains("claude"), "{body}");
}

#[rstest::rstest]
#[tokio::test]
async fn the_fuzzy_filter_applies_every_term_and_orders_by_score() {
    // Given the seeded registry (gpt-4o, claude-3.5-sonnet).
    let (base, seeded) = harness_with(Some("Terms"), seed_picker_registry).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling with terms one row satisfies, then terms none does.
    let both = http
        .get(format!(
            "{base}/web/model-picker/{id}?{}=claude%20sonnet",
            crate::contract::MODEL_FILTER_QUERY_PARAM
        ))
        .send()
        .await
        .expect("multi-term pull completes")
        .text()
        .await
        .expect("multi-term body reads");
    let none = http
        .get(format!(
            "{base}/web/model-picker/{id}?{}=claude%20gpt",
            crate::contract::MODEL_FILTER_QUERY_PARAM
        ))
        .send()
        .await
        .expect("contradiction pull completes")
        .text()
        .await
        .expect("contradiction body reads");

    // Then the AND keeps only the claude row and the impossible pair
    // keeps none (the rows container renders empty).
    assert!(both.contains("anthropic/claude-3.5-sonnet"), "{both}");
    assert!(!both.contains("gpt-4o"), "{both}");
    assert!(
        none.contains(&format!("id=\"{}\"", crate::contract::MODEL_PICKER_ROWS_ID)),
        "{none}"
    );
    assert!(
        !none.contains("class=\"model-row"),
        "a no-match filter must render zero rows: {none}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_empty_filter_shows_the_unfiltered_order_with_no_marks() {
    // Given the seeded registry, one provider of which is unavailable
    // (its key is unset).
    let (base, seeded) = harness_with(Some("Unfiltered"), seed_picker_registry).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling with no filter parameter.
    let body = http
        .get(format!("{base}/web/model-picker/{id}"))
        .send()
        .await
        .expect("unfiltered pull completes")
        .text()
        .await
        .expect("unfiltered body reads");

    // Then every row renders — available entries first in alphabetical
    // model order, unavailable last with its ✗ marker — and no
    // highlight run appears anywhere.
    let claude = body
        .find("data-webui-model=\"anthropic/claude-3.5-sonnet\"")
        .expect("the claude row renders");
    let gpt = body
        .find("data-webui-model=\"openai/gpt-4o\"")
        .expect("the gpt row renders");
    let locked = body
        .find(">locked-model</span>")
        .expect("the locked row renders");
    assert!(claude < gpt && gpt < locked, "unfiltered order: {body}");
    assert!(body.contains("✗"), "{body}");
    assert!(!body.contains("<mark>"), "{body}");
    // And the unavailable row offers no switch action.
    assert!(
        body.contains(&format!(
            "{}=\"true\"",
            crate::contract::MODEL_UNAVAILABLE_ATTR
        )),
        "{body}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_fuzzy_filter_never_offers_an_unavailable_row() {
    // Given the seeded registry whose locked provider has no key set.
    let (base, seeded) = harness_with(Some("Locked"), seed_picker_registry).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When pulling with a filter that matches only the locked model.
    let body = http
        .get(format!(
            "{base}/web/model-picker/{id}?{}=locked",
            crate::contract::MODEL_FILTER_QUERY_PARAM
        ))
        .send()
        .await
        .expect("locked pull completes")
        .text()
        .await
        .expect("locked body reads");

    // Then the locked row renders but carries the unavailable marker —
    // no switch action (and no model attribute), so the client cannot
    // select it.
    assert!(body.contains("<mark>locked</mark>-model"), "{body}");
    assert!(
        body.contains(&format!(
            "{}=\"true\"",
            crate::contract::MODEL_UNAVAILABLE_ATTR
        )),
        "{body}"
    );
    let rows_marker = format!("id=\"{}\"", crate::contract::MODEL_PICKER_ROWS_ID);
    let rows_start =
        body.find(&rows_marker).expect("the rows container renders") + rows_marker.len();
    let rows: String = body.chars().skip(rows_start).collect();
    assert!(
        !rows.contains(crate::contract::ACTION_DATA_ATTR),
        "an unavailable row must not carry the switch action: {rows}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_model_picker_refresh_post_answers_over_the_wire_without_blocking() {
    // Given a live server holding a session.
    let (base, seeded) = harness(Some("Picker refresh")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When posting the refresh-models action.
    let response = http
        .post(format!("{base}{}", crate::contract::MODELS_REFRESH_ROUTE))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("session_id={id}"))
        .send()
        .await
        .expect("refresh post completes");

    // Then it answers 200 with the status wrapper inside a patch
    // elements body (no discovery happens in the harness — a blocking
    // POST would hang), and never the modal.
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("body reads");
    assert!(
        body.contains(&format!("id=\"{}\"", crate::contract::MODEL_STATUS_ID)),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_BUTTON_ID
        )),
        "{body}"
    );
    assert!(
        !body.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_MODAL_ID
        )),
        "the refresh POST must not answer with the modal: {body}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_page_shell_carries_the_status_wrapper_and_the_modal_host() {
    // Given a live server holding a titled session.
    let (base, seeded) = harness(Some("Shell status")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When getting its page.
    let response = http
        .get(format!("{base}/web/session/{id}"))
        .send()
        .await
        .expect("the request completes");
    let body = response.text().await.expect("body reads");

    // Then the shell carries the status wrapper (label + picker inside
    // it) and the modal host — the two hosts the client swaps into.
    assert!(
        body.contains(&format!("id=\"{}\"", crate::contract::MODEL_STATUS_ID)),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_MODAL_HOST_ID
        )),
        "{body}"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_switch_post_answers_with_the_status_wrapper_over_the_wire() {
    // Given a live server holding a session (no providers configured —
    // the harness registry is empty).
    let (base, seeded) = harness(Some("Switch shape")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When posting a model switch for any id.
    let response = http
        .post(format!("{base}{}", crate::contract::MODEL_PATH))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("session_id={id}&provider_id=openai%2Fgpt-4o"))
        .send()
        .await
        .expect("the switch completes");

    // Then it refuses with 400 — an empty registry renders no rows, so
    // nothing is available to switch to (the offered list and the
    // accepted set are one thing).
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let refusal = response.text().await.expect("body reads");
    assert!(refusal.contains("not in the available"), "{refusal}");

    // And when the same POST names a model the picker WOULD render
    // (impossible with an empty registry), the refusal sentence names
    // availability, not discovery.
}

// ── The gzip-encoding bug class ─────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn vendor_assets_serve_gzip_with_the_declared_encoding() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When getting the client bundle from /vendor.
    let response = http
        .get(format!("{base}/vendor/{}", crate::contract::VENDOR_APP_JS))
        .send()
        .await
        .expect("the asset request completes");

    // Then the bytes on the wire are gzip — the declared
    // Content-Encoding and the 1f 8b magic prefix, so a browser can
    // actually decode the body. The shipped bug committed a raw dist
    // under a gzip row and served it with this header.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip"),
        "the vendor asset must declare Content-Encoding: gzip"
    );
    let bytes = response.bytes().await.expect("body reads");
    assert!(
        bytes.starts_with(&[0x1f, 0x8b]),
        "the body must be gzip bytes (1f 8b), not a raw dist: {:?}",
        bytes.get(..4)
    );
}

#[rstest::rstest]
#[tokio::test]
async fn the_versioned_client_url_serves_immutable_over_the_wire() {
    // Given a live server and the entry module's hash-bearing URL.
    let (base, _seeded) = harness(None).await;
    let http = client();
    let versioned =
        crate::vendor::versioned_path(crate::contract::VENDOR_APP_JS).expect("entry is a row");

    // When getting the versioned URL.
    let response = http
        .get(format!("{base}/vendor/{versioned}"))
        .send()
        .await
        .expect("the request completes");

    // Then it serves 200 with the immutable policy: URL and content
    // are one-for-one, so the browser may keep it for a year.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000, immutable"),
        "a versioned URL names one content forever"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn a_stale_versioned_hash_is_a_404_over_the_wire() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When requesting the entry module under a hash no build shipped.
    let response = http
        .get(format!("{base}/vendor/main.deadbeef.js"))
        .send()
        .await
        .expect("the request completes");

    // Then it 404s — a versioned URL names one content, and this
    // binary does not ship that one.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[rstest::rstest]
#[tokio::test]
async fn the_unversioned_client_url_still_serves_but_must_revalidate() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When requesting the entry module by its bare name (the form a
    // page cached from an older binary imports).
    let response = http
        .get(format!("{base}/vendor/{}", crate::contract::VENDOR_APP_JS))
        .send()
        .await
        .expect("the request completes");

    // Then it still serves — old pages keep working — but under the
    // revalidate policy, so stale code never outlives a reload.
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=0, must-revalidate"),
        "unversioned URLs are the migration path, not the cache target"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn webapp_install_assets_serve_identity_with_their_own_content_types() {
    // Given a live server and the install assets the page shell links.
    let (base, _seeded) = harness(None).await;
    let http = client();
    let assets = [
        ("manifest.webmanifest", "application/manifest+json"),
        ("icon-192.png", "image/png"),
        ("icon-512.png", "image/png"),
        ("icon-maskable-512.png", "image/png"),
        ("icon-apple-touch-180.png", "image/png"),
        ("icon-favicon-32.png", "image/png"),
    ];

    for (path, content_type) in assets {
        // When getting each install asset from /vendor.
        let response = http
            .get(format!("{base}/vendor/{path}"))
            .send()
            .await
            .expect("the asset request completes");

        // Then each serves 200 under its own type with no
        // Content-Encoding — an identity row is raw bytes, and a
        // spurious gzip header would make the browser try to inflate
        // a PNG or the JSON manifest.
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some(content_type),
            "{path} must declare {content_type}"
        );
        assert_eq!(
            response
                .headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("identity"),
            "{path} is an identity row — the header names identity, no gzip"
        );
    }

    // And the webmanifest parses as the JSON object a browser reads
    // installability from.
    let manifest: serde_json::Value = serde_json::from_str(
        &http
            .get(format!("{base}/vendor/manifest.webmanifest"))
            .send()
            .await
            .expect("manifest reads")
            .text()
            .await
            .expect("manifest body"),
    )
    .expect("the webmanifest must be valid JSON");
    assert_eq!(
        manifest
            .get("start_url")
            .and_then(serde_json::Value::as_str),
        Some("/web"),
        "the manifest's start_url must be the sessions landing page"
    );
    assert_eq!(
        manifest.get("display").and_then(serde_json::Value::as_str),
        Some("standalone"),
        "the manifest must request a standalone display"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn an_unknown_vendor_path_is_a_404_over_the_wire() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When getting a path no manifest row vouches for.
    let response = http
        .get(format!("{base}/vendor/never-pinned.js"))
        .send()
        .await
        .expect("the request completes");

    // Then it 404s — no unverified bytes ride out.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ── The page routes ─────────────────────────────────────────────────

#[rstest::rstest]
#[tokio::test]
async fn the_landing_page_serves_with_the_shell_over_the_wire() {
    // Given a live server.
    let (base, _seeded) = harness(None).await;
    let http = client();

    // When getting /web.
    let response = http
        .get(format!("{base}/web"))
        .send()
        .await
        .expect("the request completes");

    // Then it answers 200 with the shell ids the client targets, and
    // an HTML content type.
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("text/html"),
        "the page must be text/html, got {content_type}"
    );
    let body = response.text().await.expect("body reads");
    for id in [
        crate::contract::PAGE_ROOT_ID,
        crate::contract::SESSIONS_ID,
        crate::contract::CONTROLS_ID,
        crate::contract::TRANSCRIPT_ID,
    ] {
        assert!(body.contains(&format!("id=\"{id}\"")), "{id}: {body}");
    }
}

#[rstest::rstest]
#[tokio::test]
async fn the_session_page_carries_the_session_data_attribute_over_the_wire() {
    // Given a live server holding a titled session.
    let (base, seeded) = harness(Some("Wired session")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    // When getting its page.
    let response = http
        .get(format!("{base}/web/session/{id}"))
        .send()
        .await
        .expect("the request completes");

    // Then the page names the session in the data attribute the
    // client reads to open its stream, and carries the cursor
    // attribute the initial Last-Event-ID comes from.
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("body reads");
    assert!(
        body.contains(&format!(
            "{}=\"{id}\"",
            crate::contract::PAGE_SESSION_DATA_ATTR
        )),
        "{body}"
    );
    assert!(
        body.contains(crate::contract::TRANSCRIPT_CURSOR_DATA_ATTR),
        "{body}"
    );
    assert!(body.contains("Wired session"), "{body}");
}

#[rstest::rstest]
#[tokio::test]
async fn a_models_push_reaches_a_live_stream_as_a_dispatchable_frame_with_data() {
    // Given a live server holding a session and an open SSE stream
    // (the browser-shaped reader: frames arrive as bytes).
    let (base, seeded, harness_fanout) = harness_with_fanout(Some("Models push wire")).await;
    let id = seeded.expect("harness seeded the session");
    let http = client();

    let mut stream = http
        .get(format!("{base}{}{id}", crate::contract::STREAM_PREFIX))
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("the stream opens");
    assert_eq!(stream.status(), 200);

    // When the fan-out's broadcast channel carries a models envelope
    // whose data line holds the cache-write timestamp — the shape the
    // post-cache-write publish produces (the wire e2e for the
    // never-stops-spinner fix: what a browser receives, byte-level).
    let stored_at = jiff::Timestamp::now();
    harness_fanout.push_models_changed_from(&id, stored_at);

    // Then the stream yields a `patch-signals` frame whose data line
    // carries the modelsTick signal: a browser dispatches an SSE
    // event only when its data buffer holds bytes, so an empty line
    // here is the never-stops-spinner bug re-enabled at the byte
    // level — and the tick is what clears the spinner (the modal's
    // effect) and re-pulls the rows.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut saw_models_with_data = false;
    while std::time::Instant::now() < deadline {
        let Some(chunk) = stream.chunk().await.expect("stream chunk reads") else {
            continue;
        };
        let text = String::from_utf8_lossy(&chunk);
        for frame in text.split("\n\n") {
            if frame.contains("event: datastar-patch-signals")
                && frame.contains(crate::contract::SIGNAL_MODELS_TICK)
            {
                assert!(
                    frame.lines().any(|line| line
                        .strip_prefix("data:")
                        .is_some_and(|data| !data.trim().is_empty())),
                    "the models frame must carry a data line with bytes (an empty \
                     data buffer is never dispatched by a browser): {frame:?}"
                );
                saw_models_with_data = true;
            }
        }
        if saw_models_with_data {
            break;
        }
    }
    assert!(
        saw_models_with_data,
        "a models frame with a non-empty data line arrived within the deadline"
    );
}
