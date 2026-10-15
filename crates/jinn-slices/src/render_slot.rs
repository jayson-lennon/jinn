//! The slice-owned draw registry — one draw function per render region.
//!
//! A region is a *place on screen that composition fills*, not a slice.
//! Composition decides where the chat log goes; the chat-log slice
//! decides what is painted there. This registry is the seam between
//! the two: a slice registers [`DrawFn`] for a [`Region`] at
//! activation, and the render pass calls it with the rect it computed.
//!
//! Two properties follow from that split, and both are the point:
//!
//! * **The slice names its own region, not its own element.** A draw
//!   function is registered under a [`Region`] key, so composition
//!   stops string-matching a slice's element name in order to find it.
//! * **An unregistered region paints nothing.** That is the same
//!   degradation `OverlayViews` has always had, and the startup
//!   pairing check is where a missing registration is caught.
//!
//! The registry is generic over the render context, so it can be
//! declared here without naming `jinn-kernel`'s [`RenderCtx`]:
//! `jinn-kernel` must not depend on a slice crate, and this crate
//! depends only on `jinn-theme` and `ratatui`. The kernel instantiates
//! it at its own `RenderCtx`; a slice instantiates it at whatever
//! context its draw function needs.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::ConfigLayer;
use crate::OverlayViews;
use crate::Slices;
use crate::render_facts::RenderFacts;

/// A region of the screen a slice has claimed for drawing.
///
/// Composition resolves a region to a rect and asks the registry who
/// draws there. The variants are the regions the chat layout and the
/// full-width tab fill; a slice registers against the one it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Region {
    /// The scrolling conversation history.
    ChatLog,
    /// The message composer at the bottom of the content area.
    ChatInput,
    /// The one-row busy indicator above the chat bottom line.
    StreamingIndicator,
    /// The two-row status block at the bottom of the main column.
    StatusBar,
    /// The vertical minimap column beside the chat log.
    Minimap,
    /// The sidebar column, right of the chat border.
    Sidebar,
    /// The audit popup overlaying the chat log.
    AuditPopup,
    /// The autocomplete popup anchored to the input box.
    Autocomplete,
    /// Surfaces that overflow the region that anchors them, painted in a
    /// top layer so no column drawn later can cover them.
    ///
    /// A popup wider than the sidebar it hangs off reaches left across the
    /// chat column, so it cannot paint from inside that column's draw call:
    /// the render pass draws the chat log after it, and chat would win. The
    /// layer is a separate region dispatched after the base columns for
    /// exactly that reason.
    ///
    /// The order *within* the layer is a call sequence in the registering
    /// slice, not a number here. When a second slice registers against this
    /// region, the numbers arrive with that registration: each registrant
    /// carries a `u16` priority, the layer sorts by it, and ties break by
    /// call order. Until then there is one registrant and the sequence is
    /// the whole of the ordering.
    FloatingSurfaces,
}

impl Region {
    /// Every region, in no particular order. Backs a startup check
    /// that reports which regions went unregistered.
    #[must_use]
    pub fn all() -> &'static [Region] {
        &[
            Region::ChatLog,
            Region::ChatInput,
            Region::StreamingIndicator,
            Region::StatusBar,
            Region::Minimap,
            Region::Sidebar,
            Region::AuditPopup,
            Region::Autocomplete,
            Region::FloatingSurfaces,
        ]
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Region::ChatLog => "chat-log",
            Region::ChatInput => "chat-input",
            Region::StreamingIndicator => "streaming-indicator",
            Region::StatusBar => "status-bar",
            Region::Minimap => "minimap",
            Region::Sidebar => "sidebar",
            Region::AuditPopup => "audit-popup",
            Region::Autocomplete => "autocomplete",
            Region::FloatingSurfaces => "floating-surfaces",
        };
        f.write_str(name)
    }
}

/// The rects a draw function is given for one frame.
///
/// Most regions need only `area` — the rect they paint into. The chat
/// log needs a second: it paints into a sub-rect of the content area
/// but registers its mouse selection against the gutter-excluded
/// content area, and whether that rect exists at all depends on which
/// column holds focus. `select` carries it; `None` means the slice
/// should register no selection for this frame.
#[derive(Debug, Clone, Copy)]
pub struct DrawTarget {
    /// The rect this region paints into.
    pub area: Rect,
    /// The rect to register as a mouse-selectable region, when one
    /// applies to this frame.
    pub select: Option<Rect>,
}

impl DrawTarget {
    /// A target for a region that paints into `area` and needs no
    /// enclosing rect.
    #[must_use]
    pub fn new(area: Rect) -> Self {
        Self { area, select: None }
    }

    /// A target for a region that also registers `select` as its
    /// mouse-selectable rect.
    #[must_use]
    pub fn with_select(area: Rect, select: Option<Rect>) -> Self {
        Self { area, select }
    }
}

/// What a draw function may read about the frame it paints.
///
/// The draw registry is a `'static` cell, so it cannot be keyed on a
/// borrowed context type. It is keyed on the *state* type `S` instead,
/// which is `'static`, and the per-frame context reaches the draw
/// function as `&dyn DrawContext<S>` — a trait object with no lifetime
/// of its own.
///
/// `S` is the application state (`AppState` in this workspace). Naming
/// it as a parameter is what keeps this crate free of `jinn-kernel`.
pub trait DrawContext<S>: Send + Sync {
    /// The application state for this frame, read-only.
    fn state(&self) -> &S;

    /// The slice registry, for a draw function that resolves a cell
    /// another slice minted.
    fn slices(&self) -> &Slices;

    /// The overlay-view registry, for a draw function that paints a
    /// slice overlay inline.
    fn overlay_views(&self) -> &OverlayViews<RenderFacts>;

    /// The live configuration layer, read at the point of use.
    fn config(&self) -> &ConfigLayer;
}

/// A slice-registered draw function: paints one region for one frame.
///
/// `rects` is the frame's selectable-region accumulator. A draw
/// function pushes a rect when the region it paints supports mouse
/// selection — this is why the accumulator is a parameter rather than
/// a field on the context: the accumulator is composition's, and the
/// decision is the slice's.
pub type DrawFn<S> = Arc<
    dyn Fn(&mut Frame<'_>, DrawTarget, &dyn DrawContext<S>, &mut Vec<Rect>) + Send + Sync + 'static,
>;

/// The frame a draw function paints into, plus the render context.
///
/// Wrapping `ratatui::Frame` behind a type alias keeps the draw
/// function's signature readable and gives the trait bound one place
/// to live.
pub type SliceFrame<'f> = ratatui::Frame<'f>;

/// A draw function wrapped for `Debug` (closures are not `Debug`).
#[derive(Clone)]
struct DrawEntry<S: 'static>(DrawFn<S>);

impl<S: 'static> fmt::Debug for DrawEntry<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DrawFn(..)")
    }
}

/// The registry of slice-registered draw functions, keyed by region.
///
/// The collection is the cell payload and nothing more: the cell that
/// holds it supplies the lock, so this type carries no interior
/// mutability of its own. Callers reach it through
/// [`Slices::register_render_slot`](crate::Slices::register_render_slot)
/// and [`Slices::draw_for`](crate::Slices::draw_for) rather than
/// holding a handle to it.
#[derive(Debug)]
pub struct RenderSlots<S: 'static> {
    slots: HashMap<Region, DrawEntry<S>>,
}

// Hand-written rather than derived: a derive would demand `S: Default`,
// and the payload's element type is what is empty, not the state type.
impl<S: 'static> Default for RenderSlots<S> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }
}

impl<S: 'static> RenderSlots<S> {
    /// Registers the draw function for `region`, replacing any previous
    /// one. Called once per region at slice activation; a slice that
    /// claims no region registers nothing.
    pub(crate) fn register(&mut self, region: Region, draw: DrawFn<S>) {
        self.slots.insert(region, DrawEntry(draw));
    }

    /// Returns the draw function registered for `region`, if any.
    #[must_use]
    pub(crate) fn draw(&self, region: Region) -> Option<DrawFn<S>> {
        self.slots.get(&region).map(|entry| entry.0.clone())
    }

    /// The regions that have a draw function, sorted for stable display.
    #[must_use]
    pub(crate) fn registered(&self) -> Vec<Region> {
        let mut regions: Vec<Region> = self.slots.keys().copied().collect();
        regions.sort();
        regions
    }
}

#[cfg(test)]
mod tests {
    use super::DrawContext;
    use super::Region;
    use super::RenderSlots;

    #[rstest::rstest]
    #[test]
    fn register_then_resolve_roundtrips_region() {
        // Given an empty draw registry.
        let mut slots = RenderSlots::<u8>::default();

        // When registering a draw function for a region.
        slots.register(
            Region::ChatLog,
            std::sync::Arc::new(|_, _, _ctx: &dyn DrawContext<u8>, _| {}),
        );

        // Then the registry resolves it back for that region.
        assert!(slots.draw(Region::ChatLog).is_some());
        // And an unregistered region resolves nothing.
        assert!(slots.draw(Region::StatusBar).is_none());
    }

    #[rstest::rstest]
    #[test]
    fn reregister_replaces_the_previous_draw_fn() {
        // Given a registry with one draw function for a region.
        let mut slots = RenderSlots::<u8>::default();
        slots.register(Region::ChatLog, std::sync::Arc::new(|_, _, _, _| {}));
        slots.register(Region::ChatLog, std::sync::Arc::new(|_, _, _, _| {}));

        // When enumerating the registered regions.
        let registered = slots.registered();

        // Then the region appears once, not twice.
        assert_eq!(registered, vec![Region::ChatLog]);
    }

    #[rstest::rstest]
    #[test]
    fn every_region_has_a_distinct_display_name() {
        // Given every region the layout fills.
        let regions = Region::all();

        // When rendering each to its display name.
        let names: Vec<String> = regions.iter().map(ToString::to_string).collect();

        // Then the names are unique, so a diagnostic naming one region
        // never names two.
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "duplicate region name: {names:?}"
        );
    }

    #[rstest::rstest]
    #[test]
    fn empty_registry_reports_no_registered_regions() {
        // Given an empty draw registry.
        let slots = RenderSlots::<u8>::default();

        // When enumerating the registered regions.
        let registered = slots.registered();

        // Then nothing is registered.
        assert_eq!(registered, [] as [Region; 0]);
    }
}
