//! Observable behavior tests for [`ScoreMarketActor`]: cache-vs-seed
//! selection, TTL expiry with an injected clock, refresh dropping the
//! cache, and the event stream's shape.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test code"
)]

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::bus::HarnessServices as _;
use jinn_model_market_msg::EciLoaded;
use jinn_model_market_msg::EciSource;
use jinn_model_market_msg::RefreshFailed;
use jinn_model_market_msg::RefreshScores;
use jinn_model_market_msg::RefreshScoresForce;
use jinn_model_market_msg::ValsLoaded;
use jinn_model_market_msg::ValsSource;
use jinn_slices::cell::TypedCell;
use jinn_testutil::bus_harness::TestHarness;
use jinn_testutil::bus_harness::await_recorded;
use marked_path::MarkedPathAccess as _;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::eci::SEED_BENCHMARK_METADATA;
use crate::eci::SEED_ECI_SCORES;
use crate::eci::SEED_MODEL_METADATA;
use crate::score_actor::ScoreMarketActor;
use crate::score_actor::ScoreMarketActorDeps;

/// Installs the process-wide rustls crypto provider for this test
/// binary — reqwest is built `rustls-no-provider`, so without this every
/// `Client::new()` panics with "No provider set". Idempotent; the result
/// is deliberately ignored.
#[ctor::ctor]
fn install_rustls_provider_for_tests() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A controlled clock owned by one fixture: the test sets `ms` directly.
struct FakeClock {
    ms: Arc<std::sync::atomic::AtomicU64>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            ms: Arc::new(std::sync::atomic::AtomicU64::new(1_000_000)),
        }
    }

    fn now(&self) -> u64 {
        self.ms.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn advance(&self, ms: u64) {
        self.ms.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// The dep-shaped callable reading this clock.
    fn dep(&self) -> std::sync::Arc<dyn Fn() -> u64 + Send + Sync> {
        let ms = Arc::clone(&self.ms);
        std::sync::Arc::new(move || ms.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// The test fixture: harness, temp cache root, data cell, a spawned
/// actor pointed at controllable URLs, and the fixture's own clock.
struct ScoreFixture {
    _dir: jinn_testutil::JinnTempDir,
    harness: TestHarness,
    data_cell: TypedCell<jinn_model_market_msg::MarketDataState>,
    /// The fixture's own AppPaths; the scores cache dir resolves from it
    /// exactly as the actor's does at spawn.
    paths: jinn_common::app_paths::AppPaths,
    clock: FakeClock,
}

/// Builds the fixture with the actor's zip URL aimed at `zip_url` and
/// the fixture's own clock wired into the deps. The actor's cache dir
/// resolves through `AppPaths::scores_cache_dir()` — the same one layout
/// constant production uses.
async fn score_fixture_at(zip_url: String) -> ScoreFixture {
    score_fixture_urls(
        zip_url,
        "http://127.0.0.1:9/benchmarks/vals_index".to_owned(),
    )
    .await
}

/// Builds the fixture with both of the actor's URLs aimed at given
/// strings — the vals tests point the page URL at a live mock.
async fn score_fixture_urls(zip_url: String, vals_page_url: String) -> ScoreFixture {
    let dir = jinn_testutil::tempdir();
    let harness = TestHarness::new().await;
    let mut services = harness.services().await;
    services.paths = jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path());
    let data_cell =
        services
            .slices
            .reader::<jinn_model_market_msg::MarketDataState>(
                &jinn_model_market_msg::market_data_slot(),
            )
            .expect("the cell catalog registers the market data slot");
    let paths = services.paths.clone();
    let clock = FakeClock::new();

    let _actor = ScoreMarketActor::spawn(
        harness.system(),
        ScoreMarketActorDeps {
            deps: ActorDeps {
                services: services.clone(),
            },
            data_cell: data_cell.clone(),
            cache_dirs: crate::score_cache::ScoreCacheDirs::new(&services.paths.scores_cache_dir()),
            client: reqwest::Client::new(),
            zip_url,
            vals_page_url,
            now_ms: clock.dep(),
        },
    );

    ScoreFixture {
        _dir: dir,
        harness,
        data_cell,
        paths,
        clock,
    }
}

/// Builds a valid `benchmark_data.zip` in memory carrying exactly the
/// three extraction targets (the seed bytes stand in for Epoch's, so the
/// parse round-trips for free).
fn zip_bytes() -> Vec<u8> {
    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    zip.start_file(
        "epoch_capabilities_index/eci_scores.csv",
        SimpleFileOptions::default(),
    )
    .expect("start eci_scores member");
    zip.write_all(SEED_ECI_SCORES).expect("write eci member");
    zip.start_file("benchmark_metadata.csv", SimpleFileOptions::default())
        .expect("start metadata member");
    zip.write_all(SEED_BENCHMARK_METADATA)
        .expect("write metadata member");
    zip.start_file("model_metadata.csv", SimpleFileOptions::default())
        .expect("start roster member");
    zip.write_all(SEED_MODEL_METADATA)
        .expect("write roster member");
    zip.finish().expect("finish zip").into_inner()
}

/// Serves one zip body from a mockito server. Returns the server (keep
/// alive for the mock's lifetime) and the mock (assert with `.assert()`).
async fn serve_zip() -> (mockito::ServerGuard, mockito::Mock) {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/data/benchmark_data.zip")
        .with_status(200)
        .with_body(zip_bytes())
        .create_async()
        .await;
    (server, mock)
}

/// The served zip's URL.
fn zip_url(server: &mockito::ServerGuard) -> String {
    format!("{}/data/benchmark_data.zip", server.url())
}

#[rstest::rstest]
#[tokio::test]
async fn first_run_without_network_uses_seed() {
    // Given a fixture with an empty cache dir and an actor whose URLs
    // point at ports nothing listens on (both score sources fail).
    let fixture = score_fixture_at("http://127.0.0.1:9/data/benchmark_data.zip".to_owned()).await;
    let eci_dir = fixture.paths.scores_cache_dir();
    assert!(!eci_dir.exists(), "precondition: no cache dir");
    let loads = fixture.harness.spawn_recorder::<EciLoaded>().await;
    let failures = fixture.harness.spawn_recorder::<RefreshFailed>().await;

    // When a RefreshScores arrives (no server up — both requests fail).
    fixture.harness.publish(RefreshScores).await;
    let reported = await_recorded::<RefreshFailed>(&failures, 2, Duration::from_secs(5)).await;

    // Then the refresh reports the ECI failure…
    let eci_failures: Vec<_> = reported.iter().filter(|f| f.what == "eci").collect();
    assert_eq!(eci_failures.len(), 1);
    assert_eq!(reported[0].what, "eci");
    // …and the cell resolves the index from the compiled seed bytes —
    // the ECI column still populates, eci_source = Seed, nothing is
    // written to disk as a download proxy.
    assert!(!eci_dir.exists(), "the seed must never be written to disk");
    // And the seed load itself is announced (the ECI column populates).
    let seed_loads = await_recorded::<EciLoaded>(&loads, 1, Duration::from_secs(5)).await;
    assert_eq!(seed_loads.len(), 1);
    assert_eq!(seed_loads[0].source, EciSource::Seed);
    // (Guard scoped to the block: cells are parking_lot locks on the
    // render thread; never held across an await.)
    let (index_nonempty, has_opus, source, loading) = {
        let data = fixture.data_cell.read();
        let index_present = data.eci.as_ref().expect("seed data lands in the cell");
        (
            !index_present.is_empty(),
            index_present.get("Claude Opus 5.5").is_some(),
            data.eci_source,
            data.eci_loading,
        )
    };
    assert!(index_nonempty);
    assert!(has_opus);
    assert_eq!(source, EciSource::Seed);
    assert!(!loading);
}

#[rstest::rstest]
#[tokio::test]
async fn fresh_cache_serves_without_a_request() {
    // Given a complete, fresh cache dir (written by a real extraction
    // into the fixture's own cache root) and no reachable download URL.
    let fixture = score_fixture_at("http://127.0.0.1:9/data/benchmark_data.zip".to_owned()).await;
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).eci;
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), fixture.clock.now())
        .expect("seed the cache with a real extraction");
    let loads = fixture.harness.spawn_recorder::<EciLoaded>().await;

    // When a RefreshScores arrives with the clock still inside the TTL.
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<EciLoaded>(&loads, 1, Duration::from_secs(5)).await;

    // Then the cache serves: source = Download, matched = every scored
    // row, total = every row.
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, EciSource::Download);
    assert_eq!(landed[0].matched, 275);
    assert_eq!(landed[0].total, 277);
}

#[rstest::rstest]
#[tokio::test]
async fn expired_eci_ttl_refetches_from_the_served_zip() {
    // Given a complete cache (written into the fixture's own cache root
    // well before the TTL expires) and a mock serving the refetch.
    let (server, mock) = serve_zip().await;
    let fixture = score_fixture_at(zip_url(&server)).await;
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).eci;
    let written_at = fixture.clock.now();
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), written_at)
        .expect("seed the cache");
    let loads = fixture.harness.spawn_recorder::<EciLoaded>().await;

    // When the clock advances past SCORE_TTL_MS and a RefreshScores
    // arrives.
    fixture.clock.advance(crate::score_cache::SCORE_TTL_MS + 1);
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<EciLoaded>(&loads, 1, Duration::from_secs(5)).await;

    // Then the fetch hit the mock and the extraction re-landed with a
    // fresh stamp. (The mock was created expecting exactly one hit;
    // `assert_async` fails when the count differs.)
    mock.assert_async().await;
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, EciSource::Download);
    let stamp_millis = {
        let data = fixture.data_cell.read();
        data.eci_fetched_at
            .expect("a successful load stamps the cell")
            .as_millisecond()
    };
    assert_eq!(
        u64::try_from(stamp_millis).unwrap_or(0),
        written_at + crate::score_cache::SCORE_TTL_MS + 1,
        "the refetch re-stamps from the injected clock's advanced value"
    );
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_scores_force_drops_the_eci_cache() {
    // Given a complete cache dir (the fixture's own) and a mock serving
    // the zip.
    let (server, _mock) = serve_zip().await;
    let fixture = score_fixture_at(zip_url(&server)).await;
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).eci;
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), fixture.clock.now())
        .expect("seed the cache");
    assert!(eci_dir.exists(), "precondition: cache present");
    let loads = fixture.harness.spawn_recorder::<EciLoaded>().await;

    // When a RefreshScoresForce arrives (forced download against the up mock).
    fixture.harness.publish(RefreshScoresForce).await;
    let landed = await_recorded::<EciLoaded>(&loads, 1, Duration::from_secs(5)).await;

    // Then the old dir was dropped, the download re-extracted exactly
    // the three CSVs, and the cell shows Download.
    assert!(eci_dir.exists(), "the refresh re-extracts the cache");
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, EciSource::Download);
    let names: Vec<String> = std::fs::read_dir(eci_dir.as_path())
        .expect("cache dir readable")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    for expected in [
        "eci_scores.csv",
        "benchmark_metadata.csv",
        "model_metadata.csv",
        "fetched_at",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "extraction must contain {expected}; dir holds {names:?}",
        );
    }
    assert_eq!(names.len(), 4, "extraction writes exactly the four files");
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_scores_force_reports_a_failing_download_and_the_seed_stands() {
    // Given a cache dir (the fixture's own) and a refresh whose download
    // fails (no server).
    let fixture = score_fixture_at("http://127.0.0.1:9/data/benchmark_data.zip".to_owned()).await;
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).eci;
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), fixture.clock.now())
        .expect("seed the cache");
    let loads = fixture.harness.spawn_recorder::<EciLoaded>().await;

    // When a RefreshScoresForce arrives.
    fixture.harness.publish(RefreshScoresForce).await;
    let landed = await_recorded::<EciLoaded>(&loads, 1, Duration::from_secs(5)).await;

    // Then the dropped dir is gone (refresh drops even when the later
    // download fails) and the seed stands in the cell.
    assert!(
        !eci_dir.exists(),
        "a failed refresh must not leave a half cache behind"
    );
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, EciSource::Seed);
    let (has_opus, source) = {
        let data = fixture.data_cell.read();
        (
            data.eci
                .as_ref()
                .expect("seed lands")
                .get("Claude Opus 5.5")
                .is_some(),
            data.eci_source,
        )
    };
    assert!(has_opus);
    assert_eq!(source, EciSource::Seed);
}

#[rstest::rstest]
#[test]
fn extraction_writes_exactly_the_three_csvs_plus_stamp() {
    // Given a temp cache root and a real zip.
    let dir = jinn_testutil::tempdir();
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    )
    .eci;

    // When extracting.
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), 123)
        .expect("extraction succeeds");

    // Then exactly the three CSVs (plus the stamp) landed, byte-identical
    // to the sources.
    for (name, expected) in [
        ("eci_scores.csv", SEED_ECI_SCORES),
        ("benchmark_metadata.csv", SEED_BENCHMARK_METADATA),
        ("model_metadata.csv", SEED_MODEL_METADATA),
    ] {
        let written = std::fs::read(
            eci_dir
                .join(name)
                .expect("cache dir joins a bare file name"),
        )
        .expect("file written");
        assert_eq!(written, expected, "{name} must be extracted verbatim");
    }
    let stamp = std::fs::read_to_string(
        eci_dir
            .join("fetched_at")
            .expect("cache dir joins a bare file name"),
    )
    .expect("stamp written");
    assert_eq!(stamp, "123");
    assert_eq!(
        std::fs::read_dir(eci_dir.as_path()).expect("dir").count(),
        4,
        "nothing beyond the three CSVs and the stamp"
    );
}

#[rstest::rstest]
#[test]
fn load_eci_fresh_returns_none_when_any_csv_is_missing() {
    // Given a cache dir with only one of the three CSVs.
    let dir = jinn_testutil::tempdir();
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    )
    .eci;
    std::fs::create_dir_all(eci_dir.as_path()).expect("mkdir");
    std::fs::write(
        eci_dir
            .join("fetched_at")
            .expect("cache dir joins a bare file name"),
        "1000",
    )
    .expect("stamp");
    std::fs::write(
        eci_dir
            .join("eci_scores.csv")
            .expect("cache dir joins a bare file name"),
        SEED_ECI_SCORES,
    )
    .expect("partial file");

    // When reading fresh inside the TTL.
    let loaded = crate::score_cache::load_eci_fresh(&eci_dir, 1_000, || 2_000);

    // Then nothing loads — a partial cache never serves.
    assert!(loaded.is_none());
}

#[rstest::rstest]
#[test]
fn load_eci_fresh_returns_none_past_the_ttl() {
    // Given a complete, stamped cache dir.
    let dir = jinn_testutil::tempdir();
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    )
    .eci;
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), 1_000).expect("extraction");

    // When reading with now just past the TTL.
    let loaded = crate::score_cache::load_eci_fresh(&eci_dir, 1_000, || 2_001);

    // Then nothing loads.
    assert!(loaded.is_none());
}

#[rstest::rstest]
#[test]
fn load_eci_fresh_serves_a_complete_cache_inside_the_ttl() {
    // Given a complete, stamped cache dir.
    let dir = jinn_testutil::tempdir();
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    )
    .eci;
    crate::score_cache::download_and_extract(&eci_dir, &zip_bytes(), 1_000).expect("extraction");

    // When reading with now just inside the TTL (one tick before it
    // lapses).
    let loaded = crate::score_cache::load_eci_fresh(&eci_dir, 1_000, || 1_999);

    // Then the cache serves with its stamp.
    let cached_eci = loaded.expect("fresh cache serves");
    assert_eq!(cached_eci.fetched_at_ms, 1_000);
    assert_eq!(cached_eci.files.len(), 3);
}

#[rstest::rstest]
#[test]
fn drop_cache_dir_is_success_when_nothing_is_there() {
    // Given a cache root with no eci dir.
    let dir = jinn_testutil::tempdir();
    let eci_dir = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    )
    .eci;

    // When dropping.
    // Then the drop succeeds (nothing to do is not an error).
    crate::score_cache::drop_cache_dir(&eci_dir).expect("drop succeeds");
}

#[rstest::rstest]
#[test]
fn parse_seed_produces_the_full_index_with_metadata() {
    // Given the shipped seed bytes.
    // When parsing all three.
    let data = crate::eci::seed_data().expect("seed parses");

    // Then the index holds every scored version (the seed's two CI-less
    // rows — e.g. `GPT-5` — are skipped but still counted in total) and
    // the metadata tables ride along.
    assert_eq!(data.index.len(), 275);
    assert_eq!(data.total_rows, 277);
    assert!(data.index.get("GPT-5").is_none());
    assert!(data.index.get("Claude Opus 5.5").is_some());
    assert_eq!(data.benchmark_metadata.len(), 91);
    assert!(
        data.benchmark_metadata
            .iter()
            .any(|m| m.benchmark == "GPQA diamond" && m.in_eci && m.random_baseline == 0.25)
    );
    assert!(!data.roster_names.is_empty(), "expected a roster, got none");
}

// ── Vals: fetch, cache, and per-source failure isolation ────────────────

/// A tiny synthetic vals payload — 2 models, never a roster copy.
fn vals_payload() -> String {
    serde_json::json!({
        "metadata": {"version": "2.2", "updated": "2026-11-18"},
        "tasks": {
            "overall": {
                "z-ai/glm-5.3": {"accuracy": 58.4},
                "openai/gpt-6.2-sol": {"accuracy": 77.2}
            }
        }
    })
    .to_string()
}

/// Serves a leaderboard page naming `data_path` and that path's JSON
/// body from one mockito server.
async fn serve_vals(data_path: &str) -> (mockito::ServerGuard, mockito::Mock, mockito::Mock) {
    let mut server = mockito::Server::new_async().await;
    let page = format!(
        "<html><script src=\"/_astro/other.HASH.js\"></script>\
         <a href=\"/_astro/{data_path}\">data</a></html>"
    );
    let page_mock = server
        .mock("GET", "/benchmarks/vals_index")
        .with_status(200)
        .with_body(page)
        .create_async()
        .await;
    let data_mock = server
        .mock("GET", format!("/_astro/{data_path}").as_str())
        .with_status(200)
        .with_body(vals_payload())
        .create_async()
        .await;
    (server, page_mock, data_mock)
}

/// A fixture whose vals page URL points at a live mock server; the ECI
/// zip URL stays dead so ECI falls back to the seed deterministically.
async fn vals_fixture(server: &mockito::ServerGuard) -> ScoreFixture {
    score_fixture_urls(
        "http://127.0.0.1:9/data/benchmark_data.zip".to_owned(),
        format!("{}/benchmarks/vals_index", server.url()),
    )
    .await
}

#[rstest::rstest]
#[tokio::test]
async fn vals_load_fills_the_vals_fields_without_touching_eci_state() {
    // Given a fixture whose vals page mock names the hashed data file
    // (the ECI zip URL is dead — ECI deterministically falls back to
    // the seed).
    let (server, page_mock, data_mock) =
        serve_vals("benchmark_view_vals_index.D4UVDIPY.json").await;
    let fixture = vals_fixture(&server).await;
    let vals_loads = fixture.harness.spawn_recorder::<ValsLoaded>().await;

    // When a RefreshScores arrives.
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<ValsLoaded>(&vals_loads, 1, Duration::from_secs(5)).await;

    // Then the vals index landed in the cell sourced Download.
    page_mock.assert_async().await;
    data_mock.assert_async().await;
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, ValsSource::Download);
    let (index_present, stamp_present, loading, vals_source, eci_ok) = {
        let data = fixture.data_cell.read();
        (
            data.vals.as_ref().expect("vals index lands").len(),
            data.vals_fetched_at.is_some(),
            data.vals_loading,
            data.vals_source,
            data.eci.is_some(),
        )
    };
    assert_eq!(index_present, 2);
    assert!(stamp_present);
    assert!(!loading);
    assert_eq!(vals_source, ValsSource::Download);
    // And the ECI half stayed healthy: its seed stood in (isolation in
    // the success direction).
    assert!(eci_ok);
}

#[rstest::rstest]
#[tokio::test]
async fn a_vals_failure_blanks_only_the_vals_column() {
    // Given a fixture whose ECI URL is dead (seed stands) and whose
    // vals URL points at nothing.
    let fixture = score_fixture_at("http://127.0.0.1:9/data/benchmark_data.zip".to_owned()).await;
    let failures = fixture.harness.spawn_recorder::<RefreshFailed>().await;

    // When a RefreshScores arrives (both sources fail).
    fixture.harness.publish(RefreshScores).await;
    let reported = await_recorded::<RefreshFailed>(&failures, 2, Duration::from_secs(5)).await;

    // Then the vals failure is reported separately from the ECI one.
    let vals_failures: Vec<_> = reported.iter().filter(|f| f.what == "vals").collect();
    let eci_failures: Vec<_> = reported.iter().filter(|f| f.what == "eci").collect();
    assert_eq!(vals_failures.len(), 1, "one vals failure reported");
    assert_eq!(eci_failures.len(), 1, "one eci failure reported");
    let (vals_none, eci_present, source) = {
        let data = fixture.data_cell.read();
        (
            data.vals.is_none(),
            data.eci.as_ref().expect("seed stands").len(),
            data.eci_source,
        )
    };
    // And the cell shows exactly the isolation contract: vals empty,
    // ECI seed standing.
    assert!(vals_none);
    assert!(eci_present > 0);
    assert_eq!(source, EciSource::Seed);
}

#[rstest::rstest]
#[tokio::test]
async fn a_vals_failure_leaves_a_healthy_eci_load_untouched() {
    // Given a fixture whose ECI zip is served by a live mock and whose
    // vals page points at nothing.
    let mut eci_server = mockito::Server::new_async().await;
    let eci_mock = eci_server
        .mock("GET", "/data/benchmark_data.zip")
        .with_status(200)
        .with_body(zip_bytes())
        .create_async()
        .await;
    let fixture = score_fixture_urls(
        format!("{}/data/benchmark_data.zip", eci_server.url()),
        "http://127.0.0.1:9/benchmarks/vals_index".to_owned(),
    )
    .await;
    let eci_loads = fixture.harness.spawn_recorder::<EciLoaded>().await;
    let failures = fixture.harness.spawn_recorder::<RefreshFailed>().await;

    // When a RefreshScores arrives.
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<EciLoaded>(&eci_loads, 1, Duration::from_secs(5)).await;
    let _ = await_recorded::<RefreshFailed>(&failures, 1, Duration::from_secs(5)).await;

    // Then ECI loaded from the download regardless of the vals outage.
    eci_mock.assert_async().await;
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, EciSource::Download);
    drop(eci_server);
}

#[rstest::rstest]
#[tokio::test]
async fn a_fresh_vals_cache_serves_without_a_request() {
    // Given a vals cache planted by a real store (data + stamp) and a
    // refresh whose page URL is dead.
    let fixture = score_fixture_at("http://127.0.0.1:9/data/benchmark_data.zip".to_owned()).await;
    let vals_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).vals;
    crate::score_cache::store_vals(&vals_dir, vals_payload().as_bytes(), fixture.clock.now())
        .expect("vals cache written");
    let page_mock_page = mockito::Server::new_async().await; // nothing served
    drop(page_mock_page);
    let vals_loads = fixture.harness.spawn_recorder::<ValsLoaded>().await;

    // When a RefreshScores arrives with the clock inside the TTL.
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<ValsLoaded>(&vals_loads, 1, Duration::from_secs(5)).await;

    // Then the cache served: source = Download, no outbound page fetch
    // (the refresh landed with the dead URL never consulted).
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].source, ValsSource::Download);
    assert_eq!(landed[0].matched, 2);
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_scores_force_refetches_vals_past_a_stale_cache() {
    // Given a week-stale vals cache and a live page mock.
    let (server, _page_mock, data_mock) =
        serve_vals("benchmark_view_vals_index.D4UVDIPY.json").await;
    let fixture = vals_fixture(&server).await;
    let vals_dir = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir()).vals;
    crate::score_cache::store_vals(&vals_dir, b"{}", fixture.clock.now()).expect("stale written");
    fixture.clock.advance(crate::score_cache::SCORE_TTL_MS + 1);
    let vals_loads = fixture.harness.spawn_recorder::<ValsLoaded>().await;

    // When a RefreshScores arrives (TTL expired — the download runs).
    fixture.harness.publish(RefreshScores).await;
    let landed = await_recorded::<ValsLoaded>(&vals_loads, 1, Duration::from_secs(5)).await;

    // Then the refetch hit the live data mock and re-landed the index.
    data_mock.assert_async().await;
    assert_eq!(landed.len(), 1);
    assert_eq!(landed[0].matched, 2);
}

#[rstest::rstest]
#[tokio::test]
async fn refresh_scores_force_drops_both_cache_dirs() {
    // Given both cache dirs planted with a real fetch and a live page
    // mock (the ECI zip stays dead — its drop still runs).
    let (server, _page, _data) = serve_vals("benchmark_view_vals_index.D4UVDIPY.json").await;
    let fixture = vals_fixture(&server).await;
    let dirs = crate::score_cache::ScoreCacheDirs::new(&fixture.paths.scores_cache_dir());
    crate::score_cache::store_vals(&dirs.vals, vals_payload().as_bytes(), fixture.clock.now())
        .expect("vals cache");
    crate::score_cache::download_and_extract(&dirs.eci, &zip_bytes(), fixture.clock.now())
        .expect("eci cache");
    assert!(dirs.vals.as_path().exists());
    assert!(dirs.eci.as_path().exists());

    // When a RefreshScoresForce arrives.
    let vals_loads = fixture.harness.spawn_recorder::<ValsLoaded>().await;
    fixture.harness.publish(RefreshScoresForce).await;
    let landed = await_recorded::<ValsLoaded>(&vals_loads, 1, Duration::from_secs(5)).await;

    // Then the vals dir was dropped and the live mock re-filled it
    // (json + stamp only). The ECI half's URL is dead in this fixture,
    // so its drop leaves nothing behind — the seed stands, the dir
    // stays absent, which is the failed-refresh shape the ECI tests
    // already pin.
    assert!(dirs.vals.as_path().exists(), "vals re-stored");
    assert!(
        !dirs.eci.as_path().exists(),
        "the dead ECI URL's drop is not rolled back by a vals success"
    );
    let vals_names: Vec<String> = std::fs::read_dir(dirs.vals.as_path())
        .expect("vals dir readable")
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(vals_names.len(), 2, "json + stamp only, got {vals_names:?}");
    assert_eq!(landed.len(), 1);
}

#[rstest::rstest]
#[tokio::test]
async fn store_vals_writes_the_json_and_the_stamp_verbatim() {
    // Given a temp scores root and a payload.
    let dir = jinn_testutil::tempdir();
    let dirs = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    );

    // When storing.
    crate::score_cache::store_vals(&dirs.vals, vals_payload().as_bytes(), 123).expect("store");

    // Then the bytes are verbatim and the stamp is written.
    let written = std::fs::read(dirs.vals.join("vals_index.json").expect("bare-name join"))
        .expect("file written");
    assert_eq!(written, vals_payload().as_bytes());
    let stamp = std::fs::read_to_string(dirs.vals.join("fetched_at").expect("bare-name join"))
        .expect("stamp written");
    assert_eq!(stamp, "123");
}

#[rstest::rstest]
#[test]
fn load_vals_fresh_serves_inside_the_ttl_and_none_past_it() {
    // Given a stored vals cache.
    let dir = jinn_testutil::tempdir();
    let dirs = crate::score_cache::ScoreCacheDirs::new(
        &jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir(),
    );
    crate::score_cache::store_vals(&dirs.vals, b"{}", 1_000).expect("store");

    // When reading just inside, then just past, the TTL.
    let fresh = crate::score_cache::load_vals_fresh(&dirs.vals, 1_000, || 1_999);
    let stale = crate::score_cache::load_vals_fresh(&dirs.vals, 1_000, || 2_001);

    // Then inside serves the bytes and stamp; past returns None.
    assert_eq!(fresh.expect("fresh serves").fetched_at_ms, 1_000);
    assert!(stale.is_none());
}

#[rstest::rstest]
#[test]
fn score_cache_dirs_split_under_one_root() {
    // Given a resolved scores dir.
    let dir = jinn_testutil::tempdir();
    let scores =
        jinn_common::app_paths::AppPaths::new_in(&dir.path.as_marked_path()).scores_cache_dir();

    // When splitting into per-source dirs.
    let dirs = crate::score_cache::ScoreCacheDirs::new(&scores);

    // Then each subdir sits under the scores root, named for its
    // source.
    assert!(
        dirs.eci.as_path().starts_with(scores.as_path()),
        "eci subdir under the scores root"
    );
    assert!(
        dirs.vals.as_path().starts_with(scores.as_path()),
        "vals subdir under the scores root"
    );
    assert!(dirs.eci.as_path().ends_with("eci"));
    assert!(dirs.vals.as_path().ends_with("vals"));
}
