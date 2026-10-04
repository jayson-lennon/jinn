//! Reading `[[stream_rules.entry]]` from `jinn.toml`.
//!
//! One function, because the section is read once at activation and never
//! cached: a reload is observed by the next launch, and a rule the user
//! deleted is not left armed by a stale copy.

use jinn_config::ConfigLayer;
use jinn_preferences_config::schemas::StreamRuleConfig;

/// Reads the configured stream rules, in file order.
///
/// A malformed section reads as no rules rather than failing activation: a
/// typo in the rules list must not stop jinn from starting.
pub fn read_rules(config: &ConfigLayer) -> Vec<StreamRuleConfig> {
    match config.get_list::<StreamRuleConfig>() {
        Ok(rules) => rules,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "stream rules section is malformed, continuing with no stream rules"
            );
            Vec::new()
        }
    }
}
