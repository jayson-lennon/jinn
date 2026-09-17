//! Token estimation for prompt assembly budgeting.
//!
//! Provides a [`TokenEstimator`] trait for estimating token counts from text,
//! [`CharRatioEstimator`] as a simple heuristic implementation (1 token ≈ 4 characters),
//! and [`estimate_entry_tokens`] for estimating the token cost of individual chat entries.

use std::sync::OnceLock;

use jinn_core_types::tool_types::ToolDefinition;
use crate::protocol::{ChatEntry, ChatEntryKind, ContextOverride};
use unicode_segmentation::UnicodeSegmentation;

/// Estimates the token count of text.
///
/// Used by budget-based strategies to decide how much history to include.
/// Implementations may range from simple heuristics to real tokenizer calls.
pub trait TokenEstimator: Send + Sync {
    /// Estimate the number of tokens in `text`.
    fn estimate(&self, text: &str) -> usize;

    /// The name of this estimator, for debugging.
    fn name(&self) -> &'static str;
}

/// Estimate the token count for a single chat entry's contribution to LLM context.
///
/// Uses the same fields that `entries_to_messages` would convert to LLM messages.
/// Entries not in context (per `is_in_context()`) contribute 0 tokens.
///
/// Budget-facing view: context *membership* gates the result, not entry content.
/// For the context-independent per-entry count persisted on `ChatEntry`, use
/// [`estimate_entry_content_tokens`].
pub fn estimate_entry_tokens(estimator: &dyn TokenEstimator, entry: &ChatEntry) -> usize {
    // Entries not in context contribute 0 tokens.
    if !entry.is_in_context() {
        return 0;
    }
    estimate_entry_content_tokens(estimator, entry)
}

/// Estimate the token count of a chat entry's content, independent of context state.
///
/// Same per-kind shaping as [`estimate_entry_tokens`] (estimator prefixes, image
/// attachment cost) but with no `is_in_context` short-circuit: an entry excluded
/// from the assembled prompt still counts its text. This is the function behind
/// the persisted `ChatEntry::token_count` field — the count is a fact about the
/// entry's immutable content, so it never depends on context membership.
///
/// Kind-level exclusions are preserved verbatim: `Annotation` and `Transient`
/// shape their text the same as any other kind here; the `Error` arm still
/// keys off `context_override` because the override changes the content's
/// framing, not its context membership.
pub fn estimate_entry_content_tokens(estimator: &dyn TokenEstimator, entry: &ChatEntry) -> usize {
    match &entry.kind {
        ChatEntryKind::User {
            expanded,
            attachments,
            ..
        } => estimator.estimate(expanded) + image_attachment_tokens(attachments),
        ChatEntryKind::Assistant(text)
        | ChatEntryKind::System(text)
        | ChatEntryKind::Compaction { summary: text, .. } => estimator.estimate(text),
        ChatEntryKind::ToolCall {
            name, arguments, ..
        } => estimator.estimate(name) + estimator.estimate(arguments),
        ChatEntryKind::ToolResult { name, content, .. } => {
            estimator.estimate(name) + estimator.estimate(content)
        }
        // Actor entries produce LlmMessage::User with a prefix when in context.
        ChatEntryKind::Actor { source, text } => {
            estimator.estimate(&format!("[Actor: {source}] {text}"))
        }
        // Error entries produce LlmMessage::User. The prefix matches the
        // renderer: `[Error]` for Default (incl. pinned) and the actionable
        // framing for ForcedInclude. Keeping this in sync with
        // entries_to_messages prevents budget drift.
        ChatEntryKind::Error(text) => {
            let formatted = match entry.context_override() {
                ContextOverride::ForcedInclude => {
                    format!(
                        "The user has shared the following output for you to address:\n\n{text}"
                    )
                }
                ContextOverride::Default | ContextOverride::ForcedExclude => {
                    format!("[Error] {text}")
                }
            };
            estimator.estimate(&formatted)
        }
        // Thinking entries produce LlmMessage::User with [Thinking] prefix when in context.
        ChatEntryKind::Thinking(text) => estimator.estimate(&format!("[Thinking] {text}")),
        // Transient entries produce LlmMessage::User with [Transient] prefix when in context.
        ChatEntryKind::Transient(text) => estimator.estimate(&format!("[Transient] {text}")),
        // Annotations are display-only and excluded from context (0 tokens).
        ChatEntryKind::Annotation { .. } => 0,
    }
}

/// Flat token cost attributed to each image attachment for budgeting.
///
/// Real per-provider image token math varies (Anthropic tile estimate,
/// OpenAI tile-based, Gemini tiles); this flat heuristic keeps the budget
/// honest enough that context assembly is not surprised by image cost.
const IMAGE_ATTACHMENT_TOKENS: usize = 765;

/// Returns the total flat token cost for the image attachments in a user entry.
fn image_attachment_tokens(attachments: &[jinn_provider::Attachment]) -> usize {
    attachments
        .iter()
        .filter(|a| a.is_image())
        .map(|_| IMAGE_ATTACHMENT_TOKENS)
        .sum()
}

/// Estimates the token cost of tool definitions as serialized JSON schemas.
///
/// Each [`ToolDefinition`] is serialized to JSON via `serde_json::to_string`
/// and estimated using the given estimator. This captures the cost of the
/// name, description, parameters schema, prompt_snippet, and prompt_guidelines
/// as they appear in the API request.
pub fn estimate_tool_schema_tokens(
    estimator: &dyn TokenEstimator,
    tools: &[ToolDefinition],
) -> usize {
    tools
        .iter()
        .map(|td| {
            let json = serde_json::to_string(td).unwrap_or_default();
            estimator.estimate(&json)
        })
        .sum()
}

/// Simple heuristic estimator: 1 token ≈ 4 grapheme clusters.
///
/// Good enough for initial use. Uses grapheme-cluster counting via
/// `unicode-segmentation` rather than byte length. Always returns at least 1
/// to avoid zero-token estimates.
pub struct CharRatioEstimator;

impl TokenEstimator for CharRatioEstimator {
    #[expect(
        clippy::integer_division,
        reason = "1 token ≈ 4 characters is intentional rounding"
    )]
    fn estimate(&self, text: &str) -> usize {
        text.graphemes(true).count() / 4 + 1
    }

    fn name(&self) -> &'static str {
        "char_ratio"
    }
}

/// Counts tokens in text using a specific tokenizer.
///
/// Unlike [`TokenEstimator`] which is a rough heuristic for budget planning,
/// [`TokenCounter`] produces counts suitable for recording in the session's
/// immutable token ledger. Implementations may use real tokenizers (tiktoken)
/// or simple heuristics.
pub trait TokenCounter: Send + Sync {
    /// Count the number of tokens in `text`.
    fn count(&self, text: &str) -> usize;

    /// The name of this counter, for debugging.
    fn name(&self) -> &'static str;
}

/// Token counter using the `tiktoken` crate with a configurable encoding.
///
/// Wraps a `tiktoken::CoreBPE` encoder. The encoding is chosen at construction
/// time and does not change - counts are deterministic for a given text input.
/// This is important: once a count is recorded in the token ledger, it is
/// immutable regardless of future model/tokenizer changes.
#[derive(Clone, Copy)]
pub struct TiktokenCounter {
    encoder: &'static tiktoken::CoreBpe,
    encoding_name: &'static str,
}

impl TiktokenCounter {
    /// Create a counter using the `o200k_base` encoding (GPT-4o, o1, o3).
    ///
    /// This is a reasonable default for most LLM interactions.
    ///
    /// # Panics
    ///
    /// Panics if the `o200k_base` encoding is unavailable, which should never happen
    /// as it is a built-in tiktoken encoding.
    #[must_use]
    #[expect(clippy::expect_used, reason = "infallible")]
    pub fn o200k_base() -> Self {
        static ENCODING: OnceLock<Option<&'static tiktoken::CoreBpe>> = OnceLock::new();
        let encoder = ENCODING
            .get_or_init(|| tiktoken::get_encoding("o200k_base"))
            .expect("o200k_base encoding should always be available");
        Self {
            encoder,
            encoding_name: "o200k_base",
        }
    }
}

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        self.encoder.count(text)
    }

    fn name(&self) -> &'static str {
        self.encoding_name
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;
    use crate::protocol::ChatEntry;

    /// A simple estimator that returns the byte length of text.
    struct ByteLenEstimator;

    impl TokenEstimator for ByteLenEstimator {
        fn estimate(&self, text: &str) -> usize {
            text.len()
        }
        fn name(&self) -> &'static str {
            "byte_len"
        }
    }

    #[rstest::rstest]
    #[test]
    fn estimate_entry_tokens_sums_name_and_content_for_tool_call() {
        // Given a ToolCall entry.
        let estimator = ByteLenEstimator;
        let entry = ChatEntry::tool_call("id-1", "read_file", "{\"path\": \"/tmp\"}");

        // When estimating tokens.
        let tokens = estimate_entry_tokens(&estimator, &entry);

        // Then the estimate is the SUM of name + arguments lengths.
        let name_len = estimator.estimate("read_file");
        let args_len = estimator.estimate("{\"path\": \"/tmp\"}");
        assert_eq!(
            tokens,
            name_len + args_len,
            "ToolCall tokens should be sum of name + arguments, not product"
        );
        assert!(tokens > 0, "should have non-zero token estimate");
    }

    #[rstest::rstest]
    #[test]
    fn estimate_entry_tokens_sums_name_and_content_for_tool_result() {
        // Given a ToolResult entry.
        let estimator = ByteLenEstimator;
        let entry = ChatEntry::tool_result(
            "id-1",
            "read_file",
            "file contents here",
            crate::feat::session::tool_result_status::ToolResultStatus::Success,
        );

        // When estimating tokens.
        let tokens = estimate_entry_tokens(&estimator, &entry);

        // Then the estimate is the SUM of name + content lengths.
        let name_len = estimator.estimate("read_file");
        let content_len = estimator.estimate("file contents here");
        assert_eq!(
            tokens,
            name_len + content_len,
            "ToolResult tokens should be sum of name + content, not product"
        );
        assert!(tokens > 0, "should have non-zero token estimate");
    }

    #[rstest::rstest]
    #[test]
    fn estimate_entry_tokens_adds_image_cost_per_attachment() {
        use jinn_provider::Attachment;

        // Given a user entry with two image attachments.
        let estimator = ByteLenEstimator;
        let mut entry = ChatEntry::user("describe");
        if let crate::protocol::ChatEntryKind::User { attachments, .. } = &mut entry.kind {
            attachments.push(Attachment::image("image/png", vec![1]));
            attachments.push(Attachment::image("image/png", vec![2]));
        }

        // When estimating tokens.
        let tokens = estimate_entry_tokens(&estimator, &entry);

        // Then the estimate is text cost + 2 × flat image cost.
        assert_eq!(tokens, estimator.estimate("describe") + 765 + 765);
    }

    #[rstest::rstest]
    #[test]
    fn content_estimator_counts_excluded_entry_but_budget_estimator_returns_zero() {
        // Given a user entry force-excluded from context.
        let estimator = ByteLenEstimator;
        let mut entry = ChatEntry::user("some excluded text");
        entry.apply_context_override(
            crate::protocol::ContextOverride::ForcedExclude,
            crate::protocol::ChangeSource::User,
        );

        // When estimating the entry's content tokens.
        let content = estimate_entry_content_tokens(&estimator, &entry);

        // Then the content count reflects the text despite exclusion.
        assert!(
            content > 0,
            "content count must ignore context membership, got {content}"
        );

        // And the budget-facing estimator still returns 0 for the same entry.
        assert_eq!(estimate_entry_tokens(&estimator, &entry), 0);
    }

    #[rstest::rstest]
    #[test]
    fn patched_tiktoken_serves_o200k_base() {
        // Given the vendored tiktoken build (o200k family only).

        // When requesting the o200k_base encoding by name.
        let enc = tiktoken::get_encoding("o200k_base");

        // Then the encoder is available (jinn's only encoding).
        assert!(enc.is_some(), "o200k_base must be available");
    }

    #[rstest::rstest]
    #[test]
    fn patched_tiktoken_removed_encoding_is_unavailable() {
        // Given the vendored tiktoken build (o200k family only).

        // When requesting an encoding whose vocabulary is not embedded.
        let enc = tiktoken::get_encoding("cl100k_base");

        // Then no encoder is returned (removed from the binary).
        assert!(enc.is_none(), "cl100k_base should be compiled out");
    }

    #[rstest::rstest]
    #[test]
    fn tiktoken_counter_counts_real_bpe_tokens() {
        // Given a TiktokenCounter over the o200k_base encoding.

        // When counting a string with multiple BPE tokens.
        let counter = TiktokenCounter::o200k_base();
        let tokens = counter.count("hello world");

        // Then the count reflects real BPE segmentation (2 tokens), not a heuristic.
        assert_eq!(
            tokens, 2,
            "o200k_base should split 'hello world' into 2 tokens"
        );
    }
}
