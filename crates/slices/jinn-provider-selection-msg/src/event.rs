//! Provider events. The prompt-scan pair lives in `jinn-session-init-msg`.

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_provider::ModelInfo;
use jinn_provider_config::ModelCache;

use jinn_slices::BusMessage;

/// The active provider was switched.
///
/// Emitted after a successful [`ProviderSwitch`](crate::ProviderSwitch) command.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A session switched to a new provider.")]
pub struct ProviderSwitched {
    /// The session that switched provider.
    pub session_id: SessionId,
    /// The display name of the new provider.
    pub provider_name: String,
}

impl BusMessage for ProviderSwitched {}

/// Models refresh completed with results and errors.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Model discovery finished across providers.")]
pub struct ModelsRefreshed {
    /// The session that triggered the refresh (for routing the result back).
    pub session_id: SessionId,
    /// Provider name to list of discovered model metadata.
    pub results: std::collections::HashMap<String, Vec<ModelInfo>>,
    /// Provider name to error message for providers that failed.
    pub errors: std::collections::HashMap<String, String>,
}

impl BusMessage for ModelsRefreshed {}

/// The merged model cache was written into the provider cell.
///
/// Emitted by [`crate::ProviderActor`]'s handler **strictly after**
/// `store_model_cache` lands the merged cache in the cell, on both the
/// refresh path (`ModelsRefreshed`) and the boot path
/// (`ModelCacheLoaded`). This is the push trigger `/web2`'s outbound
/// actor subscribes — deliberately a distinct schema, never a
/// re-subscription of the raw discovery facts:
///
/// - `ModelsRefreshed` is the discovery actor's own publish, so a push
///   riding it could render before the cell holds the new cache and the
///   page's re-pull would read stale rows with no correction coming.
/// - `ModelCacheLoaded` is *handled* by `ProviderActor`; emitting it
///   would loop the actor onto itself.
///
/// The payload carries one thing: `stored_at`, the write's timestamp,
/// stamped by the emitter strictly after the cell write. It is
/// provenance, not content — every consumer still renders by reading
/// the cell (or re-pulling the picker rows built from it), never from
/// the event. The subscriber carries the timestamp on its push so the
/// frame always has a body to deliver.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The merged model cache was stored in the provider cell.")]
pub struct ModelCacheStored {
    /// When the merged cache was written to the provider cell. Read
    /// provenance for signal-only consumers; never a data source.
    pub stored_at: jiff::Timestamp,
}
impl BusMessage for ModelCacheStored {}

/// Model cache loaded from disk at startup.
///
/// Emitted by boot's `ProviderInitActor` after loading the cache from disk.
/// `ProviderActor` handles this by writing the cache into the provider cell
/// and reloading picker entries.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Model cache was loaded from disk.")]
pub struct ModelCacheLoaded {
    /// The loaded model cache.
    pub cache: ModelCache,
}

impl BusMessage for ModelCacheLoaded {}
