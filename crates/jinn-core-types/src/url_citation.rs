//! Web-search citation payload.
//!
//! [`UrlCitation`] is a provider-agnostic value type: the fields of a
//! `url_citation` annotation attached to an assistant message. It lives in
//! `jinn-core-types` (the same home as `Attachment`/`ToolCall`) so the
//! `ChatEntry` vocabulary and provider-side stream plumbing can share it
//! without a dependency edge; the provider crate re-exports it.

use serde::{Deserialize, Serialize};

/// A single web-search source citation from OpenRouter.
///
/// Carries the fields of a `url_citation` annotation attached to an assistant
/// message. `content` and the index range are frequently omitted by the
/// provider, hence `Option` with `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlCitation {
    /// The source URL.
    pub url: String,
    /// A human-readable title for the source.
    pub title: String,
    /// Snippet text, when the provider includes it.
    #[serde(default)]
    pub content: Option<String>,
    /// Start of the cited character span in the assistant text, if reported.
    #[serde(default)]
    pub start_index: Option<u32>,
    /// End of the cited character span in the assistant text, if reported.
    #[serde(default)]
    pub end_index: Option<u32>,
}
