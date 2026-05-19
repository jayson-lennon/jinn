//! Color type with multi-format TOML deserialization.
//!
//! Supports ANSI names, hex strings, RGB arrays, and ANSI code prefixes.

use ratatui::style::Color;
use serde::de::{self, Visitor};

/// A color value that deserializes from TOML in multiple formats.
///
/// # Formats
///
/// - **ANSI name**: `"yellow"`, `"DarkGray"` (case-insensitive ratatui Color name)
/// - **Hex**: `"#FFA500"` or `"#ffa500"`
/// - **RGB array**: `[255, 165, 0]`
/// - **ANSI code**: `"A80"` — prefix `A` followed by 0–255, resolved to RGB
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColor(pub Color);

impl ThemeColor {
    /// Returns the inner ratatui color.
    #[must_use]
    pub const fn inner(self) -> Color {
        self.0
    }
}

impl std::fmt::Display for ThemeColor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Color::Rgb(r, g, b) => write!(f, "#{r:02X}{g:02X}{b:02X}"),
            Color::Black => write!(f, "black"),
            Color::Red => write!(f, "red"),
            Color::Green => write!(f, "green"),
            Color::Yellow => write!(f, "yellow"),
            Color::Blue => write!(f, "blue"),
            Color::Magenta => write!(f, "magenta"),
            Color::Cyan => write!(f, "cyan"),
            Color::White => write!(f, "white"),
            Color::Gray => write!(f, "gray"),
            Color::DarkGray => write!(f, "darkgray"),
            Color::LightRed => write!(f, "lightred"),
            Color::LightGreen => write!(f, "lightgreen"),
            Color::LightYellow => write!(f, "lightyellow"),
            Color::LightBlue => write!(f, "lightblue"),
            Color::LightMagenta => write!(f, "lightmagenta"),
            Color::LightCyan => write!(f, "lightcyan"),
            Color::Reset => write!(f, "reset"),
            Color::Indexed(_) => write!(f, "{:?}", self.0),
        }
    }
}

/// Parse a ratatui Color from a name string (case-insensitive).
fn color_from_name(name: &str) -> Option<Color> {
    match name.to_ascii_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "lightred" => Some(Color::LightRed),
        "lightgreen" => Some(Color::LightGreen),
        "lightyellow" => Some(Color::LightYellow),
        "lightblue" => Some(Color::LightBlue),
        "lightmagenta" => Some(Color::LightMagenta),
        "lightcyan" => Some(Color::LightCyan),
        "reset" => Some(Color::Reset),
        _ => None,
    }
}

/// Parse a hex color string like "#FFA500" into RGB.
fn parse_hex(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let r = ((value >> 16) & 0xFF) as u8;
    let g = ((value >> 8) & 0xFF) as u8;
    let b = (value & 0xFF) as u8;
    Some(Color::Rgb(r, g, b))
}

/// Parse an ANSI code string like "A80" into RGB via anstyle-lossy.
fn parse_ansi_code(s: &str) -> Option<Color> {
    let num_str = s.strip_prefix('A')?;
    let code: u8 = num_str.parse().ok()?;
    let ansi256 = anstyle::Ansi256Color(code);
    let rgb = anstyle_lossy::xterm_to_rgb(ansi256, anstyle_lossy::palette::Palette::default());
    Some(Color::Rgb(rgb.0, rgb.1, rgb.2))
}

/// Parse a color from a TOML string value.
fn parse_color_string(s: &str) -> Option<Color> {
    if s.starts_with('#') {
        parse_hex(s)
    } else if let Some(rest) = s.strip_prefix('A') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            parse_ansi_code(s)
        } else {
            color_from_name(s)
        }
    } else {
        color_from_name(s)
    }
}

struct ThemeColorVisitor;

impl<'de> Visitor<'de> for ThemeColorVisitor {
    type Value = ThemeColor;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a color string (ANSI name, hex, or ANSI code) or an RGB array [r, g, b]"
        )
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        parse_color_string(v)
            .map(ThemeColor)
            .ok_or_else(|| de::Error::custom(format!("invalid color string: {v}")))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let r: u8 = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("expected 3 RGB values"))?;
        let g: u8 = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("expected 3 RGB values"))?;
        let b: u8 = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("expected 3 RGB values"))?;
        Ok(ThemeColor(Color::Rgb(r, g, b)))
    }
}

impl<'de> serde::Deserialize<'de> for ThemeColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ThemeColorVisitor)
    }
}

impl serde::Serialize for ThemeColor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Color::Rgb(r, g, b) => {
                use serde::ser::SerializeTuple;
                let mut tuple = serializer.serialize_tuple(3)?;
                tuple.serialize_element(&r)?;
                tuple.serialize_element(&g)?;
                tuple.serialize_element(&b)?;
                tuple.end()
            }
            _ => serializer.serialize_str(&self.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use ratatui::style::Color;

    // --- ANSI name parsing ---

    #[rstest::rstest]
    #[case("yellow", Color::Yellow)]
    #[case("Yellow", Color::Yellow)]
    #[case("YELLOW", Color::Yellow)]
    #[case("darkgray", Color::DarkGray)]
    #[case("DarkGray", Color::DarkGray)]
    #[case("darkgrey", Color::DarkGray)]
    #[case("white", Color::White)]
    #[case("red", Color::Red)]
    #[case("green", Color::Green)]
    #[case("cyan", Color::Cyan)]
    #[case("gray", Color::Gray)]
    #[case("reset", Color::Reset)]
    fn ansi_name_parses(#[case] input: &str, #[case] expected: Color) {
        // Given a color name string.
        // When parsing.
        let result = color_from_name(input);
        // Then it resolves to the expected Color.
        assert_eq!(result, Some(expected));
    }

    #[rstest::rstest]
    fn unknown_name_returns_none() {
        // Given an invalid color name.
        // When parsing.
        let result = color_from_name("notacolor");
        // Then it returns None.
        assert_eq!(result, None);
    }

    // --- Hex parsing ---

    #[rstest::rstest]
    #[case("#FFA500", Color::Rgb(255, 165, 0))]
    #[case("#ffa500", Color::Rgb(255, 165, 0))]
    #[case("#000000", Color::Rgb(0, 0, 0))]
    #[case("#FFFFFF", Color::Rgb(255, 255, 255))]
    fn hex_parses(#[case] input: &str, #[case] expected: Color) {
        // Given a hex color string.
        // When parsing.
        let result = parse_hex(input);
        // Then it resolves to the expected RGB Color.
        assert_eq!(result, Some(expected));
    }

    #[rstest::rstest]
    #[case("#FFF")]
    #[case("#12345")]
    #[case("FFA500")]
    fn invalid_hex_returns_none(#[case] input: &str) {
        // Given an invalid hex string.
        // When parsing.
        let result = parse_hex(input);
        // Then it returns None.
        assert_eq!(result, None);
    }

    // --- ANSI code parsing ---

    #[rstest::rstest]
    fn ansi_code_80_resolves() {
        // Given an ANSI 256-color code "A80".
        // When parsing.
        let result = parse_ansi_code("A80");
        // Then it resolves to an RGB Color.
        assert!(result.is_some());
        let Color::Rgb(r, g, b) = result.unwrap() else {
            panic!("expected Rgb color");
        };
        assert!((r, g, b) != (0, 0, 0)); // just verify it's valid
    }

    #[rstest::rstest]
    fn ansi_code_0_resolves() {
        // Given ANSI code "A0".
        // When parsing.
        let result = parse_ansi_code("A0");
        // Then it resolves.
        assert!(result.is_some());
    }

    #[rstest::rstest]
    fn ansi_code_too_large_returns_none() {
        // Given an invalid code "A256" (u8 max is 255).
        // When parsing.
        let result = parse_ansi_code("A256");
        // Then it returns None.
        assert_eq!(result, None);
    }

    // --- parse_color_string dispatch ---

    #[rstest::rstest]
    fn string_dispatches_to_hex() {
        // Given a string starting with #.
        let result = parse_color_string("#112233");
        // Then it is treated as hex.
        assert_eq!(result, Some(Color::Rgb(0x11, 0x22, 0x33)));
    }

    #[rstest::rstest]
    fn string_dispatches_to_ansi_code() {
        // Given a string starting with A followed by digits.
        let result = parse_color_string("A1");
        // Then it is treated as an ANSI code.
        assert!(result.is_some());
    }

    #[rstest::rstest]
    fn string_dispatches_to_name() {
        // Given a plain color name.
        let result = parse_color_string("cyan");
        // Then it is treated as an ANSI name.
        assert_eq!(result, Some(Color::Cyan));
    }
}
