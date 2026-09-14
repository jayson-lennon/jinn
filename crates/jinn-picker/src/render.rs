//! The erased render driver — spec data into selection-widget chrome.
//!
//! One function dispatches on the spec's [`WidgetKind`] to drive
//! `SelectionWidget`, `TreePickerWidget`, or `PreviewSelectionWidget` with
//! the spec's title, colors, preview scroll, preview cache — and its two
//! derived footer lines: the custom status line (or blank) above the
//! keybind line generated from the spec's [`BindRow`]s.
//!
//! The keybind line is the second consumer of bind rows (the keymap is the
//! first): each row renders its notation in `accent_action` and its label
//! in `muted_text`, joined by `·`, with the standard tail appended. The
//! footer drift test in the kernel pins this to the drawn geometry.

use jinn_selection_widget::PreviewSelectionWidget;
use jinn_selection_widget::SelectionWidget;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::Frame;

use crate::ctx::StatusCtx;
use crate::host::Palette;
use crate::host::PickerHost;
use crate::registry::BindRow;
use crate::registry::ErasedPickerSpec;
use crate::registry::Tail;

/// The keybind line built from a spec's bind rows — exposed for the drift
/// test, which compares it against what the render pass actually draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindLine(pub Vec<Span<'static>>);

impl KeybindLine {
    /// The plain text of the line (spans concatenated).
    #[must_use]
    pub fn text(&self) -> String {
        self.0.iter().map(|span| span.content.to_string()).collect()
    }
}

/// The tail text appended to generated keybind lines.
pub(crate) const STANDARD_TAIL: &str = "Enter confirm · ESC cancel";

/// Builds the keybind line from bind rows: `TAB toggle · CTRL+L load ·
/// Enter confirm · ESC cancel`, keys styled `accent_action`, text
/// `muted_text`.
#[must_use]
pub fn keybind_line(rows: &[BindRow], tail: Tail, palette: &Palette) -> KeybindLine {
    let key = Style::default().fg(palette.accent_action);
    let text = Style::default().fg(palette.muted_text);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for row in rows {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_owned(), text));
        }
        spans.push(Span::styled(format!("{} ", row.notation), key));
        spans.push(Span::styled(row.label.to_owned(), text));
    }
    if tail == Tail::Standard {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_owned(), text));
        }
        spans.push(Span::styled(STANDARD_TAIL.to_owned(), text));
    }
    KeybindLine(spans)
}

/// Renders the spec's picker into `area` using the host lens.
pub(crate) fn render_spec<T>(
    spec: &crate::registry::TypedSpec<T>,
    frame: &mut Frame<'_>,
    area: Rect,
    host: &dyn PickerHost,
) -> bool
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    let palette = host.palette();
    let id = spec.id();
    let Some(selection) = host
        .selection_state_ref(id)
        .and_then(|any| any.downcast_ref::<jinn_selection_widget::SelectionState<crate::entry::PickerEntry<T>>>())
    else {
        // No compatible storage lent — the caller falls back to legacy.
        return false;
    };

    // Status line (or blank placeholder) above the keybind line.
    let status_line = {
        let ctx = StatusCtx::new(id, host);
        spec.status_line(&ctx)
    };
    let keybind = keybind_line(spec.binds(), spec.keybind_tail, &palette);
    let footers: Vec<Line<'static>> = vec![
        status_line.unwrap_or_else(|| Line::from(String::new())),
        Line::from(keybind.0),
    ];

    let title = Line::from(spec.title());
    let colors = palette.selection_colors();

    match spec.widget_kind() {
        crate::widget::WidgetKind::List => {
            SelectionWidget::new(selection)
                .title(title)
                .title_style(Style::default().fg(palette.popup_title))
                .footers(footers)
                .colors(colors)
                .render(frame, area);
        }
        crate::widget::WidgetKind::Preview => {
            let cache = host.preview_cache(id);
            let mut widget = PreviewSelectionWidget::new(selection)
                .title(title)
                .title_style(Style::default().fg(palette.popup_title))
                .footers(footers)
                .colors(colors)
                .preview_scroll(host.preview_scroll(id));
            if let Some(cache) = cache.as_deref() {
                widget = widget.preview_cache(cache);
            }
            widget.render(frame, area);
        }
        crate::widget::WidgetKind::Tree => {
            // Tree pickers are not part of the pilot migration surface; the
            // arm exists so the erased dispatch is total over WidgetKind.
            // Rendering falls back to the list widget over the same items.
            SelectionWidget::new(selection)
                .title(title)
                .title_style(Style::default().fg(palette.popup_title))
                .footers(footers)
                .colors(colors)
                .render(frame, area);
        }
    }

    true
}

#[cfg(test)]
mod tests {
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test module, panics are acceptable"
)]

    use super::*;
    use crate::registry::BindRow;

    fn palette() -> Palette {
        Palette {
            border: ratatui::style::Color::DarkGray,
            filter_text: ratatui::style::Color::White,
            separator: ratatui::style::Color::DarkGray,
            footer: ratatui::style::Color::DarkGray,
            highlight_bg: ratatui::style::Color::DarkGray,
            muted_text: ratatui::style::Color::Gray,
            accent_action: ratatui::style::Color::LightRed,
            popup_title: ratatui::style::Color::Cyan,
            primary_text: ratatui::style::Color::White,
        }
    }

    fn row(notation: &'static str, label: &'static str) -> BindRow {
        BindRow {
            notation,
            label,
            category_hint: "general",
        }
    }

    #[test]
    fn keybind_line_lists_rows_with_standard_tail() {
        // Given two bind rows and a standard tail.
        let rows = [row("<tab>", "toggle"), row("<c-l>", "load")];

        // When building the keybind line.
        let line = keybind_line(&rows, Tail::Standard, &palette());

        // Then the text lists each key and label plus the tail.
        assert_eq!(
            line.text(),
            "<tab> toggle · <c-l> load · Enter confirm · ESC cancel"
        );
    }

    #[test]
    fn keybind_line_without_tail_omits_it() {
        // Given bind rows and Tail::None.
        let rows = [row("<c-r>", "refresh")];

        // When building the line.
        let line = keybind_line(&rows, Tail::None, &palette());

        // Then only the rows appear.
        assert_eq!(line.text(), "<c-r> refresh");
    }

    #[test]
    fn keybind_line_styles_keys_with_accent_and_text_with_muted() {
        // Given one bind row.
        let rows = [row("<tab>", "toggle")];

        // When building the line.
        let line = keybind_line(&rows, Tail::None, &palette());

        // Then the key spans carry accent_action and label spans muted_text.
        assert_eq!(line.0[0].style.fg, Some(palette().accent_action));
        assert_eq!(line.0[1].style.fg, Some(palette().muted_text));
    }

    #[test]
    fn keybind_line_from_an_empty_row_set_is_only_the_tail() {
        // Given no bind rows with a standard tail.
        // When building the line.
        let line = keybind_line(&[], Tail::Standard, &palette());

        // Then only the tail appears (no leading separator).
        assert_eq!(line.text(), "Enter confirm · ESC cancel");
    }

    #[test]
    fn palette_converts_to_selection_colors() {
        // Given a palette.
        let p = palette();

        // When converting to selection colors.
        let colors = p.selection_colors();

        // Then the shared fields carry over.
        assert_eq!(colors.border, p.border);
        assert_eq!(colors.footer, p.footer);
        assert_eq!(colors.highlight_bg, p.highlight_bg);
    }
}
