//! The slice registry — dynamic, per-slice typed storage.
//!
//! Keys are dynamic strings ([`SlotKey`]), not an enum of known features.
//! [`Slices::register`] mints the one write handle ([`TypedCell`]) per
//! slot; everyone else resolves read handles. The registry is cheap to
//! clone and every clone shares the same cells.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::slice_scope::SliceScopeId;

/// Uniquely addresses one slice cell.
///
/// `namespace` separates built-ins from plugin contributions
/// (`builtin` vs the plugin's name); `name` is the feature-chosen slice
/// name; `version` lets a slice payload evolve under a new key instead
/// of migrating in place.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotKey {
    /// Namespace owning the slot: `builtin` or a plugin name.
    namespace: String,
    /// Feature-chosen slice name, e.g. `status`.
    name: String,
    /// Payload schema version; bump to replace rather than mutate.
    version: u32,
}

impl SlotKey {
    /// A key for a built-in feature slice.
    #[must_use]
    pub fn builtin(namespace: &str, name: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            version: 1,
        }
    }

    /// A key for a plugin-contributed slice.
    ///
    /// Guest slices are namespaced by plugin name so two plugins can
    /// never collide with each other or with built-ins.
    #[must_use]
    pub fn plugin(plugin_name: &str, name: &str) -> Self {
        Self {
            namespace: plugin_name.to_owned(),
            name: name.to_owned(),
            version: 1,
        }
    }

    /// The namespace component.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The slice-name component.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The payload schema version.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }
}

impl fmt::Display for SlotKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@v{}", self.namespace, self.name, self.version)
    }
}

/// Error returned by [`Slices::register`] when the slot already exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotTaken {
    /// The contested slot key.
    pub key: SlotKey,
}

impl fmt::Display for SlotTaken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slot already registered: {}", self.key)
    }
}

impl std::error::Error for SlotTaken {}

/// Dynamic registry of per-slice typed cells.
///
/// Cheap to clone: every clone shares the same cells, so a handle
/// obtained before cloning still observes updates made through the
/// clone's registry (and vice versa). Cloning a `Slices` does **not**
/// mint new write capabilities — [`TypedCell`]s are minted only by
/// [`register`](Self::register).
#[derive(Clone, Debug, Default)]
pub struct Slices {
    cells: Arc<RwLock<HashMap<SlotKey, SlotEntry>>>,
    /// Ordered tab scope ids, appended by slice activation. Drives the
    /// `<Tab>` cycle and the tab bar; a slice nobody registered is
    /// simply absent.
    tab_scopes: Arc<RwLock<Vec<SliceScopeId>>>,
    /// Tab scope → the slot backing that tab's content.
    tab_slots: Arc<RwLock<HashMap<SliceScopeId, SlotKey>>>,
    /// Overlay geometry functions keyed by the scope that owns the
    /// overlay, appended by slice activation. The render pass consults
    /// these instead of branching on hard-coded scope variants.
    overlays: Arc<RwLock<HashMap<SliceScopeId, OverlayEntry>>>,
    /// Overlay scope → the slot backing the overlay's content, so the
    /// render pass can resolve the scope's view through the viewport.
    overlay_slots: Arc<RwLock<HashMap<SliceScopeId, SlotKey>>>,
    /// Overlay scopes whose rect registers as a selectable region.
    overlay_selectable: Arc<RwLock<HashMap<SliceScopeId, bool>>>,
    /// Slice feature flags, set at activation from the slice's own
    /// config section. Read-model for gating decisions (e.g. a route
    /// action asking "is this slice enabled?").
    flags: Arc<RwLock<HashMap<String, bool>>>,
}

/// A slice-registered overlay geometry function: resolves the screen
/// rect its overlay occupies for the given frame area, or `None` when
/// the overlay shouldn't render.
pub type OverlayFn =
    Arc<dyn Fn(&ratatui::layout::Rect) -> Option<ratatui::layout::Rect> + Send + Sync>;

/// An overlay function wrapped for `Debug` (closures are not `Debug`).
#[derive(Clone)]
struct OverlayEntry(OverlayFn);

impl std::fmt::Debug for OverlayEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OverlayFn(..)")
    }
}

/// One registered slot: the erased cell plus its payload's type name.
///
/// The type name exists for diagnostics — the startup view/slot pairing
/// check reports "slot X holds `Y`, expected `Z`" instead of a bare
/// "unregistered" for what is actually a wiring type error.
#[derive(Debug)]
struct SlotEntry {
    cell: Arc<dyn Any + Send + Sync>,
    type_name: &'static str,
}

impl Slices {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `initial` under `key` and mints the one write handle.
    ///
    /// # Errors
    ///
    /// Returns [`SlotTaken`] if the key is already registered. The
    /// original cell is untouched and its handle remains valid.
    pub fn register<T>(
        &self,
        key: SlotKey,
        initial: T,
    ) -> Result<crate::cell::TypedCell<T>, SlotTaken>
    where
        T: Any + Send + Sync,
    {
        let cell = crate::cell::TypedCell::new(initial);
        let mut cells = self.cells.write();
        if cells.contains_key(&key) {
            return Err(SlotTaken { key });
        }
        let entry = SlotEntry {
            cell: Arc::new(cell.clone()),
            type_name: std::any::type_name::<T>(),
        };
        cells.insert(key, entry);
        Ok(cell)
    }

    /// Returns a read handle to the cell registered under `key`, if its
    /// payload type is `T`.
    ///
    /// `None` means unregistered **or** type-mismatched — callers at
    /// startup should treat the mismatch as a wiring bug and fail fast
    /// (the [`view::Viewport`](crate::view::Viewport) does exactly that).
    #[must_use]
    pub fn reader<T>(&self, key: &SlotKey) -> Option<crate::cell::TypedCell<T>>
    where
        T: Any + Send + Sync,
    {
        let cells = self.cells.read();
        let entry = cells.get(key)?;
        let cell = entry
            .cell
            .clone()
            .downcast::<crate::cell::TypedCell<T>>()
            .ok()?;
        Some((*cell).clone())
    }

    /// Returns the registered payload's type name for `key`, if present.
    ///
    /// Diagnostic support for the view/slot pairing check: distinguishes
    /// "slot missing" from "slot holds a different payload type".
    #[must_use]
    pub fn slot_type(&self, key: &SlotKey) -> Option<&'static str> {
        let cells = self.cells.read();
        Some(cells.get(key)?.type_name)
    }

    /// Enumerates every registered slot, sorted for stable display.
    ///
    /// Backs the dashboard's dynamic slice list and the future canvas
    /// export; a slice nobody registered simply doesn't appear.
    #[must_use]
    pub fn slots(&self) -> Vec<SlotKey> {
        let cells = self.cells.read();
        let mut keys: Vec<SlotKey> = cells.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Registers `scope` as a tab, appended in tab order.
    ///
    /// Called once per tab-like slice at activation, together with the
    /// slot that backs the tab's content. Duplicate scope registration
    /// is a no-op (a tab must not appear twice in the cycle); the slot
    /// mapping is overwritten (last activation wins).
    /// Sets a slice feature flag. Called at activation from the
    /// slice's config-section value.
    pub fn set_flag(&self, slice: &str, enabled: bool) {
        self.flags.write().insert(slice.to_owned(), enabled);
    }

    /// Reads a slice feature flag. Unset flags are `false`.
    #[must_use]
    pub fn flag(&self, slice: &str) -> bool {
        self.flags.read().get(slice).copied().unwrap_or(false)
    }

    pub fn register_tab_scope(&self, scope: SliceScopeId, slot: SlotKey) {
        {
            let mut tabs = self.tab_scopes.write();
            if !tabs.contains(&scope) {
                tabs.push(scope.clone());
            }
        }
        self.tab_slots.write().insert(scope, slot);
    }

    /// The registered tab scopes, in tab order.
    #[must_use]
    pub fn tab_scopes(&self) -> Vec<SliceScopeId> {
        self.tab_scopes.read().clone()
    }

    /// The slot backing a tab's content, if `scope` is a registered tab.
    #[must_use]
    pub fn tab_slot(&self, scope: &SliceScopeId) -> Option<SlotKey> {
        self.tab_slots.read().get(scope).cloned()
    }

    /// Registers the overlay geometry function for `scope`.
    ///
    /// Called once per overlay-like slice at activation; re-registration
    /// overwrites (last activation wins).
    pub fn register_overlay(&self, scope: SliceScopeId, overlay: OverlayFn) {
        self.overlays.write().insert(scope, OverlayEntry(overlay));
    }

    /// Registers the slot backing `scope`'s overlay content.
    ///
    /// Called by overlay slices at activation; the render pass pairs the
    /// scope's geometry fn with this slot's view.
    pub fn register_overlay_slot(&self, scope: SliceScopeId, slot: SlotKey) {
        self.overlay_slots.write().insert(scope, slot);
    }

    /// Returns the slot backing `scope`'s overlay content, if any.
    #[must_use]
    pub fn overlay_slot(&self, scope: &SliceScopeId) -> Option<SlotKey> {
        self.overlay_slots.read().get(scope).cloned()
    }

    /// Returns the overlay geometry function registered for `scope`, if any.
    #[must_use]
    pub fn overlay(&self, scope: &SliceScopeId) -> Option<OverlayFn> {
        self.overlays.read().get(scope).map(|e| e.0.clone())
    }

    /// Returns whether `scope`'s overlay rect registers as a selectable
    /// region. Popups that render focusable content opt in at activation
    /// (`register_overlay_selectable`); drawers default to `false`.
    #[must_use]
    pub fn overlay_selectable(&self, scope: &SliceScopeId) -> bool {
        self.overlay_selectable
            .read()
            .get(scope)
            .copied()
            .unwrap_or(false)
    }

    /// Marks `scope`'s overlay rect as selectable.
    pub fn register_overlay_selectable(&self, scope: &SliceScopeId) {
        self.overlay_selectable.write().insert(scope.clone(), true);
    }
}

#[cfg(test)]
mod tests {
    use super::Slices;
    use super::SlotKey;

    #[derive(Debug, Default)]
    struct Payload {
        value: u32,
    }

    #[rstest::rstest]
    #[test]
    fn register_duplicate_slot_fails() {
        // Given a registry with a registered slot.
        let slices = Slices::new();
        let key = SlotKey::builtin("test", "dup");
        let first = slices
            .register(key.clone(), Payload::default())
            .expect("first register");

        // When registering the same slot again.
        let result = slices.register(key.clone(), Payload::default());

        // Then registration fails with SlotTaken.
        assert_eq!(result.unwrap_err().key, key);
        // And the original handle still updates the original cell.
        first.update(|p| {
            p.value = 7;
        });
        assert_eq!(first.read().value, 7);
    }

    #[rstest::rstest]
    #[test]
    fn reader_observes_owner_update() {
        // Given a registered cell and a reader handle for the same slot.
        let slices = Slices::new();
        let key = SlotKey::builtin("test", "observe");
        let owner = slices
            .register(key.clone(), Payload::default())
            .expect("register");
        let reader = slices.reader::<Payload>(&key).expect("reader resolves");

        // When the owner updates the cell.
        owner.update(|p| {
            p.value = 42;
        });

        // Then the reader observes the update.
        assert_eq!(reader.read().value, 42);
    }

    #[rstest::rstest]
    #[test]
    fn slots_enumerates_registered_keys() {
        // Given a registry with two registered slots.
        let slices = Slices::new();
        let a = SlotKey::builtin("a", "one");
        let b = SlotKey::plugin("plug", "two");
        let _ = slices
            .register(a.clone(), Payload::default())
            .expect("register a");
        let _ = slices
            .register(b.clone(), Payload::default())
            .expect("register b");

        // When enumerating slots.
        let slots = slices.slots();

        // Then both keys are present, sorted.
        assert_eq!(slots, vec![a, b]);
    }

    #[rstest::rstest]
    #[test]
    fn reader_type_mismatch_resolves_none() {
        // Given a slot registered with `Payload`.
        let slices = Slices::new();
        let key = SlotKey::builtin("test", "typed");
        let _ = slices
            .register(key.clone(), Payload::default())
            .expect("register");

        // When resolving the slot as a different type.
        let wrong = slices.reader::<Vec<u8>>(&key);

        // Then resolution fails (the wiring bug surface).
        assert!(wrong.is_none());
        // And the slot's registered type is reported for diagnostics.
        assert_eq!(
            slices.slot_type(&key),
            Some(std::any::type_name::<Payload>())
        );
    }
}
