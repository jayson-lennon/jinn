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

use crate::server::ServerShared;
use crate::server::router;

/// One live server on an OS-assigned loopback port, probed with
/// reqwest, optionally seeded with one titled session (the session's
/// id returned with the base URL). The serve task runs for the test's
/// life.
async fn harness(titled: Option<&str>) -> (String, Option<SessionId>) {
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
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone())
    };
    let shared = ServerShared::new(
        state,
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
        "serve-test".to_owned(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an OS-assigned loopback port binds");
    let addr = listener.local_addr().expect("listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://{addr}"), seeded)
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

    // Then the response is the WHOLE page — `<!DOCTYPE html>` and the
    // page shell — not the sessions fragment. This is the shape the
    // client's page swap consumes; the shipped bug served a fragment
    // here and the swap destroyed the page.
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.expect("body reads");
    assert!(
        body.starts_with("<!DOCTYPE html>"),
        "activate must return a whole page, got a fragment: {body}"
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

    // Then it answers 200 with the button fragment immediately (no
    // discovery happens in the harness — a blocking POST would hang).
    assert_eq!(response.status(), 200);
    let body = response.text().await.expect("body reads");
    assert!(
        body.contains(&format!(
            "id=\"{}\"",
            crate::contract::MODEL_PICKER_BUTTON_ID
        )),
        "{body}"
    );
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
