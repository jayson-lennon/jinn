//! The score actor — loads, caches, and refreshes both benchmark score
//! sources off the key thread: the Epoch ECI zip and the Vals Index
//! JSON.
//!
//! Handles [`RefreshScores`], [`RefreshScoresForce`], and
//! [`MarketModelsRefreshed`]; writes only the [`MarketDataState`] cell
//! (its score fields: `eci`, `vals`, the per-source stamps/loading/
//! source flags, `last_error`); emits [`EciLoaded`], [`ValsLoaded`], and
//! [`RefreshFailed`].
//!
//! Selection order per refresh, per source: a fresh disk cache (within
//! [`SCORE_TTL_MS`], injectable clock) serves without a request; a
//! missing or stale cache triggers a download. On ECI total failure the
//! compiled seed stands in; on a vals failure there is no stand-in (no
//! vals snapshot ships in the repo) — the VALS column stays blank and
//! the status line reports it.
//!
//! # Failure isolation is per-source
//!
//! The two sources share one trigger body but never each other's error
//! paths: a vals fetch or parse failure reports `RefreshFailed {
//! what: "vals" }` and touches only the cell's vals fields — the ECI
//! column, the table rows, and detail mode are untouched. The converse
//! holds: an ECI failure never blanks vals.
//!
//! # Two triggers, one duty
//!
//! The forced refresh — drop both cache dirs, then force the download
//! path, refresh means refresh — is reached by two triggers: the
//! [`RefreshScoresForce`] command, and the [`MarketModelsRefreshed`]
//! event the OpenRouter market actor emits when the models half of
//! `<c-r>` is done. Both run the same body, so one press refreshes the
//! whole popup.
//!
//! A failed cache drop is reported as `RefreshFailed` without skipping
//! the download attempt.
//!
//! # Delivery story
//!
//! This actor is the only declarant of [`RefreshScoresForce`] and of
//! [`MarketModelsRefreshed`], and the only emitter of [`EciLoaded`] and
//! [`ValsLoaded`]. One declarant per schema means the route table never
//! engages its round-robin, so no refresh is silently lost to a second
//! handler.

use jinn_kernel::common::actor_deps::ActorDeps;
use jinn_kernel::common::actor_deps::BusPublish;
use jinn_model_market_msg::EciLoaded;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketModelsRefreshed;
use jinn_model_market_msg::RefreshFailed;
use jinn_model_market_msg::RefreshScores;
use jinn_model_market_msg::RefreshScoresForce;
use jinn_model_market_msg::ValsLoaded;
use jinn_slices::cell::TypedCell;
use trouper::actor::ActorPath;
use trouper::actor::MsgHandler;
use trouper::actor::ServiceActor;
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;
use trouper::system::ActorSystem;

use crate::score_cache::ScoreCacheDirs;

/// The score actor's static trouper path.
pub const SCORE_ACTOR_PATH: &str = "model-market-scores";

/// The cache TTL, re-exported beside the actor that enforces it.
pub use crate::score_cache::SCORE_TTL_MS;

/// Dependencies for [`ScoreMarketActor`].
#[derive(Clone)]
pub struct ScoreMarketActorDeps {
    /// Universal actor deps (services including the bus).
    pub deps: ActorDeps,
    /// The market data cell — this actor is its score-fields writer.
    pub data_cell: TypedCell<MarketDataState>,
    /// The resolved per-source cache dirs
    /// ([`jinn_common::app_paths::AppPaths::scores_cache_dir`] split
    /// into subdirectories), passed directly so the actor cannot
    /// re-derive a divergent path. Tests point them at a tempdir under
    /// the fixture root.
    pub cache_dirs: ScoreCacheDirs,
    /// The HTTP client for both downloads (tests point it at a mock).
    pub client: reqwest::Client,
    /// The epoch zip URL (tests point it at a mock server).
    pub zip_url: String,
    /// The vals leaderboard page URL the data URL is derived from
    /// (tests point it at a mock server).
    pub vals_page_url: String,
    /// Injectable clock, ms since the epoch. Production passes
    /// [`score_market_clock`]; tests pass a controlled closure.
    pub now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
}

/// The score actor: cache-or-download for both score sources.
pub struct ScoreMarketActor {
    /// Universal actor deps (the bus lives here).
    deps: ActorDeps,
    /// The market data cell — the only cell this actor writes.
    data_cell: TypedCell<MarketDataState>,
    /// Resolved per-source cache dirs
    /// ([`jinn_common::app_paths::AppPaths::scores_cache_dir`]). Set
    /// once at spawn from the deps.
    cache_dirs: ScoreCacheDirs,
    /// The HTTP client for both downloads.
    client: reqwest::Client,
    /// The epoch zip URL.
    zip_url: String,
    /// The vals leaderboard page URL.
    vals_page_url: String,
    /// Injectable clock.
    now_ms: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl ServiceActor for ScoreMarketActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "trait contract: init is never called (spawn uses init_with)"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, error_stack::Report<RegistryError>> {
        // Never called: the spawn helper injects the deps via `init_with`.
        Err(
            error_stack::IntoReport::into_report(RegistryError::InvalidSpec)
                .attach("ScoreMarketActor is spawned via init_with"),
        )
    }
}

impl BusPublish for ScoreMarketActor {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl ScoreMarketActor {
    /// Spawns the actor at its static path and returns the path.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(system: &ActorSystem, actor_deps: ScoreMarketActorDeps) -> ActorPath {
        let path = ActorPath::new(SCORE_ACTOR_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let actor_deps = actor_deps.clone();
                move || {
                    let actor_deps = actor_deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            deps: actor_deps.deps,
                            data_cell: actor_deps.data_cell,
                            cache_dirs: actor_deps.cache_dirs,
                            client: actor_deps.client,
                            zip_url: actor_deps.zip_url,
                            vals_page_url: actor_deps.vals_page_url,
                            now_ms: actor_deps.now_ms,
                        })
                    })
                }
            })
            // Every subscription and ALL event emissions are
            // load-bearing: a message the actor handles but does not
            // declare is never delivered; an event it sends but does not
            // declare is dead-lettered at flush and never routes. The
            // chain event is declared (subscribed), never emitted — the
            // models actor is its emitter.
            .handles::<RefreshScores>()
            .handles::<RefreshScoresForce>()
            .handles::<MarketModelsRefreshed>()
            .emits::<EciLoaded>()
            .emits::<ValsLoaded>()
            .emits::<RefreshFailed>()
            .start();
        path
    }
}

impl MsgHandler<RefreshScores> for ScoreMarketActor {
    async fn handle(&mut self, _msg: &RefreshScores, _ctx: &mut MsgCtx<'_>) {
        self.handle_refresh_scores().await;
    }
}

impl MsgHandler<RefreshScoresForce> for ScoreMarketActor {
    async fn handle(&mut self, _msg: &RefreshScoresForce, _ctx: &mut MsgCtx<'_>) {
        self.handle_force_refresh().await;
    }
}

impl MsgHandler<MarketModelsRefreshed> for ScoreMarketActor {
    async fn handle(&mut self, _msg: &MarketModelsRefreshed, _ctx: &mut MsgCtx<'_>) {
        self.handle_force_refresh().await;
    }
}

mod handlers;

#[cfg(test)]
#[path = "score_actor_tests.rs"]
mod score_actor_tests;

/// Parses the seed bytes and lands them in the cell — the ECI-first-run
/// fallback. The seed always parses (it is a compile-time fixture); the
/// `Err` arm reports a `RefreshFailed` naming the parse bug.
pub async fn load_seed(actor: &ScoreMarketActor) {
    match crate::eci::seed_data() {
        Ok(data) => {
            let now = (actor.now_ms)();
            actor
                .finish_eci_load(data, jinn_model_market_msg::EciSource::Seed, now)
                .await;
        }
        Err(report) => {
            actor
                .fail_eci_load(format!("shipped ECI seed failed to parse: {report}"))
                .await;
        }
    }
}

/// The production clock: real wall time, ms since the epoch.
#[must_use]
pub fn score_market_clock() -> std::sync::Arc<dyn Fn() -> u64 + Send + Sync> {
    std::sync::Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    })
}

/// The resolved per-source cache dir type, re-exported for the deps.
pub type CacheDirs = ScoreCacheDirs;
