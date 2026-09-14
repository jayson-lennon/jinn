//! The spec builder — every dimension of a picker's behavior, authored once.
//!
//! [`PickerSpec`] is created through [`PickerSpec::new`] + builder methods
//! and is consumed by [`crate::registry::PickerRegistry::register`], which
//! erases `T` behind [`crate::registry::ErasedPickerSpec`]. All hooks are
//! optional and have documented defaults: unstyled label rows, label search
//! text, empty preview, no cache identity, no custom status, standard
//! keybind tail, and no-op lifecycles.

use ratatui::text::Line;

use crate::ctx::ActionCtx;
use crate::ctx::LoadCtx;
use crate::ctx::PreviewCtx;
use crate::ctx::RowCtx;
use crate::ctx::StatusCtx;
use crate::entry::RenderHooks;
use crate::hooks::PickerBindAction;
use crate::hooks::PickerLifecycleFn;
use crate::hooks::PickerLoadFn;
use crate::hooks::PickerPreviewFn;
use crate::hooks::PickerPreviewKeyFn;
use crate::hooks::PickerRowFn;
use crate::hooks::PickerSearchFn;
use crate::hooks::PickerStatusFn;
use crate::id::PickerId;
use crate::outcome::PickerOutcome;
use crate::preview_key::PreviewKey;
use crate::registry::BindRow;
use crate::registry::Tail;
use crate::widget::PickerWidget;

/// The full behavioral description of one picker.
///
/// Built with the builder pattern; `T` is the domain entry type. Specs are
/// `Send + Sync` so the registry can live in the kernel's shared services.
pub struct PickerSpec<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    id: PickerId,
    title: Option<&'static str>,
    widget: PickerWidget,
    load: Option<PickerLoadFn<T>>,
    hooks: RenderHooks<T>,
    status: Option<PickerStatusFn>,
    binds: Vec<BindRow>,
    actions: Vec<PickerBindAction>,
    keybind_tail: Tail,
    on_open: Option<PickerLifecycleFn>,
    on_confirm: Option<PickerLifecycleFn>,
    on_close: Option<PickerLifecycleFn>,
}

impl<T> PickerSpec<T>
where
    T: std::fmt::Debug + Send + Sync + 'static,
{
    /// Starts a spec for the picker identified by `id`.
    #[must_use]
    pub fn new(id: PickerId) -> Self {
        Self {
            id,
            title: None,
            widget: PickerWidget::default(),
            load: None,
            hooks: RenderHooks::default(),
            status: None,
            binds: Vec::new(),
            actions: Vec::new(),
            keybind_tail: Tail::Standard,
            on_open: None,
            on_confirm: None,
            on_close: None,
        }
    }

    /// Sets the popup border title. Default: the picker id.
    #[must_use]
    pub fn title(mut self, title: &'static str) -> Self {
        self.title = Some(title);
        self
    }

    /// Sets the widget chrome. Default: [`PickerWidget::List`].
    #[must_use]
    pub fn widget(mut self, widget: PickerWidget) -> Self {
        self.widget = widget;
        self
    }

    /// Declares the entry loader, run when the picker's entries are built.
    /// Default: no synchronous load on open.
    #[must_use]
    pub fn load<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut LoadCtx<'_>) -> Vec<T> + Send + Sync + 'static,
    {
        self.load = Some(PickerLoadFn::new(f));
        self
    }

    /// Declares the row renderer. Default: the plain search-text label.
    #[must_use]
    pub fn row<F>(mut self, f: F) -> Self
    where
        F: Fn(&T, RowCtx<'_>) -> Line<'static> + Send + Sync + 'static,
    {
        self.hooks.row = Some(PickerRowFn::new(f));
        self
    }

    /// Declares the search-text computer, run once per entry at load.
    /// Default: the row's plain text.
    #[must_use]
    pub fn search<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> String + Send + Sync + 'static,
    {
        self.hooks.search = Some(PickerSearchFn::new(f));
        self
    }

    /// Declares the preview renderer. Default: an empty preview pane.
    #[must_use]
    pub fn preview<F>(mut self, f: F) -> Self
    where
        F: Fn(&T, &PreviewCtx<'_>) -> Vec<Line<'static>> + Send + Sync + 'static,
    {
        self.hooks.preview = Some(PickerPreviewFn::new(f));
        self
    }

    /// Declares the preview cache identity. Entries without a key render
    /// live. Caching requires this *and* the kernel supplying a cache via
    /// [`crate::PickerHost::preview_cache`]. Default: no keys (caching off
    /// per entry).
    #[must_use]
    pub fn preview_key<F>(mut self, f: F) -> Self
    where
        F: Fn(&T) -> Option<PreviewKey> + Send + Sync + 'static,
    {
        self.hooks.preview_key = Some(PickerPreviewKeyFn::new(f));
        self
    }

    /// Declares the custom status line above the keybind line. `None`
    /// renders a blank line, keeping the geometry stable. Declaring a
    /// status hook reserves its row (`bottom_rows` grows by one). Default:
    /// no status hook.
    #[must_use]
    pub fn status<F>(mut self, f: F) -> Self
    where
        F: Fn(&StatusCtx<'_>) -> Option<Line<'static>> + Send + Sync + 'static,
    {
        self.status = Some(PickerStatusFn::new(f));
        self
    }

    /// Declares a keybind row: the keymap binding, its keybind-line entry,
    /// and the action dispatched when the key fires. Rows stay in
    /// declaration order across all three consumers. Category hint:
    /// `general`.
    #[must_use]
    pub fn bind<F>(self, notation: &'static str, label: &'static str, action: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.push_bind(notation, label, "general", action)
    }

    /// [`Self::bind`] with the `navigation` category hint (preview
    /// scrolling and the like).
    #[must_use]
    pub fn bind_navigation<F>(
        self,
        notation: &'static str,
        label: &'static str,
        action: F,
    ) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.push_bind(notation, label, "navigation", action)
    }

    /// Appends the standard keybind tail ("Enter confirm · ESC cancel") to
    /// the generated keybind line. Default: standard tail.
    #[must_use]
    pub fn keybind_tail(mut self, tail: Tail) -> Self {
        self.keybind_tail = tail;
        self
    }

    /// Declares the open behavior. Default: no messages, picker stays open.
    #[must_use]
    pub fn on_open<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.on_open = Some(PickerLifecycleFn::new(f));
        self
    }

    /// Declares the confirm behavior (Enter). Default: no messages.
    #[must_use]
    pub fn on_confirm<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.on_confirm = Some(PickerLifecycleFn::new(f));
        self
    }

    /// Declares the close behavior (ESC revert path only — never the
    /// confirm path). Default: no messages.
    #[must_use]
    pub fn on_close<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.on_close = Some(PickerLifecycleFn::new(f));
        self
    }

    /// Finishes the builder chain. The spec is then registered via
    /// [`crate::registry::PickerRegistry::register`], which erases `T`.
    #[must_use]
    pub fn build(self) -> Self {
        self
    }

    /// Shared tail of [`Self::bind`] / [`Self::bind_navigation`]: the row
    /// and its action are pushed in lockstep so they stay index-aligned.
    fn push_bind<F>(mut self, notation: &'static str, label: &'static str, category_hint: &'static str, action: F) -> Self
    where
        F: Fn(&mut ActionCtx<'_>) -> PickerOutcome + Send + Sync + 'static,
    {
        self.binds.push(BindRow {
            notation,
            label,
            category_hint,
        });
        self.actions.push(PickerBindAction::new(action));
        self
    }

    /// Dismantles the spec for registration (crate-private; the registry
    /// erases these parts into a boxed `ErasedPickerSpec`).
    #[expect(clippy::type_complexity, reason = "one-shot dismantle into the registry")]
    pub(crate) fn into_parts(
        self,
    ) -> (
        PickerId,
        Option<&'static str>,
        crate::widget::WidgetKind,
        bool,
        bool,
        Vec<BindRow>,
        Vec<PickerBindAction>,
        Tail,
        Option<PickerLoadFn<T>>,
        RenderHooks<T>,
        Option<PickerStatusFn>,
        Option<PickerLifecycleFn>,
        Option<PickerLifecycleFn>,
        Option<PickerLifecycleFn>,
    ) {
        let reset_scroll_on_selection_change = match &self.widget {
            PickerWidget::Preview(spec) => spec.reset_scroll_on_selection_change,
            PickerWidget::List | PickerWidget::Tree => false,
        };
        (
            self.id,
            self.title,
            self.widget.kind(),
            reset_scroll_on_selection_change,
            self.status.is_some(),
            self.binds,
            self.actions,
            self.keybind_tail,
            self.load,
            self.hooks,
            self.status,
            self.on_open,
            self.on_confirm,
            self.on_close,
        )
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
    use crate::id::PickerId;

    #[derive(Debug)]
    struct Entry {
        #[expect(dead_code, reason = "guards against accidentally matching on fields")]
        name: String,
    }

    #[test]
    fn spec_defaults_to_list_widget_and_standard_tail() {
        // Given a bare spec.
        let spec = PickerSpec::<Entry>::new(PickerId::new("t1"));

        // When dismantling it for registration.
        let (id, title, kind, _reset, _has_status, binds, _actions, tail, _load, _hooks, _status, _open, _confirm, _close) =
            spec.into_parts();

        // Then defaults apply: id-titled list picker with no binds.
        assert_eq!(id, PickerId::new("t1"));
        assert!(title.is_none());
        assert_eq!(kind, crate::widget::WidgetKind::List);
        assert!(binds.is_empty());
        assert_eq!(tail, Tail::Standard);
    }

    #[test]
    fn declaring_a_status_reserves_a_bottom_row() {
        // Given a spec with a status hook.
        let spec = PickerSpec::<Entry>::new(PickerId::new("t2"))
            .status(|_ctx: &StatusCtx<'_>| None);

        // When dismantling it.
        let (_id, _title, _kind, _reset, has_status, _binds, _actions, _tail, _load, _hooks, _status, _open, _confirm, _close) =
            spec.into_parts();

        // Then the status row is reserved.
        assert!(has_status);
    }

    #[test]
    fn binds_and_actions_stay_index_aligned() {
        // Given a spec with two binds in different categories.
        let spec = PickerSpec::<Entry>::new(PickerId::new("t3"))
            .bind("<tab>", "toggle", |_ctx: &mut ActionCtx<'_>| {
                PickerOutcome::empty()
            })
            .bind_navigation("<c-d>", "page", |_ctx: &mut ActionCtx<'_>| {
                PickerOutcome::empty()
            });

        // When dismantling it.
        let (_id, _title, _kind, _reset, _has_status, binds, actions, _tail, _load, _hooks, _status, _open, _confirm, _close) =
            spec.into_parts();

        // Then rows carry declaration order and their hints.
        assert_eq!(binds.len(), 2);
        assert_eq!(actions.len(), 2);
        assert_eq!(binds[0].notation, "<tab>");
        assert_eq!(binds[0].category_hint, "general");
        assert_eq!(binds[1].notation, "<c-d>");
        assert_eq!(binds[1].category_hint, "navigation");
    }
}
