//! The model-market slice — an OpenRouter-only model picker with cost
//! projection and benchmark score joins (Epoch ECI, Vals Index).
//!
//! `<leader>so` opens a centered full-table popup (dynamic scope
//! [`jinn_model_market_msg::market_ui_scope`]): one row per text-output
//! OpenRouter listing, columns for prices, a render-time cost projection
//! under the fixed [`jinn_model_market_msg::PROJECTION_MIX`], and the
//! benchmark scores joined by name — the Epoch Capabilities Index and
//! the Vals Index overall accuracy. The popup is
//! two-state: ENTER drills into the highlighted model's *detail mode* —
//! its provider endpoints as table rows, Default (auto-route) first —
//! where ENTER on a row switches the session model through the same
//! `ProviderSwitch` crossing the provider picker publishes and pins the
//! chosen endpoint through the existing `SetEndpointDefault` persist
//! path; ESC or backspace returns to the table with its state intact.
//! In table mode, ENTER keeps its drill-in; only detail-mode ENTER
//! switches.
//!
//! Two cells, two owners. [`jinn_model_market_msg::MarketDataState`] is
//! written only by the slice's actors (fetch, parse, cache — nothing
//! blocking ever runs on the key thread);
//! [`jinn_model_market_msg::MarketUiState`] is written only by the sync
//! side (route actions, the input hook, the render pass). The opener
//! resets the UI cell and publishes `FetchModels`; the actor serves warm
//! cache or network.
//!
//! `lib.rs` declares every module of the slice; the territories that
//! fill them own whole files and never touch this one:
//!
//! - `routes` — the opener row, every in-popup key, the filter hook
//! - `view` — the overlay render pass (table, status line, footer)
//! - `ui_state` — pure helpers over the two cells
//! - `format` — display formatting shared by view and tests
//! - `seed` — the compiled-in epoch CSVs (T2)
//! - `eci`, `score_cache`, `score_actor` — score download/parse/cache
//!   (T2): the ECI zip pipeline and the vals JSON pipeline live in one
//!   actor with per-source error paths
//! - `vals` — the Vals Index fetch + tolerant parse (T2)
//! - `eci_aliases` — the compiled-in ECI name-join alias table (T2)
//! - `vals_aliases` — the compiled-in vals slug-join alias table (T2)
//! - `or_models`, `or_endpoints`, `market_cache`, `rows` — the
//!   OpenRouter fetch/cache/row-build layer (T3)
//! - `openrouter_actor` — the models/endpoints actor (T3)
//! - `help` — the `?` help card (T4)

pub mod format;
pub mod routes;
pub mod ui_state;
pub mod view;

// T2 — score data territory.
pub mod eci;
pub mod eci_aliases;
pub mod score_actor;
pub mod score_cache;
pub mod seed;
pub mod vals;
pub mod vals_aliases;

// T3 — OpenRouter data + caching territory.
pub mod market_cache;
pub mod openrouter_actor;
pub mod or_endpoints;
pub mod or_models;
pub mod rows;

// T4 — help card + attribution territory.
pub mod help;

use jinn_slices::SliceHost;

pub use jinn_model_market_msg::market_data_slot;
pub use jinn_model_market_msg::market_ui_scope;
pub use jinn_model_market_msg::market_ui_slot;

/// Activates the model-market slice: resolves both cells from the
/// catalog, registers the centered overlay geometry + view, and attaches
/// every route row and the filter's input hook.
///
/// Neither cell is minted here: the shared cell catalog
/// (`jinn_cell_catalog::register_all_cells`) registers every slice cell
/// in one place, so this resolves the handles the same way every other
/// consumer does.
///
/// # Panics
///
/// Panics if the catalog has not run — the overlay would otherwise
/// render against absent cells and paint nothing.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub fn activate(host: &mut SliceHost<'_, jinn_slices::RenderFacts>) {
    let data = host
        .slices()
        .reader::<jinn_model_market_msg::MarketDataState>(&market_data_slot())
        .expect("the cell catalog registers the model-market data slot before any slice activates");
    let ui = host
        .slices()
        .reader::<jinn_model_market_msg::MarketUiState>(&market_ui_slot())
        .expect("the cell catalog registers the model-market UI slot before any slice activates");

    host.register_overlay(
        market_ui_scope(),
        std::sync::Arc::new(view::market_overlay_rect),
    );
    host.register_overlay_selectable(&market_ui_scope());
    host.register_overlay_slot(market_ui_scope(), market_ui_slot());
    host.register_overlay_view(
        market_ui_scope(),
        std::sync::Arc::new(view::render_market_popup),
    );
    routes::attach_market_rows(host.key_routes(), &ui, &data);
    routes::register_market_input_hook(host.key_routes(), &ui, &data);
}

/// Spawns the slice's two data-plane actors: [`score_actor::
/// ScoreMarketActor`] and [`openrouter_actor::OpenRouterMarketActor`].
///
/// Called by composition, not by [`activate`]: the spawns need `Services`
/// (paths, api keys, the trouper system) and must run where those live —
/// the provider-selection precedent derives its deps from `Services`
/// exactly this way. Both actors' subscriptions are live when this
/// returns, so composition must call it before any user interaction can
/// open the popup; within the boot list the whole independents block sits
/// ahead of the interactive tail, which is the guarantee the popup's
/// keys ride. The tail also publishes `RefreshScores`: the score columns
/// have data before the popup can open (001 §1), warm cache within the
/// TTL serves with no request, and an expired one downloads — the lazy
/// weekly check is real for both sources.
///
/// # Panics
///
/// Panics if the catalog has not run (the actors would write into
/// absent cells) or an actor's path is already taken — both are
/// composition bugs that must abort launch, not continue degraded.
pub async fn activate_actors(services: &jinn_kernel::Services) {
    activate_actors_with(services, ActorEndpoints::production()).await;
}

/// The outbound endpoints the two market actors talk to.
///
/// Production resolves all to the real constants; tests override them
/// with dead URLs so a subscription claim never depends on the network
/// being down (or up).
#[derive(Clone, Debug)]
pub struct ActorEndpoints {
    /// The epoch zip URL.
    pub zip_url: String,
    /// The vals leaderboard page URL the data URL is derived from.
    pub vals_page_url: String,
    /// The OpenRouter API base URL.
    pub base_url: String,
}

impl ActorEndpoints {
    /// The production endpoints.
    #[must_use]
    pub fn production() -> Self {
        Self {
            zip_url: score_cache::ECI_ZIP_URL.to_owned(),
            vals_page_url: score_cache::VALS_PAGE_URL.to_owned(),
            base_url: String::new(),
        }
    }
}

/// [`activate_actors`] with explicit endpoints — the test seam. Ends
/// with the same boot `RefreshScores` kick the production entry point
/// publishes.
///
/// # Panics
///
/// Panics if the catalog has not run (the actors would write into
/// absent cells) or an actor's path is already taken — both are
/// composition bugs that must abort launch, not continue degraded.
#[expect(
    clippy::expect_used,
    reason = "bootstrap assertion: broken slice wiring must abort launch, not continue degraded"
)]
pub async fn activate_actors_with(services: &jinn_kernel::Services, endpoints: ActorEndpoints) {
    let actor_deps = jinn_kernel::common::actor_deps::ActorDeps {
        services: services.clone(),
    };
    let data_cell = services
        .slices
        .reader::<jinn_model_market_msg::MarketDataState>(&market_data_slot())
        .expect("the cell catalog registers the model-market data slot before any actor spawns");
    let ui_cell = services
        .slices
        .reader::<jinn_model_market_msg::MarketUiState>(&market_ui_slot())
        .expect("the cell catalog registers the model-market UI slot before any actor spawns");

    // Scores first: the score actor's `EciLoaded` event is what the
    // OpenRouter actor consumes to re-join rows, so the event finds a
    // subscriber whether or not this load lands before the sibling
    // spawns (bus publishes are unordered; the OR actor handles the
    // event arriving early or late either way).
    let _scores = score_actor::ScoreMarketActor::spawn(
        &services.trouper_system,
        score_actor::ScoreMarketActorDeps {
            deps: actor_deps.clone(),
            data_cell: data_cell.clone(),
            cache_dirs: score_cache::ScoreCacheDirs::new(&services.paths.scores_cache_dir()),
            client: reqwest::Client::new(),
            zip_url: endpoints.zip_url,
            vals_page_url: endpoints.vals_page_url,
            now_ms: score_actor::score_market_clock(),
        },
    );
    let _openrouter = openrouter_actor::OpenRouterMarketActor::spawn(
        &services.trouper_system,
        openrouter_actor::OpenRouterMarketActorDeps {
            deps: actor_deps,
            data: data_cell,
            ui: ui_cell,
            base_url: endpoints.base_url,
            cache_dir: None,
            opener: std::sync::Arc::new(openrouter_actor::DefaultPageOpener),
        },
    );
    // The boot kick. Published after both spawns return — the score
    // subscription is live by then, so the load cannot drop.
    services
        .bus
        .publish(jinn_model_market_msg::RefreshScores)
        .await;
}
