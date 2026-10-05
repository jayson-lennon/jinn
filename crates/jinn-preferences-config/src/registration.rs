//! Central section registration — the launch-time fail-fast gate's roster.
//!
//! [`ConfigLayer::validate`] walks the sections registered on it, so a
//! section that is never registered is silently unvalidated: a
//! malformed table boots to a running app that quietly reads defaults.
//! This module is the single exhaustive roster, called from composition
//! before the validate call.
//!
//! Registration order is error order. `validate` fails on the *first*
//! malformed section in registration order, so sections are listed in the
//! order they appear in the shipped `default_jinn.toml` and a user is
//! told about the section nearest the top of their file.

use jinn_config::ConfigLayer;

use crate::schemas::{
    AutoPruneConfig, ChatLogConfig, CompactionConfig, CwdSelectorConfig, DiscordConfig,
    InteractiveTermPrefs, McpServersConfig, MinimapConfig, RequestRetryConfig, SkillsConfig,
    StallWatchdogConfig, StreamRuleWatchdogConfig, ToolCallWatchdogConfig, ToolsConfig,
    WebSearchConfig,
};

/// Registers every `jinn.toml` section that can be validated at launch.
///
/// A section absent from this roster is never checked, so adding a new
/// section type without adding it here is a silent hole rather than a
/// compile error. Keep the list exhaustive.
///
/// # Not every section is registered
///
/// [`jinn_config::ConfigList`] sections are deliberately absent.
/// `ConfigLayer::register` is bound to `Configurable`, which supplies
/// `from_table`; `ConfigList` has no equivalent and an absent list has
/// no `Default` for a check to layer over. So these four sections are
/// read on demand and are *not* fail-fast validated:
///
/// - `project` — [`crate::schemas::ProjectConfig`]
/// - `session_lifecycle` — [`crate::schemas::SessionLifecycle`]
/// - `stream_rules.entry` — [`crate::schemas::StreamRuleConfig`]
/// - `watchdog.stream_rules` — [`crate::schemas::StreamRuleWatchdogConfig`]
///
/// The removed `stream_rules.max_interrupts` budget is deliberately absent
/// from this roster: nothing registers it, so no code path can write it back
/// into a user's file. It is read for migration only.
/// - `tools.bash_command_policy` — [`crate::schemas::LegacyCommandPolicyRule`],
///   read only to migrate an older file into stream rules
///
/// Closing that gap means changing the `ConfigList` trait in
/// `jinn-config`, which is out of scope for centralizing the schemas.
/// Until then a malformed list section surfaces at first read rather
/// than at launch.
pub fn register_all_sections(config: &ConfigLayer) {
    config.register::<ToolsConfig>();
    config.register::<ChatLogConfig>();
    config.register::<SkillsConfig>();
    config.register::<CompactionConfig>();
    config.register::<RequestRetryConfig>();
    config.register::<WebSearchConfig>();
    config.register::<CwdSelectorConfig>();
    config.register::<MinimapConfig>();
    config.register::<StallWatchdogConfig>();
    config.register::<ToolCallWatchdogConfig>();
    config.register::<StreamRuleWatchdogConfig>();
    config.register::<AutoPruneConfig>();
    config.register::<InteractiveTermPrefs>();
    config.register::<McpServersConfig>();
    config.register::<DiscordConfig>();
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test assertions need expect/panic to state what went wrong"
    )]
    use jinn_config::{ConfigSectionError, testutil::config_layer};

    use super::register_all_sections;

    #[rstest::rstest]
    #[test]
    fn malformed_registered_section_fails_validation_naming_the_section() {
        // Given a document whose auto_prune token threshold is given a string.
        let config =
            config_layer("[context_curation.auto_prune]\naccumulation_threshold_tokens = \"x\"");

        // When the roster is registered and the layer validated.
        register_all_sections(&config);
        let result = config.validate();

        // Then validation fails naming the offending section.
        let Err(error) = result else {
            panic!("expected a malformed-section error, got Ok");
        };
        assert!(matches!(
            error,
            ConfigSectionError::Malformed { key, .. } if key == "context_curation.auto_prune"
        ));
    }

    #[rstest::rstest]
    #[test]
    fn malformed_optional_field_in_a_later_section_is_rejected() {
        // Given a document whose tools optional output cap is given a string.
        let config = config_layer("[tools]\nmax_output_lines = \"lots\"");

        // When the roster is registered and the layer validated.
        register_all_sections(&config);
        let result = config.validate();

        // Then validation fails naming the section holding the field.
        assert!(matches!(
            result,
            Err(ConfigSectionError::Malformed { key, .. }) if key == "tools"
        ));
    }

    #[rstest::rstest]
    #[test]
    fn malformed_non_table_segment_fails_validation() {
        // Given a document where a key segment on the section path is a scalar.
        let config = config_layer("[context_curation]\nauto_prune = 3");

        // When the roster is registered and the layer validated.
        register_all_sections(&config);
        let result = config.validate();

        // Then validation fails, so the shape of the path is checked too.
        assert!(matches!(
            result,
            Err(ConfigSectionError::NotATable { key, .. }) if key == "context_curation.auto_prune"
        ));
    }

    #[rstest::rstest]
    #[test]
    fn absent_sections_validate_cleanly() {
        // Given a document that declares none of the registered sections.
        let config = config_layer("");

        // When the roster is registered and the layer validated.
        register_all_sections(&config);
        let result = config.validate();

        // Then validation passes, since an absent section is not an error.
        assert!(result.is_ok());
    }

    #[rstest::rstest]
    #[test]
    fn first_malformed_section_in_document_order_is_reported() {
        // Given a document malformed in two registered sections.
        let config = config_layer(
            "[chat_log]\nmin_collapse_count = \"x\"\n\
             [context_curation.auto_prune]\naccumulation_threshold_tokens = \"x\"",
        );

        // When the roster is registered and the layer validated.
        register_all_sections(&config);
        let result = config.validate();

        // Then the earlier section in document order is the one reported.
        assert!(matches!(
            result,
            Err(ConfigSectionError::Malformed { key, .. }) if key == "chat_log"
        ));
    }
}
