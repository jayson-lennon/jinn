//! Reasoning-loop-watchdog configuration schema — the `jinn.toml`
//! `[watchdog.stuck_loop]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default maximum tolerated loop detections before the watchdog
/// surrenders the turn.
const DEFAULT_MAX_FAILURES: u8 = 3;

/// Reasoning-loop-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each watchdog check, so an edit applies at the next check.
///
/// Serialized as `[watchdog.stuck_loop]` in `jinn.toml`. The watchdog
/// is always on; the knob only tunes when it intervenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StuckLoopWatchdogConfig {
    /// Detected repetition loops tolerated before the watchdog cancels
    /// the turn. A response that completes without a detected loop
    /// debits the count by one. Default: 3.
    #[serde(default = "default_max_failures")]
    pub max_failures: u8,
}

fn default_max_failures() -> u8 {
    DEFAULT_MAX_FAILURES
}

impl StuckLoopWatchdogConfig {
    /// The surrender threshold for the accumulator.
    ///
    /// A zero maximum is nonsense (the watchdog would surrender the
    /// first detected loop regardless of configuration intent), so
    /// consumers floor it at one.
    #[must_use]
    pub fn effective_max_failures(&self) -> u8 {
        self.max_failures.max(1)
    }
}

impl Default for StuckLoopWatchdogConfig {
    fn default() -> Self {
        Self {
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }
}

impl jinn_config::Configurable for StuckLoopWatchdogConfig {
    const KEY: &'static str = "watchdog.stuck_loop";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn max_failures_passes_through_when_valid() {
        // Given a config with an explicit maximum.
        let config = StuckLoopWatchdogConfig { max_failures: 2 };

        // When reading the effective threshold.
        // Then the value passes through unchanged.
        assert_eq!(config.effective_max_failures(), 2);
    }

    #[rstest::rstest]
    #[test]
    fn zero_max_failures_floors_at_one() {
        // Given a config with a zero maximum.
        let config = StuckLoopWatchdogConfig { max_failures: 0 };

        // When reading the effective threshold.
        // Then it floors at one failure.
        assert_eq!(config.effective_max_failures(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [watchdog.stuck_loop] section.
        #[derive(Default, serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            stuck_loop: StuckLoopWatchdogConfig,
        }
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited default applies (3 failures).
        assert_eq!(wrapper.watchdog.stuck_loop.max_failures, 3);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section.
        #[derive(serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            stuck_loop: StuckLoopWatchdogConfig,
        }
        #[derive(serde::Deserialize)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper =
            toml::from_str("[watchdog.stuck_loop]\nmax_failures = 2").expect("parses");

        // Then the user's value wins over the default.
        assert_eq!(wrapper.watchdog.stuck_loop.max_failures, 2);
    }
}
