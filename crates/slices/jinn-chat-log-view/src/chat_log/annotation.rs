//! Annotation entry rendering - displays url_citation sources from server-side
//! web search tools (e.g. OpenRouter's `openrouter:web_search`).
//!
//! A collapsible block like tool entries and compaction blocks, collapsed by
//! default: the header shows the source count plus an expand hint, and the
//! citation list stays hidden until the user expands the entry (`e`).

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::shared::{Pad, RenderContext, pad_entry, pad_line_to_width};

/// Render a grouped annotation block: a header line, then one line per
/// citation showing `<title> <url>`.
///
/// Collapsed by default, showing only the header and a muted `(e to expand)`
/// hint. Expanded renders the full citation list without the hint.
pub fn to_lines(
    citations: &[jinn_provider::UrlCitation],
    ctx: &RenderContext,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(citations.len() + 2);

    let muted_style = Style::default().fg(ctx.theme.muted_text);
    let body_style = Style::default().fg(ctx.theme.primary_text);

    // Header line: bright, attention-grabbing bar spanning the full row,
    // like challenge alerts — sources are the "where did this come from"
    // record and must stand out from surrounding log text.
    let mut header_line = Line::from(Span::styled(
        format!("Sources ({})", citations.len()),
        Style::default()
            .fg(ctx.theme.sources_header_fg)
            .bg(ctx.theme.sources_header_bg),
    ));
    pad_line_to_width(
        &mut header_line,
        ctx.content_width,
        Style::default().bg(ctx.theme.sources_header_bg),
    );
    lines.push(header_line);

    if ctx.is_expanded {
        push_citation_lines(&mut lines, citations, body_style, muted_style);
    } else {
        // Collapsed: hint line, matching the compaction block.
        lines.push(Line::from(Span::styled("(e to expand)", muted_style)));
    }

    pad_entry(&mut lines, Pad::Both);
    lines
}

/// Pushes one line per citation: `• <title> <url>` with a muted URL.
fn push_citation_lines(
    lines: &mut Vec<Line<'static>>,
    citations: &[jinn_provider::UrlCitation],
    body_style: Style,
    url_style: Style,
) {
    for citation in citations {
        let title = if citation.title.is_empty() {
            "(untitled)"
        } else {
            citation.title.as_str()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("• {title} "), body_style),
            Span::styled(citation.url.clone(), url_style),
        ]));
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]

    use super::*;
    use jinn_theme::default_theme;

    fn citation(title: &str, url: &str) -> jinn_provider::UrlCitation {
        jinn_provider::UrlCitation {
            url: url.to_owned(),
            title: title.to_owned(),
            content: None,
            start_index: None,
            end_index: None,
        }
    }

    fn all_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.clone()))
            .collect()
    }

    #[rstest::rstest]
    fn collapsed_shows_header_and_hint_without_citations() {
        // Given a collapsed annotation entry with one citation.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };

        // When rendering.
        let lines = to_lines(&[citation("Source A", "https://example.com/a")], &ctx);

        // Then there are 4 lines: top pad + header + hint + bottom pad.
        assert_eq!(lines.len(), 4);
        // And the header line contains the source count.
        let header: String = lines[1].spans.iter().map(|s| s.content.clone()).collect();
        assert!(header.contains("Sources (1)"));
        // And the hint line tells the user how to expand.
        let hint: String = lines[2].spans.iter().map(|s| s.content.clone()).collect();
        assert_eq!(hint, "(e to expand)");
        // And no citation title or URL is visible.
        let text = all_text(&lines);
        assert!(!text.contains("Source A"));
        assert!(!text.contains("https://example.com/a"));
    }

    #[rstest::rstest]
    fn expanded_shows_citations_without_hint() {
        // Given an expanded annotation entry with one citation.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: true,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };

        // When rendering.
        let lines = to_lines(&[citation("Source A", "https://example.com/a")], &ctx);

        // Then some content line contains the citation title and URL.
        let text = all_text(&lines);
        assert!(
            text.contains("Source A") && text.contains("https://example.com/a"),
            "expanded mode should contain the citation title and URL"
        );
        // And no line contains the expand hint.
        assert!(
            !text.contains("e to expand"),
            "expanded mode should not show the hint"
        );
    }

    #[rstest::rstest]
    fn expanded_shows_all_citations() {
        // Given an expanded annotation entry with three citations.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: true,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        let citations = [
            citation("Source A", "https://example.com/a"),
            citation("", "https://example.com/b"),
            citation("Source C", "https://example.com/c"),
        ];

        // When rendering.
        let lines = to_lines(&citations, &ctx);

        // Then each citation gets its own line.
        assert_eq!(lines.len(), 6);
        let text = all_text(&lines);
        // And the untitled citation falls back to a placeholder.
        assert!(text.contains("(untitled)"));
        assert!(text.contains("https://example.com/b"));
    }

    #[rstest::rstest]
    fn header_uses_sources_theme_colors() {
        // Given a collapsed annotation entry.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        let theme = default_theme();

        // When rendering.
        let lines = to_lines(&[citation("Source A", "https://example.com/a")], &ctx);

        // Then the header text span uses the sources header background and
        // foreground from the theme.
        let header_span = &lines[1].spans[0];
        assert_eq!(header_span.content, "Sources (1)");
        assert_eq!(header_span.style.fg, Some(theme.sources_header_fg));
        assert_eq!(header_span.style.bg, Some(theme.sources_header_bg));
    }

    #[rstest::rstest]
    fn header_background_spans_full_row_width() {
        // Given a collapsed annotation entry rendered at width 80.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        let theme = default_theme();

        // When rendering.
        let lines = to_lines(&[citation("Source A", "https://example.com/a")], &ctx);

        // Then the header's total width spans the full content width,
        // padded with the header background so the bar fills the row.
        assert_eq!(lines[1].width() as u16, 80);
        let pad_span = lines[1].spans.last().expect("padding span");
        assert_eq!(pad_span.style.bg, Some(theme.sources_header_bg));
    }

    #[rstest::rstest]
    fn hint_stays_muted_when_header_is_bright() {
        // Given a collapsed annotation entry.
        let ctx = RenderContext {
            content_width: 80,
            is_selected: false,
            is_expanded: false,
            tool_entry_max_lines: 20,
            theme: default_theme(),
            paired_status: None,
            is_streaming: false,
            is_waiting_on_subagent: false,
            graphics: Default::default(),
            session_id: jinn_core_types::SessionId::new(),
        };
        let theme = default_theme();

        // When rendering.
        let lines = to_lines(&[citation("Source A", "https://example.com/a")], &ctx);

        // Then the hint line still uses muted_text as foreground.
        let hint_line = &lines[2];
        let has_muted_fg = hint_line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(theme.muted_text));
        assert!(has_muted_fg, "hint should use muted_text foreground");
    }
}
