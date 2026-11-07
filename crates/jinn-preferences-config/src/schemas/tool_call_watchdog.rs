//! Tool-call-watchdog configuration schema — the `jinn.toml`
//! `[tool_call_watchdog]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default maximum tolerated failures before the watchdog trips.
const DEFAULT_MAX_FAILURES: u8 = 4;

/// Tool-call-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each watchdog check, so an edit applies at the next check.
///
/// Serialized as `[tool_call_watchdog]` in `jinn.toml`. The watchdog is
/// always on; the knob only tunes when it intervenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallWatchdogConfig {
    /// Consecutive tool failures tolerated before the watchdog cancels
    /// the stream. A successful tool result debits the count by one.
    /// Default: 4.
    #[serde(default = "default_max_failures")]
    pub max_failures: u8,
}

fn default_max_failures() -> u8 {
    DEFAULT_MAX_FAILURES
}

impl ToolCallWatchdogConfig {
    /// The trip threshold for the accumulator.
    ///
    /// A zero maximum is nonsense (the watchdog would kill the first
    /// failing call regardless of configuration intent), so consumers
    /// floor it at one.
    #[must_use]
    pub fn effective_max_failures(&self) -> u8 {
        self.max_failures.max(1)
    }
}

impl Default for ToolCallWatchdogConfig {
    fn default() -> Self {
        Self {
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }
}

impl jinn_config::Configurable for ToolCallWatchdogConfig {
    const KEY: &'static str = "watchdog.tool_call";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn max_failures_passes_through_when_valid() {
        // Given a config with an explicit maximum.
        let config = ToolCallWatchdogConfig { max_failures: 2 };

        // When reading the effective threshold.
        // Then the value passes through unchanged.
        assert_eq!(config.effective_max_failures(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn zero_max_failures_floors_at_one() {
        // Given a config with a zero maximum.
        let config = ToolCallWatchdogConfig { max_failures: 0 };

        // When reading the effective threshold.
        // Then it floors at one failure.
        assert_eq!(config.effective_max_failures(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [tool_call_watchdog] section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            tool_call_watchdog: ToolCallWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited default applies (4 failures).
        assert_eq!(wrapper.tool_call_watchdog.max_failures, 4);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            tool_call_watchdog: ToolCallWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper =
            toml::from_str("[tool_call_watchdog]\nmax_failures = 2").expect("parses");

        // Then the user's value wins over the default.
        assert_eq!(wrapper.tool_call_watchdog.max_failures, 2);
    }
}
