//! Provider picker TUI footers — the refresh/mode hint lines.
//!
//! The row *data* (availability, alias resolution, remote merge, sort,
//! active promotion) is the shared builder in
//! `jinn_provider_selection_msg::provider_rows` — one builder for the
//! TUI popup and the the web UI model picker, so the two surfaces cannot drift. The
//! row rendering (ratatui lines, highlight mapping) lives beside the
//! entry type in the msg crate's `entries` module; this module holds
//! only what is TUI-chrome: the two footer lines and truncation.

use jinn_theme::Theme;

/// Formats the two footer lines for the provider picker.
///
/// - Line 1: refresh info (`CTRL+R to refresh | Updated ...`).
/// - Line 2: mode info (`CTRL+A (toggle alloy) ...`) — always starts with the
///   toggle hint, then appends mode-specific ENTER/selection semantics.
///
/// Each line is independently truncated to `width`.
pub fn format_footers(
    model_cache: Option<&jinn_provider_config::ModelCache>,
    width: usize,
    theme: &Theme,
    selected_count: usize,
    alloy_mode: bool,
) -> Vec<ratatui::text::Line<'static>> {
    vec![
        truncate_line(refresh_line(model_cache, theme), width),
        truncate_line(mode_line(theme, selected_count, alloy_mode), width),
    ]
}

/// Line 1 of the provider footer: refresh keybind + last-updated time.
fn refresh_line(
    model_cache: Option<&jinn_provider_config::ModelCache>,
    theme: &Theme,
) -> ratatui::text::Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);
    let pipe = Span::styled("|".to_owned(), gray);

    let left = Span::styled("CTRL+R to refresh ".to_owned(), orange);

    let ts = model_cache.and_then(|c| c.last_updated_at);
    let right = match ts {
        Some(ts) => {
            let elapsed = jiff::Timestamp::now() - ts;
            let secs = elapsed.total(jiff::Unit::Second).unwrap_or(0.0).round() as u64;
            let duration = std::time::Duration::from_secs(secs);
            let human = humantime::format_duration(duration);
            let age_color = age_color(secs, theme);
            let formatted_ts = format!("{ts:.0}");
            let mid = format!(" Updated {formatted_ts} (");
            let ago = format!("{human} ago)");
            vec![
                Span::styled(mid, gray),
                Span::styled(ago, Style::default().fg(age_color)),
            ]
        }
        None => {
            vec![Span::styled(" Updated never".to_owned(), gray)]
        }
    };

    let mut spans = vec![left, pipe.clone()];
    spans.extend(right);
    Line::from(spans)
}

/// Line 2 of the provider footer: mode info.
///
/// Always leads with `CTRL+A (toggle alloy)`. Single mode then shows
/// `Enter selects`; alloy mode shows `N selected · TAB toggle · Enter adds+confirms`.
fn mode_line(
    theme: &Theme,
    selected_count: usize,
    alloy_mode: bool,
) -> ratatui::text::Line<'static> {
    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    let gray = Style::default().fg(theme.muted_text);
    let orange = Style::default().fg(theme.accent_action);

    let toggle = Span::styled("CTRL+A (toggle alloy)".to_owned(), orange);

    let rest: Vec<Span<'static>> = if alloy_mode {
        vec![
            Span::styled(format!(" · {selected_count} selected"), orange),
            Span::styled(" · TAB toggle".to_owned(), gray),
            Span::styled(" · Enter adds+confirms".to_owned(), gray),
        ]
    } else {
        vec![Span::styled(" · Enter selects".to_owned(), gray)]
    };

    let mut spans = vec![toggle];
    spans.extend(rest);
    Line::from(spans)
}

/// Returns the age-based color for the "time ago" text.
///
/// - `<= 2 weeks` → age_fresh
/// - `> 2 weeks, <= 4 weeks` → warning
/// - `> 4 weeks` → age_stale
pub fn age_color(secs: u64, theme: &Theme) -> ratatui::style::Color {
    const TWO_WEEKS: u64 = 14 * 24 * 60 * 60;
    const FOUR_WEEKS: u64 = 28 * 24 * 60 * 60;
    if secs <= TWO_WEEKS {
        theme.age_fresh
    } else if secs <= FOUR_WEEKS {
        theme.warning
    } else {
        theme.age_stale
    }
}

/// Truncates a styled line to fit within `width` terminal columns.
pub fn truncate_line(
    line: ratatui::text::Line<'static>,
    width: usize,
) -> ratatui::text::Line<'static> {
    use ratatui::text::{Line, Span};
    use unicode_segmentation::UnicodeSegmentation as _;

    let total_len: usize = line
        .spans
        .iter()
        .map(|s| s.content.graphemes(true).count())
        .sum();
    if total_len <= width {
        return line;
    }

    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        let char_count = span.content.graphemes(true).count();
        if remaining == 0 {
            break;
        }
        if char_count <= remaining {
            spans.push(span);
            remaining -= char_count;
        } else {
            let truncated: String = span.content.graphemes(true).take(remaining).collect();
            spans.push(Span::styled(truncated, span.style));
            remaining = 0;
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod entries_tests;
