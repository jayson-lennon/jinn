//! The popup's UI cell payload: what only the open menu mutates.
//!
//! Written by the sync side only — the opener resets it, route actions
//! and the input hook move it, the render pass measures it. The actors
//! never touch it: [`MarketDataState`](crate::MarketDataState) is theirs.

use jinn_slices::SliceScopeId;
use jinn_slices::SlotKey;

/// The row count to assume before the first render pass measures the
/// popup. A fallback, not a default: the render pass overwrites it.
pub const RESULTS_VIEWPORT_FALLBACK: usize = 20;

/// The sort-column cycle order: the default first, then left-to-right
/// across the sortable columns.
pub const SORT_CYCLE: [SortColumn; 7] = [
    SortColumn::Proj,
    SortColumn::In,
    SortColumn::Out,
    SortColumn::Cache,
    SortColumn::Eci,
    SortColumn::Vals,
    SortColumn::Model,
];

/// The detail-mode sort cycle: the default first, then the eight detail
/// columns in header order, wrapping.
pub const DETAIL_SORT_CYCLE: [DetailSort; 8] = [
    DetailSort::Endpoint,
    DetailSort::In,
    DetailSort::Out,
    DetailSort::Cache,
    DetailSort::Proj,
    DetailSort::Lat,
    DetailSort::Tp,
    DetailSort::Up,
];

/// The fixed cost projection every PROJ value is computed under, as
/// `(input tokens, cache-hit fraction, output tokens)`. Cache misses
/// bill at the prompt price (locked decision F). The default mix; the
/// `m` key flips the projection to the session/tree usage-derived mix
/// ([`MixSource::Usage`]).
pub const PROJECTION_MIX: (u32, f64, u32) = (10_000_000, 0.98, 50_000);

/// Where the projection's `(input, cache-hit, output)` mix comes from.
///
/// `Fixed` is the shipped constant ([`PROJECTION_MIX`]). `Usage` derives
/// the mix from the current session tree's measured token usage — the
/// session itself when the tree is a single session — so PROJ ranks
/// models under the workload actually run, not a modeled one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MixSource {
    /// The shipped [`PROJECTION_MIX`] constant — the default.
    #[default]
    Fixed,
    /// Usage-derived from the current tree (or lone session): input =
    /// effective sent, output = received, cache hit = cached/measured.
    /// Falls back to `Fixed` semantics when the usage has no measured
    /// requests (a zero mix projects every model to $0.00).
    Usage,
}

/// Which column the sort is keyed on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortColumn {
    /// Projected $/turn under [`PROJECTION_MIX`] — the default.
    #[default]
    Proj,
    /// Prompt price, $/M.
    In,
    /// Completion price, $/M.
    Out,
    /// Cached-input read price, $/M (`None` sorts last either way).
    Cache,
    /// Epoch Capabilities Index (unmatched rows sort last either way).
    Eci,
    /// Vals Index overall accuracy (unmatched rows sort last either
    /// way).
    Vals,
    /// `author/slug[:variant]`, lexicographic.
    Model,
}

/// Steps `from` `steps` positions around [`SORT_CYCLE`], wrapping.
///
/// Index arithmetic is denied by lint, so the cycle is walked by
/// `nth` over an endlessly-chained iterator.
fn cycle_step(from: SortColumn, steps: usize) -> SortColumn {
    let offset = SORT_CYCLE.iter().position(|c| *c == from).unwrap_or(0) + steps;
    SORT_CYCLE
        .iter()
        .cycle()
        .nth(offset)
        .copied()
        .unwrap_or_default()
}

impl SortColumn {
    /// The next column in the `h`/`l` cycle.
    #[must_use]
    pub fn next(self) -> Self {
        cycle_step(self, 1)
    }

    /// The previous column in the `h`/`l` cycle.
    #[must_use]
    pub fn prev(self) -> Self {
        cycle_step(self, SORT_CYCLE.len() - 1)
    }

    /// The column header the table shows.
    #[must_use]
    pub fn header(self) -> &'static str {
        match self {
            Self::Proj => "PROJ",
            Self::In => "IN/M",
            Self::Out => "OUT/M",
            Self::Cache => "CACHE/M",
            Self::Eci => "ECI",
            Self::Vals => "VALS",
            Self::Model => "MODEL",
        }
    }
}

/// Which detail-mode column the sort is keyed on. The cycle order is the
/// detail table's header order; the *default* is `Proj` (the mode's
/// reason to exist is the per-endpoint projection), mirroring the
/// table's own PROJ default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetailSort {
    /// The endpoint's display name — `provider_name` plus a non-empty
    /// tag; the Default row is the name-sort's `None` key, tie-broken
    /// by name like the table's `Model`.
    Endpoint,
    /// Endpoint prompt price, $/M.
    In,
    /// Endpoint completion price, $/M.
    Out,
    /// Endpoint cached-input read price (`None` sorts last either way).
    Cache,
    /// Projected $/turn under [`PROJECTION_MIX`] — the default.
    #[default]
    Proj,
    /// Median time-to-first-token, ms (`None` sorts last either way).
    Lat,
    /// Median output throughput, tok/s (`None` sorts last either way).
    Tp,
    /// One-day uptime fraction (`None` sorts last either way).
    Up,
}

/// Steps `from` `steps` positions around [`DETAIL_SORT_CYCLE`], wrapping.
///
/// Index arithmetic is denied by lint, so the cycle is walked by
/// `nth` over an endlessly-chained iterator.
fn detail_cycle_step(from: DetailSort, steps: usize) -> DetailSort {
    let offset = DETAIL_SORT_CYCLE
        .iter()
        .position(|c| *c == from)
        .unwrap_or(0)
        + steps;
    DETAIL_SORT_CYCLE
        .iter()
        .cycle()
        .nth(offset)
        .copied()
        .unwrap_or_default()
}

impl DetailSort {
    /// The next column in the `h`/`l` cycle.
    #[must_use]
    pub fn next(self) -> Self {
        detail_cycle_step(self, 1)
    }

    /// The previous column in the `h`/`l` cycle.
    #[must_use]
    pub fn prev(self) -> Self {
        detail_cycle_step(self, DETAIL_SORT_CYCLE.len() - 1)
    }

    /// The column header the detail table shows. Symmetry with
    /// [`SortColumn::header`]: the rendered headers live in the view's
    /// column table, which remains the source of truth on screen.
    #[must_use]
    pub fn header(self) -> &'static str {
        match self {
            Self::Endpoint => "ENDPOINT",
            Self::In => "IN /M",
            Self::Out => "OUT /M",
            Self::Cache => "CACHE /M",
            Self::Proj => "PROJ",
            Self::Lat => "LAT p50",
            Self::Tp => "TP p50",
            Self::Up => "UP 1d",
        }
    }
}

/// The model-market picker's per-open state.
#[derive(Debug, Clone)]
pub struct MarketUiState {
    /// The highlighted row's index into the *filtered, sorted*
    /// projection — not into `MarketDataState.rows`.
    pub selection: usize,
    /// The filter text (`f`/`i` focuses the bar; typing narrows rows).
    pub filter: String,
    /// Whether the filter bar owns typing. While this is set the input
    /// hook claims every printable character, so `h`/`l`/`s`/`f`
    /// type into the filter instead of firing their table actions —
    /// the table actions are hook cases, not route rows, and the hook
    /// decides by focus.
    pub filter_focused: bool,
    /// The sort key; `h`/`l` cycle it, `s` flips direction.
    pub sort_col: SortColumn,
    /// The sort direction.
    pub sort_desc: bool,
    /// Whether the help card is showing (`?` toggles).
    pub help_visible: bool,
    /// Render-measured result-row count for paging; falls back to
    /// [`RESULTS_VIEWPORT_FALLBACK`] before the first frame.
    pub results_viewport: usize,
    /// Monotonic sequence the UI stamps on every fetch request so the
    /// endpoint actor can drop stale debounced fetches.
    pub fetch_seq: u64,
    /// The detail mode's pin: `None` = table mode; `Some(id)` = detail
    /// mode, pinned to that model until ESC/backspace. The pin is the
    /// stale guard — render trusts `data.detail` only when its
    /// `model_id` equals this, so detail for another model never
    /// renders under this mode.
    pub detail_model: Option<String>,
    /// The detail-mode highlight, into the detail rows projection
    /// (Default sentinel first, then the endpoints).
    pub detail_selection: usize,
    /// The detail-mode sort key; `h`/`l` cycle it, `s` flips direction.
    /// Separate from the table's `sort_col`: leaving detail mode
    /// restores the table's sort untouched.
    pub detail_sort_col: DetailSort,
    /// The detail-mode sort direction.
    pub detail_sort_desc: bool,
    /// Where the PROJ projection's mix comes from: the shipped constant
    /// or the session/tree's measured usage. `m` flips it; the flag
    /// survives detail round-trips (like sort) and resets on open.
    pub mix_source: MixSource,
    /// The resolved mix the render pass projects and sorts under —
    /// [`PROJECTION_MIX`] for [`MixSource::Fixed`], the usage-derived
    /// values for [`MixSource::Usage`]. Render-measured state like
    /// `results_viewport`: recomputed each frame, read by the cells.
    pub active_mix: (u32, f64, u32),
}

/// Resolves the mix the frame projects and sorts under: the shipped
/// constant for [`MixSource::Fixed`], the usage-derived values for
/// [`MixSource::Usage`]. A `usage` of `None` (no facts) or one without
/// measured requests (`measured_sent == 0` — a fresh session projects
/// every model to $0.00, a useless ranking) degrades to the constant,
/// never to a wrong mix.
#[must_use]
pub fn resolve_mix(source: MixSource, usage: Option<(u64, u64, u64, u64)>) -> (u32, f64, u32) {
    match source {
        MixSource::Fixed => PROJECTION_MIX,
        MixSource::Usage => usage.map_or(PROJECTION_MIX, |(sent, received, cached, measured)| {
            if measured == 0 {
                return PROJECTION_MIX;
            }
            let input = sent.max(1) as f64;
            let output = received as f64;
            let cache_hit = cached as f64 / measured as f64;
            (
                input.round() as u32,
                cache_hit.clamp(0.0, 1.0),
                output.round() as u32,
            )
        }),
    }
}

impl Default for MarketUiState {
    fn default() -> Self {
        Self {
            selection: 0,
            filter: String::new(),
            filter_focused: false,
            sort_col: SortColumn::default(),
            sort_desc: false,
            help_visible: false,
            results_viewport: RESULTS_VIEWPORT_FALLBACK,
            fetch_seq: 0,
            detail_model: None,
            detail_selection: 0,
            detail_sort_col: DetailSort::default(),
            detail_sort_desc: false,
            mix_source: MixSource::default(),
            active_mix: PROJECTION_MIX,
        }
    }
}

impl MarketUiState {
    /// Resets to the opener's defaults: table mode, filter empty, PROJ
    /// ascending, help hidden.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Enters detail mode pinned to `model_id`: the highlight and the
    /// detail sort land on their defaults, and the pin is the mode.
    /// Touches nothing table-side — the filter, focus, sort, and
    /// selection survive the round trip.
    pub fn enter_detail(&mut self, model_id: String) {
        self.detail_model = Some(model_id);
        self.detail_selection = 0;
        self.detail_sort_col = DetailSort::default();
        self.detail_sort_desc = false;
    }

    /// Leaves detail mode: the pin clears and the table mode's state is
    /// exactly as it was before drilling in. Touches nothing table-side.
    pub fn leave_detail(&mut self) {
        self.detail_model = None;
    }

    /// Flips the projection mix between the shipped constant and the
    /// session/tree usage-derived mix (`m`). The flag survives detail
    /// round-trips; `reset` (the opener) lands it back on `Fixed`.
    pub fn toggle_mix_source(&mut self) {
        self.mix_source = match self.mix_source {
            MixSource::Fixed => MixSource::Usage,
            MixSource::Usage => MixSource::Fixed,
        };
    }
}

/// The scope the popup's in-popup keys live in.
///
/// Dynamic and input-capturing: the scope owns the popup's keys and the
/// filter's typing carve-out, so the kernel holds no picker variant.
#[must_use]
pub fn market_ui_scope() -> SliceScopeId {
    SliceScopeId::new("model-market", "picker")
}

/// The UI cell's slot key, registered by the cell catalog before any
/// slice activates.
#[must_use]
pub fn market_ui_slot() -> SlotKey {
    SlotKey::builtin("model-market", "picker")
}
