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
use jinn_selection_widget::TreePickerWidget;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

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
    T: jinn_selection_widget::TreeItem + std::fmt::Debug + Send + Sync + 'static,
{
    let palette = host.palette();
    let id = spec.id();

    // Tree specs lend TreePickerState<PickerEntry<T>> and render through
    // TreePickerWidget; everything else uses the flat SelectionState.
    if spec.widget_kind() == crate::widget::WidgetKind::Tree {
        let Some(tree_state) = host.selection_state_ref(id).and_then(|any| {
            any.downcast_ref::<jinn_selection_widget::TreePickerState<crate::entry::PickerEntry<T>>>()
        }) else {
            // No compatible tree storage lent — the caller falls back to
            // legacy. (A Tree spec must not silently render as a flat list.)
            return false;
        };
        let status_line = {
            let ctx = StatusCtx::new(id, host);
            spec.status_line(&ctx)
        };
        let keybind = keybind_line(spec.binds(), spec.keybind_tail, &palette);
        let footers: Vec<Line<'static>> = vec![
            status_line.unwrap_or_else(|| Line::from(String::new())),
            Line::from(keybind.0),
        ];
        TreePickerWidget::new(tree_state)
            .title(Line::from(spec.title()))
            .title_style(Style::default().fg(palette.popup_title))
            .footers(footers)
            .colors(palette.selection_colors())
            .tree_prefix_color(palette.muted_text)
            .render(frame, area);
        return true;
    }

    let Some(selection) = host.selection_state_ref(id).and_then(|any| {
        any.downcast_ref::<jinn_selection_widget::SelectionState<crate::entry::PickerEntry<T>>>()
    }) else {
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
        // Tree specs are fully handled above, before the flat lend.
        crate::widget::WidgetKind::Tree => {}
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

    #[rstest::rstest]
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

    #[rstest::rstest]
    #[test]
    fn keybind_line_without_tail_omits_it() {
        // Given bind rows and Tail::None.
        let rows = [row("<c-r>", "refresh")];

        // When building the line.
        let line = keybind_line(&rows, Tail::None, &palette());

        // Then only the rows appear.
        assert_eq!(line.text(), "<c-r> refresh");
    }

    #[rstest::rstest]
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

    #[rstest::rstest]
    #[test]
    fn keybind_line_from_an_empty_row_set_is_only_the_tail() {
        // Given no bind rows with a standard tail.
        // When building the line.
        let line = keybind_line(&[], Tail::Standard, &palette());

        // Then only the tail appears (no leading separator).
        assert_eq!(line.text(), "Enter confirm · ESC cancel");
    }

    #[rstest::rstest]
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

#[cfg(test)]
mod tree_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test module, panics are acceptable"
    )]

    use super::*;
    use crate::builder::PickerSpec;
    use crate::id::PickerId;
    use crate::registry::PickerRegistry;
    use crate::test_host::FakeHost;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A tree entry with a real parent chain for ancestor-filter tests.
    #[derive(Debug, Clone)]
    struct Node {
        id: String,
        parent: Option<String>,
        label: String,
    }

    impl Node {
        fn root(id: &str, label: &str) -> Self {
            Self {
                id: id.to_owned(),
                parent: None,
                label: label.to_owned(),
            }
        }

        fn child(id: &str, parent: &str, label: &str) -> Self {
            Self {
                id: id.to_owned(),
                parent: Some(parent.to_owned()),
                label: label.to_owned(),
            }
        }
    }

    impl jinn_selection_widget::TreeItem for Node {
        fn id(&self) -> &str {
            &self.id
        }

        fn parent_id(&self) -> Option<&str> {
            self.parent.as_deref()
        }

        fn display_label(&self) -> &str {
            &self.label
        }

        fn render_row(&self, _is_selected: bool) -> Line<'static> {
            Line::raw(self.label.clone())
        }

        fn render_row_with_highlight(
            &self,
            _is_selected: bool,
            _match_indices: &[std::ops::Range<usize>],
        ) -> Line<'static> {
            Line::raw(self.label.clone())
        }
    }

    fn tree_spec() -> PickerSpec<Node> {
        PickerSpec::<Node>::new(PickerId::new("tree-test"))
            .title(" Tree Test ")
            .widget(crate::PickerWidget::Tree)
            .row(|entry: &Node, ctx: &crate::ctx::RowCtx<'_>| {
                let mut spans = Vec::new();
                if !ctx.tree_prefix.is_empty() {
                    spans.push(ratatui::text::Span::styled(
                        ctx.tree_prefix.to_owned(),
                        ctx.tree_style,
                    ));
                }
                spans.push(ratatui::text::Span::raw(entry.label.clone()));
                Line::from(spans)
            })
            .search(|entry: &Node| entry.label.clone())
    }

    fn seeded_host(nodes: Vec<Node>) -> FakeHost {
        let mut registry = PickerRegistry::new();
        registry.register(tree_spec());
        let items = registry
            .make_items("tree-test", nodes)
            .expect("spec registered");
        let mut host = FakeHost::new();
        host.set_tree_selection(
            PickerId::new("tree-test"),
            jinn_selection_widget::TreePickerState::with_items(items),
        );
        host
    }

    fn render_with(host: &FakeHost, spec: PickerSpec<Node>) -> ratatui::buffer::Buffer {
        let mut registry = PickerRegistry::new();
        registry.register(spec);
        let spec_handle = registry.get("tree-test").expect("registered");
        let _ = spec_handle;
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut drew = true;
        terminal
            .draw(|frame| {
                drew = spec_handle.render(frame, frame.area(), host);
            })
            .expect("draw");
        assert!(drew, "Tree spec must render against tree storage");
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<Vec<_>>()
            .join("")
    }

    #[rstest::rstest]
    #[test]
    fn tree_spec_renders_through_treepickerwidget() {
        // Given a tree spec with a root and a child, lent as tree storage.
        let host = seeded_host(vec![
            Node::root("r", "Root"),
            Node::child("c", "r", "Child"),
        ]);

        // When rendering.
        let buffer = render_with(&host, tree_spec());

        // Then both rows appear (title bar + two result rows).
        let text = buffer_text(&buffer);
        assert!(text.contains("Root"), "root row rendered: {text}");
        assert!(text.contains("Child"), "child row rendered: {text}");
        assert!(text.contains(" Tree Test "), "title rendered: {text}");
    }

    #[rstest::rstest]
    #[test]
    fn child_filter_match_keeps_ancestors_visible() {
        // Given a tree with root -> child, and a filter matching only the child.
        let mut host = seeded_host(vec![
            Node::root("r", "alpha root"),
            Node::child("c", "r", "beta child"),
        ]);
        let state = host
            .selection_tree_mut::<crate::entry::PickerEntry<Node>>(PickerId::new("tree-test"))
            .expect("tree storage");
        state.insert_text("beta");

        // When rendering the filtered picker.
        let buffer = render_with(&host, tree_spec());
        let text = buffer_text(&buffer);

        // Then the matched child is visible AND its ancestor chain is kept.
        assert!(text.contains("beta child"), "matched child visible: {text}");
        assert!(
            text.contains("alpha root"),
            "ancestor of matched child stays visible: {text}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn row_hook_receives_tree_prefix_for_children() {
        // Given the tree spec's row hook forwards ctx.tree_prefix.
        // (tree_spec builds rows as prefix + label.)
        let host = seeded_host(vec![
            Node::root("r", "Root"),
            Node::child("c", "r", "Child"),
        ]);

        // When rendering.
        let buffer = render_with(&host, tree_spec());

        // Then the child row carries a connector glyph before its label,
        // while the root row has none.
        let row_of = |needle: &str| -> String {
            for y in 0..buffer.area.height {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                if row.contains(needle) {
                    return row;
                }
            }
            String::new()
        };
        let child_row = row_of("Child");
        let root_row = row_of("Root");
        assert!(
            child_row.contains('\u{251c}') || child_row.contains('\u{2514}'),
            "child row shows a tee/elbow connector: {child_row:?}"
        );
        assert!(
            !root_row.contains('\u{251c}') && !root_row.contains('\u{2514}'),
            "root row has no connector: {root_row:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn tree_spec_renders_false_without_tree_storage() {
        // Given the host lends a flat SelectionState (wrong storage).
        let mut host = FakeHost::new();
        let registry = PickerRegistry::new();
        let items = registry
            .make_items("tree-test", vec![Node::root("r", "Root")])
            .unwrap_or_default();
        let mut selection = jinn_selection_widget::SelectionState::new();
        selection.set_items(items);
        host.set_selection(PickerId::new("tree-test"), selection);

        // When rendering a Tree spec against it via an erased handle.
        let mut tree_registry = PickerRegistry::new();
        tree_registry.register(tree_spec());
        let spec = tree_registry.get("tree-test").expect("registered");
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut drew = true;
        terminal
            .draw(|frame| {
                drew = spec.render(frame, frame.area(), &host);
            })
            .expect("draw");

        // Then the render call reports no compatible storage (false), the
        // documented fall-back-to-legacy signal.
        assert!(
            !drew,
            "Tree spec with flat storage must signal incompatibility"
        );
    }
}
