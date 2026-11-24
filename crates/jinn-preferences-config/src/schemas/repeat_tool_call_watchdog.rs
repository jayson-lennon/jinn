//! Repeat-tool-call-watchdog configuration schema — the `jinn.toml`
//! `[watchdog.repeat_tool_call]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default maximum tolerated repeats before the watchdog nudges.
const DEFAULT_MAX_REPEATS: u8 = 4;

/// Repeat-tool-call-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each watchdog check, so an edit applies at the next check.
///
/// Serialized as `[watchdog.repeat_tool_call]` in `jinn.toml`. Zero
/// disables the watchdog (the actor is not spawned); a nonzero value is
/// the trip threshold, not floored — unlike the sibling watchdogs whose
/// zero floors at one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatToolCallWatchdogConfig {
    /// Identical repeats (same tool, arguments byte-equal after
    /// whitespace normalization, result byte-equal after whitespace
    /// normalization on a bounded prefix) tolerated before the watchdog
    /// nudges the turn. A differing call debits the count by one. `0`
    /// disables the watchdog. Default: 4.
    #[serde(default = "default_max_repeats")]
    pub max_repeats: u8,
}

fn default_max_repeats() -> u8 {
    DEFAULT_MAX_REPEATS
}

impl Default for RepeatToolCallWatchdogConfig {
    fn default() -> Self {
        Self {
            max_repeats: DEFAULT_MAX_REPEATS,
        }
    }
}

impl jinn_config::Configurable for RepeatToolCallWatchdogConfig {
    const KEY: &'static str = "watchdog.repeat_tool_call";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [watchdog.repeat_tool_call] section.
        #[derive(Default, serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            repeat_tool_call: RepeatToolCallWatchdogConfig,
        }
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited default applies (4 repeats).
        assert_eq!(wrapper.watchdog.repeat_tool_call.max_repeats, 4);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section.
        #[derive(Default, serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            repeat_tool_call: RepeatToolCallWatchdogConfig,
        }
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper =
            toml::from_str("[watchdog.repeat_tool_call]\nmax_repeats = 2").expect("parses");

        // Then the user's value wins over the default.
        assert_eq!(wrapper.watchdog.repeat_tool_call.max_repeats, 2);
    }

    #[rstest::rstest]
    #[test]
    fn zero_is_stored_verbatim_as_the_disable_value() {
        // Given a config with a zero maximum.
        let config = RepeatToolCallWatchdogConfig { max_repeats: 0 };

        // Then zero survives unchanged — it is the disable value, not a
        // floor candidate.
        assert_eq!(config.max_repeats, 0);
    }
}
