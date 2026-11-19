//! Model-market crossing contracts — the two cell payloads every side of
//! the slice boundary reads, plus the fetch/refresh commands and events.
//!
//! The split mirrors the slice's ownership rule: [`MarketDataState`] is
//! written only by the slice's actors (the fetched table, the score
//! indexes, the cache stamps), while [`MarketUiState`] is written only
//! by the synchronous side (route actions, the input hook, the render
//! pass). A writer on the wrong side is the sync-sibling anti-pattern;
//! the two payloads' docs name their owner so the boundary stays
//! visible.
//!
//! The crossing-schema ids (`FetchModels`, `ModelsLoaded`, …) derive from
//! the type names, so they are part of the wire contract and must not be
//! renamed.

mod commands;
mod data_state;
mod row;
mod scores;
mod ui_state;

pub use commands::*;
pub use data_state::{MarketDataState, MarketDetail, market_data_slot};
pub use row::{MarketEndpointRow, MarketRow};
pub use scores::{EciIndex, EciScore, EciSource, ValsIndex, ValsScore, ValsSource};
pub use ui_state::{
    DETAIL_SORT_CYCLE, DetailSort, MarketUiState, MixSource, PROJECTION_MIX,
    RESULTS_VIEWPORT_FALLBACK, SORT_CYCLE, SortColumn, market_ui_scope, market_ui_slot,
    resolve_mix,
};

#[cfg(test)]
mod tests;
