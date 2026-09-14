//! Transient preview-scroll storage, shared by all spec-driven pickers.
//!
//! One map keyed by [`PickerId`] replaces per-picker scroll fields on the
//! kernel's picker states. Scroll state is transient UI state — the ideal
//! map citizen. Access flows through [`crate::host::PickerHost`]'s scroll
//! accessors so the crate never reaches into kernel storage directly.

use std::collections::HashMap;

use crate::id::PickerId;

/// Preview scroll offsets for every picker that has scrolled its preview.
#[derive(Debug, Default, Clone)]
pub struct PickerScrolls(HashMap<PickerId, usize>);

impl PickerScrolls {
    /// The stored scroll offset for `id`, or 0 when it has never scrolled.
    #[must_use]
    pub fn get(&self, id: PickerId) -> usize {
        self.0.get(&id).copied().unwrap_or_default()
    }

    /// Stores the scroll offset for `id`.
    pub fn set(&mut self, id: PickerId, scroll: usize) {
        self.0.insert(id, scroll);
    }

    /// Clears the stored offset for `id` (next read returns 0).
    pub fn reset(&mut self, id: PickerId) {
        self.0.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_scrolls_default_to_zero() {
        // Given fresh scroll storage.
        let scrolls = PickerScrolls::default();

        // When reading a picker that never scrolled.
        // Then the offset is zero.
        assert_eq!(scrolls.get(PickerId::new("skill")), 0);
    }

    #[test]
    fn set_stores_and_reset_clears_scroll() {
        // Given storage holding a scroll for "skill".
        let mut scrolls = PickerScrolls::default();
        let skill = PickerId::new("skill");
        scrolls.set(skill, 42);

        // When resetting it.
        scrolls.reset(skill);

        // Then the offset reads zero again.
        assert_eq!(scrolls.get(skill), 0);
    }
}
