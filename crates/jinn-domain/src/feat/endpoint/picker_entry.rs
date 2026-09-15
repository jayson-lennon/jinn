//! OpenRouter endpoint picker entry — one row per routing upstream.

use std::ops::Range;

use crate::feat::theme::Theme;
use jinn_selection_widget::{PickerItem, PreviewContent, highlight_text_with_bg};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Sentinel provider id marking the "auto-route" (no pin) choice.
///
/// Stored as the `tag` of the special first row so the confirm handler can
/// detect "user picked the default" uniformly by comparing `tag`.
pub const AUTO_ROUTE_SENTINEL_TAG: &str = "";

/// A single OpenRouter upstream ready for display in the endpoint picker.
///
/// Carries the routing [`tag`](EndpointEntry::tag) so the confirm handler can
/// read it back without re-parsing the display name. The optional metadata
/// (uptime, pricing, quantization) populates the picker's preview pane.
#[derive(Debug, Clone)]
pub struct EndpointEntry {
    /// The OpenRouter routing slug (e.g. `"anthropic"`). Empty for the
    /// "Default (auto-route)" sentinel row.
    pub tag: String,
    /// Human-readable upstream name (e.g. `"Anthropic"`, `"Default"`).
    pub provider_name: String,
    /// 30-minute uptime percentage, if reported by OpenRouter.
    pub uptime_30m: Option<f64>,
    /// Per-token prompt price, if reported.
    pub prompt_price: Option<String>,
    /// Per-token completion price, if reported.
    pub completion_price: Option<String>,
    /// Quantization level, if reported.
    pub quantization: Option<String>,
    /// Max completion tokens the upstream supports, if reported.
    pub max_completion_tokens: Option<u32>,
    /// Whether this is the currently pinned endpoint.
    pub is_active: bool,
    /// Theme for rendering.
    pub theme: Theme,
}

impl EndpointEntry {
    /// Builds the "Default (auto-route)" sentinel row.
    #[must_use]
    pub fn auto_route(is_active: bool, theme: Theme) -> Self {
        Self {
            tag: AUTO_ROUTE_SENTINEL_TAG.to_owned(),
            provider_name: "Default".to_owned(),
            uptime_30m: None,
            prompt_price: None,
            completion_price: None,
            quantization: None,
            max_completion_tokens: None,
            is_active,
            theme,
        }
    }

    /// One-line summary of availability metadata for the preview pane.
    #[must_use]
    pub fn availability_summary(&self) -> String {
        let uptime = self.uptime_30m.map_or_else(
            || "uptime unknown".to_owned(),
            |u| format!("uptime {u:.1}%"),
        );
        let quant = self
            .quantization
            .clone()
            .unwrap_or_else(|| "quant unknown".to_owned());
        format!("{uptime} · {quant}")
    }
}

impl PickerItem for EndpointEntry {
    fn display_label(&self) -> &str {
        &self.provider_name
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        render_endpoint_row(
            &self.provider_name,
            &self.tag,
            self.is_active,
            is_selected,
            &[],
            &self.theme,
        )
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        render_endpoint_row(
            &self.provider_name,
            &self.tag,
            self.is_active,
            is_selected,
            match_indices,
            &self.theme,
        )
    }
}

impl PreviewContent for EndpointEntry {
    fn preview_lines(&self, _width: usize) -> Vec<Line<'static>> {
        // The auto-route sentinel has no metadata; show a one-line explanation.
        if self.tag.is_empty() {
            return vec![
                Line::from("Let OpenRouter choose the upstream each turn.")
                    .style(Style::default().fg(self.theme.muted_text)),
            ];
        }

        let gray = Style::default().fg(self.theme.muted_text);
        let primary = Style::default().fg(self.theme.primary_text);
        let row = |label: &str, value: &str| {
            Line::from(vec![
                Span::styled(format!("{label}: "), gray),
                Span::styled(value.to_owned(), primary),
            ])
        };

        let uptime = self
            .uptime_30m
            .map_or_else(|| "unknown".to_owned(), |u| format!("{u:.1}%"));
        let quant = self
            .quantization
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let prompt = self
            .prompt_price
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let completion = self
            .completion_price
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let max_tokens = self
            .max_completion_tokens
            .map_or_else(|| "unknown".to_owned(), |n| n.to_string());

        vec![
            row("Tag", &self.tag),
            row("Uptime (30m)", &uptime),
            row("Quantization", &quant),
            row("Prompt price", &prompt),
            row("Completion price", &completion),
            row("Max completion", &max_tokens),
        ]
    }

    // Metadata is static per entry; cache by tag so the preview pane
    // doesn't re-render every frame.
    fn cache_key(&self) -> Option<String> {
        (!self.tag.is_empty()).then(|| self.tag.clone())
    }
}

/// Renders a single endpoint picker row: `marker name  (tag)`.
fn render_endpoint_row(
    name: &str,
    tag: &str,
    is_active: bool,
    is_selected: bool,
    match_indices: &[Range<usize>],
    theme: &Theme,
) -> Line<'static> {
    let active_marker = Span::styled(
        if is_active { "● " } else { "  " },
        if is_active {
            Style::default()
                .fg(theme.picker_active_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    );

    let label_style = if is_selected {
        Style::default()
            .fg(theme.primary_text)
            .bg(theme.picker_selected_bg)
    } else {
        Style::default()
    };

    let suffix = if tag.is_empty() {
        "(auto-route)".to_owned()
    } else {
        format!("({tag})")
    };

    let name_spans = if match_indices.is_empty() {
        vec![Span::styled(format!("{name}  "), label_style)]
    } else {
        let mut spans =
            highlight_text_with_bg(name, label_style, match_indices, theme.picker_highlight_bg);
        spans.push(Span::styled("  ".to_owned(), label_style));
        spans
    };

    let mut all_spans = vec![active_marker];
    all_spans.extend(name_spans);
    all_spans.push(Span::styled(suffix, label_style));
    Line::from(all_spans)
}

impl jinn_selection_widget::TreeItem for EndpointEntry {
    fn id(&self) -> &str {
        if self.tag.is_empty() {
            "__auto_route__"
        } else {
            &self.tag
        }
    }

    fn parent_id(&self) -> Option<&str> {
        None
    }

    fn display_label(&self) -> &str {
        &self.provider_name
    }

    fn render_row(&self, is_selected: bool) -> ratatui::text::Line<'static> {
        PickerItem::render_row_with_highlight(self, is_selected, &[])
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[std::ops::Range<usize>],
    ) -> ratatui::text::Line<'static> {
        PickerItem::render_row_with_highlight(self, is_selected, match_indices)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]
    use super::*;
    use crate::feat::theme::default_theme;

    #[rstest::rstest]
    #[test]
    fn auto_route_entry_has_empty_tag() {
        // Given building the auto-route sentinel.
        // When constructing the entry.
        let entry = EndpointEntry::auto_route(true, default_theme());

        // Then its tag is empty (the no-pin sentinel) and name is "Default".
        assert_eq!(entry.tag, "");
        assert_eq!(entry.provider_name, "Default");
    }

    #[rstest::rstest]
    #[test]
    fn availability_summary_includes_uptime_when_present() {
        // Given an endpoint with uptime reported.
        let entry = EndpointEntry {
            tag: "anthropic".to_owned(),
            provider_name: "Anthropic".to_owned(),
            uptime_30m: Some(99.7),
            prompt_price: None,
            completion_price: None,
            quantization: Some("fp16".to_owned()),
            max_completion_tokens: None,
            is_active: false,
            theme: default_theme(),
        };

        // When summarizing availability.
        // Then it contains the uptime value and quantization.
        assert!(entry.availability_summary().contains("99.7%"));
        assert!(entry.availability_summary().contains("fp16"));
    }
}
