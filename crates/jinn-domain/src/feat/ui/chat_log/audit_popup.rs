//! Audit popup - text rendering for a chat entry's context-change history.
//!
//! Produces the [`Line`]s shown inside the audit popup overlay. Pure function:
//! given a [`ChatEntry`] and a [`Theme`], returns the header line followed by
//! one body line per recorded [`ContextChangeEvent`] (oldest first).
//!
//! Popup geometry and rendering live in the TUI layer; this module owns only
//! the textual content.

use crate::protocol::{
    ChangeSource, ChatEntry, ContextChangeEvent, ContextOverride,
};
use crate::protocol::EntryTiming;
use crate::feat::theme::Theme;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Render the audit popup's text for `entry`.
///
/// Returns a header line followed by one body line per event in `context_history`
/// (chronological order). For entries with no history, returns a header plus a
/// single `(no events recorded)` body line.
///
/// All lines carry the infopopup background color so they render cleanly over
/// the chat-log area underneath.
pub fn format_audit_lines(entry: &ChatEntry, theme: &Theme) -> Vec<Line<'static>> {
    let header_style = Style::default()
        .fg(theme.infopopup_title)
        .bg(theme.infopopup_bg);
    let body_style = Style::default()
        .fg(theme.infopopup_fg)
        .bg(theme.infopopup_bg);

    let mut lines = Vec::new();
    lines.push(centered_title("Metadata", AUDIT_POPUP_WIDTH, header_style));
    lines.push(Line::from(vec![Span::styled(
        format!("Sent: {}", format_timestamp(&entry.timing.at())),
        body_style,
    )]));

    if let EntryTiming::Streamed { .. } = &entry.timing {
        let ttft_text = match entry.timing.ttft() {
            Some(d) => format_duration(&d),
            None => "(pending)".to_owned(),
        };
        let duration_text = match entry.timing.total_duration() {
            Some(d) => format_duration(&d),
            None => "(pending)".to_owned(),
        };
        lines.push(Line::from(vec![
            Span::styled("TTFT: ".to_owned(), body_style),
            Span::styled(
                ttft_text,
                Style::default()
                    .fg(theme.input_mode_queue)
                    .bg(theme.infopopup_bg),
            ),
            Span::styled("  Stream Duration: ".to_owned(), body_style),
            Span::styled(
                duration_text,
                Style::default().fg(theme.streaming).bg(theme.infopopup_bg),
            ),
        ]));
    }

    lines.push(Line::from(vec![Span::styled(String::new(), body_style)]));

    let count = entry.context_history.len();
    let current = context_override_label(entry.context_override);
    let audit_label = format!("audit ({count} events) ({current})");
    lines.push(centered_title(
        &audit_label,
        AUDIT_POPUP_WIDTH,
        header_style,
    ));

    if entry.context_history.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "(no events recorded)",
            body_style,
        )]));
    } else {
        for event in &entry.context_history {
            lines.push(format_event_line(event, body_style));
        }
    }

    lines
}

/// Format one event as `[source] From -> To`.
fn format_event_line(event: &ContextChangeEvent, style: Style) -> Line<'static> {
    let text = format!(
        "[{}] {} -> {}",
        source_label(&event.source),
        context_override_label(event.from),
        context_override_label(event.to),
    );
    Line::from(vec![Span::styled(text, style)])
}

/// Render the human-readable label for a [`ChangeSource`].
///
/// `User` becomes the literal string `"user"`. `Worker { name }` and
/// `Internal { label }` use the inner string verbatim, with no `worker:` /
/// `internal:` prefix — the bracket convention in the rendered line already
/// communicates "this is a non-user source".
fn source_label(source: &ChangeSource) -> String {
    match source {
        ChangeSource::User => "user".to_owned(),
        ChangeSource::Worker { name } => name.clone(),
        ChangeSource::Internal { label } => label.clone(),
    }
}

/// Render the human-readable label for a [`ContextOverride`].
fn context_override_label(o: ContextOverride) -> &'static str {
    match o {
        ContextOverride::Default => "Default",
        ContextOverride::ForcedInclude => "ForcedInclude",
        ContextOverride::ForcedExclude => "ForcedExclude",
    }
}

/// Build a title line centered within `width` columns, padded with `-----`.
///
/// The label is surrounded by one space on each side, then `-` characters
/// fill the remaining width. If the padding is odd, the extra `-` goes right.
fn centered_title(label: &str, width: u16, style: Style) -> Line<'static> {
    let inner = (width as usize).saturating_sub(2);
    let dash_budget = inner.saturating_sub(label.len() + 2); // +2 for spaces around label
    let left = dash_budget / 2;
    let right = dash_budget - left;
    let text = format!("{} {} {}", "-".repeat(left), label, "-".repeat(right));
    Line::from(vec![Span::styled(text, style)])
}

/// Fixed width of the audit popup, in terminal columns.
///
/// Deliberately constant regardless of terminal size so the popup is always the
/// same shape. See `audit_popup_rect` for placement.
pub const AUDIT_POPUP_WIDTH: u16 = 70;

/// Format a timestamp as `YYYY-MM-DD HH:MM:SS (<relative>)`.
///
/// The absolute part uses UTC. The relative part is a human-readable
/// string like \"5 minutes ago\", \"2 hours ago\", \"3 days ago\", etc.
fn format_timestamp(ts: &jiff::Timestamp) -> String {
    let absolute = ts.strftime("%Y-%m-%d %H:%M:%S").to_string();
    let relative = format_relative_time(ts);
    format!("{absolute} ({relative})")
}

/// Compute a human-readable relative time string from a timestamp to now.
///
/// Produces strings like "2 seconds ago", "5 minutes ago", "2 hours ago",
/// \"3 days ago\", \"5 months ago\", \"1 year ago\".
fn format_relative_time(ts: &jiff::Timestamp) -> String {
    let now = jiff::Timestamp::now();
    // Timestamp::since only supports up to Unit::Hour (days require calendar context).
    // So we get hours/minutes/seconds and derive days/months/years from total hours.
    let Ok(span) = now.since((jiff::Unit::Hour, *ts)) else {
        return "unknown".to_owned();
    };

    let total_hours = span.get_hours().unsigned_abs();
    let minutes = span.get_minutes().unsigned_abs() as u32;

    // Derive larger units from total hours (approximate).
    let years = total_hours / (365 * 24);
    let months = total_hours / (30 * 24);
    let days = total_hours / 24;
    let hours = total_hours % 24;

    if years > 0 {
        format_units(years, "year")
    } else if months > 0 {
        format_units(months, "month")
    } else if days > 0 {
        format_units(days, "day")
    } else if hours > 0 {
        format_units(hours, "hour")
    } else if minutes > 0 {
        format_units(minutes, "minute")
    } else {
        let seconds = span.get_seconds().unsigned_abs() as u32;
        if seconds > 0 {
            format_units(seconds, "second")
        } else {
            "just now".to_owned()
        }
    }
}

/// Format a count with a pluralized unit and \"ago\" suffix.
fn format_units(count: u32, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// Format a [`jiff::SignedDuration`] as a human-readable elapsed-time string.
///
/// Durations under 60 seconds render as `X.Xs` (e.g. "2.1s", "0.5s").
/// Durations of 60 seconds or more render as `Xm Ys` (e.g. "1m 23s").
fn format_duration(d: &jiff::SignedDuration) -> String {
    let total_secs = d.as_secs();
    if total_secs < 60 {
        let tenths = (d.subsec_millis() / 100) as u32;
        format!("{total_secs}.{tenths}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}s")
    }
}

/// Compute the screen rectangle for the audit popup.
///
/// Placement rules (in priority order):
///
/// 1. **Right-aligned** to the chat-log area: the popup's right edge equals
///    `chat_log_area.x + chat_log_area.width`. If `chat_log_area.width` is
///    smaller than `AUDIT_POPUP_WIDTH`, the popup shrinks to fit and its left
///    edge is clamped to `chat_log_area.x`.
/// 2. **Top edge aligned to `entry_top_y`** (the screen row of the selected
///    entry's first line). If `entry_top_y < chat_log_area.y` (the entry is
///    scrolled above the viewport), the popup pins to `chat_log_area.y`.
/// 3. **Bottom edge clamped to `frame_area.bottom()`**: if the popup would
///    extend below the visible terminal, it slides up so its bottom edge sits
///    exactly at `frame_area.bottom()`. After this clamp, if `popup_y` would
///    drop below `chat_log_area.y`, it is re-clamped to `chat_log_area.y`
///    (popup may overflow the bottom in extremely short terminals — this is
///    accepted as the lesser evil vs. invisible popup).
///
/// `content_line_count` is the number of body lines (header + events or
/// `(no events recorded)`). The popup reserves 2 additional rows for top and
/// bottom borders.
pub fn audit_popup_rect(
    chat_log_area: ratatui::layout::Rect,
    entry_top_y: u16,
    content_line_count: usize,
) -> ratatui::layout::Rect {
    // Width: fixed 60, but never wider than the chat-log area.
    let popup_width = AUDIT_POPUP_WIDTH.min(chat_log_area.width);
    let popup_x = chat_log_area.x + chat_log_area.width.saturating_sub(popup_width);

    // Height: content + top + bottom borders.
    let popup_height = (content_line_count as u16).saturating_add(2);

    // Step 1: pin to entry top, but never above the chat-log area.
    let pinned_top = entry_top_y.max(chat_log_area.y);

    // Step 2: if the popup would extend below the chat-log area, slide it up.
    let area_bottom = chat_log_area.y.saturating_add(chat_log_area.height);
    let mut popup_y = pinned_top;
    let popup_bottom = popup_y.saturating_add(popup_height);
    if popup_bottom > area_bottom {
        popup_y = area_bottom.saturating_sub(popup_height);
    }

    // Step 3: never slide above the chat-log area (last-resort clamp).
    popup_y = popup_y.max(chat_log_area.y);

    ratatui::layout::Rect::new(popup_x, popup_y, popup_width, popup_height)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unreachable,
        clippy::string_slice,
        clippy::uninlined_format_args,
        reason = "test code"
    )]
    //! BDD-style tests for `format_audit_lines`.
    //!
    //! Each test asserts exactly one observable behavior of the formatter:
    //! header text, body text per source variant, ordering, or empty-history
    //! fallback.

    use super::*;
    use crate::protocol::ChatEntry;
    use crate::feat::theme::default_theme;

    /// Convenience: format `entry` with the default theme.
    fn format(entry: &ChatEntry) -> Vec<Line<'static>> {
        format_audit_lines(entry, &default_theme())
    }

    /// Read the unstyled string of a line (for assertions that don't care about color).
    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.clone())
            .collect::<Vec<_>>()
            .join("")
    }

    #[rstest::rstest]
    #[test]
    fn centered_title_produces_exact_width_line() {
        // Given label "Metadata" and popup width 60.
        // Content area is 58 (60 - 2 borders).
        let theme = default_theme();
        let style = Style::default()
            .fg(theme.infopopup_title)
            .bg(theme.infopopup_bg);
        let line = centered_title("Metadata", 60, style);

        // Then the rendered text is exactly 58 characters (content width).
        assert_eq!(text(&line).len(), 58);
    }

    #[rstest::rstest]
    #[test]
    fn centered_title_centers_label_with_dash_padding() {
        // Given label "Metadata" and popup width 60.
        // Content area is 58 (60 - 2 borders).
        let theme = default_theme();
        let style = Style::default()
            .fg(theme.infopopup_title)
            .bg(theme.infopopup_bg);
        let line = centered_title("Metadata", 60, style);
        let rendered = text(&line);

        // Then the label is surrounded by spaces and dashes.
        assert!(rendered.contains(" Metadata "));
        // And the line starts and ends with dashes.
        assert!(rendered.starts_with('-'));
        assert!(rendered.ends_with('-'));
        // And dash count is 48 (58 content - 8 label - 2 spaces).
        let dash_count = rendered.chars().filter(|c| *c == '-').count();
        assert_eq!(dash_count, 48);
    }

    #[rstest::rstest]
    #[test]
    fn format_timestamp_recent_shows_absolute_and_relative() {
        // Given a timestamp 30 seconds ago.
        let ts = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_secs(30))
            .expect("30s ago is valid");

        // When formatting.
        let result = format_timestamp(&ts);

        // Then the absolute portion looks like a date-time.
        assert!(
            result.contains('T') || result.contains(' '),
            "should contain date/time separator: {result}"
        );
        // And the relative portion says "30 seconds ago".
        assert!(
            result.contains("30 seconds ago"),
            "should say '30 seconds ago' for 30s ago: {result}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_timestamp_old_shows_absolute_and_days_ago() {
        // Given a timestamp 5 days ago.
        let ts = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_hours(24 * 5))
            .expect("5 days ago is valid");

        // When formatting.
        let result = format_timestamp(&ts);

        // Then the relative portion says \"5 days ago\".
        assert!(
            result.contains("5 days ago"),
            "should say '5 days ago': {result}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_includes_metadata_section_before_audit() {
        // Given a default entry.
        let entry = ChatEntry::user("hi");

        // When formatting.
        let lines = format(&entry);

        // Then line 0 is the centered Metadata title.
        let title = text(&lines[0]);
        assert!(
            title.contains("Metadata"),
            "first line should contain Metadata: {title}"
        );
        // And line 1 is the Sent: line.
        let sent = text(&lines[1]);
        assert!(
            sent.starts_with("Sent:"),
            "second line should start with Sent:: {sent}"
        );
        // And line 3 is the centered audit title.
        let audit = text(&lines[3]);
        assert!(
            audit.contains("audit"),
            "fourth line should contain audit: {audit}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_includes_blank_line_between_sections() {
        // Given a default entry.
        let entry = ChatEntry::user("hi");

        // When formatting.
        let lines = format(&entry);

        // Then line 2 (between Metadata and audit) is blank.
        assert_eq!(text(&lines[2]), "");
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_empty_history_returns_header_and_no_events_line() {
        // Given a default entry (no history, Default override).
        let entry = ChatEntry::user("hi");

        // When formatting.
        let lines = format(&entry);

        // Then line count is 5: Metadata title, Sent, blank, audit title, placeholder.
        assert_eq!(lines.len(), 5);
        assert_eq!(
            text(&lines[3]),
            "-------------------- audit (0 events) (Default) --------------------"
        );
        // And the body is the placeholder.
        assert_eq!(text(&lines[4]), "(no events recorded)");
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_one_user_event_renders_bracketed_source() {
        // Given a user entry that was excluded by the user.
        let mut entry = ChatEntry::user("hi");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);

        // When formatting.
        let lines = format(&entry);

        // Then the header shows the current override.
        assert_eq!(lines.len(), 5);
        assert_eq!(
            text(&lines[3]),
            "----------------- audit (1 events) (ForcedExclude) -----------------"
        );
        // And the body line shows the transition with [user] source.
        assert_eq!(text(&lines[4]), "[user] Default -> ForcedExclude");
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_worker_event_uses_name_in_brackets() {
        // Given an entry excluded by the compactor worker.
        let mut entry = ChatEntry::user("hi");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Worker {
                name: "compactor".to_owned(),
            },
        );

        // When formatting.
        let lines = format(&entry);

        // Then the source label is the bare worker name (no `worker:` prefix).
        assert_eq!(text(&lines[4]), "[compactor] Default -> ForcedExclude");
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_internal_event_uses_label_in_brackets() {
        // Given an entry excluded by an internal sweep.
        let mut entry = ChatEntry::user("hi");
        entry.apply_context_override(
            ContextOverride::ForcedExclude,
            ChangeSource::Internal {
                label: "dangling-cleanup".to_owned(),
            },
        );

        // When formatting.
        let lines = format(&entry);

        // Then the source label is the bare internal label.
        assert_eq!(
            text(&lines[4]),
            "[dangling-cleanup] Default -> ForcedExclude"
        );
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_multiple_events_preserves_insertion_order() {
        // Given an entry toggled twice (Default -> Exclude -> Default).
        let mut entry = ChatEntry::user("hi");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        entry.apply_context_override(ContextOverride::Default, ChangeSource::User);

        // When formatting.
        let lines = format(&entry);

        // Then 6 lines total (3 metadata + 3 audit) and order matches insertion.
        assert_eq!(lines.len(), 6);
        assert_eq!(text(&lines[4]), "[user] Default -> ForcedExclude");
        assert_eq!(text(&lines[5]), "[user] ForcedExclude -> Default");
    }

    #[rstest::rstest]
    #[test]
    fn format_audit_lines_header_uses_current_override_not_last_event() {
        // Given an entry with one event but the override was reset to Default
        // (so context_history has length 1, current state is Default).
        let mut entry = ChatEntry::user("hi");
        entry.apply_context_override(ContextOverride::ForcedExclude, ChangeSource::User);
        entry.apply_context_override(ContextOverride::Default, ChangeSource::User);

        // When formatting.
        let lines = format(&entry);

        // Then the header parenthetical shows the *current* Default, not the last event's `to`.
        assert!(text(&lines[3]).contains("(Default) ---"));
        // And event count is 2, not 1.
        assert!(text(&lines[3]).contains("(2 events)"));
    }

    mod rect_tests {
        //! Unit tests for `audit_popup_rect`.
        //!
        //! Each test pins one placement rule: in-viewport, scrolled-above,
        //! overflow-bottom, fits-exactly, right-edge alignment, very-small terminal.

        use super::*;
        use ratatui::layout::Rect;

        /// Helper to construct a typical chat-log layout (no separate frame).
        fn layout() -> Rect {
            // 100x40 chat-log area.
            Rect::new(0, 0, 100, 40)
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_in_viewport_top_aligns_to_entry() {
            // Given entry at row 10, content has 3 lines, area is 40 rows tall.
            let chat = layout();

            // When computing the rect.
            let rect = audit_popup_rect(chat, 10, 3);

            // Then the popup top is pinned to the entry's row.
            assert_eq!(rect.y, 10);
            // And the popup height is content (3) + 2 borders.
            assert_eq!(rect.height, 5);
            // And the popup width is the constant 60.
            assert_eq!(rect.width, AUDIT_POPUP_WIDTH);
            // And the popup right edge equals the chat-log right edge.
            assert_eq!(rect.x + rect.width, chat.x + chat.width);
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_entry_scrolled_above_viewport_pins_to_chat_top() {
            // Given entry_top_y is above the chat-log top (scrolled off-screen).
            // When computing with entry_top_y = 0 but chat_log_area.y = 5.
            let chat_offset = Rect::new(0, 5, 100, 35);
            let rect = audit_popup_rect(chat_offset, 0, 3);

            // Then the popup pins to the chat-log top, not above it.
            assert_eq!(rect.y, chat_offset.y);
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_overflow_bottom_slides_up() {
            // Given entry near the bottom and a tall popup that would overflow.
            let chat = layout();
            // Entry at row 35, popup needs 8 rows (content 6 + 2 borders).
            // area_bottom = 40, so popup would render at 35..43 — overflow by 3.
            let rect = audit_popup_rect(chat, 35, 6);

            // Then popup slides up so its bottom edge equals area bottom.
            assert_eq!(rect.y + rect.height, chat.y + chat.height);
            // And the popup y is 32 (40 - 8).
            assert_eq!(rect.y, 32);
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_fits_exactly_does_not_slide() {
            // Given entry at a row where the popup fits exactly to area bottom.
            let chat = layout();
            // 3 content lines + 2 borders = 5 height. Entry at row 35 → 35+5=40 = area_bottom.
            let rect = audit_popup_rect(chat, 35, 3);

            // Then no slide; popup top equals entry top.
            assert_eq!(rect.y, 35);
            assert_eq!(rect.y + rect.height, chat.y + chat.height);
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_right_edge_aligns_to_chat_log_right_edge() {
            // Given a chat-log area offset from the frame's left edge (sidebar takes 20 cols).
            let chat = Rect::new(20, 0, 80, 40);

            // When computing the rect.
            let rect = audit_popup_rect(chat, 5, 2);

            // Then popup right edge = chat right edge = 100.
            assert_eq!(rect.x + rect.width, chat.x + chat.width);
            assert_eq!(rect.x + rect.width, 100);
            // And popup left edge = 100 - 70 = 30.
            assert_eq!(rect.x, 30);
        }

        #[rstest::rstest]
        #[test]
        fn audit_popup_rect_chat_log_narrower_than_70_shrinks_popup_width() {
            // Given a chat-log area only 50 columns wide.
            let chat = Rect::new(0, 0, 50, 40);
            // When computing the rect.
            let rect = audit_popup_rect(chat, 5, 2);
            // Then popup width shrinks to chat width (50), not the constant 70.
            assert_eq!(rect.width, 50);
            // And popup left edge is at chat left edge.
            assert_eq!(rect.x, chat.x);
        }
    }
    mod format_duration_tests {
        //! Tests for the `format_duration` free function.

        use super::*;

        #[rstest::rstest]
        #[test]
        fn format_duration_seconds() {
            // Given a 2.1-second duration.
            let d = jiff::SignedDuration::from_secs(2) + jiff::SignedDuration::from_millis(100);

            // When formatting.
            let result = format_duration(&d);

            // Then it renders as "2.1s".
            assert_eq!(result, "2.1s");
        }

        #[rstest::rstest]
        #[test]
        fn format_duration_sub_second() {
            // Given a 0.5-second duration.
            let d = jiff::SignedDuration::from_millis(500);

            // When formatting.
            let result = format_duration(&d);

            // Then it renders as "0.5s".
            assert_eq!(result, "0.5s");
        }

        #[rstest::rstest]
        #[test]
        fn format_duration_minutes() {
            // Given an 83-second duration (1m 23s).
            let d = jiff::SignedDuration::from_secs(83);

            // When formatting.
            let result = format_duration(&d);

            // Then it renders as "1m 23s".
            assert_eq!(result, "1m 23s");
        }
    }

    // ── Timing line tests ──────────────────────────────────────────────

    /// Helper: create a streamed entry with specific timing.
    fn streamed_entry(
        dispatched_at: &str,
        first_token_at: Option<&str>,
        finished_at: Option<&str>,
    ) -> ChatEntry {
        let dispatched = dispatched_at.parse().expect("valid timestamp");
        let first_token = first_token_at.map(|s| s.parse().expect("valid timestamp"));
        let finished = finished_at.map(|s| s.parse().expect("valid timestamp"));
        let mut entry = ChatEntry::assistant("hello");
        entry.timing = EntryTiming::Streamed {
            dispatched_at: dispatched,
            first_token_at: first_token,
            finished_at: finished,
        };
        entry
    }

    /// Helper: find the timing line (contains both TTFT and Stream Duration).
    fn find_timing_line(lines: &[Line<'_>]) -> Option<usize> {
        lines.iter().position(|l| text(l).contains("TTFT:"))
    }

    #[rstest::rstest]
    #[test]
    fn instant_entry_shows_only_sent_no_timing() {
        // Given an instant entry (default user entry).
        let entry = ChatEntry::user("hi");

        // When formatting.
        let lines = format(&entry);

        // Then no line contains TTFT or Stream Duration.
        for (i, line) in lines.iter().enumerate() {
            let t = text(line);
            assert!(
                !t.contains("TTFT:"),
                "line {i} should not contain TTFT: {t}"
            );
            assert!(
                !t.contains("Stream Duration:"),
                "line {i} should not contain Stream Duration: {t}"
            );
        }
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_shows_ttft_and_duration() {
        // Given a streamed entry with both timestamps set.
        let entry = streamed_entry(
            "2024-01-15T10:30:00Z",
            Some("2024-01-15T10:30:02Z"),
            Some("2024-01-15T10:30:15Z"),
        );

        // When formatting.
        let lines = format(&entry);

        // Then there is a line containing both TTFT and Stream Duration.
        let idx = find_timing_line(&lines).expect("should have a timing line");
        let t = text(&lines[idx]);
        assert!(t.contains("TTFT:"), "timing line should contain TTFT: {t}");
        assert!(
            t.contains("Stream Duration:"),
            "timing line should contain Stream Duration: {t}"
        );
        // And TTFT shows ~2s.
        assert!(t.contains("2.0s"), "TTFT should be 2.0s: {t}");
        // And Stream Duration shows ~13s (finished_at 15s - first_token_at 2s).
        assert!(t.contains("13.0s"), "Stream Duration should be 13.0s: {t}");
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_shows_pending_for_missing_first_token() {
        // Given a streamed entry with no first_token_at.
        let entry = streamed_entry("2024-01-15T10:30:00Z", None, Some("2024-01-15T10:30:15Z"));

        // When formatting.
        let lines = format(&entry);

        // Then the timing line shows (pending) for TTFT.
        let idx = find_timing_line(&lines).expect("should have a timing line");
        let t = text(&lines[idx]);
        assert!(t.contains("(pending)"), "TTFT should show (pending): {t}");
    }

    #[rstest::rstest]
    #[test]
    fn streamed_entry_shows_pending_for_missing_finished() {
        // Given a streamed entry with no finished_at.
        let entry = streamed_entry("2024-01-15T10:30:00Z", Some("2024-01-15T10:30:02Z"), None);

        // When formatting.
        let lines = format(&entry);

        // Then the timing line shows (pending) for Stream Duration.
        let idx = find_timing_line(&lines).expect("should have a timing line");
        let t = text(&lines[idx]);
        assert!(
            t.contains("(pending)"),
            "Stream Duration should show (pending): {t}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn ttft_value_uses_queue_color() {
        // Given a streamed entry.
        let entry = streamed_entry(
            "2024-01-15T10:30:00Z",
            Some("2024-01-15T10:30:02Z"),
            Some("2024-01-15T10:30:15Z"),
        );
        let theme = default_theme();

        // When formatting.
        let lines = format_audit_lines(&entry, &theme);

        // Then the TTFT value span uses the queue color.
        let idx = find_timing_line(&lines).expect("should have a timing line");
        let ttft_span = &lines[idx].spans[1]; // index 1 is the TTFT value
        assert_eq!(
            ttft_span.style.fg,
            Some(theme.input_mode_queue),
            "TTFT value should use input_mode_queue color"
        );
    }

    #[rstest::rstest]
    #[test]
    fn duration_value_uses_streaming_color() {
        // Given a streamed entry.
        let entry = streamed_entry(
            "2024-01-15T10:30:00Z",
            Some("2024-01-15T10:30:02Z"),
            Some("2024-01-15T10:30:15Z"),
        );
        let theme = default_theme();

        // When formatting.
        let lines = format_audit_lines(&entry, &theme);

        // Then the Stream Duration value span uses the streaming color.
        let idx = find_timing_line(&lines).expect("should have a timing line");
        let duration_span = &lines[idx].spans[3]; // index 3 is the Stream Duration value
        assert_eq!(
            duration_span.style.fg,
            Some(theme.streaming),
            "Stream Duration value should use streaming color"
        );
    }
}
