//! Database-maintenance configuration schema — the `jinn.toml`
//! `[maintenance]` section.
//!
//! Pure serde data plus the retention-string grammar. The session prune
//! actor (`crates/slices/jinn-session-store`) imports the shape from here
//! and reads a snapshot at each sweep. The section stays named
//! `[maintenance]` because it is the home for general database-maintenance
//! settings, of which retention is the first; an absent section
//! deserializes to the default, and the default disables deletion: jinn
//! never deletes sessions the user has not explicitly told it to.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Database-maintenance configuration.
///
/// # Behavior: Snapshot Read.
///
/// Re-read at each maintenance pass, so an edit applies at the next pass.
///
/// Serialized as `[maintenance]` in `jinn.toml`. Every knob defaults to a
/// no-op, so merely adding the section changes nothing until a value is
/// filled in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceConfig {
    /// Delete session trees idle longer than this, e.g. `"90d"`, `"24h"`,
    /// `"30m"`, `"2w"`. A tree is deleted whole and only when every member
    /// is archived, outside the Library, and idle past this threshold.
    ///
    /// Absent or empty means never delete. This is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_retention: Option<String>,
}

impl jinn_config::Configurable for MaintenanceConfig {
    const KEY: &'static str = "maintenance";
}

/// Parses a retention string into a duration.
///
/// Grammar: `<u64><unit>` with an optional surrounding whitespace and the
/// unit one of `m` (minutes), `h` (hours), `d` (days), `w` (weeks). The
/// conversion saturates rather than overflowing. Anything else — an empty
/// string, a bare number, a unknown unit, trailing junk — is `None`, which
/// every consumer must treat as "deletion disabled"; a retention typo may
/// cost a warning, never a failed startup or a wrong delete.
#[must_use]
pub fn retention_duration(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    let split = trimmed.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = trimmed.split_at(split);
    let amount = digits.parse::<u64>().ok()?;
    let seconds = match unit {
        "m" => amount.saturating_mul(60),
        "h" => amount.saturating_mul(60 * 60),
        "d" => amount.saturating_mul(24 * 60 * 60),
        "w" => amount.saturating_mul(7 * 24 * 60 * 60),
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

impl MaintenanceConfig {
    /// The configured session retention, or `None` when deletion is
    /// disabled (absent value, empty string, or unparseable value).
    ///
    /// Saturating: an overflowing retention clamps to `Duration::MAX`,
    /// which reads as "effectively never" rather than wrapping to a tiny
    /// window that would delete fresh sessions.
    #[must_use]
    pub fn retention_duration(&self) -> Option<Duration> {
        self.session_retention
            .as_deref()
            .and_then(retention_duration)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn section_defaults_when_absent_from_toml() {
        // Given a jinn.toml without the [maintenance] section.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            #[serde(default)]
            maintenance: MaintenanceConfig,
        }

        // When deserializing.
        let wrapper: Wrapper = toml::from_str("").expect("empty toml parses");

        // Then retention is unset — deletion is disabled by default.
        assert_eq!(wrapper.maintenance.session_retention, None);
        assert_eq!(wrapper.maintenance.retention_duration(), None);
    }

    #[rstest::rstest]
    #[test]
    fn empty_retention_string_reads_disabled() {
        // Given a config whose retention is the empty string.
        let config = MaintenanceConfig {
            session_retention: Some(String::new()),
        };

        // When reading the retention duration.
        // Then it is None — an explicit empty value disables deletion too.
        assert_eq!(config.retention_duration(), None);
    }

    #[rstest::rstest]
    #[case::minutes("30m", Duration::from_mins(30))]
    #[case::hours("24h", Duration::from_hours(24))]
    #[case::days("90d", Duration::from_hours(90 * 24))]
    #[case::weeks("2w", Duration::from_hours(2 * 7 * 24))]
    #[case::whitespace_tolerant("  7d ", Duration::from_hours(7 * 24))]
    fn retention_parses_unit_suffixes(#[case] raw: &str, #[case] expected: Duration) {
        // Given a retention string with a supported unit suffix.
        // When parsing it.
        let parsed = retention_duration(raw);
        // Then it converts to the exact duration.
        assert_eq!(parsed, Some(expected), "input was {raw:?}");
    }

    #[rstest::rstest]
    #[case::empty("")]
    #[case::bare_number("90")]
    #[case::seconds_unit("90s")]
    #[case::unknown_unit("90x")]
    #[case::unit_without_number("d")]
    #[case::trailing_junk("90dd")]
    #[case::leading_junk("x90d")]
    #[case::negative("-90d")]
    #[case::float("1.5d")]
    fn retention_rejects_malformed_values(#[case] raw: &str) {
        // Given a malformed retention string.
        // When parsing it.
        let parsed = retention_duration(raw);
        // Then it is None — the consumer's "deletion disabled" signal.
        assert_eq!(parsed, None, "input was {raw:?}");
    }

    #[rstest::rstest]
    #[test]
    fn retention_parsing_reports_a_clear_failure_reason() {
        // Given a config with a value the grammar cannot read.
        let config = MaintenanceConfig {
            session_retention: Some("soon".to_owned()),
        };

        // When reading the retention duration.
        // Then the raw value stays visible on the config for the consumer's
        // warning, while the duration reads disabled.
        assert_eq!(config.session_retention.as_deref(), Some("soon"));
        assert_eq!(config.retention_duration(), None);
    }

    #[rstest::rstest]
    #[test]
    fn huge_retention_saturates_rather_than_wrapping() {
        // Given a retention whose seconds overflow u64.
        let config = MaintenanceConfig {
            session_retention: Some(format!("{}w", u64::MAX)),
        };

        // When converting to a duration.
        // Then the conversion saturates to the largest representable whole
        // seconds instead of wrapping to a tiny window that would delete
        // fresh sessions.
        assert_eq!(
            config.retention_duration(),
            Some(Duration::from_secs(u64::MAX))
        );
    }
}
