//! Fuzzy-tool-call-watchdog configuration schema — the `jinn.toml`
//! `[watchdog.fuzzy_tool_call]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.

use serde::{Deserialize, Serialize};

/// Default maximum tolerated near-repeats before the watchdog nudges.
const DEFAULT_MAX_REPEATS: u8 = 8;

/// Default argument-similarity threshold.
const DEFAULT_SIMILARITY: f32 = 0.85;

/// Default maximum argument length the fuzzy watchdog examines.
const DEFAULT_MAX_ARGUMENT_CHARS: usize = 500;

/// Fuzzy-tool-call-watchdog configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each watchdog check, so an edit applies at the next check.
///
/// Serialized as `[watchdog.fuzzy_tool_call]` in `jinn.toml`. Zero
/// `max_repeats` disables the watchdog (the actor is not spawned); a
/// nonzero value is the trip threshold, not floored — unlike the sibling
/// watchdogs whose zero floors at one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuzzyToolCallWatchdogConfig {
    /// Near-identical repeats (same tool, arguments within
    /// `similarity` of the previous call, result likewise) tolerated
    /// before the watchdog nudges the turn. A call that differs beyond
    /// the threshold debits the count by one. `0` disables the
    /// watchdog. Default: 8.
    #[serde(default = "default_max_repeats")]
    pub max_repeats: u8,
    /// Normalized Levenshtein similarity (0.0–1.0) at or above which a
    /// call's arguments count as a repeat of the previous call's.
    /// Default: 0.85.
    #[serde(default = "default_similarity")]
    pub similarity: f32,
    /// Calls whose whitespace-normalized arguments exceed this many
    /// characters are skipped entirely by the watchdog (a long argument
    /// is usually a script, not a stuck call). `0` means no limit.
    /// Default: 500.
    #[serde(default = "default_max_argument_chars")]
    pub max_argument_chars: usize,
}

fn default_max_repeats() -> u8 {
    DEFAULT_MAX_REPEATS
}

fn default_similarity() -> f32 {
    DEFAULT_SIMILARITY
}

fn default_max_argument_chars() -> usize {
    DEFAULT_MAX_ARGUMENT_CHARS
}

impl Default for FuzzyToolCallWatchdogConfig {
    fn default() -> Self {
        Self {
            max_repeats: DEFAULT_MAX_REPEATS,
            similarity: DEFAULT_SIMILARITY,
            max_argument_chars: DEFAULT_MAX_ARGUMENT_CHARS,
        }
    }
}

impl jinn_config::Configurable for FuzzyToolCallWatchdogConfig {
    const KEY: &'static str = "watchdog.fuzzy_tool_call";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [watchdog.fuzzy_tool_call] section.
        #[derive(Default, serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            fuzzy_tool_call: FuzzyToolCallWatchdogConfig,
        }
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then the inherited defaults apply (8 repeats, 0.85, 500 chars).
        assert_eq!(wrapper.watchdog.fuzzy_tool_call.max_repeats, 8);
        assert!((wrapper.watchdog.fuzzy_tool_call.similarity - 0.85).abs() < f32::EPSILON);
        assert_eq!(wrapper.watchdog.fuzzy_tool_call.max_argument_chars, 500);
    }

    #[rstest::rstest]
    #[test]
    fn section_parses_from_toml_table() {
        // Given a jinn.toml carrying the section with every key.
        #[derive(Default, serde::Deserialize)]
        struct WatchdogSection {
            #[serde(default)]
            fuzzy_tool_call: FuzzyToolCallWatchdogConfig,
        }
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct Wrapper {
            watchdog: WatchdogSection,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str(
            "[watchdog.fuzzy_tool_call]\nmax_repeats = 3\nsimilarity = 0.9\nmax_argument_chars = 0",
        )
        .expect("parses");

        // Then the user's values win over the defaults.
        assert_eq!(wrapper.watchdog.fuzzy_tool_call.max_repeats, 3);
        assert!((wrapper.watchdog.fuzzy_tool_call.similarity - 0.9).abs() < 1e-6);
        assert_eq!(wrapper.watchdog.fuzzy_tool_call.max_argument_chars, 0);
    }

    #[rstest::rstest]
    #[test]
    fn zero_is_stored_verbatim_as_the_disable_value() {
        // Given a config with a zero maximum.
        let config = FuzzyToolCallWatchdogConfig {
            max_repeats: 0,
            similarity: 0.85,
            max_argument_chars: 500,
        };

        // Then zero survives unchanged — it is the disable value, not a
        // floor candidate.
        assert_eq!(config.max_repeats, 0);
    }
}
