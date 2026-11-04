//! The apps picker's identity: its dynamic scope and its cell slot key.
//!
//! Lives in the `-msg` crate beside the state it names, like every
//! sibling picker: any slice can ask "is the apps picker open?" or
//! resolve the cell without depending on the implementation crate.

use jinn_slices::SliceScopeId;
use jinn_slices::SlotKey;

/// How many rows the result pane measured, set by the render pass.
///
/// Paging needs a real row count: `SelectionState`'s `max_visible`
/// argument decides whether the scroll window follows the cursor, so a
/// constant would let the highlight walk off-screen.
pub const RESULTS_VIEWPORT_FALLBACK: usize = 20;

/// The apps picker's dynamic scope (input-capturing: the filter is text).
#[must_use]
pub fn apps_picker_scope() -> SliceScopeId {
    SliceScopeId::new("apps", "picker")
}

/// The apps picker's cell key, minted by the shared cell catalog.
#[must_use]
pub fn apps_picker_slot() -> SlotKey {
    SlotKey::builtin("apps", "picker")
}

/// The saved-app catalog's cell key: the in-memory authority the picker
/// reads and the store actor republishes into.
#[must_use]
pub fn apps_catalog_slot() -> SlotKey {
    SlotKey::builtin("apps", "catalog")
}

/// The rename popup's dynamic scope (input-capturing), pushed on
/// `<c-r>` from the picker — the session-rename shape.
#[must_use]
pub fn apps_rename_scope() -> SliceScopeId {
    SliceScopeId::new("apps", "rename")
}

#[cfg(test)]
mod tests {
    use super::{apps_catalog_slot, apps_picker_scope, apps_picker_slot, apps_rename_scope};

    #[rstest::rstest]
    #[test]
    fn the_picker_slot_and_scope_are_stable() {
        // Given the picker's identity accessors.
        // When reading the slot and scope twice.
        // Then both are stable values.
        assert_eq!(apps_picker_slot(), apps_picker_slot());
        assert_eq!(apps_picker_scope(), apps_picker_scope());
    }

    #[rstest::rstest]
    #[test]
    fn the_catalog_slot_and_rename_scope_are_stable() {
        // Given the catalog cell's key and the rename popup's scope.
        // When reading each twice.
        // Then both are stable values.
        assert_eq!(apps_catalog_slot(), apps_catalog_slot());
        assert_eq!(apps_rename_scope(), apps_rename_scope());
    }
}
