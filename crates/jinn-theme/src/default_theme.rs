//! Default theme - parsed from the embedded `themes/default.toml` at compile time.

use crate::theme::{Theme, ThemeFile};

/// Embedded default theme TOML - the single source of truth.
pub(crate) const DEFAULT_TOML: &str = include_str!("../../../res/themes/default.toml");

/// Returns the default theme, parsed from the embedded TOML file.
///
/// The TOML is included at compile time via `include_str!` and parsed on
/// every call. The string is small (~1KB) so this is fast enough without
/// caching. Changing `themes/default.toml` changes the default appearance
/// of the application.
///
/// # Panics
///
/// Panics if the embedded `default.toml` is not valid TOML or does not
/// produce a valid theme. This is a compile-time bug.
#[must_use]
pub fn default_theme() -> Theme {
    #[expect(
        clippy::expect_used,
        reason = "embedded TOML is a compile-time artifact"
    )]
    let file: ThemeFile = toml::from_str(DEFAULT_TOML)
        .expect("embedded default.toml should be valid - this is a compile-time bug");
    file.resolve_standalone()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    /// Every color field of a theme, paired with its name.
    ///
    /// Shared by the tests below so each one reads as a recipe rather than a
    /// wall of field accesses.
    fn color_fields(theme: &Theme) -> Vec<(&'static str, ratatui::style::Color)> {
        vec![
            ("focus_accent", theme.focus_accent),
            ("border_unfocused", theme.border_unfocused),
            ("popup_title", theme.popup_title),
            ("primary_text", theme.primary_text),
            ("muted_text", theme.muted_text),
            ("subagent_fg", theme.subagent_fg),
            ("subagent_bg", theme.subagent_bg),
            ("error_text", theme.error_text),
            ("success", theme.success),
            ("warning", theme.warning),
            ("streaming", theme.streaming),
            ("gutter_bg", theme.gutter_bg),
            ("gutter_context_included", theme.gutter_context_included),
            ("user_block_bg", theme.user_block_bg),
            ("tool_fg", theme.tool_fg),
            ("tool_success_bg", theme.tool_success_bg),
            ("tool_failure_bg", theme.tool_failure_bg),
            ("tool_pending_bg", theme.tool_pending_bg),
            ("challenge_alert_bg", theme.challenge_alert_bg),
            ("challenge_alert_fg", theme.challenge_alert_fg),
            ("compaction_block_bg", theme.compaction_block_bg),
            ("rule_interrupt_bg", theme.rule_interrupt_bg),
            ("sources_header_bg", theme.sources_header_bg),
            ("sources_header_fg", theme.sources_header_fg),
            ("truncation_fg", theme.truncation_fg),
            ("picker_active_marker", theme.picker_active_marker),
            ("picker_selected_bg", theme.picker_selected_bg),
            ("picker_highlight_bg", theme.picker_highlight_bg),
            ("tab_active_fg", theme.tab_active_fg),
            ("tab_active_bg", theme.tab_active_bg),
            ("tab_inactive_fg", theme.tab_inactive_fg),
            ("selection_fg", theme.selection_fg),
            ("selection_bg", theme.selection_bg),
            ("in_flight_bg", theme.in_flight_bg),
            ("in_flight_fg", theme.in_flight_fg),
            ("accent_action", theme.accent_action),
            ("age_fresh", theme.age_fresh),
            ("age_stale", theme.age_stale),
            ("scroll_indicator_bg", theme.scroll_indicator_bg),
            ("sidebar_resize_accent", theme.sidebar_resize_accent),
            ("input_mode_queue", theme.input_mode_queue),
            ("input_mode_steer", theme.input_mode_steer),
            ("infopopup_bg", theme.infopopup_bg),
            ("infopopup_title", theme.infopopup_title),
            ("infopopup_border", theme.infopopup_border),
            ("infopopup_fg", theme.infopopup_fg),
            ("quake_bar_bg", theme.quake_bar_bg),
        ]
    }

    #[rstest::rstest]
    fn default_theme_has_no_reset_colors() {
        // Given the default theme.
        let theme = default_theme();

        // When collecting its color fields and finding the reset ones.
        let reset_fields = color_fields(&theme)
            .iter()
            .filter(|(_, color)| *color == ratatui::style::Color::Reset)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();

        // Then no field is Color::Reset (every field should have a concrete value).
        assert!(
            reset_fields.is_empty(),
            "default theme fields should not be Reset: {reset_fields:?}"
        );
    }

    #[rstest::rstest]
    fn default_theme_parses_embedded_toml() {
        // Given the embedded default.toml.
        // When calling default_theme().
        let theme = default_theme();

        // Then it returns a theme without panicking (proves the TOML parses).
        // Spot-check one field to confirm it's not Reset.
        assert_ne!(theme.focus_accent, ratatui::style::Color::Reset);
    }

    #[rstest::rstest]
    #[test]
    fn input_mode_theme_fields_have_non_reset_defaults() {
        // Given the default theme.
        let theme = default_theme();

        // When checking its two input mode fields against the reset color.
        // Then both input mode fields are non-Reset (i.e. have real fallback colors).
        assert_ne!(
            theme.input_mode_queue,
            ratatui::style::Color::Reset,
            "input_mode_queue must have a real default color"
        );
        assert_ne!(
            theme.input_mode_steer,
            ratatui::style::Color::Reset,
            "input_mode_steer must have a real default color"
        );
        // And the steer color is distinct from queue (so users can tell modes apart).
        assert_ne!(
            theme.input_mode_queue, theme.input_mode_steer,
            "queue and steer colors must differ"
        );
    }
}
