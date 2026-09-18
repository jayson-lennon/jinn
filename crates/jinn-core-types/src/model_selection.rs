//! Model selection for LLM dispatch — single model or rotating alloy.
//!
//! An alloy rotates through multiple models on each LLM call, combining
//! complementary strengths of different providers within a single conversation.
//! Based on the technique described in XBOW's "Agents Built From Alloys" research.
//!
//! The type persists across `state.toml`, session storage (SQLite), and the
//! legacy session schema, so it lives in the foundational value-type crate:
//! the preferences config, the provider, and the kernel all need it without
//! depending on each other.

use rand::Rng;
use serde::{Deserialize, Serialize};

/// Sentinel model ID used when no real provider is configured.
///
/// [`ModelSelection::default`] resolves to `Single(NO_PROVIDER_ID)`; the
/// no-provider LLM factory and session bootstrap logic key off this value.
pub const NO_PROVIDER_ID: &str = "__no_provider__";

/// Strategy for rotating through alloy members.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AlloyStrategy {
    /// Advance through models in order, wrapping around.
    RoundRobin { index: usize },
    /// Pick a random model on each call.
    Random {
        #[serde(default)]
        last_index: usize,
    },
}

/// How a session selects which model handles each LLM call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelection {
    /// A single fixed model.
    Single(String),
    /// Rotate through multiple models using the given strategy.
    Alloy {
        models: Vec<String>,
        strategy: AlloyStrategy,
    },
}

/// Borrowed view of alloy data (models + strategy).
pub struct AlloyData<'a> {
    pub models: &'a [String],
    pub strategy: &'a AlloyStrategy,
}

impl Default for ModelSelection {
    fn default() -> Self {
        Self::Single(NO_PROVIDER_ID.to_owned())
    }
}

impl std::fmt::Display for ModelSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(s) => write!(f, "{s}"),
            Self::Alloy { models, .. } => write!(f, "alloy({})", models.join(", ")),
        }
    }
}

impl ModelSelection {
    /// Resolve to a concrete model ID for this LLM call.
    ///
    /// For `Single`, returns the model string unchanged.
    /// For `Alloy` with `RoundRobin`, returns the current model and advances the index.
    /// For `Alloy` with `Random`, picks a random member.
    ///
    /// # Panics
    ///
    /// Panics if `Alloy` contains an empty models list.
    pub fn resolve_model(&mut self) -> String {
        match self {
            Self::Single(s) => s.clone(),
            Self::Alloy { models, strategy } => {
                let idx = match strategy {
                    AlloyStrategy::RoundRobin { index } => {
                        let i = *index;
                        *index = (*index + 1) % models.len();
                        i
                    }
                    AlloyStrategy::Random { last_index } => {
                        let idx = rand::rng().random_range(0..models.len());
                        *last_index = idx;
                        idx
                    }
                };
                #[expect(clippy::indexing_slicing, reason = "idx bounded by modulo or len")]
                models[idx].clone()
            }
        }
    }

    /// Whether this selection represents "no provider configured".
    pub fn is_no_provider(&self) -> bool {
        matches!(self, Self::Single(s) if s == NO_PROVIDER_ID)
    }

    /// Returns the inner string if `Single`, `None` if `Alloy`.
    pub fn as_single(&self) -> Option<&str> {
        match self {
            Self::Single(s) => Some(s),
            Self::Alloy { .. } => None,
        }
    }

    /// Returns the model ID that was last used (or would be used next).
    ///
    /// For `Single`, returns the model string. For `Alloy`, returns the
    /// model at the current round-robin index (or the first model for random).
    pub fn last_model(&self) -> Option<&str> {
        match self {
            Self::Single(s) => Some(s),
            Self::Alloy { models, strategy } => {
                let idx = match strategy {
                    AlloyStrategy::RoundRobin { index } => *index,
                    AlloyStrategy::Random { last_index } => *last_index,
                };
                models.get(idx).map(String::as_str)
            }
        }
    }

    /// Returns a display string for the selection.
    ///
    /// For `Single`, returns the model string. For `Alloy`, returns the first
    /// model in the list (used for provider filtering where the exact model
    /// doesn't matter — any member's provider suffices).
    pub fn display_str(&self) -> &str {
        match self {
            Self::Single(s) => s,
            Self::Alloy { models, .. } => models.first().map_or("", |s| s.as_str()),
        }
    }

    /// Returns the provider name for this selection.
    ///
    /// The provider name is the prefix of the model string before the first
    /// `/` (e.g. `"openrouter"` from `openrouter/openai/gpt-oss-120b`).
    /// Returns `""` for models with no `/` separator.
    ///
    /// Used to filter server tools by provider — see
    /// `ToolDefinition::available_for_provider` in `jinn-provider`.
    #[must_use]
    pub fn provider_name(&self) -> &str {
        self.display_str().split('/').next().unwrap_or("")
    }

    /// Returns the alloy data if `Alloy`, `None` if `Single`.
    pub fn as_alloy(&self) -> Option<AlloyData<'_>> {
        match self {
            Self::Alloy { models, strategy } => Some(AlloyData { models, strategy }),
            Self::Single(_) => None,
        }
    }

    /// Wrap a single model string as [`ModelSelection::Single`].
    pub fn from_single(s: String) -> Self {
        Self::Single(s)
    }
}

/// Serde compatibility module for `last_model` in the TOML state file.
///
/// Accepts both the legacy bare-string format (`last_model = "ollama/llama3"`)
/// and the new enum format (`last_model = {single = "ollama/llama3"}`).
/// Serialization always writes the new format.
///
/// Public so crates that persist a `last_model` field (preferences config,
/// session schema) can reference it from a `#[serde(with = ...)]` attribute.
pub mod last_model_compat {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::ModelSelection;

    /// Serializes the optional selection.
    ///
    /// # Errors
    ///
    /// Forwarded from `ModelSelection`'s own `Serialize` impl.
    pub fn serialize<S: Serializer>(
        value: &Option<ModelSelection>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(value, serializer)
    }

    /// Deserializes either the full selection or a legacy bare model name.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error when neither shape matches.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ModelSelection>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Compat {
            Model(ModelSelection),
            Bare(String),
        }

        let compat = Compat::deserialize(deserializer)?;
        Ok(Some(match compat {
            Compat::Model(ms) => ms,
            Compat::Bare(s) => ModelSelection::Single(s),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_models() -> Vec<String> {
        vec![
            "provider-a/model-1".to_owned(),
            "provider-b/model-2".to_owned(),
            "provider-c/model-3".to_owned(),
        ]
    }

    #[rstest::rstest]
    #[test]
    fn single_serializes_as_bare_string_key() {
        // Given a Single model selection.
        let selection = ModelSelection::Single("ollama/llama3".to_owned());

        // When serializing.
        let json = serde_json::to_string(&selection).unwrap();

        // Then it produces the externally-tagged format.
        assert_eq!(json, r#"{"single":"ollama/llama3"}"#);
    }

    #[rstest::rstest]
    #[test]
    fn single_round_trips_through_serde() {
        // Given a Single model selection.
        let selection = ModelSelection::Single("ollama/llama3".to_owned());

        // When serializing and deserializing.
        let json = serde_json::to_string(&selection).unwrap();
        let back: ModelSelection = serde_json::from_str(&json).unwrap();

        // Then it equals the original.
        assert_eq!(back, selection);
    }

    #[rstest::rstest]
    #[test]
    fn alloy_with_round_robin_round_trips_through_serde() {
        // Given an Alloy with RoundRobin strategy.
        let selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&selection).unwrap();
        let back: ModelSelection = serde_json::from_str(&json).unwrap();

        // Then it equals the original.
        assert_eq!(back, selection);
    }

    #[rstest::rstest]
    #[test]
    fn alloy_with_random_round_trips_through_serde() {
        // Given an Alloy with Random strategy.
        let selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::Random { last_index: 0 },
        };

        // When serializing and deserializing.
        let json = serde_json::to_string(&selection).unwrap();
        let back: ModelSelection = serde_json::from_str(&json).unwrap();

        // Then it equals the original.
        assert_eq!(back, selection);
    }

    #[rstest::rstest]
    #[test]
    fn default_is_single_no_provider() {
        // Given the default model selection.
        let selection = ModelSelection::default();

        // Then it is Single with the no-provider sentinel.
        assert_eq!(selection, ModelSelection::Single(NO_PROVIDER_ID.to_owned()));
    }

    #[rstest::rstest]
    #[test]
    fn resolve_model_on_single_returns_the_string() {
        // Given a Single model selection.
        let mut selection = ModelSelection::Single("ollama/llama3".to_owned());

        // When resolving.
        let model = selection.resolve_model();

        // Then it returns the model string.
        assert_eq!(model, "ollama/llama3");
    }

    #[rstest::rstest]
    #[test]
    fn resolve_model_on_round_robin_cycles_through_models() {
        // Given an Alloy with RoundRobin starting at index 0.
        let mut selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        };

        // When resolving three times.
        let first = selection.resolve_model();
        let second = selection.resolve_model();
        let third = selection.resolve_model();

        // Then they cycle in order.
        assert_eq!(first, "provider-a/model-1");
        assert_eq!(second, "provider-b/model-2");
        assert_eq!(third, "provider-c/model-3");
    }

    #[rstest::rstest]
    #[test]
    fn resolve_model_on_round_robin_wraps_around() {
        // Given an Alloy with RoundRobin starting at index 2 (last model).
        let mut selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::RoundRobin { index: 2 },
        };

        // When resolving.
        let result = selection.resolve_model();

        // Then it returns the last model.
        assert_eq!(result, "provider-c/model-3");
        // And the index wraps to 0.
        let next = selection.resolve_model();
        assert_eq!(next, "provider-a/model-1");
    }

    #[rstest::rstest]
    #[test]
    fn resolve_model_on_random_returns_a_member() {
        // Given an Alloy with Random strategy.
        let models = test_models();
        let mut selection = ModelSelection::Alloy {
            models: models.clone(),
            strategy: AlloyStrategy::Random { last_index: 0 },
        };

        // When resolving multiple times.
        for _ in 0..50 {
            let result = selection.resolve_model();

            // Then each result is a member of the models list.
            assert!(models.contains(&result));
        }
    }

    #[rstest::rstest]
    #[test]
    fn is_no_provider_true_for_sentinel() {
        // Given a Single with the no-provider sentinel.
        let selection = ModelSelection::Single(NO_PROVIDER_ID.to_owned());

        // Then is_no_provider is true.
        assert!(selection.is_no_provider());
    }

    #[rstest::rstest]
    #[test]
    fn is_no_provider_false_for_real_model() {
        // Given a Single with a real model.
        let selection = ModelSelection::Single("ollama/llama3".to_owned());

        // Then is_no_provider is false.
        assert!(!selection.is_no_provider());
    }

    #[rstest::rstest]
    #[test]
    fn is_no_provider_false_for_alloy() {
        // Given an Alloy selection.
        let selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::Random { last_index: 0 },
        };

        // Then is_no_provider is false.
        assert!(!selection.is_no_provider());
    }

    #[rstest::rstest]
    #[test]
    fn as_single_returns_str_for_single() {
        // Given a Single model selection.
        let selection = ModelSelection::Single("ollama/llama3".to_owned());

        // When calling as_single.
        // Then it returns Some with the inner string.
        assert_eq!(selection.as_single(), Some("ollama/llama3"));
    }

    #[rstest::rstest]
    #[test]
    fn as_single_returns_none_for_alloy() {
        // Given an Alloy selection.
        let selection = ModelSelection::Alloy {
            models: test_models(),
            strategy: AlloyStrategy::Random { last_index: 0 },
        };

        // When calling as_single.
        // Then it returns None.
        assert!(selection.as_single().is_none());
    }

    #[rstest::rstest]
    #[test]
    fn from_single_wraps_string() {
        // Given a model string.
        // When wrapping in from_single.
        let selection = ModelSelection::from_single("ollama/llama3".to_owned());

        // Then it is a Single variant.
        assert_eq!(
            selection,
            ModelSelection::Single("ollama/llama3".to_owned())
        );
    }

    #[rstest::rstest]
    #[test]
    fn provider_name_returns_prefix_for_single_model() {
        // Given a Single model with provider/model/sub format.
        let selection = ModelSelection::Single("openrouter/openai/gpt-oss-120b".to_owned());

        // Then provider_name returns the prefix before the first slash.
        assert_eq!(selection.provider_name(), "openrouter");
    }

    #[rstest::rstest]
    #[test]
    fn provider_name_returns_whole_string_for_no_slash() {
        // Given a Single model with no slash (bare model name).
        let selection = ModelSelection::Single("llama3".to_owned());

        // Then provider_name returns the whole string (a bare name is not
        // "openrouter", so web search is still correctly filtered out).
        assert_eq!(selection.provider_name(), "llama3");
    }

    #[rstest::rstest]
    #[test]
    fn provider_name_uses_first_model_of_alloy() {
        // Given an Alloy whose first member is on openrouter.
        let selection = ModelSelection::Alloy {
            models: vec![
                "openrouter/openai/gpt-oss-120b".to_owned(),
                "zai/glm-4.6".to_owned(),
            ],
            strategy: AlloyStrategy::RoundRobin { index: 0 },
        };

        // Then provider_name returns the first member's provider.
        assert_eq!(selection.provider_name(), "openrouter");
    }
}
