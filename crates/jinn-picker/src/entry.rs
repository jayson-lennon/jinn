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
        let line = row.run(
            entry,
            &RowCtx {
                is_selected: false,
                match_ranges: &[],
            },
        );
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
        row.run(
            &self.entry,
            &RowCtx {
                is_selected,
                match_ranges: &[],
            },
        )
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
        row.run(
            &self.entry,
            &RowCtx {
                is_selected,
                match_ranges: match_indices,
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
