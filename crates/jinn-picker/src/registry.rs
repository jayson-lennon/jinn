//! The erased spec registry — picker behavior behind a runtime id.
//!
//! Specs are built typed, then erased into `Box<dyn ErasedPickerSpec>` so a
//! single registry can hold pickers with different entry types. Lookup is by
//! `&str` because intents carry runtime strings; [`BindRow`]s are the unit
//! of truth the keymap generator, keybind line, and geometry all walk.
//!
//! The registry lives in the kernel's shared services (like `KeyRoutes`) and
//! is lent to the keymap generator, the intent handler, and the render pass.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::ctx::ActionCtx;
use crate::ctx::LoadCtx;
use crate::ctx::StatusCtx;
use crate::entry::PickerEntry;
use crate::entry::RenderHooks;
use crate::entry::make_items;
use crate::hooks::PickerLifecycleFn;
use crate::hooks::PickerLoadFn;
use crate::hooks::PickerStatusFn;
use crate::host::PickerHost;
use crate::id::PickerId;
use crate::outcome::PickerOutcome;
use crate::widget::WidgetKind;

/// The tail appended to a generated keybind line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// Append `"Enter confirm · ESC cancel"`.
    Standard,
    /// No tail (the spec's binds alone fill the line).
    None,
}

/// One declared keybind — the unit of truth for the keymap binding, the
/// keybind footer line, and (via row count) nothing else. `category_hint`
/// maps onto the keymap's category enum at generation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindRow {
    /// Key notation in keymap display form, e.g. `<tab>`, `<c-u>`.
    pub notation: &'static str,
    /// Human label in the keybind line, e.g. `"toggle"`.
    pub label: &'static str,
    /// Keymap category hint: `"navigation"`, `"input"`, or `"general"`.
    pub category_hint: &'static str,
}

/// A built spec with its entry type erased.
///
/// The kernel interacts with pickers exclusively through this trait: it
/// never names an entry type, and the crate never names the kernel's state.
pub trait ErasedPickerSpec: Send + Sync {
    /// The picker's id.
    fn id(&self) -> PickerId;

    /// The popup border title (default: the id as a spaced label).
    fn title(&self) -> &'static str;

    /// The geometry-relevant widget flavor.
    fn widget_kind(&self) -> WidgetKind;

    /// Whether the preview scroll resets when the selection changes
    /// (the `Preview` widget's follow-the-cursor behavior). `false` for
    /// non-preview pickers.
    fn resets_scroll_on_selection_change(&self) -> bool;

    /// Footer rows the picker draws: the keybind line plus a status row
    /// when a status hook is declared. Geometry derives from this.
    fn bottom_rows(&self) -> u16 {
        1 + u16::from(self.has_status())
    }

    /// Whether a custom status hook is declared.
    fn has_status(&self) -> bool;

    /// Whether an open hook is declared.
    fn has_open(&self) -> bool;

    /// Whether a confirm hook is declared.
    fn has_confirm(&self) -> bool;

    /// Whether a close hook is declared.
    fn has_close(&self) -> bool;

    /// The declared bind rows, in declaration order.
    fn binds(&self) -> &[BindRow];

    /// Runs the `action`-named bind action. Unknown action names are a
    /// caller bug — the keymap only binds declared rows.
    fn run_action(&self, action: &str, ctx: &mut ActionCtx<'_>) -> PickerOutcome;

    /// Runs the open hook.
    fn run_open(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome;

    /// Runs the confirm hook (Enter).
    fn run_confirm(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome;

    /// Runs the close hook (ESC revert path only).
    fn run_close(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome;

    /// Runs the selection-change hook with the entry at `index` — the live
    /// preview that fires when the highlighted row changes. A no-op when the
    /// spec declares no hook or `index` is out of bounds.
    fn run_selection_change(&self, index: usize, ctx: &mut ActionCtx<'_>);

    /// Whether a selection-change hook is declared.
    fn has_selection_change(&self) -> bool;

    /// The currently highlighted index, read from the host's lent selection
    /// storage. `0` when no compatible storage is lent (an empty picker's
    /// hook never runs — the index is only consulted to pick an entry).
    fn selected_index(&self, host: &dyn PickerHost) -> usize;

    /// The custom status line for this frame; `None` renders a blank row.
    fn status_line(&self, ctx: &StatusCtx<'_>) -> Option<Line<'static>>;

    /// Rebuilds the picker's entries from the spec's load hook into the
    /// host's selection storage (`set_items` with freshly wrapped items).
    /// A no-op when the spec has no load hook.
    fn reload_items(&self, host: &mut dyn PickerHost);

    /// The erased render driver: dispatches on the widget kind and drives
    /// the corresponding selection widget with the spec's title, footers,
    /// colors, preview scroll, and preview cache. Returns `false` when the
    /// host lent no compatible storage (e.g. the kind is mapped but its
    /// storage has not been wrapped yet) — callers fall back to the
    /// legacy renderer.
    fn render(&self, frame: &mut Frame<'_>, area: Rect, host: &dyn PickerHost) -> bool;

    /// Downcast seam for the registry's typed window: the spec back as an
    /// `Any` handle so `make_items` can recover the entry type.
    fn as_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
}

impl std::fmt::Debug for dyn ErasedPickerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ErasedPickerSpec")
            .field("id", &self.id().as_str())
            .finish_non_exhaustive()
    }
}

/// The type-erasing adapter: a typed [`crate::builder::PickerSpec`]
/// converted into a boxed trait object.
/// Crate-internal erased-spec data carrier; the render driver and trait
/// impls consume its fields across modules.
#[expect(
    clippy::field_scoped_visibility_modifiers,
    reason = "crate-internal data carrier; accessor boilerplate adds no safety"
)]
pub(crate) struct TypedSpec<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    pub(crate) id: PickerId,
    pub(crate) title: Option<&'static str>,
    pub(crate) widget_kind: WidgetKind,
    pub(crate) reset_scroll_on_selection_change: bool,
    pub(crate) has_status: bool,
    pub(crate) binds: Vec<BindRow>,
    pub(crate) actions: Vec<crate::hooks::PickerBindAction>,
    pub(crate) keybind_tail: Tail,
    pub(crate) load: Option<PickerLoadFn<T>>,
    pub(crate) hooks: Arc<RenderHooks<T>>,
    pub(crate) status: Option<PickerStatusFn>,
    pub(crate) on_open: Option<PickerLifecycleFn>,
    pub(crate) on_confirm: Option<PickerLifecycleFn>,
    pub(crate) on_close: Option<PickerLifecycleFn>,
    pub(crate) selection_change: Option<crate::hooks::PickerSelectionChangeFn<T>>,
}

impl<T> ErasedPickerSpec for TypedSpec<T>
where
    T: jinn_selection_widget::TreeItem + std::fmt::Debug + Send + Sync + 'static,
{
    fn id(&self) -> PickerId {
        self.id
    }

    fn title(&self) -> &'static str {
        self.title.unwrap_or(self.id.as_str())
    }

    fn widget_kind(&self) -> WidgetKind {
        self.widget_kind
    }

    fn resets_scroll_on_selection_change(&self) -> bool {
        self.reset_scroll_on_selection_change
    }

    fn has_status(&self) -> bool {
        self.has_status
    }

    fn has_open(&self) -> bool {
        self.on_open.is_some()
    }

    fn has_confirm(&self) -> bool {
        self.on_confirm.is_some()
    }

    fn has_close(&self) -> bool {
        self.on_close.is_some()
    }

    fn binds(&self) -> &[BindRow] {
        &self.binds
    }

    fn run_action(&self, action: &str, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        let Some(index) = self.binds.iter().position(|row| row.notation == action) else {
            return PickerOutcome::empty();
        };
        let Some(action_fn) = self.actions.get(index) else {
            return PickerOutcome::empty();
        };
        action_fn.run(ctx)
    }

    fn run_open(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        match self.on_open.as_ref() {
            Some(hook) => hook.run(ctx),
            None => PickerOutcome::empty(),
        }
    }

    fn run_confirm(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        match self.on_confirm.as_ref() {
            Some(hook) => hook.run(ctx),
            None => PickerOutcome::empty(),
        }
    }

    fn run_close(&self, ctx: &mut ActionCtx<'_>) -> PickerOutcome {
        match self.on_close.as_ref() {
            Some(hook) => hook.run(ctx),
            None => PickerOutcome::empty(),
        }
    }

    fn run_selection_change(&self, index: usize, ctx: &mut ActionCtx<'_>) {
        if let Some(hook) = self.selection_change.as_ref() {
            hook.run(index, ctx);
        }
    }

    fn has_selection_change(&self) -> bool {
        self.selection_change.is_some()
    }

    fn selected_index(&self, host: &dyn PickerHost) -> usize {
        host.selection_state_ref(self.id)
            .and_then(|any| {
                any.downcast_ref::<jinn_selection_widget::SelectionState<PickerEntry<T>>>()
            })
            .map_or(0, jinn_selection_widget::SelectionState::selection)
    }

    fn status_line(&self, ctx: &StatusCtx<'_>) -> Option<Line<'static>> {
        let status = self.status.as_ref()?;
        status.run(ctx)
    }

    fn reload_items(&self, host: &mut dyn PickerHost) {
        let Some(load) = self.load.as_ref() else {
            return;
        };
        let mut load_ctx = LoadCtx::new(host);
        let entries = load.run(&mut load_ctx);
        let items = make_items(entries, &self.hooks);
        let host = load_ctx.host();
        // Widget-kind decides the storage flavor: Tree specs lend
        // TreePickerState, everything else SelectionState.
        match self.widget_kind() {
            crate::widget::WidgetKind::Tree => {
                if let Some(tree) = host.selection_state(self.id).and_then(|any| {
                    any.downcast_mut::<jinn_selection_widget::TreePickerState<PickerEntry<T>>>()
                }) {
                    tree.set_items(items);
                }
            }
            crate::widget::WidgetKind::List | crate::widget::WidgetKind::Preview => {
                if let Some(selection) = host.selection_state(self.id).and_then(|any| {
                    any.downcast_mut::<jinn_selection_widget::SelectionState<PickerEntry<T>>>()
                }) {
                    selection.set_items(items);
                }
            }
        }
    }

    fn render(&self, frame: &mut Frame<'_>, area: Rect, host: &dyn PickerHost) -> bool {
        crate::render::render_spec(self, frame, area, host)
    }

    fn as_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
}

/// A shared handle to one erased spec (cheap to clone, `Send + Sync`).
#[derive(Clone)]
pub struct SpecHandle(Arc<dyn ErasedPickerSpec>);

impl std::ops::Deref for SpecHandle {
    type Target = dyn ErasedPickerSpec;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

/// All registered picker specs, keyed by [`PickerId::as_str`].
///
/// Clone follows the `Services`-container rule: specs register once at
/// composition, clones share the same table. The interior lock exists so a
/// late registration (spec migration in progress) stays sound against
/// concurrent lookup; it is never contended in normal operation.
#[derive(Clone, Default)]
pub struct PickerRegistry {
    specs: std::sync::Arc<std::sync::RwLock<HashMap<&'static str, Arc<dyn ErasedPickerSpec>>>>,
}

impl std::fmt::Debug for PickerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerRegistry")
            .field(
                "ids",
                &self
                    .specs
                    .read()
                    .map(|specs| specs.keys().copied().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .finish()
    }
}

impl PickerRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Erases and registers a built spec.
    ///
    /// `T: TreeItem` at the boundary so tree specs (whose storage lends
    /// `TreePickerState<PickerEntry<T>>`) erase uniformly with flat specs;
    /// flat entries simply have `id`/`parent_id` that are never consulted.
    pub fn register<T>(&mut self, spec: crate::builder::PickerSpec<T>)
    where
        T: jinn_selection_widget::TreeItem + std::fmt::Debug + Send + Sync + 'static,
    {
        // Move the builder's fields into the erased wrapper. Access is via
        // the crate-private accessors below (builder fields are private).
        let (
            id,
            title,
            widget_kind,
            reset_scroll_on_selection_change,
            has_status,
            binds,
            actions,
            keybind_tail,
            load,
            hooks,
            status,
            on_open,
            on_confirm,
            on_close,
            selection_change,
        ) = spec.into_parts();
        let typed = TypedSpec {
            id,
            title,
            widget_kind,
            reset_scroll_on_selection_change,
            has_status,
            binds,
            actions,
            keybind_tail,
            load,
            hooks: Arc::new(hooks),
            status,
            on_open,
            on_confirm,
            on_close,
            selection_change,
        };
        if let Ok(mut specs) = self.specs.write() {
            specs.insert(id.as_str(), Arc::new(typed));
        }
    }

    /// Looks a spec up by id string (intents carry runtime `String`s).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<SpecHandle> {
        self.specs.read().ok()?.get(id).cloned().map(SpecHandle)
    }

    /// Builds widget items from domain entries via the registered spec's
    /// hooks — the search hook runs exactly once per entry here. Domain
    /// reload paths call this instead of touching spec internals.
    #[must_use]
    pub fn make_items<T>(&self, id: &str, entries: Vec<T>) -> Option<Vec<PickerEntry<T>>>
    where
        T: std::fmt::Debug + Send + Sync + 'static,
    {
        let spec = Arc::clone(&self.get(id)?.0);
        let typed = spec.as_any_arc().downcast::<TypedSpec<T>>().ok()?;
        Some(crate::entry::make_items(entries, &typed.hooks))
    }

    /// A cheap handle-sharing clone for per-frame contexts (RenderCtx is
    /// rebuilt every frame; this avoids double-locking bookkeeping noise).
    #[must_use]
    pub fn clone_shallow(&self) -> Self {
        Self {
            specs: std::sync::Arc::clone(&self.specs),
        }
    }

    /// Every registered spec, as clonable cheap handles.
    #[must_use]
    pub fn all(&self) -> Vec<SpecHandle> {
        self.specs
            .read()
            .map(|specs| specs.values().cloned().map(SpecHandle).collect())
            .unwrap_or_default()
    }

    /// Every registered spec id.
    pub fn ids(&self) -> Vec<&'static str> {
        self.specs
            .read()
            .map(|specs| specs.keys().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test module, panics are acceptable"
    )]

    /// A schema'd stand-in message for action-closure assertions.
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    pub(super) struct RecordedToggle;

    jinn_slices::crossing_schema!(RecordedToggle, "PickerRecordedToggle",
        trouper::schema::SchemaKind::Event,
        description: "Picker registry action test message.",
        fields: []);

    use super::*;
    use crate::builder::PickerSpec;
    use crate::ctx::LoadCtx;
    use crate::id::PickerId;
    use crate::test_host::FakeHost;
    use jinn_selection_widget::PickerItem;

    #[derive(Debug, Clone)]
    struct Entry {
        name: String,
    }

    impl jinn_selection_widget::TreeItem for Entry {
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
    fn registry_round_trips_a_registered_spec_by_id() {
        // Given a registry with one registered spec.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("roundtrip"))
                .title(" Roundtrip ")
                .bind("<tab>", "toggle", |_ctx: &mut ActionCtx<'_>| {
                    PickerOutcome::empty()
                }),
        );

        // When looking it up by id string.
        let spec = registry.get("roundtrip").expect("registered");

        // Then the erased surface reports the authored data.
        assert_eq!(spec.id(), PickerId::new("roundtrip"));
        assert_eq!(spec.title(), " Roundtrip ");
        assert_eq!(spec.binds().len(), 1);
        assert_eq!(spec.bottom_rows(), 1);
        assert_eq!(spec.widget_kind(), WidgetKind::List);
    }

    #[rstest::rstest]
    #[test]
    fn get_by_unknown_id_is_none() {
        // Given a registry with one spec.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("known")));

        // When looking up a different id.
        // Then nothing is returned.
        assert!(registry.get("unknown").is_none());
    }

    #[rstest::rstest]
    #[test]
    fn run_action_dispatches_by_row_notation() {
        // Given a spec whose toggle action records a message.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("dispatch")).bind(
            "<tab>",
            "toggle",
            |_ctx: &mut ActionCtx<'_>| PickerOutcome::new_message(RecordedToggle),
        ));
        let spec = registry.get("dispatch").expect("registered");

        // When running the action by notation.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("dispatch"), &mut host);
        let outcome = spec.run_action("<tab>", &mut ctx);

        // Then the action ran (message recorded) and the picker stayed open.
        assert_eq!(outcome.message_names, ["alloc::string::String"]);
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn run_action_with_unknown_notation_is_an_empty_outcome() {
        // Given a registered spec with one bind.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("noop")).bind(
            "<tab>",
            "toggle",
            |_ctx: &mut ActionCtx<'_>| PickerOutcome::empty(),
        ));
        let spec = registry.get("noop").expect("registered");

        // When running an undeclared action name.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("noop"), &mut host);
        let outcome = spec.run_action("<x>", &mut ctx);

        // Then nothing ran and nothing closes.
        assert!(outcome.message_names.is_empty());
        assert!(!outcome.close);
    }

    #[rstest::rstest]
    #[test]
    fn absent_lifecycle_hooks_yield_empty_outcomes() {
        // Given a spec with no lifecycle hooks.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("bare")));
        let spec = registry.get("bare").expect("registered");

        // When running open, confirm, and close.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("bare"), &mut host);
        let (open, confirm, close) = (
            spec.run_open(&mut ctx),
            spec.run_confirm(&mut ctx),
            spec.run_close(&mut ctx),
        );

        // Then all three are empty, non-closing outcomes.
        assert!(open.message_names.is_empty() && !open.close);
        assert!(confirm.message_names.is_empty() && !confirm.close);
        assert!(close.message_names.is_empty() && !close.close);
    }

    #[rstest::rstest]
    #[test]
    fn make_items_builds_through_the_spec_hooks() {
        // Given a spec with a search hook.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("typed"))
                .search(|entry: &Entry| format!("search-{}", entry.name)),
        );

        // When building items through the registry.
        let items = registry
            .make_items::<Entry>(
                "typed",
                vec![Entry {
                    name: String::from("a"),
                }],
            )
            .expect("typed match");

        // Then the search hook produced the display label.
        assert_eq!(PickerItem::display_label(&items[0]), "search-a");
    }

    #[rstest::rstest]
    #[test]
    fn make_items_with_wrong_entry_type_is_none() {
        // Given a spec registered with Entry.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("mismatch")));

        // When building items with a different entry type.
        // Then nothing is returned.
        assert!(registry.make_items::<String>("mismatch", vec![]).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn ids_lists_registered_spec_ids() {
        // Given a registry with one spec.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("solo")));

        // When listing ids.
        // Then the registered id appears.
        assert_eq!(registry.ids(), ["solo"]);
    }

    #[rstest::rstest]
    #[test]
    fn reload_items_fills_selection_storage_through_the_host() {
        // Given a spec with a load hook.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("loadtest"))
                .search(|entry: &Entry| entry.name.clone())
                .load(|_ctx: &mut LoadCtx<'_>| {
                    vec![Entry {
                        name: String::from("loaded"),
                    }]
                }),
        );
        let spec = registry.get("loadtest").expect("registered");

        // When reloading items into a host with empty storage.
        let mut host = FakeHost::new();
        host.set_selection::<crate::entry::PickerEntry<Entry>>(
            PickerId::new("loadtest"),
            jinn_selection_widget::SelectionState::new(),
        );
        spec.reload_items(&mut host);

        // Then the storage holds the wrapped entries.
        let state = host
            .selection::<crate::entry::PickerEntry<Entry>>(PickerId::new("loadtest"))
            .expect("storage");
        assert_eq!(state.items().len(), 1);
        assert_eq!(PickerItem::display_label(&state.items()[0]), "loaded");
    }

    #[rstest::rstest]
    #[test]
    fn selection_change_absent_hook_reports_absent_and_no_ops() {
        // Given a registered spec without a selection-change hook.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("noscv")));
        let spec = registry.get("noscv").expect("registered");

        // When querying the hook surface and running it.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("noscv"), &mut host);
        spec.run_selection_change(0, &mut ctx);

        // Then the hook is reported absent and running is a no-op.
        assert!(!spec.has_selection_change());
    }

    #[rstest::rstest]
    #[test]
    fn erased_selection_change_dispatch_resolves_entry_by_index() {
        // Given a registered spec with a hook recording the entries it sees.
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_sink = std::sync::Arc::clone(&seen);
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("scv"))
                .search(|entry: &Entry| entry.name.clone())
                .on_selection_change(move |entry: &Entry, _ctx: &mut ActionCtx<'_>| {
                    seen_sink.lock().expect("lock").push(entry.name.clone());
                }),
        );
        let spec = registry.get("scv").expect("registered");
        assert!(spec.has_selection_change());

        // And storage preloaded with two entries.
        let items = registry
            .make_items::<Entry>(
                "scv",
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
        let mut host = FakeHost::new();
        host.set_selection::<PickerEntry<Entry>>(
            PickerId::new("scv"),
            jinn_selection_widget::SelectionState::new(),
        );
        {
            let state = host
                .selection_state(PickerId::new("scv"))
                .and_then(|any| {
                    any.downcast_mut::<jinn_selection_widget::SelectionState<PickerEntry<Entry>>>()
                })
                .expect("storage");
            state.set_items(items);
        }

        // When dispatching through the erased surface for index 0.
        let mut ctx = ActionCtx::new(PickerId::new("scv"), &mut host);
        spec.run_selection_change(0, &mut ctx);

        // Then the declared closure saw the first entry.
        assert_eq!(seen.lock().expect("lock").as_slice(), ["first"]);
    }

    #[rstest::rstest]
    #[test]
    fn erased_selection_change_dispatch_no_ops_on_out_of_bounds_index() {
        // Given a registered spec with a hook that must never fire.
        let fired: std::sync::Arc<std::sync::Mutex<bool>> =
            std::sync::Arc::new(std::sync::Mutex::new(false));
        let fired_sink = std::sync::Arc::clone(&fired);
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("scoob")).on_selection_change(
                move |_entry: &Entry, _ctx: &mut ActionCtx<'_>| {
                    *fired_sink.lock().expect("lock") = true;
                },
            ),
        );
        let spec = registry.get("scoob").expect("registered");

        // And storage with no items.
        let mut host = FakeHost::new();
        host.set_selection::<PickerEntry<Entry>>(
            PickerId::new("scoob"),
            jinn_selection_widget::SelectionState::new(),
        );

        // When dispatching for an index with no entry.
        let mut ctx = ActionCtx::new(PickerId::new("scoob"), &mut host);
        spec.run_selection_change(0, &mut ctx);

        // Then the declared closure never ran.
        assert!(!*fired.lock().expect("lock"));
    }
}
