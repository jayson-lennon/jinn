//! Model-market actor-subscription composition tests.
//!
//! The popup is live-wired (cells, routes, view) but only useful once its
//! two data-plane actors are on the bus — the whole slice shipped exactly
//! that way once: subscribed to nothing, so the opener's `FetchModels`
//! publish dropped and the table showed seed data forever. These tests
//! spawn the actors the way `src/bootstrap/slices.rs` does
//! (`jinn_model_market::activate_actors` over a catalog-seeded `Services`)
//! and prove the actors are live, subscribed, and pointed at the same
//! cache layout `AppPaths` documents.
//!
//! Hermetic by construction: endpoints are dead URLs (the claim under
//! test is "subscribed", not "online"), so nothing here depends on the
//! network being down — or, on a live machine, up.
#![allow(
    clippy::indexing_slicing,
    reason = "integration test: fixture vectors with known lengths"
)]
#![allow(clippy::expect_used, clippy::panic, reason = "test code")]

use marked_path::MarkedPathAccess;
use std::io::Write as _;
use std::time::Duration;

use jinn_model_market::ActorEndpoints;
use jinn_model_market_msg::EpochLoaded;
use jinn_model_market_msg::EpochSource;
use jinn_model_market_msg::FetchModels;
use jinn_model_market_msg::ModelsLoaded;
use jinn_model_market_msg::RefreshEpoch;
use jinn_model_market_msg::RefreshFailed;
use jinn_model_market_msg::market_data_slot;
use jinn_slices::TypedCell;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::bus_harness::await_recorded;

/// The settle window: past the 250ms endpoint debounce, small enough to
/// keep the suite snappy.
const SETTLE: Duration = Duration::from_millis(700);

/// Installs the process-wide rustls crypto provider for this test
/// binary — reqwest is built `rustls-no-provider`, so without this every
/// `Client::new()` panics with "No provider set". Idempotent; the result
/// is deliberately ignored.
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Composition the way the boot list wires it: catalog-seeded
/// `Services`, `activate` for the cells/routes/view, `activate_actors`
/// for the subscriptions. Endpoints are dead URLs — see the module doc.
async fn composed() -> jinn_kernel::Services {
    composed_with_endpoints(jinn_model_market::ActorEndpoints {
        zip_url: "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
        base_url: "http://127.0.0.1:9".to_owned(),
    })
    .await
}

/// [`composed`] with explicit endpoints — the mockito variants need a
/// live local server URL while keeping everything else hermetic.
async fn composed_with_endpoints(
    endpoints: jinn_model_market::ActorEndpoints,
) -> jinn_kernel::Services {
    let services = jinn_kernel::Services::new_fake().await;
    let mut viewport = jinn_slices::view::Viewport::new();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_model_market::activate(&mut host);
    jinn_model_market::activate_actors_with(&services, endpoints).await;
    services
}

/// Pre-boot wiring: catalog-seeded `Services` + `activate` only — the
/// caller spawns the recorders it needs before the boot kick fires, then
/// calls `activate_actors_with` itself.
async fn pre_boot() -> jinn_kernel::Services {
    let services = jinn_kernel::Services::new_fake().await;
    let mut viewport = jinn_slices::view::Viewport::new();
    let mut host = jinn_slices::SliceHost::new(
        &services.slices,
        &mut viewport,
        &services.overlay_views,
        &services.key_routes,
        &services.trouper_system,
    );
    jinn_model_market::activate(&mut host);
    services
}

/// The boot-list tail over `pre_boot` services.
async fn boot(services: &jinn_kernel::Services, endpoints: jinn_model_market::ActorEndpoints) {
    jinn_model_market::activate_actors_with(services, endpoints).await;
}

/// The data cell handle from the composed services.
fn data_of(services: &jinn_kernel::Services) -> TypedCell<jinn_model_market_msg::MarketDataState> {
    services
        .slices
        .reader(&market_data_slot())
        .expect("the cell catalog registers the market data slot")
}

/// A fresh mockito epoch server serving the seed-backed zip. Returns the
/// guard (keep alive for the mock's lifetime) and the endpoints aimed at
/// it.
async fn served_epoch() -> (
    mockito::ServerGuard,
    jinn_model_market::ActorEndpoints,
    mockito::Mock,
) {
    let mut server = mockito::Server::new_async().await;
    let zip = seed_zip_bytes();
    let mock = server
        .mock("GET", "/data/benchmark_data.zip")
        .with_status(200)
        .with_body(zip)
        .create_async()
        .await;
    let endpoints = jinn_model_market::ActorEndpoints {
        zip_url: format!("{}/data/benchmark_data.zip", server.url()),
        base_url: "http://127.0.0.1:9".to_owned(),
    };
    (server, endpoints, mock)
}

/// A valid benchmark zip carrying exactly the three extraction targets,
/// built from the compiled seed bytes so the parse round-trips.
fn seed_zip_bytes() -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("epoch_capabilities_index/eci_scores.csv", opts)
        .expect("start eci member");
    zip.write_all(jinn_model_market::epoch::SEED_ECI_SCORES)
        .expect("write eci member");
    zip.start_file("benchmark_metadata.csv", opts)
        .expect("start metadata member");
    zip.write_all(jinn_model_market::epoch::SEED_BENCHMARK_METADATA)
        .expect("write metadata member");
    zip.start_file("model_metadata.csv", opts)
        .expect("start roster member");
    zip.write_all(jinn_model_market::epoch::SEED_MODEL_METADATA)
        .expect("write roster member");
    zip.finish().expect("finish zip").into_inner()
}

// ── Boot-time epoch load (no user action) ───────────────────────────────

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn boot_loads_epoch_from_seed_without_any_user_action() {
    // Given a wired slice whose recorder is subscribed BEFORE boot — the
    // only way to observe that no user action is involved. Offline: no
    // cache dir, dead zip URL.
    let services = pre_boot().await;
    let recorder = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone())
        .spawn_recorder::<EpochLoaded>()
        .await;
    boot(
        &services,
        jinn_model_market::ActorEndpoints {
            zip_url: "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
            base_url: "http://127.0.0.1:9".to_owned(),
        },
    )
    .await;

    // When nothing at all happens — no keypress, no publish from the
    // test. The boot kick `activate_actors` publishes is the only
    // trigger.

    // Then the epoch actor answered on its own: the ECI column has data
    // before the popup can ever open (001 §1 first-run contract), sourced
    // from the compiled seed.
    let loaded = await_recorded::<EpochLoaded>(&recorder, 1, Duration::from_secs(10)).await;
    assert_eq!(loaded.len(), 1, "one boot-time EpochLoaded");
    assert_eq!(loaded[0].source, EpochSource::Seed);
    let data = data_of(&services);
    assert!(data.read().eci.is_some(), "the seed ECI index landed");
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn boot_with_expired_ttl_downloads_exactly_once_into_the_app_paths_dir() {
    // Given a mockito epoch server, and an epoch cache dir holding a
    // week-stale extraction planted BEFORE boot (fetched at ms=0, which
    // any real clock reads as past the TTL). The recorder subscribes
    // before the kick fires.
    let (server, endpoints, mock) = served_epoch().await;
    let services = pre_boot().await;
    let epoch_dir = services.paths.epoch_cache_dir();
    jinn_model_market::epoch_cache::download_and_extract(
        &epoch_dir,
        &seed_zip_bytes(),
        0, // fetched at ms=0: any real clock reads it as expired
    )
    .expect("stale cache written");
    let recorder = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone())
        .spawn_recorder::<EpochLoaded>()
        .await;
    boot(&services, endpoints).await;

    // When boot runs — the RefreshEpoch kick inside `activate_actors_with`
    // is the only trigger.

    // Then exactly one outbound zip GET answered the boot kick (the TTL
    // lazy check ran on a load path), and the fresh extraction landed at
    // AppPaths::epoch_cache_dir() — the same directory the help card and
    // THIRD_PARTY_LICENSES name, not a re-derived sibling.
    let loaded = await_recorded::<EpochLoaded>(&recorder, 1, Duration::from_secs(10)).await;
    assert_eq!(loaded[0].source, EpochSource::Download);
    mock.assert();
    assert!(
        epoch_dir
            .join("eci_scores.csv")
            .expect("typed parent joins a bare name")
            .as_path()
            .exists()
    );
    drop(server);
}

// ── Path seam: the actor's epoch dir is AppPaths' ───────────────────────

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn boot_extraction_lands_at_app_paths_epoch_cache_dir_not_a_sibling() {
    // Given a wired slice, offline, with a dead zip URL: the boot kick
    // falls back to the seed. Recorder subscribed before boot.
    let services = pre_boot().await;
    let epoch_dir = services.paths.epoch_cache_dir();
    let recorder = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone())
        .spawn_recorder::<EpochLoaded>()
        .await;
    boot(
        &services,
        jinn_model_market::ActorEndpoints {
            zip_url: "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
            base_url: "http://127.0.0.1:9".to_owned(),
        },
    )
    .await;

    // When boot runs — no user action.

    // Then the fallback is the seed, and nothing landed under a
    // double-nested path (~/.cache/jinn/jinn/epoch — the bug this test
    // exists to catch): the actor reads the AppPaths dir or nothing.
    let loaded = await_recorded::<EpochLoaded>(&recorder, 1, Duration::from_secs(10)).await;
    assert_eq!(loaded[0].source, EpochSource::Seed);
    let cache_parent = epoch_dir
        .as_path()
        .parent()
        .expect("epoch dir has a parent");
    let double_nested = cache_parent.join("jinn").join("epoch");
    assert!(
        !double_nested.exists(),
        "the epoch cache must never double-nest under {double_nested:?}"
    );
    // And the TTL check reads the same AppPaths dir: a fresh cache there
    // is served on boot (the warm-boot test proves that half).
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn warm_boot_serves_the_app_paths_cache_without_a_request() {
    // Given a fresh extraction planted at AppPaths::epoch_cache_dir()
    // BEFORE boot, and a mock that must see zero requests. Recorder
    // subscribed before the kick fires.
    let (server, endpoints, mock) = served_epoch().await;
    let services = pre_boot().await;
    let epoch_dir = services.paths.epoch_cache_dir();
    jinn_model_market::epoch_cache::download_and_extract(
        &epoch_dir,
        &seed_zip_bytes(),
        jiff::Timestamp::now().as_second() as u64,
    )
    .expect("warm cache planted");
    let recorder = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone())
        .spawn_recorder::<EpochLoaded>()
        .await;
    boot(&services, endpoints).await;

    // When boot runs — the RefreshEpoch kick is the only trigger.

    // Then the boot load served the warm cache — no outbound request.
    let loaded = await_recorded::<EpochLoaded>(&recorder, 1, Duration::from_secs(10)).await;
    assert_eq!(loaded[0].source, EpochSource::Download);
    mock.expect(0);
    drop(server);
}

// ── Publishes reach the live actors ─────────────────────────────────────

/// A models catalog body for the mockito OR server. No endpoints mock:
/// under fetch-on-drill-in-only semantics a models load alone must make
/// no endpoints request, so any one would fail the test below.
async fn served_openrouter() -> (mockito::ServerGuard, String) {
    let mut server = mockito::Server::new_async().await;
    let url = server.url();
    let catalog = serde_json::json!({
        "data": [
            {
                "id": "test/model",
                "pricing": {
                    "prompt": "0.000003",
                    "completion": "0.000015",
                    "input_cache_read": "0.0000003"
                },
                "architecture": {"output_modalities": ["text"]},
                "context_length": 100_000
            }
        ]
    })
    .to_string();
    server
        .mock("GET", "/models")
        .with_status(200)
        .with_body(catalog)
        .create_async()
        .await;
    (server, url)
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn opening_the_popup_lands_rows_without_fetching_any_detail() {
    // Given the slice composed as the boot list composes it — cells,
    // routes, view, both actors — with the OR actor pointed at a live
    // local mock serving ONLY the models catalog: any endpoints request
    // hits no mock and 501s.
    let (server, base_url) = served_openrouter().await;
    let services = composed_with_endpoints(ActorEndpoints {
        zip_url: "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
        base_url,
    })
    .await;
    let harness = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone());
    let loaded_recorder = harness.spawn_recorder::<ModelsLoaded>().await;

    // When the opener's publish (what `open_market` mints) rides the
    // real bus — one publish, zero navigation keypresses, then settle
    // past the endpoint debounce.
    services.bus.publish(FetchModels).await;
    let loaded = await_recorded::<ModelsLoaded>(&loaded_recorder, 1, SETTLE).await;

    // Then the table has rows AND the data cell carries no detail:
    // opening fetches nothing but the listing — detail rides the
    // drill-in ENTER alone.
    assert_eq!(loaded.len(), 1, "one models load");
    let data = data_of(&services);
    // The rows landing is the positive signal that the fetch completed; the
    // absence of a detail fetch is only meaningful once they have.
    jinn_testutil::wait::await_cell(&data, SETTLE, |read| !read.rows.is_empty()).await;
    let read = data.read();
    assert_eq!(
        read.rows.first().map(|r| r.id.as_str()),
        Some("openrouter/test/model"),
        "the rows landed, got: {:?}",
        read.rows
    );
    assert!(
        read.detail.is_none(),
        "opening must fetch no detail, got: {:?}",
        read.detail
    );
    drop(server);
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn open_publishes_fetch_models_and_the_actor_answers_end_to_end() {
    // Given the full composition with the popup route attached and a
    // live local OR mock, and the bus recorder watching ModelsLoaded.
    let (server, base_url) = served_openrouter().await;
    let services = composed_with_endpoints(ActorEndpoints {
        zip_url: "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
        base_url,
    })
    .await;
    let harness = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone());
    let loaded_recorder = harness.spawn_recorder::<ModelsLoaded>().await;

    // When the opener intent is dispatched through the real handler —
    // the exact keypress path production walks — over a state whose
    // scope stack carries the slice registry.
    let mut state = jinn_kernel::AppState::default();
    state.frontend.attach_slices(services.slices.clone());
    let result = jinn_kernel::IntentHandler::handle(
        &jinn_kernel::KernelIntent::Dynamic(jinn_slices::DynamicIntent::new(
            jinn_model_market::market_ui_scope(),
            "open-model-market",
            "model market",
        )),
        &mut state,
        &services.slices,
        &services.key_routes,
        jinn_slices::empty_config_layer(),
    );
    // The handler returns closures; the caller's drain onto the real
    // bus sink is what publishes them — the same drain production does.
    for publish in result.messages {
        publish(&services.bus);
    }
    let loaded = await_recorded::<ModelsLoaded>(&loaded_recorder, 1, SETTLE).await;

    // Then the opener's publish reached the live actor and the load
    // landed — the sync path and the actor meet on the real bus.
    assert!(
        result.message_names.contains(&"FetchModels"),
        "the opener publishes FetchModels"
    );
    assert_eq!(loaded.len(), 1, "the actor answered the opener");
    drop(server);
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn published_fetch_models_reaches_the_live_actor() {
    // Given the slice composed as the boot list composes it: routes and
    // view attached, both actors spawned over the catalog-seeded cells.
    let services = composed().await;
    // And the boot kick's unconditional last_error (epoch fallback over
    // a dead URL) cleared, so the assertion below cannot be satisfied
    // by an outcome this publish never produced.
    data_of(&services).update(|d| d.last_error = None);

    // When the opener's FetchModels publish rides the real bus.
    services
        .bus
        .publish(jinn_model_market_msg::FetchModels)
        .await;

    // Then the actor answered: the empty cache dir forces the network
    // path, which fails offline — the degraded-mode line lands in the
    // cell. Either way the message was consumed, not dropped
    // unsubscribed (zero outcomes is the dead-bus signature). The
    // dead-URL connect refuses instantly on Linux, but a first
    // reqwest connect on Windows can take seconds (proxy resolution,
    // firewall evaluation), so the outcome is polled to a deadline
    // rather than checked after one settle sleep.
    let data = data_of(&services);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (fetched_at, last_error) = {
            let cell = data.read();
            (cell.models_fetched_at, cell.last_error.clone())
        };
        if fetched_at.is_some() || last_error.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a published FetchModels must reach the live actor and land an outcome \
             in the data cell, got: models_fetched_at={fetched_at:?} \
             last_error={last_error:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn published_refresh_epoch_reaches_the_live_actor_offline_seed_stands() {
    // Given the composed slice, offline, AFTER boot has already run its
    // kick (so this publish is the second load).
    let services = composed().await;
    let recorder = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone())
        .spawn_recorder::<EpochLoaded>()
        .await;

    // When a RefreshEpoch arrives from the bus — the `<c-r>`-adjacent
    // shape a caller can publish; this test proves the actor's
    // subscription delivers it, nothing more.
    services.bus.publish(RefreshEpoch).await;
    let loaded = await_recorded::<EpochLoaded>(&recorder, 2, Duration::from_secs(10)).await;

    // Then the actor answered both loads (boot + this publish), and the
    // offline fallback is the seed in each.
    assert_eq!(loaded.len(), 2, "boot + explicit RefreshEpoch");
    assert!(loaded.iter().all(|e| e.source == EpochSource::Seed));
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn warm_models_cache_serves_the_popup_without_network() {
    // Given the composed slice with a warm models disk cache in the
    // fake Services' tempdir.
    let services = composed().await;
    let cache_dir = services.paths.openrouter_cache_dir();
    std::fs::create_dir_all(&cache_dir).expect("cache dir creates");
    let rows: Vec<jinn_model_market_msg::MarketRow> = vec![jinn_model_market_msg::MarketRow {
        id: "openrouter/a/small".to_owned(),
        author: "a".to_owned(),
        slug: "small".to_owned(),
        variant: None,
        prompt_price: 0.000_001,
        completion_price: 0.000_002,
        cache_read_price: None,
        has_overrides: false,
        context_length: Some(128_000),
        eci: None,
        eci_ci: None,
        is_free: false,
    }];
    let now = jiff::Timestamp::now();
    jinn_model_market::market_cache::store(
        &cache_dir
            .join("models.json")
            .expect("typed parent joins a bare name"),
        &jinn_model_market::market_cache::stamped(rows, now),
    )
    .expect("warm cache writes");

    // When the opener's FetchModels publish rides the bus.
    services
        .bus
        .publish(jinn_model_market_msg::FetchModels)
        .await;

    // Then the actor served the cache into the cell — the row on the
    // table. (The boot kick's epoch fallback may own `last_error` on an
    // offline box; that line names epoch, never models.)
    let data = data_of(&services);
    // The wait is on the cell's own content — a fixed sleep would only
    // guess at when the actor served the cache, and the read could land
    // before it, leaving the assertion to fail on an empty cell.
    jinn_testutil::wait::await_cell(&data, SETTLE, |read| !read.rows.is_empty()).await;
    let read = data.read();
    assert_eq!(
        read.rows.first().map(|r| r.id.as_str()),
        Some("openrouter/a/small"),
        "the warm cache's row lands in the cell, got: {:?}",
        read.rows
    );
    assert!(
        read.last_error
            .as_deref()
            .is_none_or(|e| !e.contains("models")),
        "a warm models cache never reports a models fetch failure, got: {:?}",
        read.last_error
    );
}

#[rstest::rstest]
#[tokio::test]
#[timeout(Duration::from_secs(20))]
async fn refresh_failure_reports_through_the_bus() {
    // Given the composed slice with no cache and no network, and
    // recorders subscribed before anything publishes.
    let services = composed().await;
    let harness = TestHarness::from_parts(services.bus.clone(), services.trouper_system.clone());
    let failures = harness.spawn_recorder::<RefreshFailed>().await;
    let loaded = harness.spawn_recorder::<EpochLoaded>().await;

    // When a RefreshAll arrives (the `<c-r>` publish): both actors
    // consume it, the epoch one falls back to seed, the OR one fails
    // its fetch and reports. Both failures are guaranteed — RefreshAll
    // clears every cache first — so both are awaited: waiting on one
    // races the two publishes and can observe only the epoch failure.
    services
        .bus
        .publish(jinn_model_market_msg::RefreshAll)
        .await;
    let reported = await_recorded::<RefreshFailed>(&failures, 2, Duration::from_secs(10)).await;

    // Then the OR actor's failure is announced on the bus (the
    // degraded-mode path), and the epoch actor still answers with seed.
    assert!(
        reported.iter().any(|f| f.what == "models"),
        "the models fetch failure is announced, got: {reported:?}"
    );
    let epoch = await_recorded::<EpochLoaded>(&loaded, 2, Duration::from_secs(10)).await;
    assert!(epoch.iter().all(|e| e.source == EpochSource::Seed));
}
