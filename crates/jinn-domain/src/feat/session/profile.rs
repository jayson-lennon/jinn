//! Per-session model and persona selection.
//!
//! [`SessionProfile`] groups the model (LLM provider) and persona
//! for a single session. These fields are given "session priority" treatment:
//! picker selections update both the session profile and the global config,
//! while session load/save only touches the session's own profile.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use jinn_core_types::model_selection::ModelSelection;

/// Default persona name used when none is explicitly set.
pub(crate) const DEFAULT_PERSONA_NAME: &str = "coding-assistant";

/// Serde default for `persona_name` - ensures old serialized sessions deserialize correctly.
fn default_persona_name() -> String {
    DEFAULT_PERSONA_NAME.to_owned()
}

/// Per-session model and persona selection.
///
/// Every session carries its own model and persona. The session profile
/// is the single source of truth for "what model/persona does this session use?"
/// The global config (`jinn.toml`) holds the user's preferred defaults
/// and is updated by the picker, but session load/restore never touches it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionProfile {
    /// The model selection for this session — either a single model or an alloy.
    /// Defaults to `Single(NO_PROVIDER_ID)` — the user must select a model.
    pub model: ModelSelection,
    /// The persona name for this session. Always populated — defaults to `"coding-assistant"`.
    /// Old serialized sessions without this field deserialize to the default.
    #[serde(default = "default_persona_name")]
    pub persona_name: String,
    /// Tool names the user has explicitly disabled for this session.
    ///
    /// Opt-out model: empty set means all tools are enabled.
    /// New tools added in future versions automatically appear.
    /// Serialized as a JSON array; `#[serde(default)]` ensures legacy
    /// sessions without this field deserialize to an empty set (all enabled).
    #[serde(default)]
    pub disabled_tools: HashSet<String>,
    /// Skill names the user has explicitly disabled for this session.
    ///
    /// Opt-out model: empty set means all skills are enabled.
    /// New skills added in future versions automatically appear.
    /// `#[serde(default)]` ensures legacy sessions without this field
    /// deserialize to an empty set (all enabled).
    #[serde(default)]
    pub disabled_skills: HashSet<String>,
    /// Reasoning effort for this session.
    ///
    /// Session-owned: seeded from the last-used `reasoning_effort` in
    /// `state.toml` at creation, then owned by the session. Never re-resolved
    /// against the live global (see `resolve_effort`). `None` means "send no
    /// effort field; let the provider decide". Legacy sessions deserialize to `None`.
    #[serde(default)]
    pub reasoning_effort: Option<crate::ReasoningEffort>,
    /// Pinned OpenRouter routing endpoint for prefix-cache affinity.
    ///
    /// `None` (the default) lets OpenRouter route automatically. When `Some`,
    /// dispatch forces this endpoint with `provider.order = [tag]` and
    /// `allow_fallbacks = false` — but only for a `Single` model served via the
    /// OpenRouter backend. Ignored for alloys and all other backends.
    /// Legacy sessions without this field deserialize to `None`.
    #[serde(default)]
    pub endpoint: Option<crate::feat::endpoint::Endpoint>,
}

impl Default for SessionProfile {
    fn default() -> Self {
        Self {
            model: ModelSelection::default(),
            persona_name: DEFAULT_PERSONA_NAME.to_owned(),
            disabled_tools: HashSet::new(),
            disabled_skills: HashSet::new(),
            reasoning_effort: None,
            endpoint: None,
        }
    }
}

/// The per-session defaults derived from `jinn.toml` at session creation.
///
/// One instance is computed from the user's preferences for each newly
/// created session (welcome session, lifecycle-created sessions, and
/// replacement sessions after close/archive). It carries every config-seeded
/// value in one place so the creation paths can't drift apart.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSeed {
    /// Tool names to start the session with disabled.
    pub disabled_tools: HashSet<String>,
    /// Skill names to start the session with disabled.
    pub disabled_skills: HashSet<String>,
    /// MCP servers to start the session with enabled.
    pub enabled_mcp: std::collections::BTreeSet<String>,
}

impl SessionSeed {
    /// Derives the seed for a new session from user preferences.
    ///
    /// Tool/skill disablement copies verbatim; enabled MCP servers are the
    /// names of configured `[mcp_server.<name>]` entries whose
    /// `auto_enable` flag is on.
    #[must_use]
    pub fn from_preferences(prefs: &jinn_preferences_config::UserPreferences) -> Self {
        Self {
            disabled_tools: prefs.disabled_tools.iter().cloned().collect(),
            disabled_skills: prefs.disabled_skills.iter().cloned().collect(),
            enabled_mcp: prefs
                .mcp_server
                .iter()
                .filter(|(_, cfg)| cfg.auto_enable)
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }

    /// True when no MCP server would be auto-enabled.
    ///
    /// Creation sites publish an [`McpEnablementChanged`](jinn_mcp_msg::McpEnablementChanged)
    /// event only when this returns false — with nothing desired there is
    /// nothing for the coordinator to reconcile, and skipping the broadcast
    /// avoids waking a subscriber on every session creation.
    #[must_use]
    pub fn has_auto_enabled_mcp(&self) -> bool {
        !self.enabled_mcp.is_empty()
    }
}

impl SessionProfile {
    /// Creates a profile seeded from config values.
    pub fn from_config(model: String) -> Self {
        Self {
            model: ModelSelection::Single(model),
            persona_name: DEFAULT_PERSONA_NAME.to_owned(),
            disabled_tools: HashSet::new(),
            disabled_skills: HashSet::new(),
            reasoning_effort: None,
            endpoint: None,
        }
    }

    /// Creates a profile from a [`ModelSelection`] (single model or alloy).
    pub fn from_model_selection(model: ModelSelection) -> Self {
        Self {
            model,
            persona_name: DEFAULT_PERSONA_NAME.to_owned(),
            disabled_tools: HashSet::new(),
            disabled_skills: HashSet::new(),
            reasoning_effort: None,
            endpoint: None,
        }
    }

    /// Creates a profile with all fields specified.
    pub fn new(
        model: ModelSelection,
        persona_name: String,
        disabled_tools: HashSet<String>,
        disabled_skills: HashSet<String>,
        reasoning_effort: Option<crate::ReasoningEffort>,
        endpoint: Option<crate::feat::endpoint::Endpoint>,
    ) -> Self {
        Self {
            model,
            persona_name,
            disabled_tools,
            disabled_skills,
            reasoning_effort,
            endpoint,
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
    use jinn_core_types::NO_PROVIDER_ID;

    use super::*;

    #[rstest::rstest]
    fn default_has_no_provider() {
        // Given a default SessionProfile.
        let profile = SessionProfile::default();

        // Then model is Single(NO_PROVIDER_ID).
        assert_eq!(
            profile.model,
            ModelSelection::Single(NO_PROVIDER_ID.to_owned())
        );
    }

    #[rstest::rstest]
    fn from_config_seeds_model() {
        // Given a model.
        let profile = SessionProfile::from_config("ollama/llama3".to_owned());

        // Then the profile uses that model.
        assert_eq!(
            profile.model,
            ModelSelection::Single("ollama/llama3".to_owned())
        );
    }

    #[rstest::rstest]
    fn disabled_tools_round_trips_through_serde() {
        // Given a profile with disabled tools.
        let mut disabled = HashSet::new();
        disabled.insert("bash".to_owned());
        disabled.insert("edit".to_owned());
        let profile = SessionProfile::new(
            ModelSelection::Single("ollama/llama3".to_owned()),
            "coding-assistant".to_owned(),
            disabled.clone(),
            HashSet::new(),
            None,
            None,
        );

        // When serialized and deserialized.
        let json = serde_json::to_string(&profile).expect("serialize");
        let restored: SessionProfile = serde_json::from_str(&json).expect("deserialize");

        // Then disabled_tools is preserved.
        assert_eq!(restored.disabled_tools, disabled);
    }

    #[rstest::rstest]
    fn disabled_skills_round_trips_through_serde() {
        // Given a profile with disabled skills.
        let mut disabled = HashSet::new();
        disabled.insert("phased-task-loop".to_owned());
        disabled.insert("web-coder".to_owned());
        let profile = SessionProfile::new(
            ModelSelection::Single("ollama/llama3".to_owned()),
            "coding-assistant".to_owned(),
            HashSet::new(),
            disabled.clone(),
            None,
            None,
        );

        // When serialized and deserialized.
        let json = serde_json::to_string(&profile).expect("serialize");
        let restored: SessionProfile = serde_json::from_str(&json).expect("deserialize");

        // Then disabled_skills is preserved.
        assert_eq!(restored.disabled_skills, disabled);
    }

    #[rstest::rstest]
    fn legacy_json_without_disabled_skills_deserializes_to_empty_set() {
        // Given JSON from an older version that lacks disabled_skills.
        let json = r#"{"model":{"single":"ollama/llama3"},"persona_name":"coding-assistant","disabled_tools":[]}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then disabled_skills is empty (all skills enabled).
        assert!(profile.disabled_skills.is_empty());
    }

    #[rstest::rstest]
    fn legacy_json_without_reasoning_effort_deserializes_to_none() {
        // Given JSON from an older version that lacks reasoning_effort.
        let json = r#"{"model":{"single":"ollama/llama3"},"persona_name":"coding-assistant","disabled_tools":[],"disabled_skills":[]}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then reasoning_effort is None ("provider decides"; the live global is
        // never consulted after creation, so this stays None until set).
        assert!(profile.reasoning_effort.is_none());
    }

    #[rstest::rstest]
    fn reasoning_effort_round_trips_through_serialization() {
        // Given a profile with a saved effort of High.
        let profile = {
            let mut p = SessionProfile::from_config("ollama/llama3".to_owned());
            p.reasoning_effort = Some(crate::ReasoningEffort::High);
            p
        };

        // When serializing then deserializing (the persist/load path).
        let json = serde_json::to_string(&profile).expect("serialize");
        let reloaded: SessionProfile = serde_json::from_str(&json).expect("deserialize");

        // Then the saved effort is preserved.
        assert_eq!(
            reloaded.reasoning_effort,
            Some(crate::ReasoningEffort::High),
            "saved effort must survive the serialize/deserialize round trip"
        );
    }

    #[rstest::rstest]
    fn default_disabled_skills_is_empty() {
        let profile = SessionProfile::default();
        assert!(profile.disabled_skills.is_empty());
    }

    #[rstest::rstest]
    fn legacy_json_without_disabled_tools_deserializes_to_empty_set() {
        // Given JSON from an older version that lacks disabled_tools.
        let json = r#"{"model":{"single":"ollama/llama3"},"persona_name":"coding-assistant"}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then disabled_tools is empty (all tools enabled).
        assert!(profile.disabled_tools.is_empty());
    }

    #[rstest::rstest]
    fn default_disabled_tools_is_empty() {
        let profile = SessionProfile::default();
        assert!(profile.disabled_tools.is_empty());
    }

    #[rstest::rstest]
    fn default_persona_name_is_coding_assistant() {
        // Given a default SessionProfile.
        let profile = SessionProfile::default();

        // Then persona_name is "coding-assistant" (not empty, not "xyzzy").
        assert_eq!(profile.persona_name, "coding-assistant");
    }

    #[rstest::rstest]
    fn legacy_json_without_persona_uses_default() {
        // Given JSON from an older version that lacks persona_name.
        let json = r#"{"model":{"single":"ollama/llama3"}}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then persona_name defaults to "coding-assistant".
        assert_eq!(profile.persona_name, "coding-assistant");
    }

    #[rstest::rstest]
    fn legacy_json_with_strategy_fields_is_ignored() {
        // Given JSON from an older version that still carries strategy / sliding_window_size.
        // These fields are now removed from SessionProfile; serde must ignore them silently.
        let json = r#"{"model":{"single":"ollama/llama3"},"strategy":"passthrough","persona_name":"coding-assistant","sliding_window_size":5}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then the known fields load normally.
        assert_eq!(
            profile.model,
            ModelSelection::Single("ollama/llama3".to_owned())
        );
        assert_eq!(profile.persona_name, "coding-assistant");
    }

    #[rstest::rstest]
    fn endpoint_round_trips_through_serde() {
        // Given a profile with a pinned endpoint.
        let profile = {
            let mut p = SessionProfile::from_config("openrouter/anthropic/claude".to_owned());
            p.endpoint = Some(crate::feat::endpoint::Endpoint {
                tag: "anthropic".to_owned(),
                provider_name: "Anthropic".to_owned(),
            });
            p
        };

        // When serializing then deserializing (the persist/load path).
        let json = serde_json::to_string(&profile).expect("serialize");
        let reloaded: SessionProfile = serde_json::from_str(&json).expect("deserialize");

        // Then the pinned endpoint is preserved.
        assert_eq!(
            reloaded.endpoint,
            Some(crate::feat::endpoint::Endpoint {
                tag: "anthropic".to_owned(),
                provider_name: "Anthropic".to_owned(),
            }),
            "endpoint pin must survive the serialize/deserialize round trip"
        );
    }

    #[rstest::rstest]
    fn legacy_json_without_endpoint_deserializes_to_none() {
        // Given JSON from an older version that lacks endpoint.
        let json = r#"{"model":{"single":"ollama/llama3"},"persona_name":"coding-assistant","disabled_tools":[],"disabled_skills":[],"reasoning_effort":null}"#;

        // When deserialized.
        let profile: SessionProfile = serde_json::from_str(json).expect("deserialize");

        // Then endpoint is None (auto-route; legacy sessions have no pin).
        assert!(profile.endpoint.is_none());
    }

    #[rstest::rstest]
    fn new_round_trips_with_endpoint_param() {
        // Given a profile built via new() with a pinned endpoint.
        let profile = SessionProfile::new(
            ModelSelection::Single("openrouter/openai/gpt-4o".to_owned()),
            "coding-assistant".to_owned(),
            HashSet::new(),
            HashSet::new(),
            None,
            Some(crate::feat::endpoint::Endpoint {
                tag: "azure".to_owned(),
                provider_name: "Azure".to_owned(),
            }),
        );

        // When serializing then deserializing.
        let json = serde_json::to_string(&profile).expect("serialize");
        let reloaded: SessionProfile = serde_json::from_str(&json).expect("deserialize");

        // Then the endpoint passed to new() is preserved.
        assert_eq!(
            reloaded.endpoint.as_ref().map(|e| e.tag.as_str()),
            Some("azure")
        );
    }

    #[rstest::rstest]
    fn session_seed_from_default_preferences_is_all_enabled() {
        // Given default (empty) user preferences.
        let prefs = jinn_preferences_config::UserPreferences::default();

        // When deriving the seed.
        let seed = SessionSeed::from_preferences(&prefs);

        // Then nothing is disabled and nothing auto-enabled.
        assert!(seed.disabled_tools.is_empty());
        assert!(seed.disabled_skills.is_empty());
        assert!(!seed.has_auto_enabled_mcp());
    }

    #[rstest::rstest]
    fn session_seed_copies_disablement_sets_from_preferences() {
        // Given preferences listing disabled tools, skills, and an
        // auto-enabled MCP server.
        let prefs = jinn_preferences_config::UserPreferences {
            disabled_tools: ["bash", "mcp__excalimate__draw"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            disabled_skills: ["phased-task-loop"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            mcp_server: [(
                "excalimate".to_owned(),
                jinn_mcp_msg::McpServerConfig {
                    command: Some("npx".to_owned()),
                    auto_enable: true,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        // When deriving the seed.
        let seed = SessionSeed::from_preferences(&prefs);

        // Then the disablement sets carry the listed names.
        assert!(seed.disabled_tools.contains("bash"));
        assert!(seed.disabled_tools.contains("mcp__excalimate__draw"));
        assert!(seed.disabled_skills.contains("phased-task-loop"));
        // And only servers with auto_enable=true are enabled.
        assert_eq!(
            seed.enabled_mcp.iter().collect::<Vec<_>>(),
            vec!["excalimate"]
        );
        assert!(seed.has_auto_enabled_mcp());
    }

    #[rstest::rstest]
    fn session_seed_excludes_servers_without_auto_enable() {
        // Given preferences with two servers where one has auto_enable off.
        let prefs = jinn_preferences_config::UserPreferences {
            mcp_server: [
                (
                    "on".to_owned(),
                    jinn_mcp_msg::McpServerConfig {
                        command: Some("a".to_owned()),
                        auto_enable: true,
                        ..Default::default()
                    },
                ),
                (
                    "off".to_owned(),
                    jinn_mcp_msg::McpServerConfig {
                        command: Some("b".to_owned()),
                        auto_enable: false,
                        ..Default::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };

        // When deriving the seed.
        let seed = SessionSeed::from_preferences(&prefs);

        // Then only the auto-enabled server is in the desired set.
        assert_eq!(seed.enabled_mcp.iter().collect::<Vec<_>>(), vec!["on"]);
    }
}
