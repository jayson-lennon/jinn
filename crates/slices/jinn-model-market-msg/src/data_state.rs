//! The fetched-data cell payload: everything the actors write and
//! everything the popup's table, detail pane, and status line read.

use std::sync::Arc;

use jiff::Timestamp;
use jinn_slices::SlotKey;

use crate::row::MarketRow;
use crate::scores::EciIndex;
use crate::scores::EciSource;
use crate::scores::ValsIndex;
use crate::scores::ValsSource;

/// The model-market's data cell: the fetched table, the two score
/// indexes, the cache stamps, and the in-flight flags.
///
/// Written only by the slice's actors. The synchronous side (route
/// actions, input hook, render pass) reads it and mutates its own
/// [`MarketUiState`](crate::MarketUiState) instead.
#[derive(Debug, Clone, Default)]
pub struct MarketDataState {
    /// One row per OpenRouter listing, empty until the first
    /// `ModelsLoaded` lands.
    pub rows: Vec<MarketRow>,
    /// The highlighted model's per-endpoint detail, once fetched.
    pub detail: Option<MarketDetail>,
    /// Parsed epoch scores keyed by ECI display name. `Arc` so a score
    /// refresh swaps one pointer instead of cloning the map per reader.
    pub eci: Option<Arc<EciIndex>>,
    /// Parsed Vals Index scores keyed by Vals' `provider/model` slug.
    /// `Arc` for the same swap-one-pointer reason; `None` until the
    /// first successful vals load — there is no shipped seed for vals.
    pub vals: Option<Arc<ValsIndex>>,
    /// When the models cache was last written, for the status line's
    /// "updated X ago" stamp.
    pub models_fetched_at: Option<Timestamp>,
    /// When the ECI data was last written.
    pub eci_fetched_at: Option<Timestamp>,
    /// When the vals data was last written. Separate from the ECI stamp
    /// so a degraded source never hides behind a healthy sibling.
    pub vals_fetched_at: Option<Timestamp>,
    /// When the highlighted model's endpoints were last written.
    pub endpoints_fetched_at: Option<Timestamp>,
    /// A models fetch is in flight.
    pub models_loading: bool,
    /// An endpoint-detail fetch is in flight.
    pub endpoints_loading: bool,
    /// An ECI download/parse is in flight.
    pub eci_loading: bool,
    /// A vals download/parse is in flight.
    pub vals_loading: bool,
    /// Where the ECI data on display came from, so the user can tell
    /// the shipped snapshot from a fresh download.
    pub eci_source: EciSource,
    /// Where the vals data on display came from.
    pub vals_source: ValsSource,
    /// The most recent failure, shown as the popup's degraded-mode line.
    pub last_error: Option<String>,
}

/// The highlighted model's per-endpoint detail rows.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketDetail {
    /// The full model id the detail was fetched for
    /// (`openrouter/{author}/{slug}[:variant]`).
    pub model_id: String,
    /// One row per listing endpoint.
    pub endpoints: Vec<crate::row::MarketEndpointRow>,
}

/// The data cell's slot key, registered by the cell catalog before any
/// slice activates.
#[must_use]
pub fn market_data_slot() -> SlotKey {
    SlotKey::builtin("model-market", "data")
}
