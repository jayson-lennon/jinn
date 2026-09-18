#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use crate::feat::context::strategy::token_estimator::{
    CharRatioEstimator, TiktokenCounter, TokenCounter, TokenEstimator, estimate_entry_tokens,
    estimate_tool_schema_tokens,
};
use crate::protocol::{ChatEntry, PinPosition};
use jinn_core_types::tool_types::ToolDefinition;

#[rstest::rstest]
fn char_ratio_returns_nonzero_for_empty_string() {
    // Given a char ratio estimator.
    let estimator = CharRatioEstimator;

    // When estimating an empty string.
    let tokens = estimator.estimate("");

    // Then at least 1 token is returned.
    assert!(tokens >= 1);
}

#[rstest::rstest]
fn char_ratio_estimates_reasonably() {
    // Given a char ratio estimator and a 100-character string.
    let estimator = CharRatioEstimator;
    let text = "a".repeat(100);

    // When estimating tokens.
    let tokens = estimator.estimate(&text);

    // Then approximately 25 tokens are returned (100/4 + 1 = 26).
    assert_eq!(tokens, 26);
}

#[rstest::rstest]
fn char_ratio_name() {
    // Given a char ratio estimator.
    let estimator = CharRatioEstimator;

    // Then its name is "char_ratio".
    assert_eq!(estimator.name(), "char_ratio");
}

#[rstest::rstest]
fn char_ratio_handles_unicode_correctly() {
    // Given a char ratio estimator and a string with multi-byte characters.
    let estimator = CharRatioEstimator;
    // "日本語" is 3 Unicode characters but 9 bytes in UTF-8.
    let text = "日本語";

    // When estimating tokens.
    let tokens = estimator.estimate(text);

    // Then it uses character count (3), not byte count (3 * 3/4 = 2, rounded = 1).
    assert_eq!(tokens, 1);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_user() {
    // Given a char ratio estimator and a user entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::user("hello world");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then it matches estimating the user text directly.
    assert_eq!(tokens, estimator.estimate("hello world"));
}

#[rstest::rstest]
fn estimate_entry_tokens_for_user_with_image_includes_flat_cost() {
    use crate::feat::session::chat_entry::ChatEntryKind;
    use jinn_provider::Attachment;

    // Given a char ratio estimator and a user entry carrying one image attachment.
    let estimator = CharRatioEstimator;
    let mut entry = ChatEntry::user("hello world");
    if let ChatEntryKind::User { attachments, .. } = &mut entry.kind {
        attachments.push(Attachment::image("image/png".to_owned(), vec![1, 2, 3]));
    }

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then the estimate exceeds the text-only estimate by the flat image cost.
    let text_only = estimator.estimate("hello world");
    assert!(
        tokens > text_only,
        "image attachment should add token cost: got {tokens}, text-only {text_only}"
    );
}

#[rstest::rstest]
fn estimate_entry_tokens_for_tool_call() {
    // Given a char ratio estimator and a tool call entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::tool_call("call_1", "echo", r#"{"input":"hi"}"#);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then it estimates name + arguments combined.
    assert_eq!(
        tokens,
        estimator.estimate("echo") + estimator.estimate(r#"{"input":"hi"}"#)
    );
}

#[rstest::rstest]
fn estimate_entry_tokens_for_system_is_zero() {
    // Given a char ratio estimator and an unpinned system entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::system("some status message");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then unpinned system entries contribute 0 tokens.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_pinned_system_is_nonzero() {
    // Given a char ratio estimator and a pinned system entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::system("important instruction").with_pin(PinPosition::Top);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then pinned system entries contribute tokens equal to their text.
    assert_eq!(tokens, estimator.estimate("important instruction"));
    assert!(tokens > 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_unpinned_actor_is_zero() {
    // Given a char ratio estimator and an unpinned actor entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::actor("echo", "HELLO");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then unpinned actor entries contribute 0 tokens.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_pinned_actor_is_nonzero() {
    // Given a char ratio estimator and a pinned actor entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::actor("echo", "HELLO").with_pin(PinPosition::Relative);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then pinned actor entries contribute tokens matching the formatted output.
    assert_eq!(tokens, estimator.estimate("[Actor: echo] HELLO"));
    assert!(tokens > 0);
}

#[rstest::rstest]
fn tiktoken_counter_counts_hello_world() {
    // Given a tiktoken counter with o200k_base.
    let counter = TiktokenCounter::o200k_base();

    // When counting "hello world".
    let count = counter.count("hello world");

    // Then it returns 2 tokens.
    assert_eq!(count, 2);
}

#[rstest::rstest]
fn tiktoken_counter_returns_nonzero_for_empty_string() {
    // Given a tiktoken counter.
    let counter = TiktokenCounter::o200k_base();

    // When counting an empty string.
    let count = counter.count("");

    // Then it returns 0.
    assert_eq!(count, 0);
}

#[rstest::rstest]
fn tiktoken_counter_name_is_o200k_base() {
    // Given a tiktoken counter.
    let counter = TiktokenCounter::o200k_base();

    // Then its name is "o200k_base".
    assert_eq!(counter.name(), "o200k_base");
}

#[rstest::rstest]
fn tiktoken_counter_counts_multibyte_characters() {
    // Given a tiktoken counter.
    let counter = TiktokenCounter::o200k_base();

    // When counting Japanese text.
    let count = counter.count("日本語テスト");

    // Then it returns a nonzero count.
    assert!(count > 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_thinking_is_zero() {
    // Given a char ratio estimator and a thinking entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::thinking("a long reasoning text that would normally cost tokens");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then thinking entries contribute 0 tokens.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_transient_is_zero() {
    // Given a char ratio estimator and a transient entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::transient("Welcome to jinn! Press i to start typing.");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then transient entries contribute 0 tokens.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_ignored_entry_is_zero() {
    // Given an ignored user entry.
    let estimator = CharRatioEstimator;
    let entry =
        ChatEntry::user("a fairly long message that would normally have tokens").with_ignored(true);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then ignored entries contribute 0 tokens.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_ignored_pinned_entry_is_nonzero() {
    // Given an ignored but pinned user entry.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::user("a fairly long message that would normally have tokens")
        .with_pin(PinPosition::Relative)
        .with_ignored(true);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then pin overrides ignored - tokens are still counted.
    assert!(tokens > 0);
}

#[rstest::rstest]
fn estimate_tool_schema_tokens_returns_zero_for_empty_tools() {
    // Given a char ratio estimator and no tools.
    let estimator = CharRatioEstimator;
    let tools: Vec<ToolDefinition> = vec![];

    // When estimating tool schema tokens.
    let tokens = estimate_tool_schema_tokens(&estimator, &tools);

    // Then the result is 0.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_tool_schema_tokens_returns_nonzero_for_tools() {
    // Given a char ratio estimator and one tool definition.
    let estimator = CharRatioEstimator;
    let tools = vec![ToolDefinition {
        name: "bash".to_owned(),
        description: "Execute bash commands".to_owned(),
        parameters: serde_json::json!({"type": "object"}),
        prompt_snippet: None,
        prompt_guidelines: vec![],
        server_tool_type: None,
    }];

    // When estimating tool schema tokens.
    let tokens = estimate_tool_schema_tokens(&estimator, &tools);

    // Then the result is nonzero (the serialized JSON has content).
    assert!(tokens > 0);
}

#[rstest::rstest]
fn estimate_tool_schema_tokens_sums_all_tools() {
    // Given a char ratio estimator and two tool definitions.
    let estimator = CharRatioEstimator;
    let tools = vec![
        ToolDefinition {
            name: "bash".to_owned(),
            description: "Execute bash commands".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        },
        ToolDefinition {
            name: "read".to_owned(),
            description: "Read file contents".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        },
    ];

    // When estimating tool schema tokens.
    let total = estimate_tool_schema_tokens(&estimator, &tools);

    // Then the result equals the sum of individual tool estimates.
    let first = estimate_tool_schema_tokens(&estimator, &tools[0..1]);
    let second = estimate_tool_schema_tokens(&estimator, &tools[1..2]);
    assert_eq!(total, first + second);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_default_error_is_zero() {
    // Given a Default-override Error entry - Default Errors are excluded
    // from context by `is_in_context`, so the estimator returns 0.
    // This guards against any drift where the estimator's Default branch
    // computes a nonzero count for entries the renderer would skip.
    let estimator = CharRatioEstimator;
    let entry = ChatEntry::error("some failure");

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then it returns 0 because the entry is out of context.
    assert_eq!(tokens, 0);
}

#[rstest::rstest]
fn estimate_entry_tokens_for_forced_include_error_uses_actionable_framing() {
    // Given a ForcedInclude Error entry. The estimator must compute the
    // count for the actionable framing (not the legacy `[Error]` prefix)
    // so budget estimates stay aligned with what the renderer emits.
    use crate::protocol::ContextOverride;
    let estimator = CharRatioEstimator;
    let text = "merge conflict report";
    let entry = ChatEntry::error(text).with_context_override(ContextOverride::ForcedInclude);

    // When estimating entry tokens.
    let tokens = estimate_entry_tokens(&estimator, &entry);

    // Then the estimate equals the count of the rendered string,
    // not the count of `[Error] {text}`.
    let rendered =
        format!("The user has shared the following output for you to address:\n\n{text}");
    assert_eq!(tokens, estimator.estimate(&rendered));
    assert_ne!(tokens, estimator.estimate(&format!("[Error] {text}")));
}
