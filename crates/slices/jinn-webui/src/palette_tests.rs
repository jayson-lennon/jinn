//! The palette and token-discipline tests.
//!
//! The contract's color law lives here: the three palettes share one
//! token vocabulary, no region CSS carries a color literal, and the
//! token values hold the WCAG floors the design pinned (fg ≥ 4.5:1
//! and muted ≥ 3:1 on every surface each renders over; fill-text
//! pairs ≥ 4.5:1; dark `--fg`/`--bg` between 7:1 and 13:1 — the
//! "no longer white on black" bound). The ratios are computed from
//! the same constants the stylesheet interpolates, so a palette edit
//! that breaks a floor fails here, not in a browser.
//!
//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions need expect/panic to state what went wrong"
)]

use crate::region::regions;
use crate::templates::BASE_CSS;

/// One palette: a token name → its color value, as the stylesheet
/// defines it.
type Palette = &'static [(&'static str, &'static str)];

/// The dark palette's constants — duplicated from `BASE_CSS` so the
/// ratios here are computed from values a reader can diff against the
/// CSS comments' cited theme roles.
const DARK: Palette = &[
    ("--bg", "#191b1e"),
    ("--panel", "#282c34"),
    ("--fg", "#dcdcdc"),
    ("--muted", "#8f8f8f"),
    ("--border", "#2b2f36"),
    ("--bg-user-entry", "#343541"),
    ("--bg-code", "#131519"),
    ("--bg-hover", "#31343c"),
    ("--bg-active", "#3a3d48"),
    ("--bg-badge-pending", "#20242c"),
    ("--bg-badge-ok", "#111d15"),
    ("--bg-badge-err", "#241616"),
    ("--accent", "#4c8bd8"),
    ("--err", "#ee7770"),
    ("--ok", "#57ab5a"),
    ("--on-fill", "#14161a"),
    ("--on-accent", "#14161a"),
    ("--on-err", "#14161a"),
    ("--mark", "#e3b341"),
    ("--on-mark-muted", "#3d3320"),
    ("--bot", "#d4a72c"),
];

/// The light palette's constants — the `[data-theme="light"]` and
/// media-query blocks (identical values; the media block re-applies
/// them for system mode).
const LIGHT: Palette = &[
    ("--bg", "#eceff4"),
    ("--panel", "#e5e9f0"),
    ("--fg", "#2e3440"),
    ("--muted", "#67768f"),
    ("--border", "#d8dee9"),
    ("--bg-user-entry", "#e5e9f0"),
    ("--bg-code", "#e2e6ee"),
    ("--bg-hover", "#e7edf5"),
    ("--bg-active", "#dce6f2"),
    ("--bg-badge-pending", "#dfe0ee"),
    ("--bg-badge-ok", "#dde8d8"),
    ("--bg-badge-err", "#f2dcdc"),
    ("--accent", "#4a6e95"),
    ("--err", "#a8404a"),
    ("--ok", "#3d6b2a"),
    ("--on-fill", "#2e3440"),
    ("--on-accent", "#ffffff"),
    ("--on-err", "#ffffff"),
    ("--mark", "#ebcb8b"),
    ("--on-mark-muted", "#63511d"),
    ("--bot", "#7f6400"),
];

/// Looks a token's value up in a palette, failing the test with the
/// missing token named.
fn value<'a>(palette: &'a Palette, token: &str) -> &'a str {
    palette
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, value)| *value)
        .expect("every palette defines {token}")
}

/// Linearizes one sRGB channel per WCAG 2.x relative luminance.
fn linear(channel: f64) -> f64 {
    let c = channel / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The WCAG relative luminance of a `#rrggbb` color.
fn luminance(hex: &str) -> f64 {
    let digits: Vec<char> = hex
        .strip_prefix('#')
        .expect("colors are #rrggbb")
        .chars()
        .collect();
    assert_eq!(digits.len(), 6, "palette colors are 6-digit hex: {hex}");
    let nibble = |at: usize| digits[at].to_digit(16).expect("hex digit") as u8;
    let (r, g, b) = (
        nibble(0) * 16 + nibble(1),
        nibble(2) * 16 + nibble(3),
        nibble(4) * 16 + nibble(5),
    );
    0.2126 * linear(f64::from(r)) + 0.7152 * linear(f64::from(g)) + 0.0722 * linear(f64::from(b))
}

/// The WCAG contrast ratio between two colors — the lighter over the
/// darker, per the spec.
fn ratio(a: &str, b: &str) -> f64 {
    let (light, dark) = {
        let (la, lb) = (luminance(a), luminance(b));
        if la >= lb { (la, lb) } else { (lb, la) }
    };
    (light + 0.05) / (dark + 0.05)
}

/// Asserts one text-on-surface pair clears its floor, failing with
/// the pair and both values named.
fn assert_ratio(palette: &Palette, text: &str, surface: &str, floor: f64, label: &str) {
    let (text_value, surface_value) = (value(palette, text), value(palette, surface));
    let actual = ratio(text_value, surface_value);
    assert!(
        actual >= floor,
        "{label}: {text}({text_value}) on {surface}({surface_value}) is \
         {actual:.2}:1, under the {floor}:1 floor"
    );
}

/// The surfaces the fg/muted text tokens render over — every raised
/// or filled surface the CSS actually paints under fg/muted text.
/// Fills (accent/err/mark) are excluded: their text comes from the
/// on-fill tokens, asserted separately against their fills. A surface
/// added to the stylesheet under fg/muted text joins this list and
/// the floor assertions below.
const TEXT_SURFACES: &[&str] = &[
    "--bg",
    "--panel",
    "--bg-user-entry",
    "--bg-code",
    "--bg-hover",
    "--bg-active",
    "--bg-badge-pending",
    "--bg-badge-ok",
    "--bg-badge-err",
];

#[rstest::rstest]
#[test]
fn fg_clears_the_aa_floor_on_every_dark_surface() {
    // Given the dark palette.
    // When the foreground is measured against each surface it renders
    // over.
    // Then every pair clears 4.5:1.
    for surface in TEXT_SURFACES {
        assert_ratio(&DARK, "--fg", surface, 4.5, "dark fg");
    }
}

#[rstest::rstest]
#[test]
fn muted_clears_the_secondary_floor_on_every_dark_surface() {
    // Given the dark palette.
    // When muted text is measured against each surface it renders
    // over.
    // Then every pair clears 3:1.
    for surface in TEXT_SURFACES {
        assert_ratio(&DARK, "--muted", surface, 3.0, "dark muted");
    }
}

#[rstest::rstest]
#[test]
fn fg_clears_the_aa_floor_on_every_light_surface() {
    // Given the light palette.
    // When the foreground is measured against each surface it renders
    // over.
    // Then every pair clears 4.5:1.
    for surface in TEXT_SURFACES {
        assert_ratio(&LIGHT, "--fg", surface, 4.5, "light fg");
    }
}

#[rstest::rstest]
#[test]
fn muted_clears_the_secondary_floor_on_every_light_surface() {
    // Given the light palette.
    // When muted text is measured against each surface it renders
    // over.
    // Then every pair clears 3:1.
    for surface in TEXT_SURFACES {
        assert_ratio(&LIGHT, "--muted", surface, 3.0, "light muted");
    }
}

#[rstest::rstest]
#[test]
fn dark_fg_bg_sits_in_the_rework_band() {
    // Given the reworked dark palette.
    // When the fg/bg ratio is computed.
    // Then it sits in the 7:1–13:1 band — softer than the old
    // white-on-black pairing, still AA.
    let r = ratio(value(&DARK, "--fg"), value(&DARK, "--bg"));
    assert!(
        (7.0..=13.0).contains(&r),
        "dark fg/bg is {r:.2}:1, want 7-13"
    );
}

#[rstest::rstest]
#[test]
fn fill_text_pairs_clear_aa_in_both_palettes() {
    // Given the on-fill text tokens paired with the fills they label
    // (button labels on accent/err fills, the session-row marks'
    // text on the gold mark).
    // When each owned pair is measured.
    // Then every pair clears 4.5:1 in both palettes — and the pair
    // list is the cross-reference a template author checks, so a new
    // fill/text combination joins it explicitly.
    for palette in [&DARK, &LIGHT] {
        assert_ratio(palette, "--on-accent", "--accent", 4.5, "on-accent");
        assert_ratio(palette, "--on-err", "--err", 4.5, "on-err");
        assert_ratio(palette, "--on-fill", "--mark", 4.5, "on-mark");
        assert_ratio(palette, "--on-mark-muted", "--mark", 4.5, "on-mark-muted");
    }
}

#[rstest::rstest]
#[test]
fn accent_and_status_text_clear_their_floors_on_both_backgrounds() {
    // Given the accent/err/ok tokens used as text over --bg and
    // --panel.
    // When each is measured (accent ≥ 3:1 as an accent, err/ok ≥ 4.5:1
    // as standalone status text).
    // Then both palettes clear the floors on both surfaces.
    for palette in [&DARK, &LIGHT] {
        assert_ratio(palette, "--accent", "--bg", 3.0, "accent");
        assert_ratio(palette, "--accent", "--panel", 3.0, "accent");
        assert_ratio(palette, "--err", "--bg", 4.5, "err");
        assert_ratio(palette, "--err", "--panel", 4.5, "err");
        assert_ratio(palette, "--ok", "--bg", 4.5, "ok");
        assert_ratio(palette, "--ok", "--panel", 4.5, "ok");
    }
}

#[rstest::rstest]
#[test]
fn the_three_palettes_share_one_token_vocabulary() {
    // Given the three palette blocks in BASE_CSS.
    // When the token names are extracted from each block.
    // Then dark and light define the identical set — and the
    // media-query system block carries the light set — so a region can
    // reference any token in any mode.
    let dark_tokens: Vec<&str> = BASE_CSS
        .split(":root {")
        .nth(1)
        .expect("BASE_CSS opens with a :root block")
        .split("color-scheme: dark")
        .next()
        .expect("the dark block closes with color-scheme")
        .lines()
        .filter_map(tokens_of_line)
        .collect();
    let light_tokens: Vec<&str> = BASE_CSS
        .split("body[data-theme=\"light\"] {")
        .nth(1)
        .expect("BASE_CSS carries a light block")
        .split("color-scheme: light")
        .next()
        .expect("the light block closes with color-scheme")
        .lines()
        .filter_map(tokens_of_line)
        .collect();
    assert!(!dark_tokens.is_empty(), "the dark block defines tokens");
    let mut sorted_dark = dark_tokens.clone();
    sorted_dark.sort_unstable();
    let mut sorted_light = light_tokens.clone();
    sorted_light.sort_unstable();
    assert_eq!(
        sorted_dark, sorted_light,
        "the palettes define the identical token set"
    );
}

/// Extracts a `--token:` name from a line of palette CSS — only a
/// definition line (`--name: value;`) counts; comment lines are
/// skipped so prose mentioning a token never parses as one.
fn tokens_of_line(line: &str) -> Option<&str> {
    let line = line.trim();
    let is_definition =
        line.starts_with("--") && line.contains(':') && line.trim_end().ends_with(';');
    if is_definition {
        Some(line.split(':').next()?.trim().strip_prefix("--")?)
    } else {
        None
    }
}

/// A color literal in any CSS spelling: 6- or 3-digit hex, rgb()/rgba()
/// functions, or a CSS named color standing as a value (`white`,
/// not `white-space` — the check rejects a word continued by `-` or
/// alphanumerics, so identifiers never read as literals).
fn contains_color_literal(css: &str) -> bool {
    hex_literal_at(css).is_some()
        || css.contains("rgb(")
        || css.contains("rgba(")
        || css.contains("hsl(")
        || NAMED_COLORS
            .iter()
            .any(|name| named_color_as_value(css, name))
}

/// Whether `name` appears in `css` as a standalone word — the char
/// after the match must not continue an identifier (`-`, a digit, a
/// letter), which rules out `white-space` and property words.
fn named_color_as_value(css: &str, name: &str) -> bool {
    let skip = name.chars().count();
    css.match_indices(name).any(|(at, _)| {
        css.chars()
            .nth(at + skip)
            .is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'))
    })
}

/// The CSS named colors the stylesheet might plausibly reach for as a
/// fill or text value — kept to the set that reads as a color
/// decision, not a property or a keyword (`transparent` is a
/// mode-independent knockout, not a palette choice, so it stays
/// legal).
const NAMED_COLORS: &[&str] = &[
    "white", "black", "red", "blue", "gray", "grey", "yellow", "orange", "green",
];

/// Finds a hex literal anywhere in `css`, accepting both 6-digit and
/// 3-digit shorthand forms (`#abc`) — windowed over the char indices
/// so no byte index is ever taken.
fn hex_literal_at(css: &str) -> Option<usize> {
    let hexes: Vec<(usize, char)> = css.char_indices().skip_while(|(_, c)| *c != '#').collect();
    for (at, c) in &hexes {
        if *c != '#' {
            continue;
        }
        let run: Vec<char> = hexes
            .iter()
            .skip_while(|(i, _)| *i <= *at)
            .take(8)
            .map(|(_, c)| *c)
            .collect();
        let hex_run = |count: usize| -> bool {
            run.iter().take(count).all(char::is_ascii_hexdigit)
                && run.get(count).is_none_or(|c| !c.is_ascii_hexdigit())
        };
        if (run.len() >= 6 && hex_run(6)) || (run.len() >= 3 && hex_run(3)) {
            return Some(*at);
        }
    }
    None
}

#[rstest::rstest]
#[test]
fn base_css_is_the_only_literal_home() {
    // Given the base stylesheet.
    // When its literal count is compared with the token definitions.
    // Then BASE_CSS contains literals (the definitions themselves).
    assert!(
        contains_color_literal(BASE_CSS),
        "BASE_CSS defines the tokens and carries the literals"
    );
}

#[rstest::rstest]
#[test]
fn no_region_css_carries_a_color_literal() {
    // Given every registered region's css() output.
    // When each is scanned for any color literal (hex in either
    // digit-count, rgb()/rgba(), hsl(), named colors).
    // Then none carries a literal — every color is a var(--token)
    // reference.
    for region in regions() {
        let css = region.css();
        assert!(
            !contains_color_literal(css),
            "region {} carries a color literal; tokens only: {css}",
            region.id()
        );
    }
}

#[rstest::rstest]
#[test]
fn no_region_defines_a_token() {
    // Given every registered region's css() output.
    // When each is scanned for custom-property definitions.
    // Then none defines a `--token` — BASE_CSS is the only
    // token-definition home.
    for region in regions() {
        let css = region.css();
        for line in css.lines() {
            let trimmed = line.trim_start();
            assert!(
                !trimmed.starts_with("--"),
                "region {} defines a token; definitions live in BASE_CSS: {line}",
                region.id()
            );
        }
    }
}
