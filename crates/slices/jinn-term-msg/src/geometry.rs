//! Terminal overlay geometry.
//!
//! The overlay is a bordered, centered rect inset from the frame by a
//! generous padding (4 columns / 3 rows per side). [`terminal_overlay_rect`]
//! is the bordered block the renderer draws (Clear + border, then the screen
//! inside); [`terminal_overlay_inner_rect`] is the block's interior — the pty
//! size in `(rows, cols)` terms, so a program lays out for exactly the
//! visible grid (WYSIWYG). The border ring lives between the two and is
//! never painted with program cells.

use ratatui::layout::Rect;

/// Horizontal padding between the overlay and the frame edge, per side.
pub const HORIZONTAL_PADDING: u16 = 4;
/// Vertical padding between the overlay and the frame edge, per side.
pub const VERTICAL_PADDING: u16 = 3;

/// Computes the overlay's bordered rect: `area` inset by the padding.
///
/// This is the full block the renderer draws — border ring included. Always
/// at least 1×1 so the renderer has somewhere to draw on tiny terminals.
#[must_use]
pub fn terminal_overlay_rect(area: Rect) -> Rect {
    let width = area.width.saturating_sub(HORIZONTAL_PADDING * 2).max(1);
    let height = area.height.saturating_sub(VERTICAL_PADDING * 2).max(1);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Computes the overlay's inner rect — the pty size in `(rows, cols)` terms.
///
/// Subtracts the block border (1 cell each side) from the bordered rect;
/// the program's grid fills the interior exactly.
#[must_use]
pub fn terminal_overlay_inner_rect(area: Rect) -> Rect {
    let overlay = terminal_overlay_rect(area);
    let width = overlay.width.saturating_sub(2).max(1);
    let height = overlay.height.saturating_sub(2).max(1);
    Rect {
        x: overlay.x + 1,
        y: overlay.y + 1,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    fn overlay_rect_is_centered_with_padding() {
        // Given an 80×24 frame.
        let area = Rect::new(0, 0, 80, 24);

        // When computing the overlay rect.
        let rect = terminal_overlay_rect(area);

        // Then it is inset by the padding (4 cols / 3 rows each side).
        assert_eq!(rect.x, 4);
        assert_eq!(rect.y, 3);
        assert_eq!(rect.width, 72);
        assert_eq!(rect.height, 18);
    }

    #[rstest::rstest]
    fn inner_rect_subtracts_the_border_ring() {
        // Given an 80×24 frame.
        let area = Rect::new(0, 0, 80, 24);

        // When computing the pty-sized inner rect.
        let inner = terminal_overlay_inner_rect(area);

        // Then it is the bordered overlay minus the border ring only — the
        // program's grid fills the interior exactly (WYSIWYG).
        assert_eq!(inner.x, 5);
        assert_eq!(inner.y, 4);
        assert_eq!(inner.width, 70);
        assert_eq!(inner.height, 16);
    }

    #[rstest::rstest]
    fn tiny_frames_stay_in_bounds() {
        // Given a 1×1 frame.
        let area = Rect::new(0, 0, 1, 1);

        // When computing the overlay rect.
        let rect = terminal_overlay_rect(area);

        // Then it stays inside the frame.
        assert_eq!(rect.width, 1);
        assert_eq!(rect.height, 1);
        assert!(rect.x + rect.width <= area.width);
        assert!(rect.y + rect.height <= area.height);
    }
}
