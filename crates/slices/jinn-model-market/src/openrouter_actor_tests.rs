//! `OpenRouterMarketActor` tests — the caching contract's heart.
//!
//! The mandatory named tests live here: warm-cache-no-request,
//! TTL-expiry-refetch, refresh-all-drops, debounce-drops-stale,
//! refresh-reaches-both-actors. Every test drives the real actor over
//! the real bus ([`TestHarness`]) with a mockito server as OpenRouter,
//! so "no outbound request" is measured by the mock expecting zero
//! hits, not by an internal flag.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_in_result,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_kernel::common::bus::HarnessServices;
use jinn_kernel::common::render_test::seed_cell;
use jinn_model_market_msg::EciLoaded;
use jinn_model_market_msg::EndpointsLoaded;
use jinn_model_market_msg::FetchEndpoints;
use jinn_model_market_msg::FetchModels;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketEndpointRow;
use jinn_model_market_msg::MarketModelsRefreshed;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::ModelsLoaded;
use jinn_model_market_msg::RefreshAll;
use jinn_model_market_msg::market_data_slot;
use jinn_model_market_msg::market_ui_slot;
use jinn_slices::RouteResult;
use jinn_slices::Slices;
use jinn_slices::TypedCell;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::bus_harness::await_recorded;
use jinn_testutil::wait::await_cell;
use marked_path::{Absolute, MarkedPathAccess as _, MarkedPathBuf};
use std::time::Duration;

use super::OpenRouterMarketActor;
use super::OpenRouterMarketActorDeps;
use crate::score_actor::ScoreMarketActor;
use crate::score_actor::ScoreMarketActorDeps;

/// The harness wait: past the 250ms debounce, small enough to keep the
/// suite snappy. Tests that wait on a full refresh — two network fetches
/// through a mockito server behind a debounce — use [`REFRESH_SETTLE`]
/// instead: under full-suite parallel load 700ms can expire before the
/// actor's task is scheduled twice, which looked like a dropped cache
/// sweep rather than a slow machine.
const SETTLE: Duration = Duration::from_millis(700);

/// The wait for tests that ride a RefreshAll through the mockito server.
const REFRESH_SETTLE: Duration = Duration::from_secs(5);

/// The score URLs pointing at a port nothing listens on — every score
/// download fails fast, which is all a delivery test needs.
const DEAD_ZIP_URL: &str = "http://127.0.0.1:9/data/benchmark_data.zip";

/// One wired test world: harness bus, the two cells, mockito URL.
struct World {
    /// The bus harness — publish here, record there.
    harness: TestHarness,
    /// The data cell the actor writes.
    data: TypedCell<MarketDataState>,
    /// The UI cell the actor reads for the debounce sequence.
    ui: TypedCell<MarketUiState>,
    /// The mockito server's root (`http://127.0.0.1:port`).
    #[expect(dead_code, reason = "kept for follow-up tests to name the server")]
    base_url: String,
    /// The market cache dir (a tempdir under the harness).
    #[expect(dead_code, reason = "kept so the tempdir outlives the test")]
    cache_dir: MarkedPathBuf<Absolute>,
    /// A clone of the actor's own API-key store handle. Clones share
    /// one `Arc<RwLock<…>>`, so inserting here after the spawn is
    /// exactly what production's env ask does after the actor spawns.
    api_keys: jinn_provider_config::ApiKeysService,
    /// The score cache dir inside the market cache dir — the score
    /// actor's own drop target.
    score_dir: MarkedPathBuf<Absolute>,
}

impl World {
    /// Builds the world and spawns the actor against `base_url` and
    /// `cache_dir`.
    ///
    /// The cells mint through a real `Slices` registry — the same
    /// registration path production's catalog walks — because
    /// `TypedCell::new` is registry-internal by design.
    async fn start(base_url: &str, cache_dir: MarkedPathBuf<Absolute>) -> Self {
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        let api_keys = deps.services.api_keys.clone();
        let slices = Slices::new();
        let data = seed_cell(&slices, &market_data_slot(), MarketDataState::default());
        let ui = seed_cell(&slices, &market_ui_slot(), MarketUiState::default());
        OpenRouterMarketActor::spawn(
            harness.system(),
            OpenRouterMarketActorDeps {
                deps,
                data: data.clone(),
                ui: ui.clone(),
                base_url: base_url.to_owned(),
                cache_dir: Some(cache_dir.clone()),
                opener: std::sync::Arc::new(super::SilentPageOpener),
            },
        );
        Self {
            harness,
            data,
            ui,
            base_url: base_url.to_owned(),
            api_keys,
            score_dir: cache_dir
                .join("scores")
                .expect("cache root joins a bare name"),
            cache_dir,
        }
    }

    /// Builds the world with BOTH market actors on the bus: the
    /// OpenRouter models actor and the score actor.
    ///
    /// The score actor's URLs point at dead ports, so its download
    /// fails fast and it falls back to the shipped seed — the delivery
    /// test is about which actors act, not about the downloads
    /// succeeding.
    async fn start_with_score_actor(base_url: &str, cache_dir: MarkedPathBuf<Absolute>) -> Self {
        let world = Self::start(base_url, cache_dir).await;
        let deps = world.harness.actor_deps().await;
        ScoreMarketActor::spawn(
            world.harness.system(),
            ScoreMarketActorDeps {
                deps,
                data_cell: world.data.clone(),
                cache_dirs: crate::score_cache::ScoreCacheDirs::new(&world.score_dir.clone()),
                client: reqwest::Client::new(),
                zip_url: DEAD_ZIP_URL.to_owned(),
                vals_page_url: DEAD_ZIP_URL.to_owned(),
                now_ms: crate::score_actor::score_market_clock(),
            },
        );
        world
    }

    /// Records `ModelsLoaded` deliveries.
    async fn models_recorder(&self) -> jinn_testutil::bus_harness::Recorder<ModelsLoaded> {
        self.harness.spawn_recorder::<ModelsLoaded>().await
    }

    /// Records `EndpointsLoaded` deliveries.
    async fn endpoints_recorder(&self) -> jinn_testutil::bus_harness::Recorder<EndpointsLoaded> {
        self.harness.spawn_recorder::<EndpointsLoaded>().await
    }
}

/// A minimal one-model catalog body for the mock.
fn catalog_body() -> String {
    serde_json::json!({
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
    .to_string()
}

/// A minimal one-endpoint detail body for the mock.
fn endpoints_body() -> String {
    serde_json::json!({
        "data": {
            "endpoints": [
                {
                    "provider_name": "TestUp",
                    "tag": "testup",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "uptime_last_1d": 99.0
                }
            ]
        }
    })
    .to_string()
}

/// Registers a models GET mock that expects exactly one hit.
async fn expect_models_once(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/models")
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await
}

/// Registers an endpoints GET mock that expects exactly one hit.
async fn expect_endpoints_once(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("GET", "/models/test/model/endpoints")
        .with_status(200)
        .with_body(endpoints_body())
        .create_async()
        .await
}

#[rstest::rstest]
#[tokio::test]
async fn warm_models_cache_makes_no_outbound_request() {
    // Given a disk cache written by a previous run and a mock server
    // with NO registered route: mockito answers 501 to any outbound
    // request, so a hidden fetch fails and never emits ModelsLoaded.
    let server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    crate::market_cache::store(
        &dir.path
            .join("models.json")
            .expect("temp root joins a bare name"),
        &crate::market_cache::stamped(fixture_rows(), at),
    )
    .expect("seed the warm cache");
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.models_recorder().await;

    // When publishing FetchModels against the request-less server.
    world.harness.publish(FetchModels).await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the load lands from the warm cache: the event arriving at
    // all is the outbound-request proof — a request would have 501'd.
    assert_eq!(loaded.len(), 1, "exactly one ModelsLoaded");
    assert!(loaded[0].from_cache, "warm cache must set from_cache");
    let rows = world.data.read().rows.clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "openrouter/test/model");
}

#[rstest::rstest]
#[tokio::test]
async fn expired_models_ttl_refetches() {
    // Given a disk cache stamped 8 days ago — past the 7-day TTL — and
    // a mock expecting exactly one GET.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(8 * 24);
    crate::market_cache::store(
        &dir.path
            .join("models.json")
            .expect("temp root joins a bare name"),
        &crate::market_cache::stamped(fixture_rows(), at),
    )
    .expect("seed the stale cache");
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.models_recorder().await;
    let mock = expect_models_once(&mut server).await;

    // When publishing FetchModels.
    world.harness.publish(FetchModels).await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the expiry refetches: the single allowed GET was consumed
    // and the event says not-from-cache.
    assert_eq!(loaded.len(), 1, "exactly one ModelsLoaded");
    assert!(!loaded[0].from_cache, "a refetch is not from_cache");
    assert_eq!(world.data.read().rows.len(), 1);
    mock.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn eci_loaded_rejoins_rows_without_a_refetch() {
    // Given an actor that fetched models while the index was absent —
    // rows built with dead ECI fields — and a models mock that would
    // FAIL if hit again (the re-join must not refetch). The catalog
    // carries a real seed joinable (Claude Sonnet 4.5, dot-vs-space).
    // The server and the temp dir outlive the setup block: the mock
    // checks read the server's live state, and the dir owns the
    // cache tree the actor writes into.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let (world, models_mock, loaded) = {
        let joinable_catalog = serde_json::json!({
            "data": [
                {
                    "id": "anthropic/claude-sonnet-4.5",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "architecture": {"output_modalities": ["text"]},
                    "context_length": 100_000
                }
            ]
        })
        .to_string();
        let models_mock = server
            .mock("GET", "/models")
            .with_status(200)
            .with_body(joinable_catalog)
            .create_async()
            .await;
        let world = World::start(&server.url(), dir.path.clone()).await;
        let recorder = world.models_recorder().await;
        world.harness.publish(FetchModels).await;
        let loaded = await_recorded(&recorder, 1, SETTLE).await;
        (world, models_mock, loaded)
    };
    assert!(!loaded[0].from_cache);
    let before = world.data.read().rows.clone();
    assert!(
        before.iter().all(|r| r.eci.is_none()),
        "precondition: rows joined against an empty index"
    );

    // When the score actor lands the index in the cell and publishes
    // EciLoaded — the raw display-name key form the real epoch uses.
    let index = crate::eci::parse_eci_scores(crate::eci::SEED_ECI_SCORES)
        .expect("seed parses")
        .0;
    world
        .data
        .update(|d| d.eci = Some(std::sync::Arc::new(index)));
    world
        .harness
        .publish(jinn_model_market_msg::EciLoaded {
            source: jinn_model_market_msg::EciSource::Seed,
            matched: 1,
            total: 1,
        })
        .await;

    // The ECI scores land on the cell's rows, so wait for the cell to
    // carry them. Reading the cell after a fixed sleep could happen
    // before the write, and the assertion would then prove nothing.
    jinn_testutil::wait::await_cell(&world.data, std::time::Duration::from_secs(10), |state| {
        state.rows.iter().any(|row| row.eci.is_some())
    })
    .await;

    // Then the rows in the cell carry ECI scores — no second fetch.
    models_mock.assert_async().await;
    let after = world.data.read();
    assert!(
        after.rows.iter().any(|r| r.eci.is_some()),
        "EciLoaded must re-join the retained raw listings"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_all_drops_every_cache() {
    // Given warm models AND endpoints caches at both levels, with mocks
    // expecting exactly one GET each for the refetch.
    // The server and the temp dir outlive the setup block: the mock
    // checks read the server's live state, and the dir owns the
    // cache tree the refresh sweeps.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let (world, recorder, models_mock, endpoints_mock) = {
        let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
        let detail = vec![MarketEndpointRow {
            tag: "stale".to_owned(),
            provider_name: "Stale".to_owned(),
            prompt_price: 1.0,
            completion_price: 1.0,
            cache_read_price: None,
            latency_p50_ms: None,
            throughput_p50: None,
            uptime_1d: None,
            quantization: None,
            overrides: Vec::new(),
        }];
        // A SECOND detail file for a model that is NOT displayed — refresh
        // must sweep the whole cache dir, not just the visible pane's file.
        crate::market_cache::store(
            &dir.path
                .join("models.json")
                .expect("temp root joins a bare name"),
            &crate::market_cache::stamped(fixture_rows(), at),
        )
        .expect("seed models cache");
        crate::market_cache::store(
            &dir.path
                .join("endpoints-test_model.json")
                .expect("temp root joins a bare name"),
            &crate::market_cache::stamped(detail.clone(), at),
        )
        .expect("seed endpoints cache");
        crate::market_cache::store(
            &dir.path
                .join("endpoints-other_model.json")
                .expect("temp root joins a bare name"),
            &crate::market_cache::stamped(detail, at),
        )
        .expect("seed the non-displayed endpoint cache");
        let world = World::start(&server.url(), dir.path.clone()).await;
        // Put a detail id on the data cell so refresh knows what to drop.
        let displayed = jinn_model_market_msg::MarketDetail {
            model_id: "openrouter/test/model".to_owned(),
            endpoints: Vec::new(),
        };
        world.data.update(|d| d.detail = Some(displayed));
        let recorder = world.models_recorder().await;
        let models_mock = expect_models_once(&mut server).await;
        let endpoints_mock = expect_endpoints_once(&mut server).await;
        (world, recorder, models_mock, endpoints_mock)
    };

    // When publishing RefreshAll.
    world.harness.publish(RefreshAll).await;
    let loaded = await_recorded(&recorder, 1, REFRESH_SETTLE).await;

    // The detail's endpoints land in the cell last — after both HTTP
    // fetches have fully resolved. Waiting for them before the mock
    // assertions closes the ordering race where `ModelsLoaded` (the first
    // fetch's event) arrives while the endpoints request is still in
    // flight: asserting then fails with "received 0" against a request
    // that is on its way, which is what flaked this test under full-suite
    // load.
    await_cell(&world.data, REFRESH_SETTLE, |state| {
        state
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.endpoints.is_empty())
    })
    .await;

    // Then both GETs were consumed — memory and disk caches are gone.
    assert_eq!(loaded.len(), 1, "models refetched");
    assert!(!loaded[0].from_cache);
    models_mock.assert_async().await;
    endpoints_mock.assert_async().await;
    // And the sweep dropped every endpoint disk file — the only one
    // present is the displayed model's freshly refetched file.
    let leftovers: Vec<String> = std::fs::read_dir(dir.path.as_path())
        .expect("cache dir readable")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("endpoints-"))
        .collect();
    assert_eq!(
        leftovers,
        vec!["endpoints-openrouter_test_model.json".to_owned()],
        "refresh must sweep ALL endpoint files, then the displayed refetch rewrites only its own"
    );
    // And the detail was refetched too, replacing the stale cell entry.
    let detail = world.data.read().detail.clone().expect("detail refetched");
    assert_eq!(detail.model_id, "openrouter/test/model");
    assert_eq!(detail.endpoints[0].tag, "testup");
}

#[rstest::rstest]
#[tokio::test]
async fn one_refresh_publish_reaches_both_market_actors_exactly_once() {
    // Given the market's two actors on one bus — the OpenRouter models
    // actor and the score actor — with both caches seeded on disk.
    // The server and the temp dir outlive the setup block: the mock
    // checks read the server's live state, and the dir owns the
    // cache tree the refresh sweeps.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let (world, score_dir, models_recorder, eci_recorder, models_mock) = {
        let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
        crate::market_cache::store(
            &dir.path
                .join("models.json")
                .expect("temp root joins a bare name"),
            &crate::market_cache::stamped(fixture_rows(), at),
        )
        .expect("seed models cache");
        let world = World::start_with_score_actor(&server.url(), dir.path.clone()).await;
        let score_dir = world.score_dir.clone();
        std::fs::create_dir_all(&score_dir).expect("seed score cache dir");
        // The recorders watch the halves' OUTPUTS. A recorder is a trouper
        // tap — a declarant — so tapping `RefreshAll` or the chain event
        // would add a second handler and flip the refresh path's route
        // policy inside the test. The outputs are inert: `ModelsLoaded` and
        // `EciLoaded` are events, and the tap only observes them.
        let models_recorder = world.models_recorder().await;
        let eci_recorder = world.harness.spawn_recorder::<EciLoaded>().await;
        let models_mock = expect_models_once(&mut server).await;
        (world, score_dir, models_recorder, eci_recorder, models_mock)
    };

    // When ONE `RefreshAll` rides the ERASED publish path — the route
    // action's path (`PublishClosure` → `sink.publish_schema` →
    // `deliver_schema_value`), whose transport the schema's declared
    // kind picks.
    let result = RouteResult::new_message(RefreshAll);
    let sink = world.harness.bus();
    for closure in result.messages {
        closure(&sink);
    }
    let models_loaded = await_recorded(&models_recorder, 1, REFRESH_SETTLE).await;
    let eci_loaded = await_recorded(&eci_recorder, 1, REFRESH_SETTLE).await;

    // Then BOTH halves acted on the single publish: the models actor
    // refetched (its cache is gone and the one allowed GET was spent)
    // and the score actor dropped its cache dir.
    assert_eq!(models_loaded.len(), 1, "the models actor refetched");
    assert!(!models_loaded[0].from_cache, "a refresh is not from_cache");
    models_mock.assert_async().await;
    assert_eq!(eci_loaded.len(), 1, "the score actor refreshed");
    let dropped = |name: &str| {
        score_dir
            .join(name)
            .expect("subdir join")
            .as_path()
            .try_exists()
            .unwrap_or(false)
    };
    assert!(
        !dropped("eci") && !dropped("vals"),
        "one publish must reach the score actor's cache drop too"
    );
    // And one publish is one refresh, not two: neither half ran twice.
    // The absence assertion below is only meaningful once the event stream
    // has stopped changing. A fixed sleep would pass on a slow host
    // simply because the unwanted event had not arrived yet.
    jinn_testutil::wait::await_settled(
        &models_recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    jinn_testutil::wait::await_settled(
        &eci_recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;
    assert!(
        models_recorder.is_empty() && eci_recorder.is_empty(),
        "one publish must deliver one refresh per half, got models: {:?}, eci: {:?}",
        models_recorder.drain(),
        eci_recorder.drain(),
    );
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_all_emits_the_chain_fact_exactly_once() {
    // Given an actor whose models cache is warm and a mock serving the
    // refetch.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    crate::market_cache::store(
        &dir.path
            .join("models.json")
            .expect("temp root joins a bare name"),
        &crate::market_cache::stamped(fixture_rows(), at),
    )
    .expect("seed the warm cache");
    let world = World::start(&server.url(), dir.path.clone()).await;
    // The chain fact's declarant set is the score actor's alone in
    // production. A tap here makes it two declarants of an EVENT, which
    // still broadcasts to both — the tap observes without changing the
    // chain's delivery.
    let chain_recorder = world
        .harness
        .spawn_recorder::<MarketModelsRefreshed>()
        .await;
    let models_mock = expect_models_once(&mut server).await;

    // When a RefreshAll arrives.
    world.harness.publish(RefreshAll).await;
    let chained = await_recorded(&chain_recorder, 1, REFRESH_SETTLE).await;

    // Then the models refresh announced itself with the chain fact.
    assert_eq!(chained.len(), 1, "exactly one chain fact");
    models_mock.assert_async().await;
    // And the refresh announced itself once — a second fact would mean a
    // second refresh.
    // The absence assertion below is only meaningful once the event stream
    // has stopped changing. A fixed sleep would pass on a slow host
    // simply because the unwanted event had not arrived yet.
    jinn_testutil::wait::await_settled(
        &chain_recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    assert!(
        chain_recorder.is_empty(),
        "one refresh emits one chain fact, got: {:?}",
        chain_recorder.drain(),
    );
}

#[rstest::rstest]
fn refresh_route_action_publishes_only_the_refresh_command() {
    // Given the popup's route table with the market rows attached.
    let slices = Slices::new();
    let ui = seed_cell(&slices, &market_ui_slot(), MarketUiState::default());
    let data = seed_cell(&slices, &market_data_slot(), MarketDataState::default());
    let routes = jinn_slices::KeyRoutes::new();
    crate::routes::attach_market_rows(&routes, &ui, &data);
    let mut state = jinn_kernel::AppState::default();

    // When the refresh action is dispatched.
    let result = routes
        .action_for(
            &jinn_slices::DynamicIntent::new(
                jinn_model_market_msg::market_ui_scope(),
                "refresh-model-market",
                "refresh",
            ),
            jinn_slices::ActionCtx {
                state: &mut state,
                slices: &slices,
                config: jinn_slices::empty_config_layer(),
                key_bytes: Vec::new(),
            },
        )
        .expect("the popup attaches a refresh action");

    // Then the action publishes exactly one message, the refresh
    // command. A second message would put two publishes on the wire
    // with no ordering between them — the chain is the score half's
    // trigger, not a second publish.
    assert_eq!(result.message_names, vec!["RefreshAll"]);
    assert_eq!(result.messages.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn endpoint_debounce_drops_stale_fetches() {
    // Given an actor with an empty cache and a mock for the NEWEST
    // model only — a hit for the stale model would fail the test.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.endpoints_recorder().await;
    let newest = expect_endpoints_once(&mut server).await;

    // When two fetches race: seq 1 for the stale model, seq 2 for the
    // newest (the UI cell's sequence reads 2 when the debounce wakes).
    world.ui.update(|s| s.fetch_seq = 2);
    world
        .harness
        .publish(FetchEndpoints {
            model_id: "openrouter/stale/model".to_owned(),
            seq: 1,
        })
        .await;
    world
        .harness
        .publish(FetchEndpoints {
            model_id: "openrouter/test/model".to_owned(),
            seq: 2,
        })
        .await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then only the newest model fetched: one EndpointsLoaded for it,
    // none for the stale one, and the single allowed GET was consumed.
    assert_eq!(loaded.len(), 1, "only the newest fetch lands");
    assert_eq!(loaded[0].model_id, "openrouter/test/model");
    let detail = world.data.read().detail.clone().expect("detail written");
    assert_eq!(detail.model_id, "openrouter/test/model");
    newest.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn equal_seq_fetch_still_serves_after_the_debounce() {
    // Given an actor whose UI sequence has not advanced since the
    // request was minted.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.endpoints_recorder().await;
    let mock = expect_endpoints_once(&mut server).await;

    // When a single fetch runs with the sequence unchanged.
    world
        .harness
        .publish(FetchEndpoints {
            model_id: "openrouter/test/model".to_owned(),
            seq: 0,
        })
        .await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the fetch is served — only strictly older sequences drop.
    assert_eq!(loaded.len(), 1);
    mock.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_models_without_a_key_still_serves_the_table() {
    // Given a world whose API-key store carries no OPENROUTER_API_KEY
    // (the harness's default) and a mock that requires NO auth header.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.models_recorder().await;
    let mock = server
        .mock("GET", "/models")
        .match_header("authorization", mockito::Matcher::Missing)
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;

    // When publishing FetchModels.
    world.harness.publish(FetchModels).await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the unauthenticated fetch serves the table — a missing key
    // must not gate /models.
    assert_eq!(loaded.len(), 1);
    assert!(!loaded[0].from_cache);
    mock.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn key_inserted_after_spawn_rides_the_bearer_header() {
    // Given a world whose API-key store was EMPTY when the actor
    // spawned — the production ordering: the env ask fills the store in
    // the startup tail, after every actor exists.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    assert!(
        world.api_keys.is_empty(),
        "precondition: the store is empty at spawn"
    );
    let recorder = world.models_recorder().await;
    let mock = server
        .mock("GET", "/models")
        .match_header("authorization", "Bearer sk-test-after-spawn")
        .with_status(200)
        .with_body(catalog_body())
        .create_async()
        .await;

    // When the key lands in the store post-spawn (the env ask's move),
    // then a FetchModels is published.
    world.api_keys.insert(
        crate::openrouter_actor::OPENROUTER_API_KEY_ENV.to_owned(),
        "sk-test-after-spawn".to_owned(),
    );
    world.harness.publish(FetchModels).await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the fetch carried the bearer header — the key was read at
    // request time, not frozen at spawn. Under the old spawn-time
    // snapshot this request went out headerless and mockito 404s it.
    assert_eq!(loaded.len(), 1);
    assert!(!loaded[0].from_cache);
    mock.assert_async().await;
}

#[rstest::rstest]
#[tokio::test]
async fn key_landing_after_spawn_also_rides_the_endpoints_fetch() {
    // Given the same post-spawn key ordering, with an endpoints mock
    // requiring the bearer header.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.endpoints_recorder().await;
    let mock = server
        .mock("GET", "/models/test/model/endpoints")
        .match_header("authorization", "Bearer sk-test-after-spawn")
        .with_status(200)
        .with_body(endpoints_body())
        .create_async()
        .await;

    // When the key lands in the store post-spawn, then an
    // EndpointsLoaded fetch is published.
    world.api_keys.insert(
        crate::openrouter_actor::OPENROUTER_API_KEY_ENV.to_owned(),
        "sk-test-after-spawn".to_owned(),
    );
    world
        .harness
        .publish(FetchEndpoints {
            model_id: "openrouter/test/model".to_owned(),
            seq: 0,
        })
        .await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the detail fetch carried the bearer header too — both fetch
    // paths read the store at request time.
    assert_eq!(loaded.len(), 1);
    mock.assert_async().await;
    // And the authenticated percentile blocks parsed through to the
    // detail row.
    let detail = world.data.read().detail.clone().expect("detail written");
    assert_eq!(detail.endpoints[0].tag, "testup");
}

#[rstest::rstest]
#[tokio::test]
async fn endpoints_fetch_serves_unauthenticated_when_the_store_stays_empty() {
    // Given a world whose API-key store never fills, and an endpoints
    // mock serving the unauthenticated wire shape: the percentile
    // blocks come back null without a usable key. (The header shape on
    // this path — an empty bearer token, no header — is the provider
    // crate's detail; upstream answers both as unauthenticated.)
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    let recorder = world.endpoints_recorder().await;
    let unauth_body = serde_json::json!({
        "data": {
            "endpoints": [
                {
                    "provider_name": "TestUp",
                    "tag": "testup",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "uptime_last_1d": 99.0,
                    "latency_last_30m": null,
                    "throughput_last_30m": null
                }
            ]
        }
    })
    .to_string();
    let mock = server
        .mock("GET", "/models/test/model/endpoints")
        .with_status(200)
        .with_body(unauth_body)
        .create_async()
        .await;

    // When publishing the detail fetch with the store still empty.
    world
        .harness
        .publish(FetchEndpoints {
            model_id: "openrouter/test/model".to_owned(),
            seq: 0,
        })
        .await;
    let loaded = await_recorded(&recorder, 1, SETTLE).await;

    // Then the unauthenticated fetch still served: an absent key must
    // not gate the detail, and the percentile fields parse as None
    // (the row's auth-gated fields).
    assert_eq!(loaded.len(), 1);
    mock.assert_async().await;
    let detail = world.data.read().detail.clone().expect("detail written");
    assert!(detail.endpoints[0].latency_p50_ms.is_none());
    assert!(detail.endpoints[0].throughput_p50.is_none());
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_models_failure_emits_refresh_failed_and_keeps_stale_rows() {
    // Given an actor whose first fetch succeeded, then the mock breaks.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let world = World::start(&server.url(), dir.path.clone()).await;
    world.data.update(|d| {
        d.rows = fixture_rows();
    });

    // When the models endpoint 500s.
    server
        .mock("GET", "/models")
        .with_status(500)
        .with_body("boom")
        .create_async()
        .await;
    world.harness.publish(FetchModels).await;

    // The error line lands in the data cell, so wait for the cell to carry
    // it. Reading the cell after a fixed sleep could happen before the
    // write, and the assertion would then prove nothing.
    jinn_testutil::wait::await_cell(&world.data, std::time::Duration::from_secs(10), |state| {
        state.last_error.is_some()
    })
    .await;

    // Then the stale rows stand and the error line is set.
    let d = world.data.read();
    assert_eq!(d.rows.len(), 1, "stale rows must not be dropped");
    assert!(d.last_error.is_some(), "the degraded-mode line is set");
}

#[rstest::rstest]
#[tokio::test]
async fn fetch_models_fetches_no_detail() {
    // Given a MULTI-row catalog and zero-expect mocks for BOTH rows'
    // endpoints — any detail request, for whichever row, fails the
    // expectations at the wire. Under fetch-on-drill-in-only semantics
    // a models load alone must make no endpoints request, whichever
    // row the table would highlight.
    // The server and the temp dir outlive the setup block: the mock
    // checks read the server's live state, and the dir owns the
    // cache tree the actor writes into.
    let mut server = mockito::Server::new_async().await;
    let dir = jinn_testutil::tempdir();
    let (world, models_recorder, endpoints_recorder, models_mock, cheap_mock, dear_mock) = {
        let catalog = serde_json::json!({
            "data": [
                {
                    "id": "test/dear",
                    "pricing": {"prompt": "0.03", "completion": "0.3"},
                    "architecture": {"output_modalities": ["text"]},
                    "context_length": 100_000
                },
                {
                    "id": "test/cheap",
                    "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                    "architecture": {"output_modalities": ["text"]},
                    "context_length": 100_000
                }
            ]
        })
        .to_string();
        let world = World::start(&server.url(), dir.path.clone()).await;
        let models_recorder = world.models_recorder().await;
        let endpoints_recorder = world.endpoints_recorder().await;
        let models_mock = server
            .mock("GET", "/models")
            .with_status(200)
            .with_body(catalog)
            .create_async()
            .await;
        let cheap_mock = server
            .mock("GET", "/models/test/cheap/endpoints")
            .expect(0)
            .create_async()
            .await;
        let dear_mock = server
            .mock("GET", "/models/test/dear/endpoints")
            .expect(0)
            .create_async()
            .await;
        (
            world,
            models_recorder,
            endpoints_recorder,
            models_mock,
            cheap_mock,
            dear_mock,
        )
    };

    // When publishing FetchModels — the popup-open publish, no nav key,
    // no drill-in.
    world.harness.publish(FetchModels).await;
    let loaded = await_recorded(&models_recorder, 1, SETTLE).await;
    // The absence assertion below is only meaningful once the event stream
    // has stopped changing. A fixed sleep would pass on a slow host
    // simply because the unwanted event had not arrived yet.
    jinn_testutil::wait::await_settled(
        &endpoints_recorder,
        std::time::Duration::from_millis(200),
        std::time::Duration::from_secs(10),
    )
    .await;

    // Then the models rows land and nothing else was requested: the
    // endpoints mocks never fired and no EndpointsLoaded rode the bus.
    assert_eq!(loaded.len(), 1, "exactly one ModelsLoaded");
    let rows = world.data.read().rows.clone();
    assert_eq!(rows.len(), 2, "the catalog rows landed");
    models_mock.assert_async().await;
    cheap_mock.assert_async().await;
    dear_mock.assert_async().await;
    assert!(
        endpoints_recorder.is_empty(),
        "a models load fetches no detail, got: {:?}",
        endpoints_recorder.drain()
    );
}

/// One-row fixture matching [`catalog_body`]'s listing.
fn fixture_rows() -> Vec<jinn_model_market_msg::MarketRow> {
    vec![jinn_model_market_msg::MarketRow {
        id: "openrouter/test/model".to_owned(),
        author: "test".to_owned(),
        slug: "model".to_owned(),
        variant: None,
        prompt_price: 3e-6,
        completion_price: 1.5e-5,
        cache_read_price: Some(3e-7),
        has_overrides: false,
        context_length: Some(100_000),
        eci: None,
        eci_ci: None,
        vals: None,
        is_free: false,
    }]
}

// ── `O`: the model-page open ─────────────────────────────────────────────

use error_stack::Report;
use jinn_model_market_msg::OpenModelPage;
use jinn_session_history_msg::PushChatEntry;

use super::PageOpenError;
use super::PageOpener;

/// An opener scripted to fail — the `xdg-open`-missing shape.
struct FailingPageOpener;

impl PageOpener for FailingPageOpener {
    fn open(&self, url: &str) -> Result<(), Report<PageOpenError>> {
        Err(Report::new(PageOpenError)
            .attach(format!("no browser opener on this platform for {url}")))
    }

    fn name(&self) -> &'static str {
        "failing"
    }
}

/// A world whose OpenRouter actor launches pages through a scripted
/// opener. The fetch caches are unused here — a page open never hits
/// the network — so the base URL can point at a dead port.
async fn start_with_opener(
    opener: std::sync::Arc<dyn PageOpener>,
) -> (World, jinn_core_types::SessionId) {
    let cache = jinn_testutil::tempdir();
    let cache_dir = cache
        .path
        .join("market-cache")
        .expect("tempdir root joins a bare name");
    let harness = TestHarness::new().await;
    let deps = harness.actor_deps().await;
    let api_keys = deps.services.api_keys.clone();
    let slices = Slices::new();
    let data = seed_cell(&slices, &market_data_slot(), MarketDataState::default());
    let ui = seed_cell(&slices, &market_ui_slot(), MarketUiState::default());
    OpenRouterMarketActor::spawn(
        harness.system(),
        OpenRouterMarketActorDeps {
            deps,
            data: data.clone(),
            ui: ui.clone(),
            base_url: "http://127.0.0.1:9".to_owned(),
            cache_dir: Some(cache_dir.clone()),
            opener,
        },
    );
    let world = World {
        harness,
        data,
        ui,
        base_url: String::new(),
        api_keys,
        score_dir: cache_dir.clone(),
        cache_dir,
    };
    let session_id = jinn_core_types::SessionId::new_v5_tag("s-page-open");
    (world, session_id)
}

/// `O` against a broken opener reports into the stamped session: one
/// `PushChatEntry`, an `Error` entry, user-facing prose naming the URL.
#[rstest::rstest]
#[tokio::test]
async fn a_failed_page_open_reports_a_chat_error_into_the_stamped_session() {
    // Given an actor whose opener always fails, and a page-open command
    // addressed to a session.
    let (world, session_id) = start_with_opener(std::sync::Arc::new(FailingPageOpener)).await;
    let chat = world.harness.spawn_recorder::<PushChatEntry>().await;

    // When the open is published.
    world
        .harness
        .publish(OpenModelPage {
            model_id: "openrouter/a/small:free".to_owned(),
            session_id: session_id.clone(),
        })
        .await;
    let pushed = await_recorded(&chat, 1, SETTLE).await;

    // Then exactly one chat entry lands, in the addressed session, an
    // Error entry carrying the URL and the failure detail.
    assert_eq!(pushed.len(), 1, "exactly one PushChatEntry, got {pushed:?}");
    assert_eq!(pushed[0].session_id, session_id);
    let jinn_core_types::ChatEntryKind::Error(text) = &pushed[0].entry.kind else {
        panic!(
            "a failed open reports an Error entry, got {:?}",
            pushed[0].entry.kind
        );
    };
    assert!(
        text.contains("https://openrouter.ai/a/small:free"),
        "the note names the URL, got: {text}"
    );
    assert!(
        text.contains("could not open"),
        "the note is user-facing prose, got: {text}"
    );
}

/// `O` against a working opener reports nothing: the recorder drains
/// empty — silence is the success contract.
#[rstest::rstest]
#[tokio::test]
async fn a_successful_page_open_reports_nothing() {
    // Given an actor whose opener succeeds.
    let (world, session_id) = start_with_opener(std::sync::Arc::new(super::SilentPageOpener)).await;
    let chat = world.harness.spawn_recorder::<PushChatEntry>().await;

    // When the open is published and the stream settles.
    world
        .harness
        .publish(OpenModelPage {
            model_id: "openrouter/a/small".to_owned(),
            session_id,
        })
        .await;
    jinn_testutil::wait::await_settled(&chat, std::time::Duration::from_millis(200), SETTLE).await;

    // Then nothing was pushed.
    assert!(
        chat.is_empty(),
        "a successful open is silent, got: {:?}",
        chat.drain()
    );
}
