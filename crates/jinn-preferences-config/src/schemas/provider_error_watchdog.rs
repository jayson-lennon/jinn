//! Provider-error-watchdog configuration schema — the `jinn.toml`
//! `[provider_error_watchdog]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default escalating waits, in seconds: one entry per retry attempt.
const DEFAULT_WAITS_SECS: &[u64] = &[10, 30, 60];

/// Provider-error-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each retry decision, so an edit applies at the next retry.
///
/// Serialized as `[provider_error_watchdog]` in `jinn.toml`. When a
/// provider stream fails with a retryable error, the watchdog waits
/// `waits_secs[attempt]` seconds and re-dispatches; a failure past the
/// list surrenders to the ordinary error settle. An empty list disables
/// the watchdog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderErrorWatchdogConfig {
    /// Seconds to wait before each successive retry. Attempt N waits
    /// `waits_secs[N-1]`; a failure past the list surrenders. An empty
    /// list disables the watchdog. Default: [10, 30, 60].
    #[serde(default = "default_waits_secs")]
    pub waits_secs: Vec<u64>,
}

fn default_waits_secs() -> Vec<u64> {
    DEFAULT_WAITS_SECS.to_vec()
}

impl ProviderErrorWatchdogConfig {
    /// The waits with every element floored at one second.
    ///
    /// A zero entry would fire on the next tick regardless of intent,
    /// which is never what a user listing a wait means.
    #[must_use]
    pub fn effective_waits_secs(&self) -> Vec<u64> {
        self.waits_secs.iter().map(|&s| s.max(1)).collect()
    }
}

impl Default for ProviderErrorWatchdogConfig {
    fn default() -> Self {
        Self {
            waits_secs: default_waits_secs(),
        }
    }
}

impl jinn_config::Configurable for ProviderErrorWatchdogConfig {
    const KEY: &'static str = "watchdog.provider_error";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn defaults_carry_the_escalating_schedule() {
        // Given the default config.
        let config = ProviderErrorWatchdogConfig::default();

        // When reading the waits.
        // Then they are 10, 30, 60.
        assert_eq!(config.waits_secs, vec![10, 30, 60]);
    }

    #[rstest::rstest]
    #[test]
    fn explicit_waits_override_the_defaults() {
        // Given a config with explicit waits.
        let config = ProviderErrorWatchdogConfig {
            waits_secs: vec![5, 10],
        };

        // When reading them back.
        // Then the user's values win.
        assert_eq!(config.effective_waits_secs(), vec![5, 10]);
    }

    #[rstest::rstest]
    #[test]
    fn empty_waits_list_signals_disable() {
        // Given a config with an empty list.
        let config = ProviderErrorWatchdogConfig { waits_secs: vec![] };

        // When reading the effective waits.
        // Then the list stays empty — the watchdog's disable signal.
        assert_eq!(config.effective_waits_secs(), Vec::<u64>::new());
    }

    #[rstest::rstest]
    #[test]
    fn zero_wait_floors_at_one_second() {
        // Given a config with a zero entry.
        let config = ProviderErrorWatchdogConfig {
            waits_secs: vec![0, 30, 0],
        };

        // When reading the effective waits.
        // Then each zero floors at one second.
        assert_eq!(config.effective_waits_secs(), vec![1, 30, 1]);
    }

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [provider_error_watchdog] section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            provider_error_watchdog: ProviderErrorWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited defaults apply (10/30/60).
        assert_eq!(wrapper.provider_error_watchdog.waits_secs, vec![10, 30, 60]);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            provider_error_watchdog: ProviderErrorWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper =
            toml::from_str("[provider_error_watchdog]\nwaits_secs = [5, 10]").expect("parses");

        // Then the user's values win over the defaults.
        assert_eq!(wrapper.provider_error_watchdog.waits_secs, vec![5, 10]);
    }
}
