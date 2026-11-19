//! The OpenRouter market actor — the models/endpoints half of the
//! slice's data plane.
//!
//! Subscribes to `FetchModels`, `FetchEndpoints`, `RefreshAll`,
//! `OpenModelPage`, `EciLoaded`, and `ValsLoaded`; writes
//! [`MarketDataState`] (the data cell — this actor is its only OR-side
//! writer) and emits `ModelsLoaded`, `EndpointsLoaded`, `RefreshFailed`,
//! `MarketModelsRefreshed`, and `PushChatEntry` (a failed `O` page
//! open). `RefreshScores` is the score actor's;
//! `EciLoaded`/`ValsLoaded` are *consumed* here: rows join against both
//! score indexes at build time, so when either lands after the models
//! (the common cold-open race), the handler rebuilds the rows from the
//! retained raw listings without a refetch. `RefreshAll` drops the OR
//! caches and refetches models plus the endpoints currently on display;
//! the refresh's score half is driven by the `MarketModelsRefreshed`
//! event this actor emits, which the score actor subscribes to.
//!
//! # Delivery story
//!
//! `RefreshAll` has exactly one handler — this actor. A schema declared
//! by two actors is routed to one of them round-robin, so a shared
//! command would silently lose half of every press. The score half
//! therefore rides the chain fact: this actor emits
//! `MarketModelsRefreshed` once its models refresh is done, and the
//! score actor acts on that fact.
//!
//! # Cache shape
//!
//! Two levels, per the contract: an in-memory map for the process
//! lifetime *and* disk files under `AppPaths::openrouter_cache_dir()`
//! so the cache survives restarts. The disk layer
//! ([`crate::market_cache`]) is clock-injected; each handler reads
//! `now` once and threads it through. The actor retains the *raw*
//! parsed listings alongside the joined rows — the raw payload is what
//! a score re-join needs, and joined rows cannot be un-joined.
//!
//! # Lock discipline
//!
//! Snapshot under the read lock → release → fetch → write the cell.
//! Never hold a cell guard across an await: the cells are
//! `parking_lot` locks on the render thread.
//!
//! # Debounce
//!
//! `FetchEndpoints` carries the UI cell's `fetch_seq` at publish time
//! (the action that mints it bumps it synchronously). The handler sleeps
//! [`ENDPOINT_DEBOUNCE`], then re-reads the UI cell's sequence and
//! drops the fetch when it advanced — a newer fetch owns the detail
//! pane now. The comparison works because the sequence lives in
//! the shared cell, not in the actor: a newer publish lands in the
//! cell while this handler sleeps, even though the actor processes
//! messages sequentially.
//!
//! The actor never fetches detail on its own initiative: `FetchEndpoints`
//! arrives only from the table-mode drill-in ENTER (sync side) or the
//! `RefreshAll` forced refetch, so a models load alone makes no
//! endpoints request.

use marked_path::{Absolute, MarkedPathBuf};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;

use jinn_applets::browser::open_in_browser;
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_model_market_msg::EciIndex;
use jinn_model_market_msg::EciLoaded;
use jinn_model_market_msg::EndpointsLoaded;
use jinn_model_market_msg::FetchEndpoints;
use jinn_model_market_msg::FetchModels;
use jinn_model_market_msg::MarketDataState;
use jinn_model_market_msg::MarketDetail;
use jinn_model_market_msg::MarketEndpointRow;
use jinn_model_market_msg::MarketModelsRefreshed;
use jinn_model_market_msg::MarketUiState;
use jinn_model_market_msg::ModelsLoaded;
use jinn_model_market_msg::OpenModelPage;
use jinn_model_market_msg::RefreshAll;
use jinn_model_market_msg::RefreshFailed;
use jinn_model_market_msg::ValsIndex;
use jinn_model_market_msg::ValsLoaded;
use jinn_session_history_msg::PushChatEntry;
use jinn_slices::TypedCell;
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use crate::market_cache;
use crate::or_endpoints::MarketEndpointInfo;
use crate::or_models::RawModel;
use crate::rows;

/// How long an endpoint fetch waits before comparing sequences.
///
/// Rapid `j`/`k` navigation mints one `FetchEndpoints` per row crossed;
/// only the last one should hit the network.
pub const ENDPOINT_DEBOUNCE: Duration = Duration::from_millis(250);

/// Environment variable the boot's env-init actor pre-populates the
/// API-key store from — during the startup tail's env ask, *after*
/// every actor has spawned. Readers must therefore resolve the key
/// from the store at request time, not at spawn. `/models` works
/// without it; the endpoint percentiles are auth-gated upstream
/// (research §2.4).
pub const OPENROUTER_API_KEY_ENV: &str = "OPENROUTER_API_KEY";

/// Cache-file names under `AppPaths::openrouter_cache_dir()`. The
/// provider-discovery `model_cache.json` is a different cache — these
/// names must never collide with it.
const MODELS_CACHE_FILE: &str = "models.json";
const ENDPOINT_CACHE_PREFIX: &str = "endpoints-";

/// Why a model-page open could not run.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct PageOpenError;

/// The browser-launch seam for `O`: production wraps the applets slice's
/// `open_in_browser` (the OS default browser); tests inject a scripted
/// fake. Colocated with the actor that consumes it, after the
/// no-standalone-traits-file convention.
pub trait PageOpener: Send + Sync {
    /// Opens `url` — or `Err` carrying the debug chain for the chat
    /// note when the opener is missing or the spawn fails.
    ///
    /// # Errors
    ///
    /// Returns [`PageOpenError`] when the page could not be opened.
    fn open(&self, url: &str) -> Result<(), Report<PageOpenError>>;

    /// Returns the name of this backend for debugging.
    fn name(&self) -> &'static str;
}

/// The production opener: the OS default browser via the applets
/// slice's launcher (`xdg-open` / `open` / `cmd /c start`, detached
/// stdio so the browser outlives jinn and never paints the TUI).
pub struct DefaultPageOpener;

impl PageOpener for DefaultPageOpener {
    fn open(&self, url: &str) -> Result<(), Report<PageOpenError>> {
        // The local error keeps the seam self-contained — the fake
        // construction never depends on the applets error type.
        open_in_browser(url)
            .map_err(|report| Report::new(PageOpenError).attach(format!("{report:?}")))
    }

    fn name(&self) -> &'static str {
        "os-default"
    }
}

/// An opener that succeeds silently — the default the cache-contract
/// tests want: the caching tests publish fetch commands, never page
/// opens, and a page open that succeeded must publish nothing, so an
/// accidental `O`-handler publish shows up as an unexpected recorder
/// entry rather than a lost message.
pub struct SilentPageOpener;

impl PageOpener for SilentPageOpener {
    fn open(&self, _url: &str) -> Result<(), Report<PageOpenError>> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "silent"
    }
}

/// The openrouter.ai page for a listing id: the id minus its
/// `openrouter/` prefix, appended to the site root. The remainder *is*
/// OpenRouter's page path verbatim, variant suffix (`:free`,
/// `:thinking`) included; a prefix-miss falls back to the raw id —
/// defensive only, `MarketRow.id` is prefixed by construction.
#[must_use]
pub fn model_page_url(model_id: &str) -> String {
    let rest = model_id.strip_prefix("openrouter/").unwrap_or(model_id);
    format!("https://openrouter.ai/{rest}")
}

/// The OpenRouter market actor.
pub struct OpenRouterMarketActor {
    /// Runtime services (bus, paths, api keys).
    deps: ActorDeps,
    /// The data cell — the actor's write surface.
    data: TypedCell<MarketDataState>,
    /// The UI cell — read-only here, for the debounce's sequence check.
    ui: TypedCell<MarketUiState>,
    /// The OpenRouter base URL (tests point this at mockito).
    base_url: String,
    /// The market cache directory (tests point this at a tempdir).
    cache_dir: MarkedPathBuf<Absolute>,
    /// The shared HTTP client.
    client: reqwest::Client,
    /// The browser-launch seam `O` rides — the OS default browser in
    /// production, a scripted fake in tests.
    opener: Arc<dyn PageOpener>,
    /// In-memory *raw* parsed listings with their fetch stamp — the
    /// re-join payload the score-loaded handlers need. `None` until the
    /// first successful models fetch.
    raw_models: Option<(Vec<RawModel>, jiff::Timestamp)>,
    /// In-memory joined models rows with their fetch stamp.
    models: Option<(Vec<jinn_model_market_msg::MarketRow>, jiff::Timestamp)>,
    /// In-memory per-model endpoint detail with fetch stamps.
    endpoints: HashMap<String, (Vec<MarketEndpointRow>, jiff::Timestamp)>,
}

/// Dependencies for [`OpenRouterMarketActor`].
#[derive(Clone)]
pub struct OpenRouterMarketActorDeps {
    /// Runtime services.
    pub deps: ActorDeps,
    /// The data cell handle (the actor writes it).
    pub data: TypedCell<MarketDataState>,
    /// The UI cell handle (read for the debounce sequence).
    pub ui: TypedCell<MarketUiState>,
    /// OpenRouter base URL; empty resolves to the production default.
    pub base_url: String,
    /// Cache directory override; `None` resolves to
    /// `AppPaths::openrouter_cache_dir()`.
    pub cache_dir: Option<MarkedPathBuf<Absolute>>,
    /// The browser-launch seam `O` rides; production wraps the applets
    /// launcher.
    pub opener: Arc<dyn PageOpener>,
}

impl OpenRouterMarketActorDeps {
    /// Production deps: base URL and cache dir resolve from services.
    #[must_use]
    pub fn production(
        deps: ActorDeps,
        data: TypedCell<MarketDataState>,
        ui: TypedCell<MarketUiState>,
    ) -> Self {
        Self {
            deps,
            data,
            ui,
            base_url: String::new(),
            cache_dir: None,
            opener: Arc::new(DefaultPageOpener),
        }
    }
}

/// Static path the actor spawns at (one instance per process).
pub const OPENROUTER_MARKET_ACTOR_PATH: &str = "jinn.model_market.openrouter_actor";

impl ServiceActor for OpenRouterMarketActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps
        // can't ride the JSON args). Err is the hard invariant every
        // actor's `start` carries.
        Err(Report::new(RegistryError::InvalidSpec)
            .attach("OpenRouterMarketActor spawns via init_with"))
    }
}

impl OpenRouterMarketActor {
    /// Spawns the actor onto the trouper system; subscriptions are live
    /// when this returns.
    ///
    /// `.handles` declares every consumed message and `.emits` every
    /// published one — both load-bearing: an undeclared emit is dropped
    /// silently at runtime.
    ///
    /// # Panics
    ///
    /// Panics if the actor's path is already taken — a composition bug.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    pub fn spawn(
        system: &trouper::system::ActorSystem,
        deps: OpenRouterMarketActorDeps,
    ) -> ActorPath {
        let path = ActorPath::new(OPENROUTER_MARKET_ACTOR_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move { Ok(Self::build(deps)) })
                }
            })
            .handles::<FetchModels>()
            .handles::<FetchEndpoints>()
            .handles::<RefreshAll>()
            .handles::<OpenModelPage>()
            .handles::<EciLoaded>()
            .handles::<ValsLoaded>()
            .emits::<ModelsLoaded>()
            .emits::<EndpointsLoaded>()
            .emits::<RefreshFailed>()
            .emits::<MarketModelsRefreshed>()
            .emits::<PushChatEntry>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }

    /// Assembles the actor from its deps: resolves the base URL and
    /// cache dir. The API key is *not* resolved here — the store fills
    /// during the startup tail's env ask, after every actor has
    /// spawned, so fetch handlers read it at request time.
    #[must_use]
    fn build(deps: OpenRouterMarketActorDeps) -> Self {
        let base_url = if deps.base_url.is_empty() {
            jinn_provider::ProviderConfig::openrouter()
                .default_base_url
                .to_owned()
        } else {
            deps.base_url
        };
        let cache_dir = deps
            .cache_dir
            .clone()
            .unwrap_or_else(|| deps.deps.services.paths.openrouter_cache_dir());
        Self {
            deps: deps.deps,
            data: deps.data,
            ui: deps.ui,
            base_url,
            cache_dir,
            client: reqwest::Client::new(),
            opener: deps.opener,
            raw_models: None,
            models: None,
            endpoints: HashMap::new(),
        }
    }
}

impl MsgHandler<FetchModels> for OpenRouterMarketActor {
    async fn handle(&mut self, _msg: &FetchModels, _ctx: &mut MsgCtx<'_>) {
        self.handle_fetch_models().await;
    }
}

impl MsgHandler<FetchEndpoints> for OpenRouterMarketActor {
    async fn handle(&mut self, msg: &FetchEndpoints, _ctx: &mut MsgCtx<'_>) {
        self.handle_fetch_endpoints(msg).await;
    }
}

impl MsgHandler<RefreshAll> for OpenRouterMarketActor {
    async fn handle(&mut self, _msg: &RefreshAll, _ctx: &mut MsgCtx<'_>) {
        self.handle_refresh_all().await;
    }
}

impl MsgHandler<OpenModelPage> for OpenRouterMarketActor {
    async fn handle(&mut self, msg: &OpenModelPage, _ctx: &mut MsgCtx<'_>) {
        self.handle_open_model_page(msg).await;
    }
}

impl MsgHandler<EciLoaded> for OpenRouterMarketActor {
    async fn handle(&mut self, _msg: &EciLoaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_scores_loaded();
    }
}

impl MsgHandler<ValsLoaded> for OpenRouterMarketActor {
    async fn handle(&mut self, _msg: &ValsLoaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_scores_loaded();
    }
}

impl BusPublish for OpenRouterMarketActor {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl OpenRouterMarketActor {
    // The cache-path helpers join bare filenames onto `cache_dir`, which is
    // typed `Absolute` at spawn; a bare-name join cannot make the result
    // relative, so the expects guard programmer error, not runtime state.
    #![allow(
        clippy::expect_used,
        reason = "bare-name joins onto a typed-absolute cache dir are infallible by construction"
    )]

    /// FetchModels: serve warm memory, then warm disk, then the network.
    ///
    /// A warm cache at either level produces no outbound request and
    /// emits `ModelsLoaded { from_cache: true }`; a miss fetches, stores
    /// at both levels, and emits `from_cache: false`. A fetch failure
    /// keeps stale rows standing and emits `RefreshFailed`.
    async fn handle_fetch_models(&mut self) {
        let now = jiff::Timestamp::now();

        // Memory first.
        if let Some((rows, at)) = &self.models {
            let (rows, at) = (rows.clone(), *at);
            self.write_models(&rows, at);
            self.publish(ModelsLoaded {
                count: row_count(&rows),
                from_cache: true,
            })
            .await;
            return;
        }

        // Disk second.
        let disk_path = self.models_cache_path();
        if let Some(rows) = market_cache::load::<Vec<jinn_model_market_msg::MarketRow>>(
            &disk_path,
            market_cache::MODELS_TTL,
            now,
        ) {
            let at = market_cache::loaded_at(&disk_path).unwrap_or(now);
            self.models = Some((rows.clone(), at));
            self.write_models(&rows, at);
            self.publish(ModelsLoaded {
                count: row_count(&rows),
                from_cache: true,
            })
            .await;
            return;
        }

        // Network last. The loading flag flips inside the cell; the
        // guard never spans the await. The key resolves here, at
        // request time: the store fills during the startup tail, so a
        // spawn-time snapshot would freeze `None` forever.
        self.data.update(|d| d.models_loading = true);
        let api_key = self.deps.services.api_keys.get(OPENROUTER_API_KEY_ENV);
        match crate::or_models::fetch_models(&self.client, &self.base_url, api_key.as_deref()).await
        {
            Ok(raw) => {
                let (eci, vals) = self.score_snapshots();
                let rows = rows::build_rows(&raw, eci.as_deref(), vals.as_deref());
                let at = jiff::Timestamp::now();
                self.raw_models = Some((raw, at));
                self.models = Some((rows.clone(), at));
                if let Err(report) =
                    market_cache::store(&disk_path, &market_cache::stamped(rows.clone(), at))
                {
                    tracing::warn!(?report, "failed to write the models disk cache");
                }
                self.write_models(&rows, at);
                self.publish(ModelsLoaded {
                    count: row_count(&rows),
                    from_cache: false,
                })
                .await;
            }
            Err(report) => {
                tracing::warn!(?report, "models fetch failed; stale rows stand");
                self.data.update(|d| {
                    d.models_loading = false;
                    d.last_error = Some("models fetch failed".to_owned());
                });
                self.publish(RefreshFailed {
                    what: "models".to_owned(),
                    message: format!("{report:?}"),
                })
                .await;
            }
        }
    }

    /// FetchEndpoints: debounce, drop stale sequences, serve caches,
    /// fetch, write the detail.
    async fn handle_fetch_endpoints(&mut self, msg: &FetchEndpoints) {
        // The debounce: yield the handler so newer publishes land in the
        // UI cell, then compare. No cell guard is held across the sleep
        // or the fetch below.
        tokio::time::sleep(ENDPOINT_DEBOUNCE).await;
        let newest = self.ui.read().fetch_seq;
        if msg.seq < newest {
            return; // a newer navigation owns the pane; drop silently
        }
        self.fetch_endpoints_for(&msg.model_id).await;
    }

    /// The endpoint-detail fetch core, shared by the debounced command
    /// and `RefreshAll`'s forced refetch: memory → disk → network.
    async fn fetch_endpoints_for(&mut self, model_id: &str) {
        // Memory cache.
        if let Some((detail_rows, at)) = self.endpoints.get(model_id) {
            let (detail_rows, at) = (detail_rows.clone(), *at);
            self.write_detail(model_id, detail_rows, at);
            self.publish(EndpointsLoaded {
                model_id: model_id.to_owned(),
            })
            .await;
            return;
        }

        // Disk cache.
        let disk_path = self.endpoints_cache_path(model_id);
        let now = jiff::Timestamp::now();
        if let Some(detail_rows) = market_cache::load::<Vec<MarketEndpointRow>>(
            &disk_path,
            market_cache::ENDPOINTS_TTL,
            now,
        ) {
            let at = market_cache::loaded_at(&disk_path).unwrap_or(now);
            self.endpoints
                .insert(model_id.to_owned(), (detail_rows.clone(), at));
            self.write_detail(model_id, detail_rows, at);
            self.publish(EndpointsLoaded {
                model_id: model_id.to_owned(),
            })
            .await;
            return;
        }

        // Network: mark in flight, then fetch. No cell guard spans
        // the await. The key resolves at request time for the same
        // reason as the models fetch above.
        self.data.update(|d| d.endpoints_loading = true);
        let listing_id = openrouter_listing_id(model_id);
        let api_key = self.deps.services.api_keys.get(OPENROUTER_API_KEY_ENV);
        match crate::or_endpoints::fetch_endpoints(
            &self.client,
            &self.base_url,
            listing_id,
            api_key.as_deref(),
        )
        .await
        {
            Ok(endpoints) => {
                let detail_rows: Vec<MarketEndpointRow> =
                    endpoints.iter().map(endpoint_row).collect();
                let at = jiff::Timestamp::now();
                self.endpoints
                    .insert(model_id.to_owned(), (detail_rows.clone(), at));
                if let Err(report) =
                    market_cache::store(&disk_path, &market_cache::stamped(detail_rows.clone(), at))
                {
                    tracing::warn!(?report, "failed to write the endpoints disk cache");
                }
                self.write_detail(model_id, detail_rows, at);
                self.publish(EndpointsLoaded {
                    model_id: model_id.to_owned(),
                })
                .await;
            }
            Err(report) => {
                tracing::warn!(?report, "endpoint fetch failed; stale detail stands");
                self.data.update(|d| {
                    d.endpoints_loading = false;
                    d.last_error = Some("endpoint fetch failed".to_owned());
                });
                self.publish(RefreshFailed {
                    what: "endpoints".to_owned(),
                    message: format!("{report:?}"),
                })
                .await;
            }
        }
    }

    /// OpenModelPage: build the openrouter.ai URL and spawn the OS
    /// opener. A successful launch reports nothing; a failed one is
    /// user-facing feedback, not a log line — a chat error entry rides
    /// `PushChatEntry` into the session the popup opened over (the
    /// same vehicle the applets picker's failed open uses).
    async fn handle_open_model_page(&mut self, msg: &OpenModelPage) {
        let url = model_page_url(&msg.model_id);
        if let Err(report) = self.opener.open(&url) {
            tracing::warn!(?report, "model page open failed");
            let note = format!("could not open {url} in a browser: {report:?}");
            self.publish(PushChatEntry {
                session_id: msg.session_id.clone(),
                entry: jinn_core_types::ChatEntry::error(note),
                pin: None,
            })
            .await;
        }
    }

    /// RefreshAll: drop the OR caches — both memory maps and every disk
    /// file (models, plus *all* `endpoints-*` detail files, not just the
    /// displayed one) — then refetch models and the endpoints currently
    /// on display.
    ///
    /// Refresh means refresh: the drops happen first and are not rolled
    /// back if a refetch fails; each failure emits `RefreshFailed` and
    /// the next step still runs. The refresh's score half is not this
    /// handler's duty: once the models half is done, the chain fact
    /// `MarketModelsRefreshed` goes out and the score actor refreshes
    /// both score indexes in reaction.
    async fn handle_refresh_all(&mut self) {
        self.raw_models = None;
        self.models = None;
        self.endpoints.clear();
        market_cache::drop_file(&self.models_cache_path());
        drop_all_endpoint_files(&self.cache_dir);

        // The stale detail id is the one the pane was showing; the
        // refetch is forced because its cache levels are gone.
        let stale = {
            let read = self.data.read();
            read.detail.as_ref().map(|d| d.model_id.clone())
        };

        self.handle_fetch_models().await;
        if let Some(id) = &stale {
            self.fetch_endpoints_for(id).await;
        }

        // The models half is done — the fact goes out even when the
        // models fetch failed: the score half's refresh does not depend
        // on it, and the popup's score columns should still be fresh.
        self.publish(MarketModelsRefreshed).await;
    }

    /// `EciLoaded`/`ValsLoaded`: a score index changed under the rows —
    /// rebuild them from the retained raw listings against the fresh
    /// indexes.
    ///
    /// This is the cold-open race fix: models frequently win the race
    /// against the score downloads, and a row's score fields are
    /// decided at build time. No refetch, no emission — the rows simply
    /// become correct on the next read of the data cell. Without raw
    /// listings (nothing fetched yet) this is a no-op; the eventual
    /// `FetchModels` will join against the then-present indexes.
    fn handle_scores_loaded(&mut self) {
        let Some((raw, at)) = self.raw_models.clone() else {
            return;
        };
        let (eci, vals) = self.score_snapshots();
        let rows = rows::build_rows(&raw, eci.as_deref(), vals.as_deref());
        self.models = Some((rows, at));
        if let Some((rows, at)) = &self.models {
            self.write_models(rows, *at);
        }
    }

    /// Writes a models load into the data cell. The rows arrive
    /// pre-joined with the score indexes current at build time.
    fn write_models(&self, rows: &[jinn_model_market_msg::MarketRow], at: jiff::Timestamp) {
        self.data.update(|d| {
            d.rows = rows.to_vec();
            d.models_fetched_at = Some(at);
            d.models_loading = false;
        });
    }

    /// Writes an endpoint-detail load into the data cell.
    fn write_detail(&self, model_id: &str, endpoints: Vec<MarketEndpointRow>, at: jiff::Timestamp) {
        self.data.update(|d| {
            d.detail = Some(MarketDetail {
                model_id: model_id.to_owned(),
                endpoints,
            });
            d.endpoints_fetched_at = Some(at);
            d.endpoints_loading = false;
        });
    }

    /// Snapshots both score indexes off the data cell.
    ///
    /// The cell read is taken and dropped before any await; the `Arc`
    /// clones are cheap by construction.
    fn score_snapshots(&self) -> (Option<Arc<EciIndex>>, Option<Arc<ValsIndex>>) {
        let read = self.data.read();
        (read.eci.clone(), read.vals.clone())
    }

    /// The models listing's disk path.
    fn models_cache_path(&self) -> MarkedPathBuf<Absolute> {
        self.cache_dir
            .join(MODELS_CACHE_FILE)
            .expect("absolute parent joins a bare filename")
    }

    /// One model's endpoint-detail disk path.
    ///
    /// The id is a file-name component: separators are flattened so a
    /// slash- or colon-bearing id cannot escape the cache dir.
    fn endpoints_cache_path(&self, model_id: &str) -> MarkedPathBuf<Absolute> {
        let flat: String = model_id
            .chars()
            .map(|c| if c == '/' || c == ':' { '_' } else { c })
            .collect();
        self.cache_dir
            .join(format!("{ENDPOINT_CACHE_PREFIX}{flat}.json"))
            .expect("absolute parent joins a bare filename")
    }
}

/// Drops every `endpoints-*` detail file in the cache directory.
///
/// `RefreshAll` means all endpoints, not just the displayed one; the
/// read_dir failure is logged and swallowed — a missing cache dir is a
/// cold cache, which is a fine starting state for a refresh.
fn drop_all_endpoint_files(cache_dir: &MarkedPathBuf<Absolute>) {
    let entries = match std::fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(?error, "no cache dir to sweep for endpoint files");
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(ENDPOINT_CACHE_PREFIX)
            && let Err(error) = std::fs::remove_file(entry.path())
        {
            tracing::warn!(?error, file = ?name, "failed to drop endpoint cache file");
        }
    }
}

/// The models event's count, clamped to the event's `u32` field.
fn row_count(rows: &[jinn_model_market_msg::MarketRow]) -> u32 {
    u32::try_from(rows.len()).unwrap_or(u32::MAX)
}

/// Converts the selection id (`openrouter/{author}/{slug}[:variant]`)
/// into the OpenRouter listing id the `/endpoints` route expects.
fn openrouter_listing_id(row_id: &str) -> &str {
    row_id.strip_prefix("openrouter/").unwrap_or(row_id)
}

/// Converts one fetched endpoint into the `-msg` detail row.
///
/// Uptime arrives as a percentage (`99.2`) and leaves as the fraction
/// (`0.992`) the `-msg` row documents; prices parse from their wire
/// strings, unparseable ones landing at zero rather than failing the
/// whole detail.
fn endpoint_row(ep: &MarketEndpointInfo) -> MarketEndpointRow {
    MarketEndpointRow {
        tag: ep.tag.clone(),
        provider_name: ep.provider_name.clone(),
        prompt_price: parse_or_zero(ep.prompt_price.as_deref()),
        completion_price: parse_or_zero(ep.completion_price.as_deref()),
        cache_read_price: ep
            .input_cache_read
            .as_deref()
            .and_then(|v| v.parse::<f64>().ok()),
        latency_p50_ms: ep.latency_p50_ms,
        throughput_p50: ep.throughput_p50,
        uptime_1d: ep.uptime_1d.map(|p| p / 100.0),
        quantization: ep.quantization.clone(),
        overrides: ep
            .overrides
            .iter()
            .map(|o| {
                (
                    o.min_prompt_tokens,
                    parse_or_zero(o.prompt.as_deref()),
                    parse_or_zero(o.completion.as_deref()),
                )
            })
            .collect(),
    }
}

/// Parses a wire price string, junk at zero.
fn parse_or_zero(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.parse::<f64>().ok()).unwrap_or_default()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "openrouter_actor_tests.rs"]
mod tests;
