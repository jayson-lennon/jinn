//! OpenRouter routing endpoint — pinning one upstream for cache affinity.
//!
//! OpenRouter can serve a single model through several upstreams (Anthropic,
//! Azure, Bedrock, Google Vertex, etc.) and load-balances across them by
//! default. Each hop lands on an upstream whose prefix cache may be cold.
//!
//! An [`Endpoint`] records the user's choice of a *single* upstream, identified
//! by its OpenRouter routing [`tag`](Endpoint::tag). When pinned on a session
//! whose model is served via the OpenRouter backend, dispatch sends
//! `provider.order = [tag]` with `allow_fallbacks = false` so every turn in the
//! conversation routes to the same upstream, keeping the prefix cache warm.
//!
//! The pin applies only to a `Single` (non-alloy) model on the OpenRouter
//! backend; it is ignored for alloys and all other backends.

use serde::{Deserialize, Serialize};

pub mod picker_entry;

/// A pinned OpenRouter routing endpoint.
///
/// `tag` is the OpenRouter routing slug sent as `provider.order[0]`;
/// `provider_name` is the human-readable label shown in the picker. Only `tag`
/// is meaningful to the API; `provider_name` is display-only metadata kept in
/// sync with the picker selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// The OpenRouter routing slug (e.g. `"anthropic"`, `"azure"`).
    pub tag: String,
    /// Human-readable upstream name (e.g. `"Anthropic"`). Display only.
    pub provider_name: String,
}
