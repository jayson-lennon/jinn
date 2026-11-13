//! The `[graphics]` section — terminal image rendering in the chat log.
//!
//! Controls whether attached and tool-result images render as actual
//! pictures (kitty graphics protocol and friends) or degrade to the
//! plain-text placeholders. Three keys: `enabled`, a `protocol`
//! override for terminals whose capability detection lies, and
//! `max_image_rows`, a display-height cap.
//!
//! # Behavior: Event-Dependent.
//!
//! The render pass owns the graphics state (the capability picker and
//! the per-image protocol state); each frame reads the live section,
//! compares it against the last applied value, and clears the protocol
//! state on any change so images re-transmit under the new settings.
//! The only piece fixed at launch is the once-per-launch terminal
//! capability query itself — protocol *detection* cannot be re-run
//! live because it probes stdin; a terminal swap mid-session (ssh
//! drop, tmux attach) needs a jinn restart to re-detect.
//!
//! The picker that the query builds lives in the chat-log-view message
//! crate; this module is data plus the protocol vocabulary — no
//! rendering, no terminal I/O.

use serde::{Deserialize, Serialize};

/// The built-in display-height cap, in terminal rows, applied when
/// `max_image_rows` is absent. Also clamped against half the current
/// viewport height at render time, never below four rows.
pub const DEFAULT_MAX_IMAGE_ROWS: u16 = 16;

/// Protocol override for the terminal-graphics backend.
///
/// `Auto` trusts the runtime capability detection; the explicit
/// variants force one protocol. A forced protocol the terminal cannot
/// actually speak degrades visibly to the plain-text placeholders —
/// the override exists precisely because detection sometimes lies, so
/// force means force.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsProtocol {
    /// Trust runtime detection (the shipped default).
    #[default]
    Auto,
    /// Kitty graphics protocol — unicode placeholder glyphs after the
    /// initial transmit; scrolls as text.
    Kitty,
    /// Sixel — immediate-mode; images clip instead of scrolling.
    Sixel,
    /// iTerm2 inline images — immediate-mode like sixel.
    ITerm2,
    /// ANSI half-block pixel art — every terminal, blocky output.
    Halfblocks,
}

/// `[graphics]` preferences.
///
/// Serialized as `[graphics]` in `jinn.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphicsConfig {
    /// Render images in the chat log when the terminal supports a
    /// graphics protocol. `false` keeps the plain-text placeholders.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Force a specific graphics protocol instead of trusting
    /// detection. The shipped default is `Auto`.
    #[serde(default)]
    pub protocol: GraphicsProtocol,

    /// Absolute cap on the rows one image may occupy in the chat log.
    /// `None` = the built-in default ([`DEFAULT_MAX_IMAGE_ROWS`]);
    /// the effective cap is additionally clamped to half the current
    /// viewport height, never below four rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_image_rows: Option<u16>,
}

fn default_enabled() -> bool {
    true
}

impl Default for GraphicsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            protocol: GraphicsProtocol::default(),
            max_image_rows: None,
        }
    }
}

impl jinn_config::Configurable for GraphicsConfig {
    const KEY: &'static str = "graphics";
}

impl GraphicsConfig {
    /// The effective display-height cap: the configured value or the
    /// built-in default.
    ///
    /// This is the absolute cap only; the render pass additionally
    /// clamps to half the current viewport height (never below four
    /// rows), because that bound depends on live layout state this
    /// schema deliberately does not see.
    #[must_use]
    pub fn effective_max_rows(&self) -> u16 {
        self.max_image_rows.unwrap_or(DEFAULT_MAX_IMAGE_ROWS)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test assertions need expect")]

    use super::{DEFAULT_MAX_IMAGE_ROWS, GraphicsConfig, GraphicsProtocol};
    use serde::Deserialize;

    fn from_table(raw: &str) -> GraphicsConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            graphics: GraphicsConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("graphics table parses")
            .graphics
    }

    #[rstest::rstest]
    #[test]
    fn empty_table_defaults_to_enabled_auto_and_no_cap() {
        // Given an empty `[graphics]` table.
        // When deserializing.
        let config = from_table("[graphics]\n");

        // Then images render by default, detection is trusted, and the
        // cap falls through to the built-in default.
        assert!(config.enabled);
        assert_eq!(config.protocol, GraphicsProtocol::Auto);
        assert_eq!(config.max_image_rows, None);
        assert_eq!(config.effective_max_rows(), DEFAULT_MAX_IMAGE_ROWS);
    }

    #[rstest::rstest]
    #[test]
    fn disabling_graphics_roundtrips() {
        // Given a table that turns graphics off.
        let config = from_table("[graphics]\nenabled = false\n");

        // When reading the section.
        // Then the flag survives the round-trip.
        assert!(!config.enabled);
    }

    #[rstest::rstest]
    #[case("auto", GraphicsProtocol::Auto)]
    #[case("kitty", GraphicsProtocol::Kitty)]
    #[case("sixel", GraphicsProtocol::Sixel)]
    #[case("iterm2", GraphicsProtocol::ITerm2)]
    #[case("halfblocks", GraphicsProtocol::Halfblocks)]
    fn protocol_override_parses_named_protocols(
        #[case] raw: &str,
        #[case] expected: GraphicsProtocol,
    ) {
        // Given a table forcing one protocol by its toml name.
        let config = from_table(&format!("[graphics]\nprotocol = \"{raw}\"\n"));

        // When reading the section.
        // Then the named protocol is preserved.
        assert_eq!(config.protocol, expected);
    }

    #[rstest::rstest]
    #[test]
    fn unknown_protocol_name_is_rejected() {
        // Given a table naming a protocol that does not exist.
        // When deserializing.
        let result = toml::from_str::<GraphicsConfig>("protocol = \"asciiart\"");

        // Then parsing fails rather than silently falling back.
        assert!(result.is_err());
    }

    #[rstest::rstest]
    #[test]
    fn max_image_rows_roundtrips() {
        // Given a table with an explicit row cap.
        let config = from_table("[graphics]\nmax_image_rows = 8\n");

        // When reading the effective cap.
        let cap = config.effective_max_rows();

        // Then the configured value wins over the built-in default.
        assert_eq!(cap, 8);
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            graphics: GraphicsConfig,
        }

        // Given the default config.
        let config = GraphicsConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: GraphicsConfig = toml::from_str::<Repr>(&format!("[graphics]\n{s}"))
            .expect("reparse")
            .graphics;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
