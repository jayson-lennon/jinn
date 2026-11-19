//! One table row: a single OpenRouter listing joined with its benchmark
//! scores (ECI and Vals Index).
//!
//! `MarketRow` is the disk-cache payload (`Serialize + Deserialize`), so
//! its field set is wire-compatible with the OpenRouter listing the
//! models actor fetches, normalized for display: prices as `f64`
//! USD/token, the score joins resolved ahead of render. PROJ is **not**
//! stored — it is computed at render time from the active mix preset, so
//! `m` re-sorts without a refetch.

use serde::{Deserialize, Serialize};

/// One OpenRouter listing as the table shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketRow {
    /// Full selection id (`openrouter/{author}/{slug}[:variant]`), what
    /// ENTER publishes and the cache keys on.
    pub id: String,
    /// The listing's author (OpenRouter's publisher namespace).
    pub author: String,
    /// The model's slug.
    pub slug: String,
    /// The variant suffix (`free`, `thinking`), without the colon.
    pub variant: Option<String>,
    /// Prompt price in USD/token.
    pub prompt_price: f64,
    /// Completion price in USD/token.
    pub completion_price: f64,
    /// Cached-input read price in USD/token. `None` when the listing
    /// publishes no cache price: the column shows `—` and the projection
    /// costs cache reads at the prompt price (locked decision F).
    pub cache_read_price: Option<f64>,
    /// The listing advertises conditional pricing overrides.
    pub has_overrides: bool,
    /// Advertised context window, in tokens.
    pub context_length: Option<u32>,
    /// The Epoch Capabilities Index after the name join; `None` when the
    /// model is unmatched (column `—`, `#` flag).
    pub eci: Option<f64>,
    /// The ECI's bootstrap confidence interval as `(low, high)`.
    pub eci_ci: Option<(f64, f64)>,
    /// The Vals Index overall accuracy after the slug join; `None` when
    /// the model is unmatched (column `—`, `#` flag). `#[serde(default)]`
    /// so disk caches written before the column existed load as blank.
    #[serde(default)]
    pub vals: Option<f64>,
    /// The listing id ends in `:free`.
    pub is_free: bool,
}

/// One per-endpoint detail row for the highlighted model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketEndpointRow {
    /// The routing tag (e.g. `azure/us-east`), empty for the auto-route.
    pub tag: String,
    /// The upstream provider's display name.
    pub provider_name: String,
    /// Prompt price in USD/token.
    pub prompt_price: f64,
    /// Completion price in USD/token.
    pub completion_price: f64,
    /// Cached-input read price in USD/token; `None` when unpublished.
    pub cache_read_price: Option<f64>,
    /// Median time-to-first-token over the last 30m, in ms. `None`
    /// without an API key (the percentiles are auth-gated).
    pub latency_p50_ms: Option<f64>,
    /// Median output throughput over the last 30m, in tok/s.
    pub throughput_p50: Option<f64>,
    /// Uptime over the last day, as a fraction in `[0, 1]`.
    pub uptime_1d: Option<f64>,
    /// The endpoint's quantization scheme, when it runs one.
    pub quantization: Option<String>,
    /// Conditional pricing rows: `(min_prompt_tokens, prompt, completion)`.
    pub overrides: Vec<(Option<u32>, f64, f64)>,
}
