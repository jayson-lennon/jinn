//! The spec-to-widget adapter — wraps authored entries for the widget layer.
//!
//! [`PickerEntry<T>`] pairs a domain entry with the spec's render hooks and
//! its precomputed search text, then implements the selection widget's
//! `PickerItem` + `PreviewContent` contracts. This is the only place the
//! crate touches the widget traits: the widget layer is consumed as-is, and
//! domain entry types never implement widget traits themselves.
//!
//! Search text is computed **once per entry** at load/refresh time by
//! [`make_items`] — the same runtime cost as the kernel's current
//! precomputed `search_text` fields, with spec-authored text instead.

use std::ops::Range;
use std::sync::Arc;

use jinn_selection_widget::PickerItem;
use jinn_selection_widget::PreviewContent;
use jinn_selection_widget::TreeItem;
use jinn_selection_widget::highlight_text;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;

use crate::ctx::PreviewCtx;
use crate::ctx::RowCtx;
use crate::hooks::PickerPreviewFn;
use crate::hooks::PickerPreviewKeyFn;
use crate::hooks::PickerRowFn;
use crate::hooks::PickerSearchFn;
use crate::preview_key::PreviewKey;

/// The render hooks an entry needs, shared across all entries of one load.
///
/// Fields are crate-visible data carriers consumed by the builder and the
/// registry's typed window — the same shape as the selection widget's own
/// state structs.
#[expect(
    clippy::field_scoped_visibility_modifiers,
    reason = "crate-internal data carrier; accessor boilerplate adds no safety"
)]
pub(crate) struct RenderHooks<T> {
    /// Row renderer (spec `.row`).
    pub(crate) row: Option<PickerRowFn<T>>,
    /// Search-text computer (spec `.search`).
    pub(crate) search: Option<PickerSearchFn<T>>,
    /// Preview renderer (spec `.preview`).
    pub(crate) preview: Option<PickerPreviewFn<T>>,
    /// Preview cache identity (spec `.preview_key`).
    pub(crate) preview_key: Option<PickerPreviewKeyFn<T>>,
}

impl<T> Default for RenderHooks<T> {
    fn default() -> Self {
        Self {
            row: None,
            search: None,
            preview: None,
            preview_key: None,
        }
    }
}

impl<T> std::fmt::Debug for RenderHooks<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RenderHooks(..)")
    }
}

impl<T> RenderHooks<T> {
    /// Shares the hooks across the entries of one load. Manual because the
    /// hooks are `Clone` regardless of `T` — a derived impl would wrongly
    /// require `T: Clone`.
    #[must_use]
    pub(crate) fn cloned(&self) -> Self {
        Self {
            row: self.row.clone(),
            search: self.search.clone(),
            preview: self.preview.clone(),
            preview_key: self.preview_key.clone(),
        }
    }

    /// The plain-text fallback label for an entry when no search hook is
    /// declared: the row line's concatenated spans, or empty when no row
    /// hook exists either.
    fn fallback_label(&self, entry: &T) -> String {
        let Some(row) = self.row.as_ref() else {
            return String::new();
        };
        let line = row.run(entry, &RowCtx::flat(false, &[]));
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }
}

/// A domain entry wrapped for the selection widget.
///
/// `T` needs only `Debug + Send + Sync + 'static` — never a widget trait.
/// The widget reads rows, previews, and the filter label through the trait
/// impls below; spec actions reach back to the domain entry via
/// [`PickerEntry::entry`] / [`PickerEntry::entry_mut`].
pub struct PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    entry: T,
    search_text: String,
    hooks: Arc<RenderHooks<T>>,
}

impl<T> Clone for PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + Clone + 'static,
{
    fn clone(&self) -> Self {
        Self {
            entry: self.entry.clone(),
            search_text: self.search_text.clone(),
            hooks: Arc::clone(&self.hooks),
        }
    }
}

impl<T> PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    /// The wrapped domain entry.
    #[must_use]
    pub fn entry(&self) -> &T {
        &self.entry
    }

    /// Mutable domain entry (toggle-style mutations by spec actions).
    pub fn entry_mut(&mut self) -> &mut T {
        &mut self.entry
    }
}

impl<T> std::fmt::Debug for PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerEntry")
            .field("entry", &self.entry)
            .field("search_text", &self.search_text)
            .finish_non_exhaustive()
    }
}

impl<T> PickerItem for PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    fn display_label(&self) -> &str {
        &self.search_text
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        let Some(row) = self.hooks.row.as_ref() else {
            return Line::raw(self.search_text.clone());
        };
        row.run(&self.entry, &RowCtx::flat(is_selected, &[]))
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        let Some(row) = self.hooks.row.as_ref() else {
            // Default highlighting over the plain label.
            let spans: Vec<Span<'static>> =
                highlight_text(&self.search_text, Style::default(), match_indices)
                    .into_iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style))
                    .collect();
            return Line::from(spans);
        };
        row.run(&self.entry, &RowCtx::flat(is_selected, match_indices))
    }
}

/// Tree delegation: structure comes from the domain entry, filter text from
/// the spec's search hook, row rendering from the spec's row hook. The tree
/// connector's placement is the row hook's decision — this impl forwards the
/// pre-computed prefix rather than prepending it (the widget default would
/// double-prefix rows that embed mid-row connectors).
impl<T> TreeItem for PickerEntry<T>
where
    T: jinn_selection_widget::TreeItem + std::fmt::Debug + Send + Sync + 'static,
{
    fn id(&self) -> &str {
        self.entry.id()
    }

    fn parent_id(&self) -> Option<&str> {
        self.entry.parent_id()
    }

    fn display_label(&self) -> &str {
        &self.search_text
    }

    fn render_row(&self, is_selected: bool) -> Line<'static> {
        PickerItem::render_row(self, is_selected)
    }

    fn render_row_with_highlight(
        &self,
        is_selected: bool,
        match_indices: &[Range<usize>],
    ) -> Line<'static> {
        PickerItem::render_row_with_highlight(self, is_selected, match_indices)
    }

    fn render_row_with_tree(
        &self,
        is_selected: bool,
        match_ranges: &[Range<usize>],
        tree_prefix: &str,
        tree_style: Style,
    ) -> Line<'static> {
        let Some(row) = self.hooks.row.as_ref() else {
            let mut spans = Vec::with_capacity(2);
            if !tree_prefix.is_empty() {
                spans.push(Span::styled(tree_prefix.to_owned(), tree_style));
            }
            spans.push(Span::raw(self.search_text.clone()));
            return Line::from(spans);
        };
        row.run(
            &self.entry,
            &RowCtx {
                is_selected,
                match_ranges,
                tree_prefix,
                tree_style,
            },
        )
    }
}

impl<T> PreviewContent for PickerEntry<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    fn preview_lines(&self, width: usize) -> Vec<Line<'static>> {
        let Some(preview) = self.hooks.preview.as_ref() else {
            return Vec::new();
        };
        // The live path renders without a cache reference of its own; the
        // widget's cached path (`preview_lines_cached` with the spec's
        // domain-supplied cache) is the caching route.
        preview.run(&self.entry, &PreviewCtx { width, cache: None })
    }

    fn cache_key(&self) -> Option<String> {
        let key_hook = self.hooks.preview_key.as_ref()?;
        let PreviewKey(inner) = key_hook.run(&self.entry)?;
        Some(inner)
    }
}

/// Builds `PickerEntry<T>` items from domain entries using the spec's hooks.
///
/// The search hook runs exactly once per entry here — at load/refresh time,
/// before any keystroke. Entries whose hooks are absent fall back to
/// defaults (label = row text, no preview, no cache key).
#[must_use]
pub(crate) fn make_items<T>(entries: Vec<T>, hooks: &RenderHooks<T>) -> Vec<PickerEntry<T>>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    entries
        .into_iter()
        .map(|entry| {
            let search_text = match hooks.search.as_ref() {
                Some(search) => search.run(&entry),
                None => hooks.fallback_label(&entry),
            };
            PickerEntry {
                entry,
                search_text,
                hooks: Arc::new(hooks.cloned()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]

    use super::*;

    #[derive(Debug)]
    struct Entry {
        name: String,
        description: String,
    }

    #[rstest::rstest]
    #[test]
    fn search_hook_runs_once_per_entry_at_load() {
        // Given a search hook counting its invocations.
        use std::sync::Arc as StdArc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        let counter = StdArc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let hooks = RenderHooks {
            search: Some(PickerSearchFn::new(move |entry: &Entry| {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                format!("{} {}", entry.name, entry.description)
            })),
            ..RenderHooks::default()
        };
        let entries = vec![
            Entry {
                name: String::from("a"),
                description: String::from("one"),
            },
            Entry {
                name: String::from("b"),
                description: String::from("two"),
            },
        ];

        // When building items.
        let items = make_items(entries, &hooks);

        // Then the search hook ran exactly once per entry.
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        assert_eq!(items[0].display_label(), "a one");
        assert_eq!(items[1].display_label(), "b two");
    }

    #[rstest::rstest]
    #[test]
    fn row_hook_renders_through_the_adapter() {
        // Given hooks with a row renderer.
        let hooks = RenderHooks {
            row: Some(PickerRowFn::new(|entry: &Entry, ctx: &RowCtx<'_>| {
                if ctx.is_selected {
                    Line::from(format!("> {}", entry.name))
                } else {
                    Line::from(entry.name.clone())
                }
            })),
            ..RenderHooks::default()
        };
        let items = make_items(
            vec![Entry {
                name: String::from("row"),
                description: String::new(),
            }],
            &hooks,
        );

        // When rendering selected and unselected.
        let unselected = items[0].render_row(false);
        let selected = items[0].render_row(true);

        // Then the row hook's styling comes through.
        assert_eq!(unselected.to_string(), "row");
        assert_eq!(selected.to_string(), "> row");
    }

    #[rstest::rstest]
    #[test]
    fn fallback_label_uses_row_text_without_a_search_hook() {
        // Given hooks with only a row renderer.
        let hooks = RenderHooks {
            row: Some(PickerRowFn::new(|entry: &Entry, _ctx: &RowCtx<'_>| {
                Line::from(vec![
                    Span::styled(entry.name.clone(), Style::default()),
                    Span::raw(" — "),
                    Span::raw(entry.description.clone()),
                ])
            })),
            ..RenderHooks::default()
        };

        // When building items.
        let items = make_items(
            vec![Entry {
                name: String::from("name"),
                description: String::from("desc"),
            }],
            &hooks,
        );

        // Then the display label is the row's concatenated spans.
        assert_eq!(items[0].display_label(), "name — desc");
    }

    #[rstest::rstest]
    #[test]
    fn preview_key_flows_into_the_cache_identity() {
        // Given hooks with a preview key.
        let hooks = RenderHooks {
            preview_key: Some(PickerPreviewKeyFn::new(|entry: &Entry| {
                Some(PreviewKey(format!("key-{}", entry.name)))
            })),
            ..RenderHooks::default()
        };
        let items = make_items(
            vec![Entry {
                name: String::from("k"),
                description: String::new(),
            }],
            &hooks,
        );

        // When reading the cache key.
        // Then the key's inner string is the cache identity.
        assert_eq!(items[0].cache_key(), Some(String::from("key-k")));
    }

    #[rstest::rstest]
    #[test]
    fn absent_hooks_degrade_to_defaults() {
        // Given empty hooks.
        let hooks = RenderHooks::default();
        let items = make_items(
            vec![Entry {
                name: String::from("d"),
                description: String::from("x"),
            }],
            &hooks,
        );

        // When reading label, row, preview, and cache key.
        // Then defaults apply: empty label, raw empty row, no preview, no key.
        assert_eq!(items[0].display_label(), "");
        assert_eq!(items[0].render_row(false).to_string(), "");
        assert!(items[0].preview_lines(80).is_empty());
        assert_eq!(items[0].cache_key(), None);
    }
}

#[cfg(test)]
mod clone_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]

    use super::*;
    use crate::ctx::RowCtx;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[derive(Debug, Clone)]
    struct Thing {
        name: String,
    }

    impl jinn_selection_widget::TreeItem for Thing {
        fn id(&self) -> &str {
            &self.name
        }

        fn parent_id(&self) -> Option<&str> {
            None
        }

        fn display_label(&self) -> &str {
            &self.name
        }

        fn render_row(&self, _is_selected: bool) -> ratatui::text::Line<'static> {
            ratatui::text::Line::raw(self.name.clone())
        }

        fn render_row_with_highlight(
            &self,
            _is_selected: bool,
            _match_indices: &[std::ops::Range<usize>],
        ) -> ratatui::text::Line<'static> {
            ratatui::text::Line::raw(self.name.clone())
        }
    }

    #[rstest::rstest]
    #[test]
    fn clone_shares_hooks_and_matches_original_render() {
        // Given an entry built through hooks that count invocations.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_row = Arc::clone(&calls);
        let mut registry = crate::registry::PickerRegistry::new();
        registry.register(
            crate::builder::PickerSpec::new(crate::id::PickerId::new("clone-test"))
                .row(move |entry: &Thing, _ctx: &RowCtx<'_>| {
                    calls_for_row.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Line::raw(entry.name.clone())
                })
                .search(|entry: &Thing| entry.name.clone()),
        );
        let items = registry
            .make_items(
                "clone-test",
                vec![Thing {
                    name: "alpha".to_owned(),
                }],
            )
            .expect("spec registered");
        let original = &items[0];

        // When cloning the wrapped entry and rendering both.
        let copy = original.clone();
        let a = PickerItem::render_row(original, false);
        let b = PickerItem::render_row(&copy, false);

        // Then the clone renders identically and shares the same hooks
        // (the invocation count covers both renders through one Arc).
        assert_eq!(
            a.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            b.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(copy.search_text, original.search_text);
    }
}
