//! Model-market commands and events.
//!
//! The sync side (route actions) publishes commands; the actors publish
//! events and write [`MarketDataState`](crate::MarketDataState). Nothing
//! here carries cell handles or app state — pure data.

use serde::{Deserialize, Serialize};

use jinn_core_types::SessionId;
use jinn_slices::BusMessage;

use crate::scores::EciSource;
use crate::scores::ValsSource;

/// Fetch the OpenRouter listings table, warm-cache-or-network.
///
/// Published by the opener. The actor serves a fresh disk/memory cache
/// without an outbound request and a stale or absent cache with one.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Fetch the OpenRouter models table, serving from cache when fresh.")]
pub struct FetchModels;

impl BusMessage for FetchModels {}

/// Fetch one model's per-endpoint detail rows.
///
/// Published by the `j`/`k` navigation actions when the highlighted row
/// has no detail yet. `seq` is the UI cell's `fetch_seq` at publish
/// time; the actor drops the fetch when a newer sequence arrived during
/// its debounce window.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Fetch one model's endpoint detail, debounced by sequence.")]
pub struct FetchEndpoints {
    /// Full model id (`openrouter/{author}/{slug}[:variant]`).
    pub model_id: String,
    /// The UI cell's fetch sequence when this request was minted.
    pub seq: u64,
}

impl BusMessage for FetchEndpoints {}

/// Open the model's openrouter.ai page in the OS default browser.
///
/// Published by the popup's `O` key — in table mode for the highlighted
/// row, in detail mode for the pinned model. The actor builds the URL
/// from the listing id and spawns the platform opener; a failed launch
/// is reported as a chat error entry into `session_id`, a successful
/// one reports nothing.
///
/// One declarant by design: the OpenRouter market actor is the only
/// handler, so the Command route resolves to exactly that actor.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Open the model's openrouter.ai page in the OS default browser.")]
pub struct OpenModelPage {
    /// Full listing id (`openrouter/{author}/{slug}[:variant]`).
    pub model_id: String,
    /// The session stamped into the UI cell at popup open — where a
    /// failed launch reports.
    pub session_id: SessionId,
}

impl BusMessage for OpenModelPage {}

/// Refresh the benchmark-score caches: the Epoch ECI zip and the Vals
/// Index JSON.
///
/// The boot kick publishes this: the actor serves a fresh disk cache
/// when one exists, else downloads. On ECI total failure (no network,
/// first run) the actor emits [`RefreshFailed`] and the seed data
/// stands; a vals failure reports on its own and blanks only the VALS
/// column.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Refresh the benchmark score data (ECI zip and Vals Index JSON).")]
pub struct RefreshScores;

impl BusMessage for RefreshScores {}

/// Drop the OR models cache and the endpoint-detail caches, then refetch.
///
/// The models half of `<c-r>`: the OR models cache (memory + disk) and
/// every `endpoints-*` detail file go, then the models table and the
/// displayed model's detail refetch. Refresh means refresh — a later step
/// failing does not roll back the earlier drops.
///
/// The score half is not this message's business. The models actor
/// finishes its work and emits [`MarketModelsRefreshed`]; the score
/// actor subscribes to that fact and refreshes both score sources in
/// reaction, so one press refreshes the whole popup.
///
/// The kind is a Command, and it stays one: an imperative instruction
/// ("refresh now") is not a fact about the past. This schema has exactly
/// one handler — the OpenRouter market actor — so the route resolves to
/// that actor and the delivery is unambiguous. A schema declared by two
/// actors would be routed to one of them round-robin, silently losing
/// half of every press; that is why the score half rides the chain
/// event instead of sharing this command.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Drop every market cache and refetch models, endpoints, and score data.")]
pub struct RefreshAll;

impl BusMessage for RefreshAll {}

/// Drop the score caches and force a fresh download of both sources.
///
/// The forced counterpart of [`RefreshScores`]: `RefreshScores` serves a
/// fresh disk cache when one exists, while this command always drops the
/// cache dirs and goes to the network. The refresh half of `<c-r>` uses
/// it; the boot kick uses `RefreshScores`.
///
/// One handler, by design: the score actor is the only actor that owns
/// the scores cache, so the command's route resolves to exactly that
/// actor — a schema declared by two actors would be routed to one of
/// them round-robin, and half the refresh would be lost.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Command)]
#[schema(description = "Drop the score caches and force a fresh download of both sources.")]
pub struct RefreshScoresForce;

impl BusMessage for RefreshScoresForce {}

/// The OpenRouter models table was refreshed.
///
/// A past-tense fact: the models half of `<c-r>` has dropped its caches
/// and refetched the listing. The score actor subscribes to it and
/// refreshes both score sources in reaction, so one press refreshes the
/// whole popup.
///
/// The event is a fact, not an instruction, and it is named for what
/// happened. The models table is the join key source for the rows' ECI
/// and VALS columns, so the score half runs after this fact lands — its
/// own `EciLoaded`/`ValsLoaded` events then re-join the rows against the
/// fresh indexes.
///
/// The OpenRouter market actor emits it and the score actor declares it:
/// exactly one declarant, so the broadcast reaches exactly that actor.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The OpenRouter models table was refreshed.")]
pub struct MarketModelsRefreshed;

impl BusMessage for MarketModelsRefreshed {}

/// The models table landed in the data cell.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "The OpenRouter models table was written to the market data cell.")]
pub struct ModelsLoaded {
    /// How many listings the table now holds.
    pub count: u32,
    /// `true` when a warm cache served the load (no network).
    pub from_cache: bool,
}

impl BusMessage for ModelsLoaded {}

/// One model's endpoint detail landed in the data cell.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A model's endpoint detail was written to the market data cell.")]
pub struct EndpointsLoaded {
    /// The model id the detail belongs to.
    pub model_id: String,
}

impl BusMessage for EndpointsLoaded {}

/// ECI data landed in the data cell, with the join's coverage.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Benchmark score data was written to the market data cell.")]
pub struct EciLoaded {
    /// Where the data came from — the shipped seed or a download.
    pub source: EciSource,
    /// How many ECI names the index holds.
    pub matched: u32,
    /// How many rows the source file carried (parse drops included).
    pub total: u32,
}

impl BusMessage for EciLoaded {}

/// Vals Index data landed in the data cell, with the join's coverage.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "Vals Index data was written to the market data cell.")]
pub struct ValsLoaded {
    /// Where the data came from — a download (there is no shipped seed).
    pub source: ValsSource,
    /// How many model slugs the index holds.
    pub matched: u32,
    /// How many `overall` records the payload carried (parse drops
    /// included).
    pub total: u32,
}

impl BusMessage for ValsLoaded {}

/// A refresh step failed. Earlier drops are not rolled back; the status
/// line carries `message` as the degraded-mode note.
#[derive(Debug, Clone, Serialize, Deserialize, trouper::schema::Event)]
#[schema(description = "A market refresh step failed; stale data stands.")]
pub struct RefreshFailed {
    /// Which step failed (`models`, `endpoints`, `eci`, `vals`).
    pub what: String,
    /// The failure detail for the status line.
    pub message: String,
}

impl BusMessage for RefreshFailed {}
