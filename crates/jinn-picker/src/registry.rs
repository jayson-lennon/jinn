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

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::builder::SharedPreviewCache;
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

    /// Footer rows the picker draws: the keybind line plus a status row
    /// when a status hook is declared. Geometry derives from this.
    fn bottom_rows(&self) -> u16 {
        1 + u16::from(self.has_status())
    }

    /// Whether a custom status hook is declared.
    fn has_status(&self) -> bool;

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

    /// The custom status line for this frame; `None` renders a blank row.
    fn status_line(&self, ctx: &StatusCtx<'_>) -> Option<Line<'static>>;

    /// Rebuilds the picker's entries from the spec's load hook into the
    /// host's selection storage (`set_items` with freshly wrapped items).
    /// A no-op when the spec has no load hook.
    fn reload_items(&self, host: &mut dyn PickerHost);

    /// The erased render driver: dispatches on the widget kind and drives
    /// the corresponding selection widget with the spec's title, footers,
    /// colors, preview scroll, and preview cache.
    fn render(&self, frame: &mut Frame<'_>, area: Rect, host: &dyn PickerHost);

    /// Downcast seam for the registry's typed window.
    fn as_any(&self) -> &dyn std::any::Any;
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
    pub(crate) has_status: bool,
    pub(crate) binds: Vec<BindRow>,
    pub(crate) actions: Vec<crate::hooks::PickerBindAction>,
    pub(crate) keybind_tail: Tail,
    pub(crate) load: Option<PickerLoadFn<T>>,
    pub(crate) hooks: Arc<RenderHooks<T>>,
    pub(crate) preview_cache: Option<SharedPreviewCache>,
    pub(crate) status: Option<PickerStatusFn>,
    pub(crate) on_open: Option<PickerLifecycleFn>,
    pub(crate) on_confirm: Option<PickerLifecycleFn>,
    pub(crate) on_close: Option<PickerLifecycleFn>,
}

impl<T> ErasedPickerSpec for TypedSpec<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
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

    fn has_status(&self) -> bool {
        self.has_status
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
        if let Some(selection) = load_ctx
            .host()
            .selection_state(self.id)
            .and_then(|any| any.downcast_mut::<jinn_selection_widget::SelectionState<PickerEntry<T>>>())
        {
            selection.set_items(items);
        }
    }

    fn render(&self, frame: &mut Frame<'_>, area: Rect, host: &dyn PickerHost) {
        crate::render::render_spec(self, frame, area, host);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A typed window onto one registered spec.
///
/// Domain reload paths need the entry type to wrap domain entries into
/// [`PickerEntry`] items; this keeps the hooks crate-private while making
/// item-building public.
pub struct TypedPicker<'a, T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    spec: &'a TypedSpec<T>,
}

impl<T> TypedPicker<'_, T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    /// Builds widget items from domain entries via the spec's hooks — the
    /// search hook runs exactly once per entry here.
    #[must_use]
    pub fn make_items(&self, entries: Vec<T>) -> Vec<PickerEntry<T>> {
        make_items(entries, &self.spec.hooks)
    }
}

/// All registered picker specs, keyed by [`PickerId::as_str`].
#[derive(Default)]
pub struct PickerRegistry {
    specs: HashMap<&'static str, Box<dyn ErasedPickerSpec>>,
}

impl std::fmt::Debug for PickerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PickerRegistry")
            .field("ids", &self.specs.keys().collect::<Vec<_>>())
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
    pub fn register<T>(&mut self, spec: crate::builder::PickerSpec<T>)
    where
        T: std::fmt::Debug + Send + Sync + 'static,
    {
        // Move the builder's fields into the erased wrapper. Access is via
        // the crate-private accessors below (builder fields are private).
        let (id, title, widget_kind, has_status, binds, actions, keybind_tail, load, hooks, preview_cache, status, on_open, on_confirm, on_close) =
            spec.into_parts();
        let typed = TypedSpec {
            id,
            title,
            widget_kind,
            has_status,
            binds,
            actions,
            keybind_tail,
            load,
            hooks: Arc::new(hooks),
            preview_cache,
            status,
            on_open,
            on_confirm,
            on_close,
        };
        self.specs.insert(id.as_str(), Box::new(typed));
    }

    /// Looks a spec up by id string (intents carry runtime `String`s).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&dyn ErasedPickerSpec> {
        self.specs.get(id).map(std::convert::AsRef::as_ref)
    }

    /// A typed window onto the spec registered under `id`, for callers that
    /// build wrapped items (domain reload paths).
    #[must_use]
    pub fn get_typed<T>(&self, id: &str) -> Option<TypedPicker<'_, T>>
    where
        T: std::fmt::Debug + Send + Sync + 'static,
    {
        let any = self.specs.get(id)?.as_any();
        any.downcast_ref::<TypedSpec<T>>()
            .map(|spec| TypedPicker { spec })
    }

    /// Every registered spec.
    pub fn all(&self) -> impl Iterator<Item = &dyn ErasedPickerSpec> {
        self.specs.values().map(std::convert::AsRef::as_ref)
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
    use crate::builder::PickerSpec;
    use crate::ctx::LoadCtx;
    use crate::id::PickerId;
    use crate::test_host::FakeHost;
    use jinn_selection_widget::PickerItem;

    #[derive(Debug)]
    struct Entry {
        name: String,
    }

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

    #[test]
    fn get_by_unknown_id_is_none() {
        // Given a registry with one spec.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("known")));

        // When looking up a different id.
        // Then nothing is returned.
        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn run_action_dispatches_by_row_notation() {
        // Given a spec whose toggle action records a message.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("dispatch"))
                .bind("<tab>", "toggle", |_ctx: &mut ActionCtx<'_>| {
                    PickerOutcome::new_message(String::from("toggled"))
                }),
        );
        let spec = registry.get("dispatch").expect("registered");

        // When running the action by notation.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("dispatch"), &mut host);
        let outcome = spec.run_action("<tab>", &mut ctx);

        // Then the action ran (message recorded) and the picker stayed open.
        assert_eq!(outcome.message_names, ["alloc::string::String"]);
        assert!(!outcome.close);
    }

    #[test]
    fn run_action_with_unknown_notation_is_an_empty_outcome() {
        // Given a registered spec with one bind.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("noop"))
                .bind("<tab>", "toggle", |_ctx: &mut ActionCtx<'_>| {
                    PickerOutcome::empty()
                }),
        );
        let spec = registry.get("noop").expect("registered");

        // When running an undeclared action name.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("noop"), &mut host);
        let outcome = spec.run_action("<x>", &mut ctx);

        // Then nothing ran and nothing closes.
        assert!(outcome.message_names.is_empty());
        assert!(!outcome.close);
    }

    #[test]
    fn absent_lifecycle_hooks_yield_empty_outcomes() {
        // Given a spec with no lifecycle hooks.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("bare")));
        let spec = registry.get("bare").expect("registered");

        // When running open, confirm, and close.
        let mut host = FakeHost::new();
        let mut ctx = ActionCtx::new(PickerId::new("bare"), &mut host);
        let (open, confirm, close) =
            (spec.run_open(&mut ctx), spec.run_confirm(&mut ctx), spec.run_close(&mut ctx));

        // Then all three are empty, non-closing outcomes.
        assert!(open.message_names.is_empty() && !open.close);
        assert!(confirm.message_names.is_empty() && !confirm.close);
        assert!(close.message_names.is_empty() && !close.close);
    }

    #[test]
    fn get_typed_builds_items_through_the_spec_hooks() {
        // Given a spec with a search hook.
        let mut registry = PickerRegistry::new();
        registry.register(
            PickerSpec::<Entry>::new(PickerId::new("typed"))
                .search(|entry: &Entry| format!("search-{}", entry.name)),
        );

        // When building items through the typed window.
        let typed = registry.get_typed::<Entry>("typed").expect("typed");
        let items = typed.make_items(vec![Entry {
            name: String::from("a"),
        }]);

        // Then the search hook produced the display label.
        assert_eq!(PickerItem::display_label(&items[0]), "search-a");
    }

    #[test]
    fn get_typed_with_wrong_entry_type_is_none() {
        // Given a spec registered with Entry.
        let mut registry = PickerRegistry::new();
        registry.register(PickerSpec::<Entry>::new(PickerId::new("mismatch")));

        // When requesting a typed window with a different entry type.
        // Then no window is returned.
        assert!(registry.get_typed::<String>("mismatch").is_none());
    }

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
        host.set_selection::<crate::entry::PickerEntry<Entry>>(PickerId::new("loadtest"),
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

}
