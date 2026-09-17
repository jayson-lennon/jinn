//! Terminal emulation — bytes in, rendered screen out.
//!
//! Wraps a [`vt100::Parser`] (the in-process terminal emulator). Raw pty
//! output is fed to [`Emulator::feed`]; the parsed screen is read back as
//! plain text (for tool results) or styled cells (for the takeover view).
//!
//! The scrollback transcript is jinn's own ring, not vt100's scrollback: the
//! transcript must capture observed screen states (for the kill result's
//! tail), which [`Emulator::sync_transcript`] appends at settle time. Capping
//! the ring keeps memory bounded for chatty programs.

use std::collections::VecDeque;

pub use jinn_term_msg::cells::{CellStyle, ScreenCells, TermCell, TermColor};
use vt100::Parser;

/// Smallest emulator grid vt100 0.15 can hold without panicking. Audit of
/// every `u16` subtraction in its grid: unguarded `rows - 1` (in `Grid::new`,
/// `clear`, `set_size`, `set_scroll_region`) needs rows >= 2, and
/// `col_wrap`'s `prev_pos.row -= scrolled` underflows only on a 1-row grid
/// (a scroll implies the cursor sat at `scroll_bottom`, which a scroll region
/// keeps >= 1). All remaining sites are guarded or need cols >= width <= 2.
const EMULATOR_MIN_ROWS: u16 = 2;
/// See [`EMULATOR_MIN_ROWS`]: unguarded `cols - width` with wide (2-cell)
/// writes needs cols >= 2.
const EMULATOR_MIN_COLS: u16 = 2;

/// How many scrollback rows vt100's grid retains.
const EMULATOR_SCROLLBACK_ROWS: usize = 1000;

/// Default transcript line cap; the kill result reports only the tail.
const DEFAULT_TRANSCRIPT_LINES: usize = 500;

/// In-process terminal emulator over a byte stream.
///
/// Owns the parser and the append-only transcript. All methods take `&mut
/// self` for feeding but `&self` for reads, so a snapshot can be taken from a
/// shared handle.
pub struct Emulator {
    parser: Parser,
    transcript: VecDeque<String>,
    transcript_cap: usize,
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new(24, 80, DEFAULT_TRANSCRIPT_LINES)
    }
}

impl Emulator {
    /// Creates an emulator of `rows`×`cols` cells with a transcript capped at
    /// `transcript_cap` lines.
    ///
    /// Dimensions are clamped to a 2×2 floor: vt100's grid arithmetic
    /// (`rows - 1` in `Grid::new`, `set_size`, `scroll_up`,
    /// `set_scroll_region`) panics on a 0-row terminal, and its wrap logic
    /// (`col_wrap`'s `prev_pos.row -= scrolled`) underflows on any 1-row
    /// grid whose content wraps. A zeroed size from upstream must never
    /// reach it.
    #[must_use]
    pub fn new(rows: u16, cols: u16, transcript_cap: usize) -> Self {
        Self {
            parser: Parser::new(
                rows.max(EMULATOR_MIN_ROWS),
                cols.max(EMULATOR_MIN_COLS),
                EMULATOR_SCROLLBACK_ROWS,
            ),
            transcript: VecDeque::with_capacity(transcript_cap.min(64)),
            transcript_cap: transcript_cap.max(1),
        }
    }

    /// Feeds raw pty output into the emulator, updating screen and transcript.
    ///
    /// HVP (`CSI … f`) is rewritten to CUP (`CSI … H`) first: vt100 0.15
    /// implements CUP but silently drops HVP, and programs like btop position
    /// every frame with the `f` form — unhandled, all output becomes one
    /// linear wrapping stream and the screen turns to soup. The two sequences
    /// are semantically identical (row;col, 1-based), so translation is lossless.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(&normalize_hvp(bytes));
    }
    /// Snapshots the visible screen into the transcript ring.
    ///
    /// Called at settle time: each settle appends the latest screen state,
    /// so the transcript reads as a sequence of observed screens. Non-empty
    /// trailing duplicate screens (repaints with no change) are skipped.
    pub fn sync_transcript(&mut self) {
        let text = self.parser.screen().contents();
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            return;
        }
        if self.transcript.back().is_some_and(|last| last == trimmed) {
            return;
        }
        if self.transcript.len() >= self.transcript_cap {
            self.transcript.pop_front();
        }
        self.transcript.push_back(trimmed.to_owned());
    }

    /// The rendered screen as plain text with trailing blank columns/rows
    /// stripped per line and overall trailing emptiness trimmed.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let (rows, cols) = self.parser.screen().size();
        let screen = self.parser.screen();
        let mut lines = Vec::with_capacity(usize::from(rows));
        for row in screen.rows(0, cols) {
            lines.push(row.trim_end().to_owned());
        }
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// The styled cells of the visible screen for the takeover renderer.
    ///
    /// Returns `(rows, cols, cells)` where cells are laid out row-major.
    /// Wide characters occupy their leading cell; their continuation cell is
    /// [`TermCell::WideSpacer`].
    #[must_use]
    pub fn cells(&self) -> ScreenCells {
        let (rows, cols) = self.parser.screen().size();
        let screen = self.parser.screen();
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    cells.push(TermCell::Blank);
                    continue;
                };
                if cell.is_wide_continuation() {
                    cells.push(TermCell::WideSpacer);
                    continue;
                }
                let style = CellStyle {
                    fg: term_color(cell.fgcolor()),
                    bg: term_color(cell.bgcolor()),
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                };
                if !cell.has_contents() {
                    cells.push(TermCell::Styled { ch: ' ', style });
                    continue;
                }
                let text = cell.contents();
                // A wide cell's `contents()` yields the full grapheme; render
                // its first char (ratatui handles the width when drawing).
                let ch = text.chars().next().unwrap_or(' ');
                cells.push(TermCell::Styled { ch, style });
            }
        }
        ScreenCells { rows, cols, cells }
    }

    /// The cursor position as `(row, col)`.
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Whether the program hid the cursor (fullscreen apps during repaints).
    #[must_use]
    pub fn cursor_hidden(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    /// The emulator size as `(rows, cols)`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Resizes the emulator grid. Must mirror the pty resize.
    ///
    /// Dimensions are clamped to a 2×2 floor — same vt100 panics as
    /// [`Emulator::new`].
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.parser
            .set_size(rows.max(EMULATOR_MIN_ROWS), cols.max(EMULATOR_MIN_COLS));
    }

    /// The transcript tail — up to `max_lines` most recent screens, joined
    /// with screen separators. Empty when nothing was ever observed.
    #[must_use]
    pub fn transcript_tail(&self, max_lines: usize) -> String {
        if self.transcript.is_empty() {
            return String::new();
        }
        let skip = self.transcript.len().saturating_sub(max_lines);
        self.transcript
            .iter()
            .skip(skip)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n── screen update ──\n")
    }
}

/// Rewrites HVP (`CSI … f`) sequences to the equivalent CUP (`CSI … H`).
///
/// vt100 0.15 handles `H` but not `f` (its `csi_dispatch` has no `'f'` arm),
/// so un-rewritten positioning is dropped entirely and the program's output
/// linear-wraps into garbage. The scanner is byte-level and allocation-light:
/// it only copies through when it actually rewrites a sequence, and it treats
/// a truncated CSI at the chunk boundary as plain data (sequences never split
/// mid-stream in practice because the pump batches by read; a split would at
/// worst degrade to today's behavior for that one sequence).
fn normalize_hvp(bytes: &[u8]) -> Vec<u8> {
    if !bytes.contains(&b'f') {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Find the next CSI introducer: ESC '['.
        let Some(rel) = bytes
            .get(i..)
            .and_then(|rest| rest.iter().position(|&b| b == 0x1b))
        else {
            // No more escapes — copy the tail verbatim.
            if let Some(tail) = bytes.get(i..) {
                out.extend_from_slice(tail);
            }
            break;
        };
        let esc = i + rel;
        if let Some(head) = bytes.get(i..esc) {
            out.extend_from_slice(head);
        }
        if bytes.get(esc + 1) != Some(&b'[') {
            // Not a CSI (OSC, ESC-only, …) — copy the ESC verbatim.
            out.push(0x1b);
            i = esc + 1;
            continue;
        }
        // Scan the CSI body: parameter/intermediate bytes until a
        // final byte (0x40..=0x7E).
        let mut j = esc + 2;
        let final_byte = loop {
            match bytes.get(j) {
                Some(&b) if (0x40..=0x7E).contains(&b) => break b,
                // Parameter (0x30..=0x3F) or intermediate (0x20..=0x2F).
                Some(b) if (0x20..=0x3F).contains(b) => j += 1,
                // Truncated / malformed — treat as end of input.
                _ => break 0,
            }
        };
        let end = if final_byte == 0 { bytes.len() } else { j + 1 };
        // The copies cannot fail: esc <= j/end <= len by construction, so
        // fall back to copying only what is valid (identical on all inputs).
        if final_byte == b'f' {
            // Rewrite the final byte to the CUP form.
            let body = bytes.get(esc..j).unwrap_or_default();
            out.extend_from_slice(body);
            out.push(b'H');
        } else {
            // Any other CSI (or truncated sequence): copy verbatim.
            let seq = bytes.get(esc..end).unwrap_or_default();
            out.extend_from_slice(seq);
        }
        i = end;
    }
    out
}

/// A styled snapshot of the visible screen's cells.

/// Converts vt100's palette color into the wire `TermColor`.
fn term_color(color: vt100::Color) -> TermColor {
    match color {
        vt100::Color::Default => TermColor::Default,
        vt100::Color::Idx(idx) => TermColor::Idx(idx),
        vt100::Color::Rgb(r, g, b) => TermColor::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    #[rstest::rstest]
    fn plain_text_renders_what_was_printed() {
        // Given a fresh emulator.
        let mut emu = Emulator::default();

        // When feeding plain printable output.
        emu.feed(b"hello world\r\nsecond line\r\n");

        // Then the plain text shows both lines without escape noise.
        let text = emu.plain_text();
        assert_eq!(text, "hello world\nsecond line");
    }

    #[rstest::rstest]
    fn ansi_styling_does_not_leak_into_plain_text() {
        // Given an emulator fed SGR-styled output.
        let mut emu = Emulator::default();

        // When feeding red bold text.
        emu.feed(b"\x1b[1;31mALERT\x1b[0m ok\r\n");

        // Then the plain text strips the escapes but keeps the words.
        assert_eq!(emu.plain_text(), "ALERT ok");
        // And the styled cell carries the red bold styling.
        let cells = emu.cells();
        let alert_cell = cells.get(0, 0).expect("cell (0,0)");
        let TermCell::Styled { ch: 'A', style } = alert_cell else {
            panic!("expected styled 'A', got {alert_cell:?}");
        };
        assert!(style.bold, "expected bold");
        assert_eq!(style.fg, TermColor::Idx(1), "expected palette red");
    }

    #[rstest::rstest]
    fn cursor_position_tracks_cursor_moves() {
        // Given an emulator fed a CUP (cursor position) sequence.
        let mut emu = Emulator::default();

        // When the program moves the cursor to row 3, col 5.
        emu.feed(b"\x1b[4;6H");

        // Then the cursor position is reported (0-indexed) as (3, 5).
        assert_eq!(emu.cursor_position(), (3, 5));
    }

    #[rstest::rstest]
    fn hvp_positions_like_cup() {
        // Given an emulator fed an HVP (`CSI … f`) cursor move — the form
        // btop uses for every frame (vt100 0.15 has no `f` arm and would
        // drop it, linear-wrapping the whole screen into soup).
        let mut emu = Emulator::default();

        // When the program moves the cursor to row 3, col 5 via HVP and
        // prints.
        emu.feed(b"\x1b[4;6fX");

        // Then the X lands at the HVP target (0-indexed (3, 5)), exactly as
        // the CUP form would have placed it.
        let text = emu.plain_text();
        assert_eq!(text, "\n\n\n     X", "HVP ignored: {text:?}");
        assert_eq!(emu.cursor_position(), (3, 6));
    }

    #[rstest::rstest]
    fn hvp_frame_renders_as_layout_not_soup() {
        // Given an emulator fed a btop-style frame: HVP-positioned full-width
        // rules and text fragments (the bug-report shape).
        let mut emu = Emulator::default();
        let frame = format!(
            "\x1b[1;1f{}\x1b[9;1f{}\x1b[3;38fCPU 12%\x1b[24;69f0/768",
            "\u{2500}".repeat(78),
            "\u{2500}".repeat(78),
        );

        // When the frame is fed.
        emu.feed(frame.as_bytes());

        // Then each fragment sits on its intended row — not one wrapping line.
        let text = emu.plain_text();
        assert!(text.lines().count() >= 3, "collapsed to soup: {text:?}");
        let row3 = text.lines().nth(2).unwrap_or_default().to_owned();
        assert!(row3.contains("CPU 12%"), "row 3 wrong: {row3:?}");
        assert!(
            !text.contains("\u{2500}CPU"),
            "fragments ran together: {text:?}"
        );
    }

    #[rstest::rstest]
    fn set_size_resizes_the_grid() {
        // Given a default-sized emulator.
        let mut emu = Emulator::default();

        // When resizing to 10x30.
        emu.set_size(10, 30);

        // Then the size reflects the change.
        assert_eq!(emu.size(), (10, 30));
        // And output longer than the new width wraps into the grid.
        emu.feed(b"012345678901234567890123456789X");
        let text = emu.plain_text();
        assert!(text.contains('X'), "wrapped content missing: {text:?}");
    }

    #[rstest::rstest]
    fn wide_character_occupies_two_cells() {
        // Given an emulator fed a double-width character (CJK).
        let mut emu = Emulator::default();

        // When printing '世' (U+4E16, double-width).
        emu.feed("世\n".as_bytes());

        // Then the leading cell holds the character.
        let cells = emu.cells();
        let lead = cells.get(0, 0).expect("cell (0,0)");
        let TermCell::Styled { ch: '世', .. } = lead else {
            panic!("expected wide lead cell, got {lead:?}");
        };
        // And the continuation cell is a wide spacer.
        assert_eq!(cells.get(0, 1), Some(&TermCell::WideSpacer));
    }

    #[rstest::rstest]
    fn zero_size_emulator_does_not_panic() {
        // Given an emulator constructed with a zeroed size (the pre-overlay
        // default that reached the pty before the (0,0) spawn fix).
        let mut emu = Emulator::new(0, 0, 4);

        // When it is fed bytes that fit the floor grid and resized to zero
        // again.
        emu.feed(b"hi\r\n");
        emu.set_size(0, 0);
        emu.sync_transcript();

        // Then the grid clamped to the 2x2 floor and content survives.
        assert_eq!(emu.size(), (2, 2));
        assert!(
            emu.plain_text().contains("hi"),
            "got: {:?}",
            emu.plain_text()
        );
    }

    #[rstest::rstest]
    fn wide_char_on_min_grid_does_not_panic() {
        // Given an emulator at the minimum 2x2 grid.
        let mut emu = Emulator::new(2, 2, 4);

        // When a double-width character is written (vt100 computes
        // `cols - width` with width = 2, panicking below the floor).
        emu.feed("\u{4e16}\u{4e16}\u{4e16}\r\n".as_bytes());

        // Then the emulator survived and holds the wide cell.
        let cells = emu.cells();
        assert_eq!(cells.rows, 2);
        assert!(matches!(
            cells.cells[0],
            TermCell::Styled { ch: '\u{4e16}', .. }
        ));
    }

    #[rstest::rstest]
    fn transcript_tail_keeps_most_recent_screens() {
        // Given an emulator whose transcript cap is two screens.
        let mut emu = Emulator::new(24, 80, 2);

        // When observing three successive screens with syncs between.
        emu.feed(b"screen one\r\n");
        emu.sync_transcript();
        emu.feed(b"\x1b[2J\x1b[Hscreen two\r\n");
        emu.sync_transcript();
        emu.feed(b"\x1b[2J\x1b[Hscreen three\r\n");
        emu.sync_transcript();

        // Then the tail of two keeps only the latest two screens.
        let tail = emu.transcript_tail(2);
        assert!(tail.contains("screen two"), "missing screen two: {tail}");
        assert!(
            tail.contains("screen three"),
            "missing screen three: {tail}"
        );
        assert!(
            !tail.contains("screen one"),
            "stale screen one kept: {tail}"
        );
    }

    #[rstest::rstest]
    fn transcript_sync_skips_repeated_identical_screens() {
        // Given an emulator that synced one screen.
        let mut emu = Emulator::default();
        emu.feed(b"stable screen\r\n");
        emu.sync_transcript();

        // When feeding nothing new and syncing again (a repaint with no
        // visible change).
        emu.sync_transcript();

        // Then the transcript holds exactly one entry.
        let tail = emu.transcript_tail(10);
        assert_eq!(tail.matches("stable screen").count(), 1);
    }

    #[rstest::rstest]
    fn transcript_tail_is_empty_before_any_sync() {
        // Given a fresh emulator.
        let emu = Emulator::default();

        // Then the transcript tail is empty.
        assert!(emu.transcript_tail(10).is_empty());
    }

    #[rstest::rstest]
    fn alternate_screen_contents_do_not_destroy_transcript() {
        // Given an emulator that observed a primary-screen line.
        let mut emu = Emulator::default();
        emu.feed(b"before tui\r\n");
        emu.sync_transcript();

        // When a fullscreen app takes the alternate screen, paints, and
        // leaves (as vim does: ?1049h … ?1049l).
        emu.feed(b"\x1b[?1049h\x1b[2J\x1b[Hvim screen\x1b[?1049l");

        // Then the transcript still contains the pre-TUI observation.
        let tail = emu.transcript_tail(10);
        assert!(
            tail.contains("before tui"),
            "lost pre-TUI transcript: {tail}"
        );
    }
}
