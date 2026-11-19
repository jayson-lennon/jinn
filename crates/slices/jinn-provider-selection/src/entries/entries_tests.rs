//! The provider picker's TUI-side footer tests: the refresh/mode hint
//! lines and truncation. Row *data* (loading, sorting, promotion) is
//! tested in the msg crate's `provider_rows_tests`; row rendering
//! beside the render impl in the msg crate's `entries_tests`.

//! Test code: assertions use expect/panic to state what went wrong.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "test code"
)]

use jinn_theme::default_theme;
use unicode_segmentation::UnicodeSegmentation;

use crate::entries::*;

/// Flattens a line's spans into a single owned string for assertion.
fn line_text(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| &*s.content).collect()
}

#[rstest::rstest]
fn format_footers_without_timestamp_shows_never() {
    // Given no model cache.
    // When formatting the footers.
    let lines = format_footers(None, 80, &default_theme(), 0, false);

    // Then line 1 contains the refresh info.
    assert_eq!(lines.len(), 2, "footer must have exactly two lines");
    let refresh = line_text(&lines[0]);
    assert!(refresh.contains("Updated never"));
    assert!(refresh.contains("CTRL+R to refresh"));
}

#[rstest::rstest]
fn format_footers_with_timestamp_shows_age() {
    // Given a model cache with a recent timestamp (1 second ago).
    let ts = jiff::Timestamp::now()
        .checked_sub(jiff::Span::new().try_seconds(1).unwrap())
        .unwrap();
    let mut cache = jinn_provider_config::ModelCache::new();
    cache.last_updated_at = Some(ts);

    // When formatting the footers.
    let lines = format_footers(Some(&cache), 120, &default_theme(), 0, false);

    // Then line 1 contains the refresh info.
    let refresh = line_text(&lines[0]);
    assert!(refresh.contains("Updated"));
    assert!(refresh.contains("ago"));
    assert!(refresh.contains("CTRL+R to refresh"));
}

#[rstest::rstest]
fn format_footers_truncates_each_line_to_width() {
    // Given no timestamp and a very narrow width.
    // When formatting the footers with width 10.
    let lines = format_footers(None, 10, &default_theme(), 0, false);

    // Then each line fits within 10 columns.
    for (i, line) in lines.iter().enumerate() {
        let total_len: usize = line
            .spans
            .iter()
            .map(|s| s.content.graphemes(true).count())
            .sum();
        assert!(total_len <= 10, "line {i} too long: {total_len}");
    }
}

#[rstest::rstest]
fn format_footers_single_mode_line2_has_toggle_hint_and_enter() {
    // Given single mode (alloy_mode = false).
    // When formatting the footers.
    let lines = format_footers(None, 120, &default_theme(), 0, false);

    // Then line 2 always shows the toggle hint and the single-mode enter hint.
    let mode = line_text(&lines[1]);
    assert!(
        mode.contains("CTRL+A (toggle alloy)"),
        "toggle hint missing: {mode}"
    );
    assert!(mode.contains("Enter selects"), "enter hint missing: {mode}");
    assert!(
        !mode.contains("selected"),
        "count should not show in single mode: {mode}"
    );
}

#[rstest::rstest]
fn format_footers_alloy_mode_line2_has_toggle_hint_count_and_tab() {
    // Given alloy mode with 3 selected entries.
    // When formatting the footers.
    let lines = format_footers(None, 120, &default_theme(), 3, true);

    // Then line 2 shows the toggle hint, the count, and the TAB hint.
    let mode = line_text(&lines[1]);
    assert!(
        mode.contains("CTRL+A (toggle alloy)"),
        "toggle hint missing: {mode}"
    );
    assert!(mode.contains("3 selected"), "count missing: {mode}");
    assert!(mode.contains("TAB toggle"), "tab hint missing: {mode}");
    assert!(
        mode.contains("Enter adds+confirms"),
        "enter hint missing: {mode}"
    );
}

#[rstest::rstest]
fn age_color_returns_light_green_within_two_weeks() {
    // Given 1 second (well within 2 weeks).
    // When computing age color.
    let color = age_color(1, &default_theme());

    // Then the color is LightGreen.
    assert_eq!(color, ratatui::style::Color::LightGreen);
}

#[rstest::rstest]
fn age_color_returns_light_green_at_exactly_two_weeks() {
    // Given exactly 2 weeks in seconds.
    let two_weeks = 14 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(two_weeks, &default_theme());

    // Then the color is LightGreen.
    assert_eq!(color, ratatui::style::Color::LightGreen);
}

#[rstest::rstest]
fn age_color_returns_yellow_between_two_and_four_weeks() {
    // Given 3 weeks in seconds (between 2 and 4 weeks).
    let three_weeks = 21 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(three_weeks, &default_theme());

    // Then the color is Yellow.
    assert_eq!(color, ratatui::style::Color::Yellow);
}

#[rstest::rstest]
fn age_color_returns_yellow_at_exactly_four_weeks() {
    // Given exactly 4 weeks in seconds.
    let four_weeks = 28 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(four_weeks, &default_theme());

    // Then the color is Yellow.
    assert_eq!(color, ratatui::style::Color::Yellow);
}

#[rstest::rstest]
fn age_color_returns_red_beyond_four_weeks() {
    // Given 5 weeks in seconds (beyond 4 weeks).
    let five_weeks = 35 * 24 * 60 * 60;

    // When computing age color.
    let color = age_color(five_weeks, &default_theme());

    // Then the color is Red.
    assert_eq!(color, ratatui::style::Color::Red);
}

#[rstest::rstest]
fn truncate_line_noop_when_fits() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line that is 10 characters wide.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 20.
    let result = truncate_line(line.clone(), 20);

    // Then the line is unchanged.
    assert_eq!(result.spans.len(), 2);
    assert_eq!(result.spans[0].content, "hello ");
    assert_eq!(result.spans[1].content, "world");
}

#[rstest::rstest]
fn truncate_line_fits_within_width() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line that is 20 characters wide.
    let line = Line::from(vec![
        Span::styled("hello world ".to_owned(), Style::default()),
        Span::styled("test12345".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the total character count is exactly 8.
    let total_len: usize = result
        .spans
        .iter()
        .map(|s| s.content.graphemes(true).count())
        .sum();
    assert_eq!(total_len, 8);
}

#[rstest::rstest]
fn truncate_keeps_first_span_whole() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the first span is kept whole.
    assert_eq!(result.spans.len(), 2);
    assert_eq!(result.spans[0].content, "hello ");
}

#[rstest::rstest]
fn truncate_truncates_second_span() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the second span is truncated.
    assert_eq!(result.spans[1].content, "wo");
}

#[rstest::rstest]
fn partial_span_retains_style() {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    // Given a line where the second span will be partially truncated.
    let line = Line::from(vec![
        Span::styled("hello ".to_owned(), Style::default()),
        Span::styled("world".to_owned(), Style::default().fg(Color::Red)),
    ]);

    // When truncating to width 8.
    let result = truncate_line(line, 8);

    // Then the partial span retains its style.
    assert_eq!(result.spans[1].style.fg, Some(Color::Red));
}
