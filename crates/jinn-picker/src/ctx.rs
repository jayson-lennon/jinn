//! Contexts lent to spec hooks at dispatch and render time.
//!
//! Each hook signature names the context that carries what it needs:
//! lifecycle and bind hooks get [`ActionCtx`] (mutable, dispatch-time),
//! the status hook gets [`StatusCtx`] (read-only, render-time), row and
//! preview hooks get the plain data of [`RowCtx`] / [`PreviewCtx`], and
//! entry loading gets [`LoadCtx`].

use jinn_selection_widget::PreviewCache;

use crate::host::PickerHost;
use crate::id::PickerId;

/// Handed to lifecycle hooks (`on_open` / `on_confirm` / `on_close`) and
/// bind actions at dispatch time.
///
/// The context is the write guard: hooks mutate selection storage and
/// kernel state through it, never through captured external handles.
pub struct ActionCtx<'a> {
    picker_id: PickerId,
    host: &'a mut dyn PickerHost,
}

impl<'a> ActionCtx<'a> {
    /// Bundles the acting picker's id with the host lens.
    #[must_use]
    pub fn new(picker_id: PickerId, host: &'a mut dyn PickerHost) -> Self {
        Self { picker_id, host }
    }

    /// The id of the picker being acted on.
    #[must_use]
    pub fn picker_id(&self) -> PickerId {
        self.picker_id
    }

    /// The host lens (scroll accessors, palette, session id).
    #[must_use]
    pub fn host(&mut self) -> &mut dyn PickerHost {
        self.host
    }

    /// Downcast lend of this picker's selection storage to its concrete
    /// `SelectionState<PickerEntry<T>>` type.
    #[must_use]
    pub fn selection<S: std::any::Any>(&mut self) -> Option<&mut S> {
        self.host
            .selection_state(self.picker_id)?
            .downcast_mut::<S>()
    }

    /// The full kernel state — the sanctioned escape hatch for spec-authored
    /// downcasts while operations are still pilot-local.
    #[must_use]
    pub fn state_any(&mut self) -> &mut dyn std::any::Any {
        self.host.state_any()
    }
}

impl std::fmt::Debug for ActionCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionCtx")
            .field("picker_id", &self.picker_id)
            .field("host", &"dyn PickerHost")
            .finish()
    }
}

/// Handed to the status hook at render time — read-only.
pub struct StatusCtx<'a> {
    picker_id: PickerId,
    host: &'a dyn PickerHost,
}

impl<'a> StatusCtx<'a> {
    /// Bundles the rendered picker's id with the read-only host lens.
    #[must_use]
    pub fn new(picker_id: PickerId, host: &'a dyn PickerHost) -> Self {
        Self { picker_id, host }
    }

    /// The id of the picker being rendered.
    #[must_use]
    pub fn picker_id(&self) -> PickerId {
        self.picker_id
    }

    /// The read-only host lens (palette, session id, scrolls).
    #[must_use]
    pub fn host(&self) -> &dyn PickerHost {
        self.host
    }

    /// Downcast read lend of this picker's selection storage.
    #[must_use]
    pub fn selection<S: std::any::Any>(&self) -> Option<&S> {
        self.host
            .selection_state_ref(self.picker_id)?
            .downcast_ref::<S>()
    }

    /// Read-only full kernel state.
    #[must_use]
    pub fn state_any_ref(&self) -> &dyn std::any::Any {
        self.host.state_any_ref()
    }
}

impl std::fmt::Debug for StatusCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusCtx")
            .field("picker_id", &self.picker_id)
            .field("host", &"dyn PickerHost")
            .finish()
    }
}

/// Per-row render data for the row hook.
pub struct RowCtx<'a> {
    /// Whether this row is the current selection.
    pub is_selected: bool,
    /// Sorted, non-overlapping byte ranges of the fuzzy-filter matches
    /// within the row's display label (empty when no filter is active).
    pub match_ranges: &'a [std::ops::Range<usize>],
}

/// Handed to the load hook when the picker's entries are (re)built.
pub struct LoadCtx<'a> {
    host: &'a mut dyn PickerHost,
}

impl<'a> LoadCtx<'a> {
    /// Bundles the loading picker's id with the host lens.
    #[must_use]
    pub fn new(host: &'a mut dyn PickerHost) -> Self {
        Self { host }
    }

    /// The host lens.
    #[must_use]
    pub fn host(&mut self) -> &mut dyn PickerHost {
        self.host
    }
}

/// Handed to the preview hook per preview render.
pub struct PreviewCtx<'a> {
    /// Columns available in the preview pane; previews wrap to this width.
    pub width: usize,
    /// The domain-supplied preview cache, present when the spec configured
    /// `.preview_cache`. Caching still requires the entry to produce a
    /// [`crate::PreviewKey`].
    pub cache: Option<&'a dyn PreviewCache>,
}
