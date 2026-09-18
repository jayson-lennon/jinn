//! Reasoning effort configuration for reasoning-capable models.
//!
//! [`ReasoningEffort`] is the user-facing dial controlling how hard a
//! reasoning model thinks before answering. It maps onto two wire shapes
//! depending on the provider backend:
//!
//! - **OpenRouter** → nested `"reasoning": { "effort": <value>, "enabled": true }`
//! - **All other OpenAI-compatible backends** → flat top-level `"reasoning_effort": <value>`
//!
//! The variant set is the **superset** of what OpenAI, OpenRouter, and ZAI
//! accept. Per the v1 "dumb" design, whatever the user picks is sent verbatim;
//! the provider returns an error if a value is unsupported for a given model.
//!
//! See [OpenRouter reasoning tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).

use serde::{Deserialize, Serialize};

/// How much reasoning effort a reasoning-capable model should apply.
///
/// Serializes to and from its lowercase wire string (e.g. `"high"`, `"max"`).
/// Deserialization is **strict**: an unknown string is an error rather than
/// silently falling back to `None`, so a malformed preference is surfaced
/// instead of swallowed.
///
/// Variant set rationale:
/// - `Max` — ZAI's default (strongest level).
/// - `Xhigh` — OpenRouter and ZAI.
/// - `High`, `Medium`, `Low`, `Minimal` — OpenAI, OpenRouter, and ZAI.
/// - `None` — OpenRouter and ZAI accept it (skip thinking on ZAI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// ZAI's strongest level (its default).
    Max,
    /// Extra-high effort (OpenRouter, ZAI).
    Xhigh,
    /// High effort (OpenAI, OpenRouter, ZAI).
    High,
    /// Medium effort (OpenAI, OpenRouter, ZAI).
    Medium,
    /// Low effort (OpenAI, OpenRouter, ZAI).
    Low,
    /// Minimal effort (OpenAI, OpenRouter, ZAI).
    Minimal,
    /// Skip reasoning (OpenRouter, ZAI).
    None,
}

impl ReasoningEffort {
    /// Returns the wire string this variant serializes to.
    ///
    /// Equivalent to `serde_json::to_value(self)` but allocation-free and
    /// infallible, so request builders can use it without pulling in serde
    /// serialization plumbing for a single string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Xhigh => "xhigh",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Minimal => "minimal",
            Self::None => "none",
        }
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

    #[rstest::rstest]
    #[case(ReasoningEffort::Max, "max")]
    #[case(ReasoningEffort::Xhigh, "xhigh")]
    #[case(ReasoningEffort::High, "high")]
    #[case(ReasoningEffort::Medium, "medium")]
    #[case(ReasoningEffort::Low, "low")]
    #[case(ReasoningEffort::Minimal, "minimal")]
    #[case(ReasoningEffort::None, "none")]
    fn serializes_to_lowercase_wire_string(
        #[case] effort: ReasoningEffort,
        #[case] expected: &str,
    ) {
        // Given a ReasoningEffort variant.
        // When serializing to a JSON value.
        // Then it produces the lowercase wire string.
        let value = serde_json::to_value(effort).expect("serialize");
        assert_eq!(value, serde_json::json!(expected));
    }

    #[rstest::rstest]
    #[case("max", ReasoningEffort::Max)]
    #[case("xhigh", ReasoningEffort::Xhigh)]
    #[case("high", ReasoningEffort::High)]
    #[case("medium", ReasoningEffort::Medium)]
    #[case("low", ReasoningEffort::Low)]
    #[case("minimal", ReasoningEffort::Minimal)]
    #[case("none", ReasoningEffort::None)]
    fn deserializes_from_lowercase_wire_string(
        #[case] wire: &str,
        #[case] expected: ReasoningEffort,
    ) {
        // Given a lowercase wire string.
        // When deserializing.
        // Then it maps to the expected variant.
        let value = serde_json::from_value::<ReasoningEffort>(serde_json::json!(wire))
            .expect("deserialize");
        assert_eq!(value, expected);
    }

    #[rstest::rstest]
    #[test]
    fn unknown_effort_string_is_rejected() {
        // Given a wire string that isn't a known effort level.
        // When deserializing.
        // Then it is an error (strict — no silent fallback).
        let result = serde_json::from_value::<ReasoningEffort>(serde_json::json!("ultra"));
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[case(ReasoningEffort::Max, "max")]
    #[case(ReasoningEffort::Xhigh, "xhigh")]
    #[case(ReasoningEffort::High, "high")]
    #[case(ReasoningEffort::Medium, "medium")]
    #[case(ReasoningEffort::Low, "low")]
    #[case(ReasoningEffort::Minimal, "minimal")]
    #[case(ReasoningEffort::None, "none")]
    fn as_str_matches_serialized_form(#[case] effort: ReasoningEffort, #[case] expected: &str) {
        // Given a ReasoningEffort variant.
        // When calling as_str().
        // Then it returns the same string serde would emit.
        assert_eq!(effort.as_str(), expected);
    }
}
