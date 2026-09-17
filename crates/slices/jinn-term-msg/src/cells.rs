//! Renderable terminal screen cells (the emulator's output grid).
//!
//! Plain serde data: the emulator (in the term slice crate) converts
//! vt100's parser state into these; renderers convert them into their
//! own cell types.

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScreenCells {
    /// Grid height in rows.
    pub rows: u16,
    /// Grid width in columns.
    pub cols: u16,
    /// Row-major cell grid, `rows * cols` entries.
    pub cells: Vec<TermCell>,
}

impl ScreenCells {
    /// The cell at `(row, col)`, or `None` when out of bounds.
    #[must_use]
    pub fn get(&self, row: u16, col: u16) -> Option<&TermCell> {
        let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
        self.cells.get(idx)
    }
}

/// One renderable cell of the terminal screen.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TermCell {
    /// The default cell (no contents, no styling).
    Blank,
    /// The right half of a double-width character.
    WideSpacer,
    /// A cell with content and optional styling.
    Styled {
        /// The character to draw.
        ch: char,
        /// Foreground/background/attributes.
        style: CellStyle,
    },
}

/// Foreground, background, and attribute styling of a cell.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct CellStyle {
    /// Foreground color.
    pub fg: TermColor,
    /// Background color.
    pub bg: TermColor,
    /// Bold attribute.
    pub bold: bool,
    /// Italic attribute.
    pub italic: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Inverse attribute.
    pub inverse: bool,
}

/// Terminal color, normalized from vt100's palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TermColor {
    /// Terminal default foreground/background.
    #[default]
    Default,
    /// One of the 256-color palette entries.
    Idx(u8),
    /// A direct RGB color.
    Rgb(u8, u8, u8),
}
