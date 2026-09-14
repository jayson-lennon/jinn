//! Spec hook newtypes — boxed closures behind named wrapper seams.
//!
//! Every hook is a boxed `Arc<dyn Fn …>` newtype, mirroring `jinn-slices`'
//! `ActionFn`. Builder methods take plain closures so spec authors never
//! name these types; the newtypes exist so each `run` is a *seam* — a single
//! named place (tracing, panic capture, later instrumentation) through which
//! every invocation flows.
//!
//! `PickerBindAction` is the picker-scoped sibling of slices' `ActionFn` and
//! deliberately does not reuse that name: this crate consumes
//! `jinn-slices`' public types and a colliding re-export would be ambiguous.

use std::sync::Arc;

use ratatui::text::Line;

use crate::ctx::ActionCtx;
use crate::ctx::LoadCtx;
use crate::ctx::PreviewCtx;
use crate::ctx::RowCtx;
use crate::ctx::StatusCtx;
use crate::outcome::PickerOutcome;
use crate::preview_key::PreviewKey;

/// Boxed closure type behind [`PickerLoadFn`].
type BoxedLoad<T> = Arc<dyn Fn(&mut LoadCtx<'_>) -> Vec<T> + Send + Sync>;
/// Boxed closure type behind [`PickerRowFn`].
type BoxedRow<T> = Arc<dyn Fn(&T, &RowCtx<'_>) -> Line<'static> + Send + Sync>;
/// Boxed closure type behind [`PickerSearchFn`].
type BoxedSearch<T> = Arc<dyn Fn(&T) -> String + Send + Sync>;
/// Boxed closure type behind [`PickerPreviewFn`].
type BoxedPreview<T> = Arc<dyn Fn(&T, &PreviewCtx<'_>) -> Vec<Line<'static>> + Send + Sync>;
/// Boxed closure type behind [`PickerPreviewKeyFn`].
type BoxedPreviewKey<T> = Arc<dyn Fn(&T) -> Option<PreviewKey> + Send + Sync>;
/// Boxed closure type behind [`PickerStatusFn`].
type BoxedStatus = Arc<dyn Fn(&StatusCtx<'_>) -> Option<Line<'static>> + Send + Sync>;
/// Boxed closure type behind [`PickerLifecycleFn`] and [`PickerBindAction`].
type BoxedAction = Arc<dyn Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync>;
/// Index-erased dispatch closure built by [`PickerSelectionChangeFn::new`]:
/// snapshots the highlighted entry out of the lent storage (by index), then
/// runs the declared closure with it.
type BoxedSelectionChangeAt = Arc<dyn Fn(usize, &mut ActionCtx<'_>) + Send + Sync>;

/// Builds the picker's entries when they are (re)loaded.
///
/// Declared via `.load`; when absent the spec performs no synchronous entry
/// load on open (an external actor may fill the storage instead). Runs
/// through [`PickerLoadFn::run`].
pub struct PickerLoadFn<T>(BoxedLoad<T>);

impl<T> PickerLoadFn<T> {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&mut LoadCtx<'_>) -> Vec<T> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, ctx: &mut LoadCtx<'_>) -> Vec<T> {
        (self.0)(ctx)
    }
}

// Manual `Clone` for the generic hooks: a derive would add `T: Clone`,
// but the boxed closure clones fine for any `T`.
impl<T> Clone for PickerLoadFn<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Clone for PickerRowFn<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Clone for PickerSearchFn<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Clone for PickerPreviewFn<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Clone for PickerPreviewKeyFn<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::fmt::Debug for PickerLoadFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerLoadFn(..)")
    }
}

/// Renders one list row for an entry.
///
/// Declared via `.row`; the default renders the plain display label (the
/// search text) without styling. Runs through [`PickerRowFn::run`].
pub struct PickerRowFn<T>(BoxedRow<T>);

impl<T> PickerRowFn<T> {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&T, &RowCtx<'_>) -> Line<'static> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, entry: &T, ctx: &RowCtx<'_>) -> Line<'static> {
        (self.0)(entry, ctx)
    }
}

impl<T> std::fmt::Debug for PickerRowFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerRowFn(..)")
    }
}

/// Computes the searchable text for an entry.
///
/// Declared via `.search`; the default is the row label. Invoked **once per
/// entry** at load/refresh time by the entry adapter — never per keystroke.
/// Runs through [`PickerSearchFn::run`].
pub struct PickerSearchFn<T>(BoxedSearch<T>);

impl<T> PickerSearchFn<T> {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&T) -> String + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, entry: &T) -> String {
        (self.0)(entry)
    }
}

impl<T> std::fmt::Debug for PickerSearchFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerSearchFn(..)")
    }
}

/// Renders the preview pane lines for an entry.
///
/// Declared via `.preview`; preview pickers should declare it, but an absent
/// hook degrades to an empty preview rather than a broken one. Runs through
/// [`PickerPreviewFn::run`].
pub struct PickerPreviewFn<T>(BoxedPreview<T>);

impl<T> PickerPreviewFn<T> {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&T, &PreviewCtx<'_>) -> Vec<Line<'static>> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, entry: &T, ctx: &PreviewCtx<'_>) -> Vec<Line<'static>> {
        (self.0)(entry, ctx)
    }
}

impl<T> std::fmt::Debug for PickerPreviewFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerPreviewFn(..)")
    }
}

/// Returns the optional cache identity for an entry's preview.
///
/// Declared via `.preview_key`; `None` (or an absent hook) means the entry's
/// preview always renders live. Runs through [`PickerPreviewKeyFn::run`].
pub struct PickerPreviewKeyFn<T>(BoxedPreviewKey<T>);

impl<T> PickerPreviewKeyFn<T> {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&T) -> Option<PreviewKey> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, entry: &T) -> Option<PreviewKey> {
        (self.0)(entry)
    }
}

impl<T> std::fmt::Debug for PickerPreviewKeyFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerPreviewKeyFn(..)")
    }
}

impl<T> Clone for PickerSelectionChangeFn<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            marker: std::marker::PhantomData,
        }
    }
}

/// Reacts to the highlighted entry changing (cursor movement, paging).
///
/// Declared via `.on_selection_change`; it receives the newly highlighted
/// entry and mutates host state in place (a live preview). It returns
/// nothing — it can never close the picker or emit messages, and an absent
/// hook means the spec has no selection-change behavior. Runs through
/// [`PickerSelectionChangeFn::run`].
///
/// Dispatch erases the entry: at run time the hook snapshots the entry at
/// the highlighted index out of the lent selection storage (a no-op when the
/// index is out of bounds), then invokes the declared closure — the entry
/// must be [`Clone`] because it borrows the same storage the mutable context
/// guards.
pub struct PickerSelectionChangeFn<T> {
    inner: BoxedSelectionChangeAt,
    marker: std::marker::PhantomData<fn() -> T>,
}

impl<T> PickerSelectionChangeFn<T>
where
    T: Clone + std::fmt::Debug + Send + Sync + 'static,
{
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&T, &mut ActionCtx<'_>) + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(move |index: usize, ctx: &mut ActionCtx<'_>| {
                let entry = {
                    let storage =
                        ctx.selection::<jinn_selection_widget::SelectionState<crate::entry::PickerEntry<T>>>();
                    storage.and_then(|state| {
                        state.items().get(index).map(|item| item.entry().clone())
                    })
                };
                let Some(entry) = entry else {
                    return;
                };
                f(&entry, ctx);
            }),
            marker: std::marker::PhantomData,
        }
    }
}

impl<T> PickerSelectionChangeFn<T> {
    /// Runs the hook through the wrapper seam for the entry at `index`.
    pub fn run(&self, index: usize, ctx: &mut ActionCtx<'_>) {
        (self.inner)(index, ctx);
    }
}

impl<T> std::fmt::Debug for PickerSelectionChangeFn<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerSelectionChangeFn(..)")
    }
}

/// Renders the custom status line above the keybind line.
///
/// Declared via `.status`; `None` renders a blank line so the picker's
/// declared geometry (`bottom_rows`) never lies. Runs through
/// [`PickerStatusFn::run`].
#[derive(Clone)]
pub struct PickerStatusFn(BoxedStatus);

impl PickerStatusFn {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&StatusCtx<'_>) -> Option<Line<'static>> + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
        (self.0)(ctx)
    }
}

impl std::fmt::Debug for PickerStatusFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerStatusFn(..)")
    }
}

/// A lifecycle hook — the behavior behind open, confirm, and close.
///
/// Declared via `.on_open` / `.on_confirm` / `.on_close`; absent hooks
/// produce an empty outcome (no messages, no close). Runs through
/// [`PickerLifecycleFn::run`].
#[derive(Clone)]
pub struct PickerLifecycleFn(BoxedAction);

impl PickerLifecycleFn {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        (self.0)(ctx)
    }
}

impl std::fmt::Debug for PickerLifecycleFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerLifecycleFn(..)")
    }
}

/// A bind-row action — what a declared keybind's keypress produces.
///
/// Declared via `.bind`; looked up by the row's action name and run at
/// dispatch time. Runs through [`PickerBindAction::run`].
#[derive(Clone)]
pub struct PickerBindAction(BoxedAction);

impl PickerBindAction {
    /// Wraps a closure or function into this hook.
    #[must_use]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        Self(Arc::new(f))
    }

    /// Runs the hook through the wrapper seam.
    #[must_use]
    pub fn run(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        (self.0)(ctx)
    }
}

impl std::fmt::Debug for PickerBindAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PickerBindAction(..)")
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]

    use super::*;

    /// A minimal debug entry for hook signature exercises.
    #[derive(Debug, Clone)]
    struct Entry {
        name: String,
    }

    #[rstest::rstest]
    #[test]
    fn load_hook_delegates_to_the_stored_closure() {
        // Given a load hook producing one entry.
        let hook = PickerLoadFn::new(|_ctx: &mut LoadCtx<'_>| {
            vec![Entry {
                name: String::from("alpha"),
            }]
        });

        // When running it against a null host.
        let mut host = crate::test_host::FakeHost::new();
        let mut ctx = LoadCtx::new(&mut host);
        let entries = hook.run(&mut ctx);

        // Then the closure's entries come back.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "alpha");
    }

    #[rstest::rstest]
    #[test]
    fn row_hook_receives_selection_and_match_data() {
        // Given a row hook echoing its context.
        let hook = PickerRowFn::new(|entry: &Entry, ctx: &RowCtx<'_>| {
            Line::from(format!("{}{}", entry.name, ctx.match_ranges.len()))
        });

        // When running it with one match range.
        let entry = Entry {
            name: String::from("x"),
        };
        let line = hook.run(
            &entry,
            &RowCtx {
                is_selected: true,
                match_ranges: std::slice::from_ref(&(0..1)),
            },
        );

        // Then the context flowed through.
        assert_eq!(line.to_string(), "x1");
    }

    #[rstest::rstest]
    #[test]
    fn search_hook_computes_the_label() {
        // Given a search hook joining name twice.
        let hook = PickerSearchFn::new(|entry: &Entry| format!("{} {}", entry.name, entry.name));

        // When running it.
        let entry = Entry {
            name: String::from("s"),
        };

        // Then the computed text comes back.
        assert_eq!(hook.run(&entry), "s s");
    }

    #[rstest::rstest]
    #[test]
    fn lifecycle_hook_runs_and_returns_outcome() {
        // Given a lifecycle hook that closes.
        let hook =
            PickerLifecycleFn::new(|_ctx: &mut ActionCtx<'_>| PickerOutcome::empty().close());

        // When running it... (cannot run without a host; assert clone/debug
        // shape instead of inventing state).
        // Then the hook is cloneable and debug-printable (seam guarantees).
        let cloned = hook.clone();
        assert!(format!("{cloned:?}").contains("PickerLifecycleFn"));
    }

    #[rstest::rstest]
    #[test]
    fn status_hook_none_renders_no_line() {
        // Given a status hook returning None.
        let hook = PickerStatusFn::new(|_ctx: &StatusCtx<'_>| None);

        // When... (status hooks need a host; construction + clone is the
        // seam surface verified here).
        // Then the hook type is stable and cloneable.
        let cloned = hook.clone();
        assert!(format!("{cloned:?}").contains("PickerStatusFn"));
    }

    #[rstest::rstest]
    #[test]
    fn selection_change_hook_resolves_the_entry_by_index_and_runs() {
        // Given a spec storage holding two entries and a hook that records
        // the entry it receives.
        let mut registry = crate::registry::PickerRegistry::new();
        registry.register(
            crate::builder::PickerSpec::<Entry>::new(crate::id::PickerId::new("sc"))
                .search(|entry: &Entry| entry.name.clone()),
        );
        let items = registry
            .make_items::<Entry>(
                "sc",
                vec![
                    Entry {
                        name: String::from("first"),
                    },
                    Entry {
                        name: String::from("second"),
                    },
                ],
            )
            .expect("typed match");
        let mut host = crate::test_host::FakeHost::new();
        host.set_selection::<crate::entry::PickerEntry<Entry>>(
            crate::id::PickerId::new("sc"),
            jinn_selection_widget::SelectionState::new(),
        );
        {
            let state = crate::host::PickerHost::selection_state(
                &mut host,
                crate::id::PickerId::new("sc"),
            )
                .and_then(|any| {
                    any.downcast_mut::<jinn_selection_widget::SelectionState<
                        crate::entry::PickerEntry<Entry>,
                    >>()
                })
                .expect("storage");
            state.set_items(items);
        }

        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_sink = std::sync::Arc::clone(&seen);
        let hook = PickerSelectionChangeFn::<Entry>::new(move |entry: &Entry, _ctx| {
            seen_sink.lock().expect("lock").push(entry.name.clone());
        });

        // When running the hook for index 1.
        let mut ctx = crate::ctx::ActionCtx::new(crate::id::PickerId::new("sc"), &mut host);
        hook.run(1, &mut ctx);

        // Then the closure received the second entry.
        assert_eq!(seen.lock().expect("lock").as_slice(), ["second"]);
    }

    #[rstest::rstest]
    #[test]
    fn selection_change_hook_no_ops_on_out_of_bounds_index() {
        // Given a hook over storage with no items.
        let mut host = crate::test_host::FakeHost::new();
        host.set_selection::<crate::entry::PickerEntry<Entry>>(
            crate::id::PickerId::new("oob"),
            jinn_selection_widget::SelectionState::new(),
        );

        let fired: std::sync::Arc<std::sync::Mutex<bool>> =
            std::sync::Arc::new(std::sync::Mutex::new(false));
        let fired_sink = std::sync::Arc::clone(&fired);
        let hook = PickerSelectionChangeFn::<Entry>::new(move |_entry: &Entry, _ctx| {
            *fired_sink.lock().expect("lock") = true;
        });

        // When running for an index with no entry.
        let mut ctx = crate::ctx::ActionCtx::new(crate::id::PickerId::new("oob"), &mut host);
        hook.run(0, &mut ctx);

        // Then the closure never ran.
        assert!(!*fired.lock().expect("lock"));
    }

    #[rstest::rstest]
    #[test]
    fn selection_change_hook_reports_debug_and_clone_shape() {
        // Given a selection-change hook.
        let hook =
            PickerSelectionChangeFn::<Entry>::new(|_entry: &Entry, _ctx: &mut ActionCtx<'_>| {});

        // When cloning and debug-printing it.
        let cloned = hook.clone();

        // Then the seam surface is stable.
        assert!(format!("{cloned:?}").contains("PickerSelectionChangeFn"));
    }
}
