//! The smoke tier: one headless-chromium test of the real page.
//!
//! Everything below it is fast and browserless; this one test is the
//! proof AC1 names — clicking a session row in a REAL browser
//! replaces the page with that session's transcript. It is gated on
//! `JINN_WEBUI_SMOKE=1` so machines without chromium (and the plain
//! `just test` loop) skip it; the operator's pre-merge run sets the
//! variable. Skips must be an explicit env opt-IN, never an env
//! opt-out — a test that silently skips everywhere is a test that
//! never runs.

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

use crate::server::ServerShared;
use crate::server::router;

/// Serves the real router on an OS-assigned loopback port with one
/// titled, entry-bearing session, and returns the base URL plus the
/// session id.
async fn serve_with_session(title: &str, entry_text: &str) -> (String, jinn_core_types::SessionId) {
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
        let app = AppState::default();
        app.session.attach_slices(slices.clone());
        State::new(app)
    };
    let session = jinn_session_state::ChatSessionState::new();
    let id = session.session_id().clone();
    state.write().session.insert(session);
    {
        let mut guard = state.write();
        let session = guard.session.get_mut(&id).expect("seeded session");
        session.set_title(title.to_owned());
        session.push_entry(jinn_core_types::ChatEntry::user(entry_text));
    }

    let (bus, _audit) = BusService::new_recording();
    let fanout = {
        use trouper::system::ActorSystem;
        let system = ActorSystem::new(trouper::system::SystemConfig::production());
        crate::fanout::WebUiFanoutActor::spawn(&system, state.clone(), bus.clone(), unread_marks)
    };
    let shared = ServerShared::new(
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
        "smoke-test".to_owned(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an OS-assigned loopback port binds");
    let addr = listener.local_addr().expect("listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(shared)).await;
    });
    (format!("http://{addr}"), id)
}

#[rstest::rstest]
#[tokio::test]
async fn clicking_a_session_in_a_real_browser_shows_its_transcript() {
    // Given the smoke gate is opted in (a chromium-bearing machine).
    if std::env::var("JINN_WEBUI_SMOKE").as_deref() != Ok("1") {
        // Skipped by contract: set JINN_WEBUI_SMOKE=1 to run.
        return;
    }
    let (base, id) = serve_with_session("Smoke target", "the smoke entry").await;

    // When a real headless chromium loads the page and clicks the
    // session row.
    let config = chaser_oxide::BrowserConfig::builder()
        .chrome_executable("/usr/bin/chromium")
        .new_headless_mode()
        .no_sandbox()
        .window_size(1280, 800)
        .build()
        .expect("smoke browser config builds");
    let (browser, mut handler) = chaser_oxide::Browser::launch(config)
        .await
        .expect("headless chromium launches");
    let driver = tokio::spawn(async move {
        use futures::StreamExt as _;
        while let Some(item) = handler.next().await {
            if item.is_err() {
                break;
            }
        }
    });

    let page = browser.new_page("about:blank").await.expect("a page opens");
    page.goto(format!("{base}/web"))
        .await
        .expect("the landing page loads");

    // The row for the seeded session, then the click that must swap
    // the whole page (AC1).
    let row = page
        .find_element(format!("[data-session-id='{id}']"))
        .await
        .expect("the seeded session's row renders in the sidebar");
    row.click().await.expect("the row clicks");

    // Then the transcript carries the session's entry — the swap
    // replaced the page and the content followed. Poll: the swap is
    // one fetch + one DOM replace after the click.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut saw_entry = false;
    while std::time::Instant::now() < deadline {
        let content = page.content().await.expect("page content reads");
        if content.contains("the smoke entry") && content.contains("Smoke target") {
            saw_entry = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert!(
        saw_entry,
        "after the click the page must show the session's title and entry — \
         the swap did not replace the transcript"
    );

    // The URL tracked the swap (pushState on activate).
    let url = page
        .url()
        .await
        .expect("page url reads")
        .unwrap_or_default();
    assert!(
        url.contains(&id.to_string()),
        "the URL must carry the activated session's id, got {url}"
    );

    drop(page);
    driver.abort();
}
