//! Emission of reasoning-effort fields into the request body.
//!
//! The wire shape depends on the backend:
//! - **OpenRouter** → nested `"reasoning": { "effort": <value>, "enabled": true }`.
//!   When effort is `None`, it still emits `{ "enabled": true }` to preserve
//!   thinking-token capture in streaming responses (today's behavior).
//! - **All other OpenAI-compatible backends** → flat top-level
//!   `"reasoning_effort": <value>`. When effort is `None`, the field is omitted.
//!
//! A user-provided `reasoning` or `reasoning_effort` key in `extra_body`
//! (e.g. hand-written in `providers.toml`) is **never** clobbered.

use serde_json::Map;

use crate::openai_compat::provider_config::ProviderConfig;
use jinn_core_types::reasoning::ReasoningEffort;

/// Insert reasoning-effort fields into `extra_body` in the backend's wire shape.
///
/// Does nothing if `extra_body` already has a `reasoning` or `reasoning_effort`
/// key (the user's explicit configuration wins).
pub fn emit_reasoning_into(
    extra_body: &mut Map<String, serde_json::Value>,
    effort: Option<ReasoningEffort>,
    config: &ProviderConfig,
) {
    // Never clobber an explicit user-provided reasoning field.
    if extra_body.contains_key("reasoning") || extra_body.contains_key("reasoning_effort") {
        return;
    }

    if config.name == "OpenRouter" {
        let value = match effort {
            Some(e) => serde_json::json!({ "effort": e.as_str(), "enabled": true }),
            None => serde_json::json!({ "enabled": true }),
        };
        extra_body.insert("reasoning".to_owned(), value);
    } else if let Some(e) = effort {
        extra_body.insert("reasoning_effort".to_owned(), serde_json::json!(e.as_str()));
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

    fn openrouter_config() -> ProviderConfig {
        ProviderConfig::openrouter()
    }

    fn other_config() -> ProviderConfig {
        // ZAI is a representative non-OpenRouter OpenAI-compatible backend.
        ProviderConfig::zai()
    }

    #[rstest::rstest]
    fn openrouter_with_effort_emits_nested_effort_and_enabled() {
        // Given an empty extra_body and OpenRouter with High effort.
        let config = openrouter_config();
        let mut extra_body = Map::new();

        // When emitting reasoning.
        emit_reasoning_into(&mut extra_body, Some(ReasoningEffort::High), &config);

        // Then the nested reasoning.effort and enabled:true are present.
        let reasoning = extra_body.get("reasoning").expect("reasoning present");
        assert_eq!(
            reasoning,
            &serde_json::json!({ "effort": "high", "enabled": true })
        );
    }

    #[rstest::rstest]
    fn openrouter_without_effort_emits_enabled_only() {
        // Given an empty extra_body and OpenRouter with no effort.
        let config = openrouter_config();
        let mut extra_body = Map::new();

        // When emitting reasoning with None effort.
        emit_reasoning_into(&mut extra_body, None, &config);

        // Then reasoning is {enabled:true} only (no regression vs today).
        let reasoning = extra_body.get("reasoning").expect("reasoning present");
        assert_eq!(reasoning, &serde_json::json!({ "enabled": true }));
    }

    #[rstest::rstest]
    fn other_backend_with_effort_emits_flat_field() {
        // Given an empty extra_body and a non-OpenRouter backend with Low effort.
        let config = other_config();
        let mut extra_body = Map::new();

        // When emitting reasoning.
        emit_reasoning_into(&mut extra_body, Some(ReasoningEffort::Low), &config);

        // Then the flat reasoning_effort field is present.
        assert_eq!(
            extra_body.get("reasoning_effort"),
            Some(&serde_json::json!("low"))
        );
        // And no nested reasoning key was added.
        assert!(extra_body.get("reasoning").is_none());
    }

    #[rstest::rstest]
    fn other_backend_without_effort_omits_field() {
        // Given an empty extra_body and a non-OpenRouter backend with no effort.
        let config = other_config();
        let mut extra_body = Map::new();

        // When emitting reasoning with None effort.
        emit_reasoning_into(&mut extra_body, None, &config);

        // Then neither reasoning key is present.
        assert!(extra_body.get("reasoning").is_none());
        assert!(extra_body.get("reasoning_effort").is_none());
    }

    #[rstest::rstest]
    fn user_provided_reasoning_field_is_not_clobbered() {
        // Given an extra_body that already has a user-provided reasoning field.
        let config = openrouter_config();
        let user_value = serde_json::json!({ "max_tokens": 4096 });
        let mut extra_body = Map::new();
        extra_body.insert("reasoning".to_owned(), user_value.clone());

        // When emitting reasoning with a conflicting effort.
        emit_reasoning_into(&mut extra_body, Some(ReasoningEffort::High), &config);

        // Then the user's reasoning field is preserved unchanged.
        assert_eq!(extra_body.get("reasoning"), Some(&user_value));
    }

    #[rstest::rstest]
    fn user_provided_reasoning_effort_field_is_not_clobbered() {
        // Given an extra_body that already has a user-provided flat reasoning_effort.
        let config = other_config();
        let mut extra_body = Map::new();
        extra_body.insert("reasoning_effort".to_owned(), serde_json::json!("max"));

        // When emitting reasoning with a conflicting effort.
        emit_reasoning_into(&mut extra_body, Some(ReasoningEffort::Low), &config);

        // Then the user's reasoning_effort field is preserved unchanged.
        assert_eq!(
            extra_body.get("reasoning_effort"),
            Some(&serde_json::json!("max"))
        );
    }
}
