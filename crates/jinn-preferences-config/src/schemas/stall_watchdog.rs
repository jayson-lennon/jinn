//! Stall-watchdog configuration schema — the `jinn.toml`
//! `[stall_watchdog]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default silence window before a restart, in seconds.
const DEFAULT_STALL_TIMEOUT_SECS: u64 = 60;

/// Default consecutive restarts before giving up.
const DEFAULT_STALL_MAX_RESTARTS: u32 = 3;

/// Stall-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each watchdog check, so an edit applies at the next check.
///
/// Serialized as `[stall_watchdog]` in `jinn.toml`. The watchdog is
/// always on; these knobs only tune when it intervenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StallWatchdogConfig {
    /// Seconds of provider-stream silence before a stalled turn is
    /// retried. Default: 60.
    #[serde(default = "default_stall_timeout_secs")]
    pub timeout_secs: u64,
    /// Consecutive silent-stall restarts allowed before the watchdog
    /// gives up and cancels the turn. Default: 3.
    #[serde(default = "default_stall_max_restarts")]
    pub max_restarts: u32,
}

fn default_stall_timeout_secs() -> u64 {
    DEFAULT_STALL_TIMEOUT_SECS
}

fn default_stall_max_restarts() -> u32 {
    DEFAULT_STALL_MAX_RESTARTS
}

impl StallWatchdogConfig {
    /// The silence window in milliseconds.
    ///
    /// Saturating: a `u64`-overflowing `timeout_secs` clamps to
    /// `u64::MAX` ms, which reads as "effectively never" rather than
    /// wrapping to a tiny window.
    #[must_use]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_secs.saturating_mul(1_000)
    }

    /// The watchdog with a zero window is nonsense (it would fire on
    /// every tick regardless of configuration intent), so consumers
    /// floor the configured value at one second.
    #[must_use]
    pub fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.max(1)
    }

    /// A zero budget is equally nonsense (no restart would ever be
    /// attempted), so consumers floor it at one restart.
    #[must_use]
    pub fn effective_max_restarts(&self) -> u32 {
        self.max_restarts.max(1)
    }
}

impl Default for StallWatchdogConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_STALL_TIMEOUT_SECS,
            max_restarts: DEFAULT_STALL_MAX_RESTARTS,
        }
    }
}

impl jinn_config::Configurable for StallWatchdogConfig {
    const KEY: &'static str = "watchdog.stall";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[case::defaults(60, 3, 60_000, 3)]
    #[case::overrides(5, 1, 5_000, 1)]
    fn timeout_and_budget_expose_their_values(
        #[case] timeout_secs: u64,
        #[case] max_restarts: u32,
        #[case] expected_ms: u64,
        #[case] expected_restarts: u32,
    ) {
        // Given a config with explicit values.
        let config = StallWatchdogConfig {
            timeout_secs,
            max_restarts,
        };

        // When reading the derived limit accessors.
        // Then the values pass through unchanged.
        assert_eq!(config.timeout_ms(), expected_ms);
        assert_eq!(config.effective_max_restarts(), expected_restarts);
    }

    #[rstest::rstest]
    #[test]
    fn zero_timeout_floors_at_one_second() {
        // Given a config with a zero window.
        let config = StallWatchdogConfig {
            timeout_secs: 0,
            max_restarts: 3,
        };

        // When reading the effective window.
        // Then it floors at one second (never zero).
        assert_eq!(config.effective_timeout_secs(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn zero_budget_floors_at_one_restart() {
        // Given a config with a zero budget.
        let config = StallWatchdogConfig {
            timeout_secs: 60,
            max_restarts: 0,
        };

        // When reading the effective budget.
        // Then it floors at one restart.
        assert_eq!(config.effective_max_restarts(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn huge_timeout_saturates_rather_than_wrapping() {
        // Given a config whose seconds overflow milliseconds.
        let config = StallWatchdogConfig {
            timeout_secs: u64::MAX,
            max_restarts: 3,
        };

        // When converting to milliseconds.
        // Then the conversion saturates instead of wrapping small.
        assert_eq!(config.timeout_ms(), u64::MAX);
    }

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [stall_watchdog] section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            stall_watchdog: StallWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited defaults apply (60s window, 3 restarts).
        assert_eq!(wrapper.stall_watchdog.timeout_secs, 60);
        assert_eq!(wrapper.stall_watchdog.max_restarts, 3);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            stall_watchdog: StallWatchdogConfig,
        }

        // When deserializing.
        let wrapper: Wrapper =
            toml::from_str("[stall_watchdog]\ntimeout_secs = 5\nmax_restarts = 1").expect("parses");

        // Then the user's values win over the defaults.
        assert_eq!(wrapper.stall_watchdog.timeout_secs, 5);
        assert_eq!(wrapper.stall_watchdog.max_restarts, 1);
    }
}
