#![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]

//! Public-API tests for [`ThemeColor`] TOML serialization.

use ratatui::style::Color;

use super::ThemeColor;

/// Wrapper struct for testing TOML round-trips of individual ThemeColor values.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ColorWrapper {
    color: ThemeColor,
}

#[rstest::rstest]
fn toml_ansi_name() {
    // Given a TOML string with an ANSI color name.
    let toml_str = "color = \"yellow\"";

    // When deserializing it.
    let wrapper: ColorWrapper = toml::from_str(toml_str).expect("parse");

    // Then it deserializes to the named color.
    assert_eq!(wrapper.color.0, Color::Yellow);
}

#[rstest::rstest]
fn toml_hex() {
    // Given a TOML string with a hex color.
    let toml_str = "color = \"#FFA500\"";

    // When deserializing it.
    let wrapper: ColorWrapper = toml::from_str(toml_str).expect("parse");

    // Then it deserializes to RGB.
    assert_eq!(wrapper.color.0, Color::Rgb(255, 165, 0));
}

#[rstest::rstest]
fn toml_rgb_array() {
    // Given a TOML array with 3 u8 values.
    let toml_str = "color = [25, 27, 30]";

    // When deserializing it.
    let wrapper: ColorWrapper = toml::from_str(toml_str).expect("parse");

    // Then it deserializes to RGB.
    assert_eq!(wrapper.color.0, Color::Rgb(25, 27, 30));
}

#[rstest::rstest]
fn toml_ansi_code() {
    // Given a TOML string with an ANSI code.
    let toml_str = "color = \"A80\"";

    // When deserializing it.
    let wrapper: ColorWrapper = toml::from_str(toml_str).expect("parse");

    // Then it deserializes to an RGB color (resolved via anstyle-lossy).
    assert!(matches!(wrapper.color.0, Color::Rgb(_, _, _)));
}

#[rstest::rstest]
fn toml_invalid_string_fails() {
    // Given a TOML string that is not a valid color.
    let toml_str = "color = \"notacolor123\"";

    // When deserializing it.
    let result: Result<ColorWrapper, _> = toml::from_str(toml_str);

    // Then deserialization fails.
    assert!(result.is_err());
}

#[rstest::rstest]
fn toml_invalid_array_fails() {
    // Given a TOML array with only 2 values.
    let toml_str = "color = [255, 165]";

    // When deserializing it.
    let result: Result<ColorWrapper, _> = toml::from_str(toml_str);

    // Then deserialization fails.
    assert!(result.is_err());
}

#[rstest::rstest]
fn serialize_rgb_round_trip() {
    // Given a ThemeColor with RGB.
    let original = ColorWrapper {
        color: ThemeColor(Color::Rgb(255, 165, 0)),
    };
    // When serializing to TOML and back.
    let toml_str = toml::to_string(&original).expect("serialize");
    let restored: ColorWrapper = toml::from_str(&toml_str).expect("parse");
    // Then the color is preserved.
    assert_eq!(original.color.0, restored.color.0);
}

#[rstest::rstest]
fn serialize_named_round_trip() {
    // Given a ThemeColor with a named color.
    let original = ColorWrapper {
        color: ThemeColor(Color::Yellow),
    };
    // When serializing to TOML and back.
    let toml_str = toml::to_string(&original).expect("serialize");
    let restored: ColorWrapper = toml::from_str(&toml_str).expect("parse");
    // Then the color is preserved.
    assert_eq!(original.color.0, restored.color.0);
}

#[cfg(test)]
mod bundled_themes_load {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use crate::default_theme;
    use crate::theme::ThemeFile;

    /// A minimal theme that omits every optional key.
    const SPARSE_THEME: &str = "# A theme may omit every optional key.\n";

    /// A theme that predates a new `Theme` field must still load: the
    /// resolver fills any omitted key from the default. This guard needs
    /// no bundled files — an empty-ish document is the degenerate
    /// everything-omitted case, which is strictly stronger than spot
    /// checking any particular shipped theme.
    #[rstest::rstest]
    #[test]
    fn a_theme_omitting_every_optional_key_resolves_every_colour() {
        // Given a theme file that names no colours at all. (The shipped
        // themes in the repo's `res/` tree are content, not test
        // fixtures, so tests construct their own.)
        let file: ThemeFile = toml::from_str(SPARSE_THEME).expect("a comment-only theme parses");

        // When resolving it against the default.
        let theme = file.resolve_with_fallback(&default_theme());

        // Then the optional key lands as a real colour rather than a
        // Reset that would silently render as the terminal's own
        // foreground.
        assert_ne!(
            theme.dormant_fg,
            ratatui::style::Color::Reset,
            "dormant_fg resolved to Reset"
        );
    }

    /// The new token's meaning is "dormant but healthy", so it must not
    /// resolve to the error colour — that is the whole reason it exists
    /// rather than reusing `error_text`.
    #[rstest::rstest]
    #[test]
    fn the_dormant_token_defaults_to_something_other_than_the_error_colour() {
        // Given the default theme.
        let theme = default_theme();

        // When the dormant colour is resolved.
        // Then dormancy is not painted as a failure.
        assert_ne!(
            theme.dormant_fg, theme.error_text,
            "a passivated actor must not read as an error"
        );
    }

    /// A theme predating `metrics_fg` (the shipped non-default themes) must
    /// resolve the key to the default theme's grey — the fallback rule that
    /// keeps old theme files rendering the metrics block, not Reset.
    #[rstest::rstest]
    #[test]
    fn a_theme_omitting_the_metrics_key_resolves_it_to_the_default_grey() {
        // Given a theme file that names no colours at all.
        let file: ThemeFile = toml::from_str(SPARSE_THEME).expect("a comment-only theme parses");

        // When resolving it against the default.
        let theme = file.resolve_with_fallback(&default_theme());

        // Then the metrics foreground is the default's grey, not Reset.
        assert_eq!(theme.metrics_fg, default_theme().metrics_fg);
        // And the default itself is a truecolor grey — ANSI "gray" is
        // palette slot 7, which renders near-white and would read as
        // "no color applied" next to the primary text.
        assert_eq!(theme.metrics_fg, ratatui::style::Color::Rgb(140, 140, 140));
    }
}

#[cfg(test)]
mod style_map_integration_tests {
    #![allow(clippy::expect_used, reason = "test assertions")]
    use ratatui::style::Style;

    use crate::theme::ThemeFile;

    #[rstest::rstest]
    #[test]
    fn style_map_returns_entry_for_every_theme_field() {
        // Given the default theme.
        let theme = crate::default_theme();
        // When building the style map.
        let map = theme.style_map();
        // Then it has one entry per Theme field. The expected count is read
        // off the theme file rather than written as a literal, because a
        // literal made a newly added key fail here as "52 != 51" — naming
        // neither the key nor the map, and asking the next person to count
        // a struct by hand. `ThemeFile` and `Theme` are maintained in step
        // by the two tests either side of this one, so comparing the three
        // sizes is exactly the "did you update all of them?" question.
        let file: ThemeFile = toml::from_str(crate::default_theme::DEFAULT_TOML)
            .expect("the embedded default theme parses");
        let field_count = toml::Value::try_from(&file)
            .expect("the theme file serializes")
            .as_table()
            .expect("a theme file is a table")
            .len();
        assert_eq!(
            map.len(),
            field_count,
            "style_map must cover every field the theme file exposes"
        );
    }

    #[rstest::rstest]
    #[test]
    fn style_map_values_are_fg_only_styles() {
        // Given the default theme.
        let theme = crate::default_theme();
        // When building the style map.
        let map = theme.style_map();
        // Then selected entries resolve to Style::default().fg(field).
        assert_eq!(
            map.get("streaming"),
            Some(&Style::default().fg(theme.streaming))
        );
        assert_eq!(
            map.get("accent_action"),
            Some(&Style::default().fg(theme.accent_action))
        );
        assert_eq!(
            map.get("muted_text"),
            Some(&Style::default().fg(theme.muted_text))
        );
        assert_eq!(
            map.get("subagent_fg"),
            Some(&Style::default().fg(theme.subagent_fg))
        );
        assert_eq!(
            map.get("in_flight_bg"),
            Some(&Style::default().fg(theme.in_flight_bg))
        );
        assert_eq!(
            map.get("in_flight_fg"),
            Some(&Style::default().fg(theme.in_flight_fg))
        );
    }
}

#[cfg(test)]
mod in_flight_tint_tests {
    use crate::theme::ThemeFile;

    /// A minimal theme carrying only the in-flight tint pair.
    const TINTED_THEME: &str = concat!(
        "in_flight_bg = \"#2d5a3d\"\n",
        "in_flight_fg = \"#e8f5e9\"\n",
    );

    /// A light theme carrying the tint pair and a pale gutter.
    const LIGHT_TINTED_THEME: &str = concat!(
        "in_flight_bg = \"#eef7ee\"\n",
        "in_flight_fg = \"#1b3a1e\"\n",
        "gutter_bg = \"#f5f5f0\"\n",
    );

    #[rstest::rstest]
    #[test]
    fn a_theme_defining_in_flight_colors_admits_them() {
        // Given a theme that authors the tint pair (the shipped themes
        // in the repo's `res/` tree are content, not test fixtures, so
        // tests construct their own).
        let file: ThemeFile = toml::from_str(TINTED_THEME).expect("parse");

        // When reading its in-flight tint colors.
        let bg = file.in_flight_bg;
        let fg = file.in_flight_fg;

        // Then both are authored, so the tint is never invisible.
        assert!(
            bg.is_some(),
            "in_flight_bg must be admitted for the tint to render"
        );
        assert!(
            fg.is_some(),
            "in_flight_fg must be admitted for the tint to render"
        );
    }

    #[rstest::rstest]
    #[test]
    fn a_light_theme_tint_is_legible_against_its_pale_gutter() {
        // Given a light theme with a pale gutter.
        let file: ThemeFile = toml::from_str(LIGHT_TINTED_THEME).expect("parse");

        // When resolving its in-flight tint and gutter colors.
        let theme = file.resolve();
        let tint_bg = theme.in_flight_bg;
        let gutter = theme.gutter_bg;

        // Then the wash is distinguishable from the gutter behind it.
        assert_ne!(
            tint_bg, gutter,
            "a tint matching the gutter would be invisible on the sidebar"
        );
        // And the text reads against the wash.
        assert_eq!(
            theme.in_flight_fg,
            crate::contrast::ensure_contrast(theme.in_flight_fg, tint_bg),
            "in_flight_fg should already be legible against in_flight_bg"
        );
    }
}

#[rstest::rstest]
fn the_bot_fg_is_hot_pink() {
    // Given the default theme.

    // When reading the color that marks bot sessions.
    let bot_fg = crate::default_theme().bot_fg;

    // Then it is hot pink — the one named color with enough distance from
    // subagent purple to be told apart at a glance.
    assert_eq!(bot_fg, Color::Rgb(255, 105, 180));
}

#[rstest::rstest]
fn the_bot_color_is_not_confusable_with_subagent_purple() {
    // Given the default theme's two session-kind markers.
    let theme = crate::default_theme();
    let to_rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (u32::from(r), u32::from(g), u32::from(b)),
        other => panic!("expected an RGB color, got {other:?}"),
    };
    let (r1, g1, b1) = to_rgb(theme.bot_fg);
    let (r2, g2, b2) = to_rgb(theme.subagent_fg);

    // When measuring how far apart they sit.
    let distance_sq = r1.abs_diff(r2).pow(2) + g1.abs_diff(g2).pow(2) + b1.abs_diff(b2).pow(2);

    // Then they are far apart — well clear of the 1500 legibility gate that
    // let the previous dusty rose pass while still reading as purple.
    assert!(
        distance_sq >= 10_000,
        "bot_fg sits only d²={distance_sq} from subagent_fg, close enough to read as the same color"
    );
}
