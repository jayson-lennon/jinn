//! Stream-rule watchdog configuration — the `jinn.toml`
//! `[watchdog.stream_rules]` section.
//!
//! Pure serde data; the watchdog slice (`crates/slices/jinn-watchdog`)
//! imports the shape from here and reads a snapshot at activation.
//!
//! The section sits under the watchdog umbrella because the knob it carries is
//! a watchdog's knob: how many consecutive rule interrupts a session may take
//! before the stream is cancelled. The rules themselves stay where the user
//! writes them, at `[[stream_rules.entry]]`.

use serde::{Deserialize, Serialize};

/// The `jinn.toml` key this section lives at.
///
/// Named so the migration warning in the watchdog slice can quote the new
/// location beside the old one.
pub const STREAM_RULE_WATCHDOG_KEY: &str = "watchdog.stream_rules";

/// Default maximum tolerated interrupts before the watchdog trips.
const DEFAULT_MAX_FAILURES: u8 = 4;

/// Stream-rule watchdog configuration.
///
/// Serialized as `[watchdog.stream_rules]` in `jinn.toml`. The watchdog is
/// always on; the knob only tunes when it intervenes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamRuleWatchdogConfig {
    /// Consecutive stream-rule interrupts tolerated before the watchdog
    /// cancels the stream. A response that completes without an interrupt
    /// debits the count by one.
    ///
    /// A session interrupted `n` times in a row has heard the correction `n`
    /// times; the `n + 1`th means the rule is matching the guidance it
    /// injects, and the correction has become a loop.
    ///
    /// Default: 4.
    #[serde(default = "default_max_failures")]
    pub max_failures: u8,
}

fn default_max_failures() -> u8 {
    DEFAULT_MAX_FAILURES
}

impl StreamRuleWatchdogConfig {
    /// The trip threshold for the accumulator.
    ///
    /// A zero maximum is nonsense (the watchdog would cancel the first
    /// interrupt the user asked a rule to make), so consumers floor it at
    /// one. The same shape the tool-call watchdog uses for its own floor.
    #[must_use]
    pub fn effective_max_failures(&self) -> u8 {
        self.max_failures.max(1)
    }
}

impl Default for StreamRuleWatchdogConfig {
    fn default() -> Self {
        Self {
            max_failures: DEFAULT_MAX_FAILURES,
        }
    }
}

impl jinn_config::Configurable for StreamRuleWatchdogConfig {
    const KEY: &'static str = STREAM_RULE_WATCHDOG_KEY;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    #[test]
    fn an_absent_section_is_the_default() {
        // Given a document with no `[watchdog.stream_rules]` section.
        let config: StreamRuleWatchdogConfig = jinn_config::testutil::config_layer("")
            .get()
            .expect("defaulted");

        // Then the maximum is the documented default.
        assert_eq!(config.max_failures, 4);
    }

    #[rstest::rstest]
    #[test]
    fn a_configured_maximum_is_read() {
        // Given a document setting the maximum.
        let config: StreamRuleWatchdogConfig = jinn_config::testutil::config_layer(
            r#"
            [watchdog.stream_rules]
            max_failures = 2
        "#,
        )
        .get()
        .expect("read");

        // Then it is what the user asked for.
        assert_eq!(config.max_failures, 2);
    }

    #[rstest::rstest]
    #[test]
    fn a_zero_maximum_floors_at_one() {
        // Given a document setting the maximum to zero.
        let config: StreamRuleWatchdogConfig = jinn_config::testutil::config_layer(
            r#"
            [watchdog.stream_rules]
            max_failures = 0
        "#,
        )
        .get()
        .expect("read");

        // Then the threshold is one, because zero would cancel on the first
        // interrupt whatever the user meant.
        assert_eq!(config.effective_max_failures(), 1);
    }

    #[rstest::rstest]
    #[test]
    fn the_section_coexists_with_its_sibling_watchdog_sections() {
        // Given a document carrying all three watchdog sections.
        let layer = jinn_config::testutil::config_layer(
            r#"
            [watchdog.stall]
            max_restarts = 5

            [watchdog.tool_call]
            max_failures = 3

            [watchdog.stream_rules]
            max_failures = 2
        "#,
        );

        // When each is read.
        let stream_rules: StreamRuleWatchdogConfig = layer.get().expect("read");
        let tool_call: super::super::tool_call_watchdog::ToolCallWatchdogConfig =
            layer.get().expect("read");
        let stall: super::super::stall_watchdog::StallWatchdogConfig = layer.get().expect("read");

        // Then each is read from the same table without disturbing the others.
        assert_eq!(stream_rules.max_failures, 2);
        assert_eq!(tool_call.max_failures, 3);
        assert_eq!(stall.max_restarts, 5);
    }
}
