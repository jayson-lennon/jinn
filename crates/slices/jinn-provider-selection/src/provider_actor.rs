//! Provider actor — applies model switches, merges model caches, and
//! loads picker entries.
//!
//! Subscribes to provider-related commands and events, writes the
//! provider cell (model cache, alloy mode, endpoint fetch state) and the
//! provider/endpoint picker fields (the picker render/navigation surface
//! on `FrontendState`), and emits events for other actors to react to.
//!
//! # State ownership
//!
//! This actor **owns** the provider cell
//! ([`ProviderCell`](jinn_provider_selection_msg::ProviderCell)) and is
//! the writer of the provider/endpoint picker `SelectionState`s on
//! `FrontendState` (the IntentHandler is the exempt sync writer for
//! navigation).
//!
//! # Lock discipline
//!
//! All handlers follow the same pattern: snapshot what is needed under
//! the state read lock → release → mutate the cell → then emit. Never
//! hold a lock during emission, and never hold the state guard across
//! the async endpoint fetch.

use error_stack::Report;
use jinn_core_types::model_selection::ModelSelection;
use jinn_kernel::common::actor_deps::{ActorDeps, BusPublish};
use jinn_kernel::common::state::State;
use jinn_provider_config::ModelCache;
use jinn_provider_config::ProviderRegistry;
use jinn_provider_config::{InputModalities, Modality, ModelInfo, ProvidersConfig};
use jinn_provider_selection_msg::endpoint::EndpointEntry;
use jinn_provider_selection_msg::{
    LoadEndpointPickerEntries, LoadProviderPickerEntries, ModelCacheLoaded, ModelCacheStored,
    ModelsRefreshed, ProviderCell, ProviderSwitch, ProviderSwitched, RefreshEndpointPickerEntries,
    SetEndpointDefault,
};
use trouper::actor::{ActorPath, MsgHandler, ServiceActor};
use trouper::context::MsgCtx;
use trouper::registry::RegistryError;

use crate::endpoint_loader::{
    build_endpoint_entries, fetch_endpoints, resolve_openrouter_target,
    unavailable_endpoint_entries,
};

/// The provider actor.
///
/// Subscribes to provider-related commands, mutates the provider cell and
/// the picker fields, and emits events via the bus.
pub struct ProviderActor {
    /// Shared application state.
    state: State,
    /// Runtime services (provider registry, API keys, LLM service factory).
    deps: ActorDeps,
    /// The provider cell — the shared model-cache + endpoint-fetch payload.
    provider_cell: jinn_slices::TypedCell<ProviderCell>,
    /// The endpoint picker's cell — where a completed fetch publishes its
    /// rows. The picker is slice-owned, so this is the only place the entry
    /// list lives; there is no kernel-side mirror to keep in step.
    endpoint_picker_cell:
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::endpoint::EndpointPickerState>,
    /// The model picker's cell — where each load publishes its rows. Slice-owned
    /// like the endpoint picker's, so there is no kernel-side mirror.
    provider_picker_cell:
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderPickerState>,
    /// In-memory, per-model cache of OpenRouter routing endpoints for the
    /// application's lifetime (not persisted to disk). Keyed by resolved model
    /// id; value is the parsed upstream list plus the fetch timestamp. The
    /// picker serves from this on open and re-fetches on-demand via `<c-r>`.
    endpoints_cache:
        std::collections::HashMap<String, (Vec<jinn_provider::EndpointInfo>, jiff::Timestamp)>,
}

/// Dependencies for [`ProviderActor`].
#[derive(Clone)]
pub struct ProviderActorDeps {
    /// Shared application state.
    pub state: State,
    /// Actor dependencies (services including bus).
    pub deps: ActorDeps,
    /// The provider cell handle.
    pub provider_cell: jinn_slices::TypedCell<ProviderCell>,
    /// The endpoint picker's cell handle.
    pub endpoint_picker_cell:
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::endpoint::EndpointPickerState>,
    /// The model picker's cell handle.
    pub provider_picker_cell:
        jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderPickerState>,
}

impl ServiceActor for ProviderActor {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "ServiceActor::start is async by trait contract"
    )]
    async fn init(_args: &trouper::json::Json) -> Result<Self, Report<RegistryError>> {
        // Never called: spawned via `spawn`'s init_with (typed deps can't
        // ride the JSON args).
        Err(Report::new(RegistryError::InvalidSpec).attach("ProviderActor spawns via init_with"))
    }
}

/// Builds the transient transcript message for a model refresh.
fn models_refresh_transcript(event: &ModelsRefreshed) -> String {
    if event.results.is_empty() && event.errors.is_empty() {
        return "Models refreshed: no providers found".to_owned();
    }

    let mut providers: Vec<&str> = event
        .results
        .keys()
        .chain(event.errors.keys())
        .map(String::as_str)
        .collect();
    providers.sort_unstable();
    providers.dedup();

    let rows = providers
        .into_iter()
        .map(|provider| {
            if let Some(models) = event.results.get(provider) {
                format!("| {provider} | {} | ✅ |", models.len())
            } else if let Some(error) = event.errors.get(provider) {
                format!("| {provider} | 0 | ❌ {error} |")
            } else {
                String::new()
            }
        })
        .collect::<Vec<_>>();

    let mut body = String::new();
    for row in rows {
        body.push_str(&row);
        body.push('\n');
    }
    format!("| Provider | Models | Status |\n|----------|--------|--------|\n{body}")
}

/// Static path the provider actor spawns at (one instance per process).
pub const PROVIDER_ACTOR_PATH: &str = "jinn.provider.actor";

impl ProviderActor {
    /// Spawns the provider actor onto the trouper system; subscriptions
    /// are live when this returns.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "port convention: spawn takes owned deps and clones into init_with"
    )]
    /// # Panics
    ///
    /// Panics if the actor's path is already taken or its topic
    /// subscription fails — both mean a wiring bug at composition.
    pub fn spawn(system: &trouper::system::ActorSystem, deps: ProviderActorDeps) -> ActorPath {
        let path = ActorPath::new(PROVIDER_ACTOR_PATH);
        trouper::builder::spawn_service_builder::<Self>(system)
            .at(path.clone())
            .init_with({
                let deps = deps.clone();
                move || {
                    let deps = deps.clone();
                    Box::pin(async move {
                        Ok(Self {
                            state: deps.state,
                            deps: deps.deps,
                            provider_cell: deps.provider_cell,
                            endpoint_picker_cell: deps.endpoint_picker_cell,
                            provider_picker_cell: deps.provider_picker_cell,
                            endpoints_cache: std::collections::HashMap::new(),
                        })
                    })
                }
            })
            .handles::<ProviderSwitch>()
            .handles::<LoadProviderPickerEntries>()
            .handles::<LoadEndpointPickerEntries>()
            .handles::<RefreshEndpointPickerEntries>()
            .handles::<ModelsRefreshed>()
            .handles::<ModelCacheLoaded>()
            .handles::<SetEndpointDefault>()
            .mailbox(64, trouper::inbox::OverloadPolicy::Block)
            .start();
        path
    }
}

impl MsgHandler<ProviderSwitch> for ProviderActor {
    async fn handle(&mut self, msg: &ProviderSwitch, _ctx: &mut MsgCtx<'_>) {
        self.handle_provider_switch(msg);
        self.publish(ProviderSwitched {
            session_id: msg.session_id.clone(),
            provider_name: msg.provider_id.to_string(),
        })
        .await;
    }
}

impl MsgHandler<LoadProviderPickerEntries> for ProviderActor {
    async fn handle(&mut self, _msg: &LoadProviderPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_provider_picker_entries();
    }
}

impl MsgHandler<LoadEndpointPickerEntries> for ProviderActor {
    async fn handle(&mut self, _msg: &LoadEndpointPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_endpoint_picker_entries(false).await;
    }
}

impl MsgHandler<RefreshEndpointPickerEntries> for ProviderActor {
    async fn handle(&mut self, _msg: &RefreshEndpointPickerEntries, _ctx: &mut MsgCtx<'_>) {
        self.handle_load_endpoint_picker_entries(true).await;
    }
}

impl MsgHandler<ModelsRefreshed> for ProviderActor {
    async fn handle(&mut self, msg: &ModelsRefreshed, _ctx: &mut MsgCtx<'_>) {
        self.handle_models_refreshed(msg).await;
    }
}

impl MsgHandler<ModelCacheLoaded> for ProviderActor {
    async fn handle(&mut self, msg: &ModelCacheLoaded, _ctx: &mut MsgCtx<'_>) {
        self.handle_model_cache_loaded(&msg.cache).await;
    }
}

impl MsgHandler<SetEndpointDefault> for ProviderActor {
    async fn handle(&mut self, msg: &SetEndpointDefault, _ctx: &mut MsgCtx<'_>) {
        self.handle_set_endpoint_default(msg);
    }
}

impl BusPublish for ProviderActor {
    fn bus(&self) -> &jinn_kernel::common::services::bus_service::BusService {
        &self.deps.services.bus
    }
}

impl ProviderActor {
    /// ProviderSwitch: update session profile and emit ProviderSwitched event.
    fn handle_provider_switch(&self, payload: &ProviderSwitch) {
        self.state.with_session(|view| {
            view.session
                .map()
                .get_or_create(&payload.session_id)
                .set_model(payload.provider_id.clone());
        });
    }

    /// LoadProviderPickerEntries: reload the provider picker entries from
    /// the current model cache + registry.
    fn handle_load_provider_picker_entries(&self) {
        let (model_cache, theme, model_selection, alloy_mode) = {
            let s = self.state.read();
            let theme = s.frontend.theme.clone();
            let model_selection = s.active_session().profile().model.clone();
            (None, theme, model_selection, self.alloy_mode())
        };
        let model_cache = model_cache.or_else(|| self.model_cache());
        // Into the picker's own cell, not the kernel's frontend state: the
        // menu is this slice's, so the actor that fetched the rows and the
        // renderer that draws them cannot end up on different stores.
        self.provider_picker_cell.update(|picker| {
            crate::loader::load_provider_picker_items(
                &self.deps.services,
                &mut picker.selection,
                model_cache.as_ref(),
                &theme,
                &model_selection,
                alloy_mode,
            );
        });
    }

    /// ModelsRefreshed: update model cache and reload provider picker entries.
    async fn handle_models_refreshed(&mut self, event: &ModelsRefreshed) {
        let now = jiff::Timestamp::now();
        let mut cache = ModelCache {
            entries: event.results.clone(),
            last_updated_at: Some(now),
        };
        {
            let registry = self.deps.services.provider_registry.read();
            merge_context_lengths_from_registry(&mut cache, &registry);
        }
        let models_dev = jinn_provider_config::ModelsDevData::load(
            &self.deps.services.paths.models_dev_user_path(),
        );
        merge_models_dev_data(&mut cache, &models_dev);
        // Config overrides apply last so explicit per-model values beat
        // models.dev enrichment (which unconditionally stamps the Image bit).
        {
            let registry = self.deps.services.provider_registry.read();
            apply_config_overrides(&mut cache, registry.config());
        }
        // Merge remote models into the registry so create_factory() can find them.
        self.deps.services.provider_registry.merge_cache(&cache);
        self.store_model_cache(cache);
        self.handle_load_provider_picker_entries();
        self.state.with_session(|view| {
            let session = view.session.map().get_or_create(&event.session_id);
            session.push_entry(jinn_core_types::ChatEntry::transient(
                models_refresh_transcript(event),
            ));
        });
        // The cache is now in the cell — the push-trigger event rides
        // strictly after the write, so a subscriber's re-read of the
        // cell sees the merged cache (the `/web2` push's ordering
        // dependency).
        self.publish_after_cache_write().await;
    }

    /// ModelCacheLoaded: restore model cache from disk and reload picker entries.
    async fn handle_model_cache_loaded(&mut self, cache: &ModelCache) {
        let mut cache = cache.clone();
        {
            let registry = self.deps.services.provider_registry.read();
            merge_context_lengths_from_registry(&mut cache, &registry);
        }
        let models_dev = jinn_provider_config::ModelsDevData::load(
            &self.deps.services.paths.models_dev_user_path(),
        );
        merge_models_dev_data(&mut cache, &models_dev);
        // Config overrides apply last so explicit per-model values beat
        // models.dev enrichment (which unconditionally stamps the Image bit).
        {
            let registry = self.deps.services.provider_registry.read();
            apply_config_overrides(&mut cache, registry.config());
        }
        // Merge remote models into the registry so create_factory() can find them.
        self.deps.services.provider_registry.merge_cache(&cache);
        self.store_model_cache(cache);
        self.handle_load_provider_picker_entries();
        // Same post-write ordering as the refresh path: `/web2`'s
        // push must not precede the cell write it answers.
        self.publish_after_cache_write().await;
    }

    /// Emits [`ModelCacheStored`] — the push trigger `/web2`'s outbound
    /// actor subscribes — through the shared bus. The bus publish path rides
    /// the system broadcast directly (the same path every other event
    /// this actor emits uses), so no outbox `.emits` declaration
    /// applies; ordering is guaranteed by call placement, strictly
    /// after `store_model_cache` in every caller. The event stamps the
    /// write's timestamp as provenance: a push must carry a body to
    /// arrive, and these are the bytes it carries.
    async fn publish_after_cache_write(&self) {
        self.publish(ModelCacheStored {
            stored_at: jiff::Timestamp::now(),
        })
        .await;
    }

    /// Writes the merged cache into the provider cell.
    fn store_model_cache(&self, cache: ModelCache) {
        self.provider_cell.update(|cell| {
            cell.model_cache = Some(cache);
        });
    }

    /// The current model cache, if the cell carries one.
    fn model_cache(&self) -> Option<ModelCache> {
        self.provider_cell.read().model_cache.clone()
    }

    /// The current alloy-selection mode.
    fn alloy_mode(&self) -> bool {
        self.provider_cell.read().is_alloy_mode()
    }

    /// Resolve the active model's backend and either serve OpenRouter routing
    /// endpoints from the in-memory cache or fetch them, then write the picker
    /// entries, fetch timestamp, and clear the loading flag.
    ///
    /// `force` is true for `<c-r>` refresh (always re-fetch) and false for picker
    /// open (serve from cache when present). The backend gate lives here (the
    /// actor owns `Services`); the picker-open validator only checks `Single`.
    ///
    /// The active row is marked from the model's `[[endpoint_defaults]]` row,
    /// so which row is active is known from the registry on the cache-hit path
    /// as well as the fetch path — no network call is needed to know it.
    ///
    /// Every terminal branch writes back all three: items, fetched_at, and
    /// loading=false — so the spinner never sticks on success, error, or the
    /// non-OpenRouter placeholder path.
    async fn handle_load_endpoint_picker_entries(&mut self, force: bool) {
        // Snapshot what we need under the read lock, then release it before
        // the async network fetch.
        let (model, pinned_tag, theme) = {
            let s = self.state.read();
            let model = s.active_session().profile().model.clone();
            let theme = s.frontend.theme.clone();
            let pinned_tag = self.pinned_tag_for(&model);
            (model, pinned_tag, theme)
        };

        let Some(target) = resolve_openrouter_target(&self.deps.services, &model) else {
            // Not served via OpenRouter (or an alloy): render the placeholder,
            // clear loading, and leave both the cache and fetched_at untouched
            // (this path never fetched anything).
            let entries = unavailable_endpoint_entries(theme, pinned_tag.as_deref());
            self.write_endpoint_items(entries);
            self.provider_cell.update(|cell| {
                cell.endpoint_loading = false;
            });
            return;
        };

        let key = target.model_id().to_owned();

        // Cache hit on a non-forced open: rebuild entries from the cached
        // upstream list (theme/pin re-derived), no network call.
        if !force && let Some((endpoints, ts)) = self.endpoints_cache.get(&key).cloned() {
            let entries = build_endpoint_entries(&endpoints, &theme, pinned_tag.as_deref());
            self.write_endpoint_items(entries);
            self.provider_cell.update(|cell| {
                cell.endpoint_fetched_at = Some(ts);
                cell.endpoint_loading = false;
            });
            return;
        }

        // Cache miss or forced refresh: fetch, store on success, build.
        let now = jiff::Timestamp::now();
        let (entries, fetched_at) = match fetch_endpoints(&target).await {
            Ok(endpoints) => {
                self.endpoints_cache.insert(key, (endpoints.clone(), now));
                (
                    build_endpoint_entries(&endpoints, &theme, pinned_tag.as_deref()),
                    Some(now),
                )
            }
            // On error: sentinel only, cache untouched, keep prior fetched_at.
            Err(()) => (
                vec![EndpointEntry::auto_route(pinned_tag.is_none(), theme)],
                None,
            ),
        };

        self.write_endpoint_items(entries);
        self.provider_cell.update(|cell| {
            // Only stamp fetched_at on a successful fetch; on error leave it.
            if let Some(at) = fetched_at {
                cell.endpoint_fetched_at = Some(at);
            }
            cell.endpoint_loading = false;
        });
    }

    /// The routing tag pinned for `model` in `providers.toml`, if any.
    ///
    /// `None` for an alloy, for a model with no row, and for the
    /// `no-provider` placeholder id — all three auto-route.
    fn pinned_tag_for(&self, model: &ModelSelection) -> Option<String> {
        let ModelSelection::Single(provider_id) = model else {
            return None;
        };
        self.deps
            .services
            .provider_registry
            .pinned_endpoint_tag(provider_id)
    }

    /// Wraps `entries` through the picker's own hooks and publishes them into
    /// its cell (the render/navigation surface).
    ///
    /// The rows are built by the picker's own actions rather than a kernel
    /// spec, so the cell is the single home: the actor that fetched them and
    /// the renderer that draws them cannot end up on different stores.
    fn write_endpoint_items(&self, entries: Vec<EndpointEntry>) {
        self.endpoint_picker_cell
            .update(|picker| crate::endpoint_picker_actions::reload(picker, entries));
    }

    /// SetEndpointDefault: persist the picker's choice as a per-model default.
    ///
    /// The write is one read-modify-write, in a fixed order: snapshot the
    /// config the registry holds → apply the row change → save through
    /// `ConfigStorage` → write the same change back into the registry. Disk
    /// before memory, so a save that fails leaves the run and the file
    /// agreeing on the old value rather than disagreeing.
    ///
    /// A `None` tag is the auto-route sentinel: the row is deleted instead of
    /// written, and the save is skipped entirely when the model has no row to
    /// begin with — there is nothing to persist and nothing to change.
    fn handle_set_endpoint_default(&self, msg: &SetEndpointDefault) {
        let mut config = self.deps.services.provider_registry.config_snapshot();

        if let Some(tag) = &msg.tag {
            set_endpoint_row(&mut config, msg.model.clone(), tag.clone());
        } else {
            let before = config.endpoint_defaults.len();
            config
                .endpoint_defaults
                .retain(|row| row.model != msg.model);
            if config.endpoint_defaults.len() == before {
                return;
            }
        }

        if let Err(report) = self.deps.services.config_storage.save(&config) {
            tracing::error!(
                model = %msg.model,
                tag = ?msg.tag,
                "{report:?}"
            );
            return;
        }

        match &msg.tag {
            Some(tag) => {
                self.deps
                    .services
                    .provider_registry
                    .set_endpoint_default(&msg.model, tag);
            }
            None => self
                .deps
                .services
                .provider_registry
                .clear_endpoint_default(&msg.model),
        }
    }
}

/// Sets the `[[endpoint_defaults]]` row for `model` to `tag`.
///
/// A field write in the same shape the registry uses, so a save patches only
/// this model's row and leaves every other row — and its comments — intact.
fn set_endpoint_row(config: &mut ProvidersConfig, model: String, tag: String) {
    match config
        .endpoint_defaults
        .iter_mut()
        .find(|row| row.model == model)
    {
        Some(row) => row.tag = tag,
        None => config
            .endpoint_defaults
            .push(jinn_provider_config::EndpointDefault { model, tag }),
    }
}

/// Merge `context_length` from the registry's resolved providers into the
/// model cache, overwriting API-discovered values.
///
/// Config precedence: `providers.toml` values (per-model `model_info`, then
/// block-level) beat API-discovered values, which in turn beat models.dev.
/// The registry's resolved providers already carry the config-side value, so
/// a `Some` here always wins; registry `None` leaves the cache value alone.
fn merge_context_lengths_from_registry(cache: &mut ModelCache, registry: &ProviderRegistry) {
    for provider in registry.providers() {
        let Some(registry_ctx) = provider.context_length else {
            continue;
        };
        let Some(models) = cache.entries.get_mut(&provider.name) else {
            continue;
        };
        for model in models.iter_mut() {
            if model.id == provider.model {
                model.context_length = Some(registry_ctx);
            }
        }
    }
}

/// Merge `context_length` and input modalities from the models.dev reference
// data into the model cache, filling in `None` slots where neither the API nor
// `providers.toml` provided a value.
//
// This is the lowest-priority merge: only `None` entries are filled,
// and existing values from the API or `providers.toml` are never overwritten.
//
// Modality stamping is unconditional (idempotent `insert`): a stale on-disk
// cache that predates the modalities field gets re-enriched from models.dev
// on every load, so the Image bit is never permanently lost across upgrades.
fn merge_models_dev_data(cache: &mut ModelCache, models_dev: &jinn_provider_config::ModelsDevData) {
    for models in cache.entries.values_mut() {
        for model in models.iter_mut() {
            models_dev.enrich(model);
        }
    }
}

/// Apply hand-authored per-model overrides from `providers.toml` onto the
/// model cache, and inject entries for static models that discovery never returned.
///
/// This is the highest-priority merge: explicit config values replace both
/// API-discovered and models.dev values. Config `context_length` fills `None`
/// slots (falling back to what discovery produced); config `input_modalities`
/// replace the discovered value outright when set.
///
/// Models that never appear in the cache get a new entry (so the status bar,
/// compaction gate, and attachment gate can resolve them); find-or-insert
/// semantics keep repeated applications idempotent.
fn apply_config_overrides(cache: &mut ModelCache, config: &ProvidersConfig) {
    for (name, entry) in &config.providers {
        for info in &entry.model_info {
            let block_ctx = info.context_length.or(entry.context_length);
            let models = cache.entries.entry(name.clone()).or_default();
            match models.iter_mut().find(|m| m.id == info.id) {
                Some(model) => {
                    if block_ctx.is_some() {
                        model.context_length = block_ctx;
                    }
                    if let Some(modalities) = parse_modalities(info.input_modalities.as_deref()) {
                        model.input_modalities = modalities;
                    }
                }
                None => {
                    models.push(ModelInfo {
                        id: info.id.clone(),
                        context_length: block_ctx,
                        input_modalities: parse_modalities(info.input_modalities.as_deref())
                            .unwrap_or_else(InputModalities::text),
                    });
                }
            }
        }

        // Static models with only a block-level context_length (no
        // `model_info` entry) still need a cache entry so the status bar and
        // compaction gate can resolve them when discovery never returned them.
        if entry.context_length.is_some() {
            inject_block_level_static_models(cache, name, entry);
        }
    }
}

/// Inserts cache entries for configured models that discovery never returned,
/// carrying the block-level `context_length`.
///
/// Text-only modalities are the conservative default; models.dev enrichment
/// (applied earlier in the pipeline) has already stamped the `Image` bit for
/// any model it knows.
fn inject_block_level_static_models(
    cache: &mut ModelCache,
    name: &str,
    entry: &jinn_provider_config::ProviderEntry,
) {
    let models = cache.entries.entry(name.to_owned()).or_default();
    for id in &entry.models {
        if !models.iter().any(|m| &m.id == id) {
            models.push(ModelInfo {
                id: id.clone(),
                context_length: entry.context_length,
                input_modalities: InputModalities::text(),
            });
        }
    }
}

/// Parses config modality strings ("text", "image") into `InputModalities`.
/// Unknown strings log a warning and are ignored; `None` (field unset)
/// returns `None` so the discovered value is kept.
fn parse_modalities(spec: Option<&[String]>) -> Option<InputModalities> {
    let spec = spec?;
    let mut out = InputModalities::default();
    for s in spec {
        match s.as_str() {
            "text" => out.insert(Modality::Text),
            "image" => out.insert(Modality::Image),
            other => tracing::warn!(
                modality = other,
                "unknown input modality in providers.toml model_info"
            ),
        }
    }
    Some(out)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use marked_path::MarkedPathAccess;

    use std::collections::BTreeMap;

    use jinn_kernel::AppState;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_kernel::common::state::State;
    use jinn_provider_config::{
        InputModalities, Modality, ModelCache, ModelInfo, ProviderEntry, ProviderRegistry,
        ProvidersConfig,
    };
    use jinn_provider_selection_msg::{ProviderCell, provider_state_slot};
    use jinn_testutil::bus_harness::{Recorder, TestHarness, await_recorded};

    use super::{
        ModelCacheLoaded, ModelsRefreshed, PROVIDER_ACTOR_PATH, ProviderActor, ProviderActorDeps,
    };
    use jinn_core_types::model_selection::ModelSelection;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_provider_selection_msg::LoadProviderPickerEntries;
    use jinn_provider_selection_msg::ProviderSwitched;
    use trouper::actor::ActorPath;

    /// The harness + state + deps pair, with the provider cell registered
    /// on the SAME `Services` the actor holds (harness `services()` mints a
    /// fresh registry per call, so the cell must be seeded once and the
    /// resulting `Services` threaded through).
    struct Ctx {
        harness: TestHarness,
        state: State,
        deps: ActorDeps,
    }

    impl Ctx {
        fn cell(&self) -> jinn_slices::TypedCell<ProviderCell> {
            self.deps
                .services
                .slices
                .reader(&provider_state_slot())
                .expect("provider cell registered")
        }

        /// The endpoint picker's cell, for the actor to publish fetches into.
        fn endpoint_picker_cell(
            &self,
        ) -> jinn_slices::cell::TypedCell<jinn_provider_selection_msg::endpoint::EndpointPickerState>
        {
            self.deps
                .services
                .slices
                .reader(&jinn_provider_selection_msg::endpoint::endpoint_picker_slot())
                .expect("endpoint picker cell registered")
        }

        /// The model picker's cell, the way `activate` mints it.
        fn provider_picker_cell(
            &self,
        ) -> jinn_slices::cell::TypedCell<jinn_provider_selection_msg::ProviderPickerState>
        {
            self.deps
                .services
                .slices
                .reader(&jinn_provider_selection_msg::provider_picker_slot())
                .expect("provider picker cell registered")
        }

        fn spawn_provider_actor(&self) {
            ProviderActor::spawn(
                &self.deps.services.trouper_system,
                ProviderActorDeps {
                    deps: self.deps.clone(),
                    state: self.state.clone(),
                    provider_cell: self.cell(),
                    endpoint_picker_cell: self.endpoint_picker_cell(),
                    provider_picker_cell: self.provider_picker_cell(),
                },
            );
        }

        fn cell_model_cache(&self) -> Option<ModelCache> {
            self.cell().read().model_cache.clone()
        }

        /// Waits until the model cache is set, then returns it.
        ///
        /// A fixed sleep would be a guess at how long the actor takes to
        /// process the event — a guess that is wrong in the direction that
        /// makes the test pass vacuously on a slow host. Waiting on the
        /// cell's own predicate is the signal that the work completed.
        async fn await_model_cache(&self) -> ModelCache {
            let cell = self.cell();
            jinn_testutil::wait::await_cell(&cell, std::time::Duration::from_secs(2), |c| {
                c.model_cache.is_some()
            })
            .await;
            self.cell_model_cache().expect("cache set after the wait")
        }
    }

    async fn create_ctx() -> Ctx {
        let harness = TestHarness::new().await;
        let deps = harness.actor_deps().await;
        let slices = deps.services.slices.clone();
        // Seed every cell the endpoint path reads, then attach *this* registry
        // to the state: `attach_slices` writes a `OnceLock`, so a state built
        // without it would keep a different registry and the actor could never
        // publish a fetch.
        let _ = slices.register(
            jinn_slices::scope_focus_slot(),
            jinn_slices::ScopeFocusState::default(),
        );
        let _ = slices.register(provider_state_slot(), ProviderCell::default());
        let _ = slices.register(
            jinn_provider_selection_msg::endpoint::endpoint_picker_slot(),
            jinn_provider_selection_msg::endpoint::EndpointPickerState::default(),
        );
        let _ = slices.register(
            jinn_provider_selection_msg::provider_picker_slot(),
            jinn_provider_selection_msg::ProviderPickerState::default(),
        );
        let app = AppState::default();
        app.frontend.attach_slices(slices);
        let state = State::new(app);
        Ctx {
            harness,
            state,
            deps,
        }
    }

    fn sample_config() -> ProvidersConfig {
        ProvidersConfig {
            providers: BTreeMap::from([(
                "ollama".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "ollama".to_owned(),
                    models: vec!["llama3".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: None,
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        }
    }

    fn cache_with_ollama_llama3(ctx: Option<u32>) -> ModelCache {
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: ctx,
                input_modalities: InputModalities::text(),
            }],
        );
        cache.last_updated_at = Some(jiff::Timestamp::now());
        cache
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_sets_model_cache_in_cell() {
        // Given a provider actor and a registry with a provider.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        let cache = cache_with_ollama_llama3(Some(8192));

        // When publishing ModelCacheLoaded via bus.
        ctx.harness.publish(ModelCacheLoaded { cache }).await;

        // Then the model cache is set in the cell.
        let loaded = ctx.await_model_cache().await;
        assert_eq!(loaded.entries["ollama"].len(), 1);
        assert_eq!(loaded.entries["ollama"][0].id, "llama3");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cache_stored_event_fires_after_the_cell_write_on_refresh() {
        // Given a provider actor with a tap on ModelCacheStored and a
        // registry with one provider.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        let stored: Recorder<jinn_provider_selection_msg::ModelCacheStored> =
            ctx.harness.spawn_recorder().await;
        ctx.spawn_provider_actor();

        let mut results = std::collections::HashMap::new();
        results.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        let event = ModelsRefreshed {
            session_id: ctx.state.read().session.active_session_id().clone(),
            results,
            errors: std::collections::HashMap::new(),
        };

        // When the refresh lands.
        ctx.harness.publish(event).await;

        // Then exactly one ModelCacheStored was delivered, and the
        // provider cell ALREADY holds the merged cache at delivery
        // time — the ordering `/web2`'s push depends on (a
        // push rendered from a predating cache would show stale rows).
        let stored_events = await_recorded(&stored, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(stored_events.len(), 1);
        let cell = ctx.cell();
        assert!(
            cell.read().model_cache.is_some(),
            "the cell must hold the merged cache by the time the stored event is delivered"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn cache_stored_event_fires_on_the_boot_path() {
        // Given a provider actor with a tap on ModelCacheStored.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        let stored: Recorder<jinn_provider_selection_msg::ModelCacheStored> =
            ctx.harness.spawn_recorder().await;
        ctx.spawn_provider_actor();

        // When the boot cache load lands.
        ctx.harness
            .publish(ModelCacheLoaded {
                cache: cache_with_ollama_llama3(None),
            })
            .await;

        // Then one ModelCacheStored was delivered and the cell holds
        // the cache — the boot path pushes too, not just refresh.
        let stored_events = await_recorded(&stored, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(stored_events.len(), 1);
        let _ = ctx.await_model_cache().await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_preserves_timestamp() {
        // Given a provider actor with a cache that has a timestamp.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        let ts = jiff::Timestamp::now();
        let mut cache = cache_with_ollama_llama3(None);
        cache.last_updated_at = Some(ts);

        // When publishing ModelCacheLoaded via bus.
        ctx.harness.publish(ModelCacheLoaded { cache }).await;

        // Then the cache's timestamp survives the load.
        let loaded = ctx.await_model_cache().await;
        assert!(loaded.last_updated_at.is_some());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn models_refreshed_fills_context_length_from_registry_when_api_returns_none() {
        // Given a registry with zai provider that has context_length: Some(128_000).
        let config = ProvidersConfig {
            providers: BTreeMap::from([(
                "zai".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "zai".to_owned(),
                    models: vec!["zai-1.5".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: Some(128_000),
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        };
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry = ProviderRegistry::from_config(config).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelsRefreshed with zai model that has context_length: None.
        let mut results = std::collections::HashMap::new();
        results.insert(
            "zai".to_owned(),
            vec![ModelInfo {
                id: "zai-1.5".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        let event = ModelsRefreshed {
            session_id: ctx.state.read().session.active_session_id().clone(),
            results,
            errors: std::collections::HashMap::new(),
        };
        ctx.harness.publish(event).await;

        // Then the context length comes from the registry, not the API.
        let cache = ctx.await_model_cache().await;
        assert_eq!(cache.entries["zai"][0].context_length, Some(128_000));

        // And the model is registered in the provider registry.
        let resolved = services
            .provider_registry
            .get(&jinn_provider_config::ProviderId::new(
                "zai/zai-1.5".to_owned(),
            ));
        assert!(
            resolved.is_some(),
            "model should be in registry after ModelsRefreshed"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn models_refreshed_appends_transient_result_to_session_history() {
        // Given a provider actor and an empty session.
        let ctx = create_ctx().await;
        ctx.spawn_provider_actor();
        let session_id = ctx.state.read().session.active_session_id().clone();
        let event = ModelsRefreshed {
            session_id: session_id.clone(),
            results: std::collections::HashMap::from([(
                "ollama".to_owned(),
                vec![ModelInfo {
                    id: "llama3".to_owned(),
                    context_length: Some(8192),
                    input_modalities: InputModalities::text(),
                }],
            )]),
            errors: std::collections::HashMap::new(),
        };

        // When publishing the refresh result.
        ctx.harness.publish(event).await;

        // Then the session history contains one transient refresh entry.
        // The wait is on the history itself: a fixed sleep would be a guess
        // at how long the actor takes, and a guess that is wrong in the
        // direction that makes the assertion pass vacuously.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            ctx.state
                .read()
                .session
                .get(&session_id)
                .is_some_and(|s| s.history().len() == 1)
        })
        .await;
        let state = ctx.state.read();
        let session = state.session.get(&session_id).expect("session");
        assert_eq!(session.history().len(), 1);
        assert!(matches!(
            &session.history()[0].kind,
            jinn_core_types::ChatEntryKind::Transient(content)
                if content.contains("ollama") && content.contains('1')
        ));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn models_refreshed_block_config_beats_api_value() {
        // Given a registry with ollama provider that has context_length: Some(4096).
        let config = ProvidersConfig {
            providers: BTreeMap::from([(
                "ollama".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "ollama".to_owned(),
                    models: vec!["llama3".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: Some(4096),
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        };
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry = ProviderRegistry::from_config(config).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelsRefreshed where API returns context_length: Some(8192).
        let mut results = std::collections::HashMap::new();
        results.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: Some(8192),
                input_modalities: InputModalities::text(),
            }],
        );
        let event = ModelsRefreshed {
            session_id: ctx.state.read().session.active_session_id().clone(),
            results,
            errors: std::collections::HashMap::new(),
        };
        ctx.harness.publish(event).await;

        // Then the configured block value wins over the API's.
        let cache = ctx.await_model_cache().await;
        assert_eq!(cache.entries["ollama"][0].context_length, Some(4096));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn models_refreshed_leaves_none_when_neither_source_has_context_length() {
        // Given a provider actor.
        let ctx = create_ctx().await;
        ctx.spawn_provider_actor();

        // When publishing ModelsRefreshed where API also returns context_length: None.
        let mut results = std::collections::HashMap::new();
        results.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        let event = ModelsRefreshed {
            session_id: ctx.state.read().session.active_session_id().clone(),
            results,
            errors: std::collections::HashMap::new(),
        };
        ctx.harness.publish(event).await;

        // Then the context length stays unset.
        let cache = ctx.await_model_cache().await;
        assert_eq!(cache.entries["ollama"][0].context_length, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn models_refreshed_does_not_touch_provider_not_in_registry() {
        // Given a provider actor.
        let ctx = create_ctx().await;
        ctx.spawn_provider_actor();

        // When publishing ModelsRefreshed with results for groq (not in registry).
        let mut results = std::collections::HashMap::new();
        results.insert(
            "groq".to_owned(),
            vec![ModelInfo {
                id: "llama3".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        let event = ModelsRefreshed {
            session_id: ctx.state.read().session.active_session_id().clone(),
            results,
            errors: std::collections::HashMap::new(),
        };
        ctx.harness.publish(event).await;

        // Then the unregistered provider is left as the refresh reported it.
        let cache = ctx.await_model_cache().await;
        assert_eq!(cache.entries["groq"][0].context_length, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_fills_context_length_from_registry_when_cache_has_none() {
        // Given a registry with zai provider that has context_length: Some(128_000).
        let config = ProvidersConfig {
            providers: BTreeMap::from([(
                "zai".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "zai".to_owned(),
                    models: vec!["zai-1.5".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: Some(128_000),
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        };
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry = ProviderRegistry::from_config(config).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelCacheLoaded with cache that has context_length: None.
        let cache = cache_with_ollama_llama3(None);
        let mut cache = cache;
        cache.entries.clear();
        cache.entries.insert(
            "zai".to_owned(),
            vec![ModelInfo {
                id: "zai-1.5".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        cache.last_updated_at = Some(jiff::Timestamp::now());

        ctx.harness.publish(ModelCacheLoaded { cache }).await;

        // Then the registry supplies the context length the cache lacked.
        let loaded = ctx.await_model_cache().await;
        assert_eq!(loaded.entries["zai"][0].context_length, Some(128_000));

        // And the model is registered in the provider registry.
        let resolved = services
            .provider_registry
            .get(&jinn_provider_config::ProviderId::new(
                "zai/zai-1.5".to_owned(),
            ));
        assert!(
            resolved.is_some(),
            "model should be in registry after ModelCacheLoaded"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_block_config_beats_api_value() {
        // Given a registry with ollama provider that has context_length: Some(4096).
        let config = ProvidersConfig {
            providers: BTreeMap::from([(
                "ollama".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "ollama".to_owned(),
                    models: vec!["llama3".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: Some(4096),
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        };
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry = ProviderRegistry::from_config(config).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelCacheLoaded with cache that has context_length: Some(8192).
        let cache = cache_with_ollama_llama3(Some(8192));

        ctx.harness.publish(ModelCacheLoaded { cache }).await;

        // Then the configured block value wins over the API's.
        let loaded = ctx.await_model_cache().await;
        assert_eq!(loaded.entries["ollama"][0].context_length, Some(4096));
    }

    /// Seeds a minimal models.dev user file into the harness's temp cache dir
    /// marking `model_id` image-capable (or not). Without this, the harness's
    /// temp-root `AppPaths` has no models.dev data and precedence over
    /// models.dev would go untested.
    fn seed_models_dev(
        services: &jinn_kernel::common::services::Services,
        model_id: &str,
        image: bool,
    ) {
        let inputs: Vec<&str> = if image {
            vec!["text", "image"]
        } else {
            vec!["text"]
        };
        let body = serde_json::json!({
            "data": {
                "ollama": {
                    "models": {
                        model_id: { "modality": { "input": inputs } }
                    }
                }
            }
        });
        let path = services.paths.models_dev_user_path();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create cache dir");
        std::fs::write(path, body.to_string()).expect("write models.dev");
    }

    fn config_with_model_info() -> ProvidersConfig {
        config_with_model_info_modalities(vec!["text".to_owned(), "image".to_owned()])
    }

    fn config_with_model_info_modalities(modalities: Vec<String>) -> ProvidersConfig {
        use jinn_provider_config::ModelInfoEntry;
        ProvidersConfig {
            providers: BTreeMap::from([(
                "ollama".to_owned(),
                ProviderEntry {
                    model_info: vec![ModelInfoEntry {
                        id: "llama3".to_owned(),
                        context_length: Some(16384),
                        input_modalities: Some(modalities),
                        extra_body: None,
                    }],
                    backend: "ollama".to_owned(),
                    models: vec!["llama3".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: None,
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_per_model_config_beats_api_value() {
        // Given a registry with a per-model context_length of 16384 and
        // models.dev data that would leave the model text-only.
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        seed_models_dev(&services, "llama3", false);
        let registry = ProviderRegistry::from_config(config_with_model_info()).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelCacheLoaded with an API-discovered value of 8192.
        let cache = cache_with_ollama_llama3(Some(8192));
        ctx.harness.publish(ModelCacheLoaded { cache }).await;
        let loaded = ctx.await_model_cache().await;
        // Then the per-model configuration wins over the API-discovered value.
        assert_eq!(loaded.entries["ollama"][0].context_length, Some(16384));
        // And the configured modalities replace the discovered text-only value.
        assert!(
            loaded.entries["ollama"][0]
                .input_modalities
                .contains(Modality::Image)
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_injects_static_only_model() {
        // Given a registry whose model_info targets a model with no cache entry.
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry = ProviderRegistry::from_config(config_with_model_info()).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing a ModelCacheLoaded that lacks the configured model.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        cache.last_updated_at = Some(jiff::Timestamp::now());
        ctx.harness.publish(ModelCacheLoaded { cache }).await;
        let loaded = ctx.await_model_cache().await;
        let injected = loaded.entries["ollama"]
            .iter()
            .find(|m| m.id == "llama3")
            .expect("injected entry");
        // Then the statically configured model is injected.
        assert_eq!(injected.context_length, Some(16384));
        assert!(injected.input_modalities.contains(Modality::Image));
        // And no duplicate entries were created.
        assert_eq!(loaded.entries["ollama"].len(), 2);
    }

    fn sample_config_with_block_ctx() -> ProvidersConfig {
        ProvidersConfig {
            providers: BTreeMap::from([(
                "ollama".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "ollama".to_owned(),
                    models: vec!["llama3".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: Some(4096),
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_injects_block_level_only_static_model() {
        // Given a registry with a block-level context_length and no model_info.
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        let registry =
            ProviderRegistry::from_config(sample_config_with_block_ctx()).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing a ModelCacheLoaded that lacks the static model entirely.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "ollama".to_owned(),
            vec![ModelInfo {
                id: "mistral".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        cache.last_updated_at = Some(jiff::Timestamp::now());
        ctx.harness.publish(ModelCacheLoaded { cache }).await;
        let loaded = ctx.await_model_cache().await;
        let injected = loaded.entries["ollama"]
            .iter()
            .find(|m| m.id == "llama3")
            .expect("injected entry");
        // Then the block-level static model is injected.
        assert_eq!(injected.context_length, Some(4096));
        // And no duplicate entries.
        assert_eq!(loaded.entries["ollama"].len(), 2);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn model_cache_loaded_config_modalities_beat_models_dev_enrichment() {
        // Given a config that explicitly declares llama3 text-only while the
        // seeded models.dev data marks it image-capable.
        let config = config_with_model_info_modalities(vec!["text".to_owned()]);
        let ctx = create_ctx().await;
        let services = ctx.deps.services.clone();
        seed_models_dev(&services, "llama3", true);
        let registry = ProviderRegistry::from_config(config).expect("registry");
        services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When publishing ModelCacheLoaded for that model.
        let cache = cache_with_ollama_llama3(Some(8192));
        ctx.harness.publish(ModelCacheLoaded { cache }).await;
        let loaded = ctx.await_model_cache().await;
        // Then the configured text-only modality wins over the models.dev stamp.
        assert!(
            !loaded.entries["ollama"][0]
                .input_modalities
                .contains(Modality::Image),
            "config [\"text\"] must beat models.dev image stamping"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_from_models_dev_fills_none() {
        // Given a cache with context_length: None and models.dev data.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "zai".to_owned(),
            vec![ModelInfo {
                id: "glm-5.1".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );

        let mut models_dev = jinn_provider_config::ModelsDevData::new();
        models_dev
            .context_lengths
            .insert("glm-5.1".to_owned(), 200_000);

        // When merging.
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then the model now has context_length from models.dev.
        assert_eq!(cache.entries["zai"][0].context_length, Some(200_000));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_from_models_dev_does_not_overwrite_existing() {
        // Given a cache with context_length: Some(100000) and models.dev has 200000.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "openai".to_owned(),
            vec![ModelInfo {
                id: "gpt-4o".to_owned(),
                context_length: Some(100_000),
                input_modalities: InputModalities::text(),
            }],
        );

        let mut models_dev = jinn_provider_config::ModelsDevData::new();
        models_dev
            .context_lengths
            .insert("gpt-4o".to_owned(), 200_000);

        // When merging.
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then the existing value is preserved.
        assert_eq!(cache.entries["openai"][0].context_length, Some(100_000));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_from_models_dev_leaves_none_when_not_in_data() {
        // Given a cache with an unknown model.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "local".to_owned(),
            vec![ModelInfo {
                id: "my-custom-llama".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );

        let models_dev = jinn_provider_config::ModelsDevData::new();

        // When merging with empty models.dev data.
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then it stays None.
        assert_eq!(cache.entries["local"][0].context_length, None);
    }

    /// A single-model cache entry keyed by provider, holding one model with
    /// the given id and context length.
    fn one_model(
        provider: &str,
        model_id: &str,
        context_length: Option<u32>,
    ) -> (String, Vec<ModelInfo>) {
        (
            provider.to_owned(),
            vec![ModelInfo {
                id: model_id.to_owned(),
                context_length,
                input_modalities: InputModalities::text(),
            }],
        )
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_priority_is_api_then_config_then_models_dev() {
        // Given four models, one per source scenario: A already has an API
        // value, B is filled in by the config merge, C only models.dev can
        // fill, and D nothing can fill.
        let mut cache = ModelCache::new();
        for entry in [
            one_model("provider-a", "model-a", Some(100_000)),
            one_model("provider-b", "model-b", None),
            one_model("provider-c", "model-c", None),
            one_model("provider-d", "model-d", None),
        ] {
            cache.entries.insert(entry.0, entry.1);
        }

        // And a config merge that filled model-b, plus a models.dev dataset
        // carrying values for all but model-d.
        cache.entries.get_mut("provider-b").unwrap()[0].context_length = Some(64_000);
        let mut models_dev = jinn_provider_config::ModelsDevData::new();
        for (id, context_length) in [
            ("model-a", 999_999),
            ("model-b", 999_999),
            ("model-c", 300_000),
        ] {
            models_dev
                .context_lengths
                .insert(id.to_owned(), context_length);
        }

        // When merging from models.dev.
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then: A keeps API value, B keeps config value, C gets models.dev, D stays None.
        assert_eq!(cache.entries["provider-a"][0].context_length, Some(100_000));
        assert_eq!(cache.entries["provider-b"][0].context_length, Some(64_000));
        assert_eq!(cache.entries["provider-c"][0].context_length, Some(300_000));
        assert_eq!(cache.entries["provider-d"][0].context_length, None);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_from_models_dev_handles_multiple_providers() {
        // Given two providers with models that have None.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "zai".to_owned(),
            vec![ModelInfo {
                id: "glm-5.1".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        cache.entries.insert(
            "anthropic".to_owned(),
            vec![ModelInfo {
                id: "claude-sonnet-4-20250514".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );

        let mut models_dev = jinn_provider_config::ModelsDevData::new();
        models_dev
            .context_lengths
            .insert("glm-5.1".to_owned(), 200_000);
        models_dev
            .context_lengths
            .insert("claude-sonnet-4-20250514".to_owned(), 200_000);

        // When merging.
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then both providers get filled.
        assert_eq!(cache.entries["zai"][0].context_length, Some(200_000));
        assert_eq!(cache.entries["anthropic"][0].context_length, Some(200_000));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn merge_from_models_dev_stamps_image_bit_on_disk_loaded_cache() {
        // Given a stale cache (as loaded from disk) whose model is image-capable
        // in models.dev but carries only the text-only default.
        let mut cache = ModelCache::new();
        cache.entries.insert(
            "openrouter".to_owned(),
            vec![ModelInfo {
                id: "xiaomi/mimo-v2.5".to_owned(),
                context_length: None,
                input_modalities: InputModalities::text(),
            }],
        );
        let mut models_dev = jinn_provider_config::ModelsDevData::new();
        models_dev
            .image_support
            .insert("xiaomi/mimo-v2.5".to_owned(), true);

        // When merging (the disk-load path re-enriches from models.dev).
        super::merge_models_dev_data(&mut cache, &models_dev);

        // Then the image bit is stamped despite the stale text-only cache.
        assert!(
            cache.entries["openrouter"][0]
                .input_modalities
                .contains(Modality::Image),
            "disk-loaded cache should gain the image bit via re-enrichment"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_dispatches_provider_switch_command() {
        // Given a provider actor.
        let ctx = create_ctx().await;
        let recorder = ctx.harness.spawn_recorder::<ProviderSwitched>().await;
        ctx.spawn_provider_actor();
        let session_id = ctx.state.read().session.active_session_id().clone();

        // When telling the actor to switch (ProviderSwitch is a COMMAND:
        // point-to-point to the actor's path).
        ctx.deps
            .services
            .trouper_system
            .tell(
                ActorPath::new(PROVIDER_ACTOR_PATH),
                jinn_provider_selection_msg::ProviderSwitch {
                    session_id: session_id.clone(),
                    provider_id: ModelSelection::Single("ollama/llama3".to_owned()),
                },
            )
            .await
            .expect("switch command delivers");

        // Then the session model is updated.
        let messages = await_recorded(&recorder, 1, std::time::Duration::from_secs(2)).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].provider_name, "ollama/llama3");

        let s = ctx.state.read();
        assert_eq!(
            s.session.active_session().profile().model,
            ModelSelection::Single("ollama/llama3".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn handle_dispatches_load_provider_picker_entries_command() {
        // Given a provider actor with a registry.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        // When telling the actor to load (a COMMAND: point-to-point).
        ctx.deps
            .services
            .trouper_system
            .tell(
                ActorPath::new(PROVIDER_ACTOR_PATH),
                LoadProviderPickerEntries,
            )
            .await
            .expect("load command delivers");

        // The picker cell is the signal that the command was processed.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            !ctx.provider_picker_cell()
                .read()
                .selection
                .items()
                .is_empty()
        })
        .await;

        // Then the model picker has entries — in its own cell, since the
        // menu is slice-owned and there is no kernel-side mirror to check.
        let cell = ctx.provider_picker_cell();
        assert!(
            !cell.read().selection.items().is_empty(),
            "picker should have entries after loading"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn endpoint_load_for_non_openrouter_model_clears_loading_and_shows_placeholder() {
        // Given a provider actor whose registry has an ollama (non-OpenRouter) model,
        // and the loading flag pre-set as the open intent would.
        let ctx = create_ctx().await;
        let registry = ProviderRegistry::from_config(sample_config()).expect("registry");
        ctx.deps.services.provider_registry.replace(registry);
        ctx.spawn_provider_actor();

        ctx.state
            .write()
            .active_session_mut()
            .set_model(ModelSelection::Single("ollama/llama3".to_owned()));
        let cell = ctx.cell();
        cell.update(|c| c.endpoint_loading = true);

        // When telling the actor to load (a COMMAND: point-to-point).
        ctx.deps
            .services
            .trouper_system
            .tell(
                ActorPath::new(PROVIDER_ACTOR_PATH),
                jinn_provider_selection_msg::LoadEndpointPickerEntries,
            )
            .await
            .expect("load command delivers");
        // The picker cell is the signal that the command was processed.
        jinn_testutil::wait::wait_for(std::time::Duration::from_secs(2), || {
            !ctx.endpoint_picker_cell()
                .read()
                .selection
                .items()
                .is_empty()
        })
        .await;

        // Then loading is cleared (no stuck spinner) and a placeholder row shows.
        assert!(
            !cell.read().endpoint_loading,
            "non-OpenRouter load must clear the loading flag"
        );
        let picker = ctx.endpoint_picker_cell();
        assert!(
            !picker.read().selection.items().is_empty(),
            "non-OpenRouter load must still show the placeholder row"
        );
    }
}

#[cfg(test)]
mod endpoint_default_tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jinn_kernel::AppState;
    use jinn_kernel::common::actor_deps::ActorDeps;
    use jinn_kernel::common::bus::HarnessServices;
    use jinn_kernel::common::state::State;
    use jinn_provider_config::{
        ConfigStorage, InMemoryConfigStorage, ProviderEntry, ProviderRegistry, ProvidersConfig,
    };
    use jinn_provider_selection_msg::{ProviderCell, SetEndpointDefault, provider_state_slot};
    use jinn_testutil::bus_harness::TestHarness;
    use trouper::actor::ActorPath;

    use super::{PROVIDER_ACTOR_PATH, ProviderActor, ProviderActorDeps};

    /// A `ConfigStorage` that counts saves and can be made to fail, so the
    /// ordering guarantee ("one save per confirm", "a failed save changes
    /// nothing in memory") is observable rather than asserted by inspection.
    #[derive(Default)]
    struct CountingConfigStorage {
        saves: AtomicUsize,
        fail: bool,
    }

    impl CountingConfigStorage {
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                saves: AtomicUsize::new(0),
                fail: true,
            })
        }

        fn working() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn save_count(&self) -> usize {
            self.saves.load(Ordering::SeqCst)
        }
    }

    impl ConfigStorage for CountingConfigStorage {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn load(
            &self,
        ) -> Result<ProvidersConfig, error_stack::Report<jinn_provider_config::ConfigError>>
        {
            InMemoryConfigStorage::new().load()
        }

        fn save(
            &self,
            config: &ProvidersConfig,
        ) -> Result<(), error_stack::Report<jinn_provider_config::ConfigError>> {
            self.saves.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(
                    error_stack::Report::new(jinn_provider_config::ConfigError::Io)
                        .attach("test storage is configured to fail"),
                );
            }
            InMemoryConfigStorage::with_config(config).save(config)
        }
    }

    /// A registry whose openrouter provider serves one model.
    fn openrouter_config() -> ProvidersConfig {
        ProvidersConfig {
            providers: std::collections::BTreeMap::from([(
                "openrouter".to_owned(),
                ProviderEntry {
                    model_info: Vec::new(),
                    backend: "openrouter".to_owned(),
                    models: vec!["anthropic/claude-sonnet-4".to_owned()],
                    base_url: None,
                    api_key_env: None,
                    requires_key: false,
                    extra_body: None,
                    context_length: None,
                },
            )]),
            aliases: vec![],
            default_provider: None,
            endpoint_defaults: vec![],
        }
    }

    /// Harness + state + deps, with a working storage and a registry built
    /// from `config`, and the provider actor spawned against them.
    async fn ctx_with(
        config: ProvidersConfig,
        storage: Arc<dyn ConfigStorage>,
    ) -> (TestHarness, State, ActorDeps) {
        let harness = TestHarness::new().await;
        let mut deps = harness.actor_deps().await;
        let slices = deps.services.slices.clone();
        let _ = slices.register(provider_state_slot(), ProviderCell::default());
        let _ = slices.register(
            jinn_provider_selection_msg::endpoint::endpoint_picker_slot(),
            jinn_provider_selection_msg::endpoint::EndpointPickerState::default(),
        );
        let _ = slices.register(
            jinn_provider_selection_msg::provider_picker_slot(),
            jinn_provider_selection_msg::ProviderPickerState::default(),
        );
        let app = AppState::default();
        app.frontend.attach_slices(slices);
        let state = State::new(app);
        deps.services
            .provider_registry
            .replace(ProviderRegistry::from_config(config).expect("registry"));
        deps.services.config_storage = jinn_provider_config::ConfigStorageService::new(storage);
        ProviderActor::spawn(
            &deps.services.trouper_system,
            ProviderActorDeps {
                deps: deps.clone(),
                state: state.clone(),
                provider_cell: deps
                    .services
                    .slices
                    .reader(&provider_state_slot())
                    .expect("provider cell"),
                endpoint_picker_cell: deps
                    .services
                    .slices
                    .reader(&jinn_provider_selection_msg::endpoint::endpoint_picker_slot())
                    .expect("endpoint picker cell"),
                provider_picker_cell: deps
                    .services
                    .slices
                    .reader(&jinn_provider_selection_msg::provider_picker_slot())
                    .expect("provider picker cell"),
            },
        );
        (harness, state, deps)
    }

    /// Tells the actor to apply a pin and waits for it to drain.
    async fn tell(deps: &ActorDeps, msg: SetEndpointDefault) {
        deps.services
            .trouper_system
            .tell(ActorPath::new(PROVIDER_ACTOR_PATH), msg)
            .await
            .expect("command delivers");
        // The handler is synchronous once dequeued; draining the actor's
        // inbox is what proves it was dequeued and run. A fixed sleep
        // would only guess at the queue hop.
        jinn_testutil::wait::await_inbox_drained(
            &deps.services.trouper_system,
            &ActorPath::new(PROVIDER_ACTOR_PATH),
            std::time::Duration::from_secs(2),
        )
        .await;
    }

    const CLAUDE: &str = "openrouter/anthropic/claude-sonnet-4";

    #[rstest::rstest]
    #[tokio::test]
    async fn set_endpoint_default_pins_the_tag_for_the_named_model() {
        // Given a registry with no endpoint pins.
        let storage = CountingConfigStorage::working();
        let (_harness, _state, deps) = ctx_with(openrouter_config(), storage.clone()).await;

        // When the actor applies a pin.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("anthropic".to_owned()),
            },
        )
        .await;

        // Then the registry resolves that model to the pinned tag.
        assert_eq!(
            deps.services
                .provider_registry
                .pinned_endpoint_tag(CLAUDE)
                .as_deref(),
            Some("anthropic")
        );
        assert!(storage.save_count() > 0, "the pin must be persisted");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_endpoint_default_persists_the_row_to_storage() {
        // Given a registry with no endpoint pins.
        let storage = CountingConfigStorage::working();
        let (_harness, _state, deps) = ctx_with(openrouter_config(), storage).await;

        // When the actor applies a pin.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("anthropic".to_owned()),
            },
        )
        .await;

        // Then the snapshot the registry holds carries the row.
        let snapshot = deps.services.provider_registry.config_snapshot();
        assert_eq!(snapshot.endpoint_defaults.len(), 1);
        assert_eq!(snapshot.endpoint_defaults[0].model, CLAUDE);
        assert_eq!(snapshot.endpoint_defaults[0].tag, "anthropic");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_endpoint_default_replaces_the_existing_row_for_the_same_model() {
        // Given a registry already pinning this model to one upstream.
        let mut config = openrouter_config();
        config
            .endpoint_defaults
            .push(jinn_provider_config::EndpointDefault {
                model: CLAUDE.to_owned(),
                tag: "anthropic".to_owned(),
            });
        let (_harness, _state, deps) = ctx_with(config, CountingConfigStorage::working()).await;

        // When the actor re-pins the same model to another upstream.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("azure".to_owned()),
            },
        )
        .await;

        // Then the row is replaced, not duplicated.
        let rows = &deps
            .services
            .provider_registry
            .config_snapshot()
            .endpoint_defaults;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tag, "azure");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_endpoint_default_leaves_other_models_rows_untouched() {
        // Given a registry pinning two different models.
        let mut config = openrouter_config();
        config
            .endpoint_defaults
            .push(jinn_provider_config::EndpointDefault {
                model: CLAUDE.to_owned(),
                tag: "anthropic".to_owned(),
            });
        config
            .endpoint_defaults
            .push(jinn_provider_config::EndpointDefault {
                model: "openrouter/openai/gpt-oss-120b".to_owned(),
                tag: "openai".to_owned(),
            });
        let (_harness, _state, deps) = ctx_with(config, CountingConfigStorage::working()).await;

        // When the actor re-pins only the first model.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("azure".to_owned()),
            },
        )
        .await;

        // Then the other model's row survives unchanged.
        let registry = &deps.services.provider_registry;
        assert_eq!(
            registry
                .pinned_endpoint_tag("openrouter/openai/gpt-oss-120b")
                .as_deref(),
            Some("openai")
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn set_endpoint_default_performs_exactly_one_save() {
        // Given a registry with no endpoint pins.
        let storage = CountingConfigStorage::working();
        let (_harness, _state, deps) = ctx_with(openrouter_config(), storage.clone()).await;

        // When the actor applies one pin.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("anthropic".to_owned()),
            },
        )
        .await;

        // Then the write is a single read-modify-write, not one per stage.
        assert_eq!(storage.save_count(), 1, "confirm must save exactly once");
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clearing_the_pin_removes_the_row() {
        // Given a registry pinning this model.
        let mut config = openrouter_config();
        config
            .endpoint_defaults
            .push(jinn_provider_config::EndpointDefault {
                model: CLAUDE.to_owned(),
                tag: "anthropic".to_owned(),
            });
        let (_harness, _state, deps) = ctx_with(config, CountingConfigStorage::working()).await;

        // When the actor applies the auto-route sentinel.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: None,
            },
        )
        .await;

        // Then the model has no pin and auto-routes again.
        assert!(
            deps.services
                .provider_registry
                .pinned_endpoint_tag(CLAUDE)
                .is_none()
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clearing_an_unpinned_model_does_not_save() {
        // Given a registry with no endpoint pins.
        let storage = CountingConfigStorage::working();
        let (_harness, _state, deps) = ctx_with(openrouter_config(), storage.clone()).await;

        // When the actor applies the auto-route sentinel to an unpinned model.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: None,
            },
        )
        .await;

        // Then nothing was written — there was no row to remove.
        assert_eq!(
            storage.save_count(),
            0,
            "a no-op delete must not rewrite the file"
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn a_failed_save_leaves_the_in_memory_pin_unchanged() {
        // Given a registry with no endpoint pins and a storage that fails.
        let (_harness, _state, deps) =
            ctx_with(openrouter_config(), CountingConfigStorage::failing()).await;

        // When the actor tries to apply a pin.
        tell(
            &deps,
            SetEndpointDefault {
                model: CLAUDE.to_owned(),
                tag: Some("anthropic".to_owned()),
            },
        )
        .await;

        // Then memory is unchanged, so the run and the file still agree.
        assert!(
            deps.services
                .provider_registry
                .pinned_endpoint_tag(CLAUDE)
                .is_none(),
            "a failed save must not update the in-memory config"
        );
    }
}
