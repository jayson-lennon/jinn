//! The term slice's dynamic scope ids.
//!
//! The overlay's two scopes, minted as data (no central enum edits):
//! `term:view` — watching a terminal; every key is a slice action. And
//! `term:control` — the user holds the pty; every key except the
//! configured control-toggle forwards via the slice's key hook. Both
//! are navigation scopes (they never capture *text* input; capture mode
//! is the key hook's domain, not the typing carve-out's).

use jinn_slices::slice_scope::SliceScopeId;

/// The term slice's identifier in scope ids and route-row metadata.
pub const SLICE_NAME: &str = "term";

/// The overlay view scope (`term:view`).
#[must_use]
pub fn view_scope() -> SliceScopeId {
    SliceScopeId::navigation(SLICE_NAME, "view")
}

/// The capture scope (`term:control`).
#[must_use]
pub fn control_scope() -> SliceScopeId {
    SliceScopeId::navigation(SLICE_NAME, "control")
}

/// Whether `scope` is one of the overlay's scopes (view or control).
#[must_use]
pub fn is_overlay_scope(scope: &SliceScopeId) -> bool {
    scope.slice() == SLICE_NAME && (scope.name() == "view" || scope.name() == "control")
}

#[cfg(test)]
mod tests {
    use super::{control_scope, is_overlay_scope, view_scope};

    #[rstest::rstest]
    #[test]
    fn overlay_scopes_are_navigation_scopes() {
        // Given both overlay scopes.
        // When checking their input-capture flag.
        // Then neither captures text input — capture mode belongs to the
        // key hook, not the typing carve-out.
        assert!(!view_scope().captures_input());
        assert!(!control_scope().captures_input());
    }

    #[rstest::rstest]
    #[test]
    fn is_overlay_scope_accepts_only_the_overlay_scopes() {
        // Given the two overlay scopes and foreign ones.
        // When testing membership.
        // Then only the overlay scopes match — including a foreign slice
        // reusing the same scope names.
        assert!(is_overlay_scope(&view_scope()));
        assert!(is_overlay_scope(&control_scope()));
        assert!(!is_overlay_scope(&jinn_slices::SliceScopeId::new(
            "quake-bar", "open"
        )));
        assert!(!is_overlay_scope(&jinn_slices::SliceScopeId::navigation(
            "other-slice", "view"
        )));
    }
}
