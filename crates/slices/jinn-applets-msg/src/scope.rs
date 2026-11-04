//! The applets picker's identity: its dynamic scope and its cell slot key.
//!
//! Lives in the `-msg` crate beside the state it names, like every
//! sibling picker: any slice can ask "is the applets picker open?" or
//! resolve the cell without depending on the implementation crate.

use jinn_slices::SliceScopeId;
use jinn_slices::SlotKey;

/// How many rows the result pane measured, set by the render pass.
///
/// Paging needs a real row count: `SelectionState`'s `max_visible`
/// argument decides whether the scroll window follows the cursor, so a
/// constant would let the highlight walk off-screen.
pub const RESULTS_VIEWPORT_FALLBACK: usize = 20;

/// The applets picker's dynamic scope (input-capturing: the filter is text).
#[must_use]
pub fn applets_picker_scope() -> SliceScopeId {
    SliceScopeId::new("applets", "picker")
}

/// The applets picker's cell key, minted by the shared cell catalog.
#[must_use]
pub fn applets_picker_slot() -> SlotKey {
    SlotKey::builtin("applets", "picker")
}

/// The saved-app catalog's cell key: the in-memory authority the picker
/// reads and the store actor republishes into.
#[must_use]
pub fn applets_catalog_slot() -> SlotKey {
    SlotKey::builtin("applets", "catalog")
}

/// The rename popup's dynamic scope (input-capturing), pushed on
/// `<c-r>` from the picker — the session-rename shape.
#[must_use]
pub fn applets_rename_scope() -> SliceScopeId {
    SliceScopeId::new("applets", "rename")
}

#[cfg(test)]
mod tests {
    use super::{
        applets_catalog_slot, applets_picker_scope, applets_picker_slot, applets_rename_scope,
    };

    #[rstest::rstest]
    #[test]
    fn the_picker_slot_and_scope_are_stable() {
        // Given the picker's identity accessors.
        // When reading the slot and scope twice.
        // Then both are stable values.
        assert_eq!(applets_picker_slot(), applets_picker_slot());
        assert_eq!(applets_picker_scope(), applets_picker_scope());
    }

    #[rstest::rstest]
    #[test]
    fn the_catalog_slot_and_rename_scope_are_stable() {
        // Given the catalog cell's key and the rename popup's scope.
        // When reading each twice.
        // Then both are stable values.
        assert_eq!(applets_catalog_slot(), applets_catalog_slot());
        assert_eq!(applets_rename_scope(), applets_rename_scope());
    }
}
