//! Chat input box intent handlers.
//!
//! Handles 16 chat-input intents:
//!
//! - **InsertChar** - inserts a character, manages autocomplete triggering/filtering/expansion.
//! - **PasteText** - bulk inserts pasted text, deactivates autocomplete.
//! - **DeleteGrapheme** - backspace with autocomplete awareness.
//! - **DeleteGraphemeForward** - forward delete with autocomplete awareness.
//! - **SubmitMessage** - validates, extracts text, resets buffer, returns `EnqueueUserMessage`.
//! - **AutocompleteConfirm** - confirms autocomplete selection or falls back to tab switch.
//! - **Cursor movement** (8 intents) - move cursor, optionally deactivating autocomplete.
//! - **EnterInsertMode** - switches to Input mode.
//! - **EnterNormalMode** - clears picker, switches to Normal mode.
//!
//! The cancel-stream confirmation is deliberately absent: escape in Normal
//! mode is a session concern, raised and confirmed by the kernel.

//! Token scanning for the input box's autocomplete triggers.
//!
//! The three triggers — `@path`, `#prompt-token`, and `/command` — each need to
//! know whether the cursor sits inside a token, and each needs to know whether a
//! freshly-typed glyph sits at a boundary where activating is correct. Those
//! are two small families of question, and this module owns both.
//!
//! They are not one algorithm. `@` and `#` are the same scan with the glyph
//! swapped: walk left to the nearest occurrence, reject it unless it opens a
//! token, extend to the next terminator, test the cursor against the span.
//! `/command` is different in kind — anchored at the start of the buffer, never
//! walking left — so it keeps its own function rather than being forced into the
//! general scanner.

use unicode_segmentation::UnicodeSegmentation as _;

use crate::ChatInputBoxState;

/// Scans leftward from the cursor for a `trigger` glyph sitting at a valid
/// token boundary, and returns the token it opens.
///
/// One algorithm serves both `@path` and `#prompt-token`: each scans leftward
/// from the cursor to the nearest occurrence of its glyph, rejects that glyph
/// if it is not at a token boundary, extends the token right to the next
/// terminator, and returns `(token_start, filter_text)` when the cursor lies
/// within it. Only the glyph and the terminator differ, so both are parameters
/// rather than two copies of the walk.
///
/// `seam` names a glyph that must not directly follow an identical one — the
/// `@@` case, where no handler is wired and the text should stay literal.
/// `None` for `#`, which has no such rule.
fn find_token_at_cursor(
    input: &ChatInputBoxState,
    trigger: &str,
    terminator: &str,
    seam: Option<&str>,
) -> Option<(usize, String)> {
    let cursor = input.cursor_pos();
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();

    let mut i = cursor;
    loop {
        if graphemes.get(i) == Some(&trigger) {
            let at_boundary = i == 0
                || graphemes.get(i.wrapping_sub(1)) == Some(&" ")
                || graphemes.get(i.wrapping_sub(1)) == Some(&"\n");
            let hits_seam = seam.is_some_and(|s| graphemes.get(i.wrapping_sub(1)) == Some(&s));
            if !at_boundary || hits_seam {
                return None;
            }

            let mut token_end = i + 1;
            while token_end < len {
                let g = graphemes.get(token_end);
                if g.is_none_or(|c| c.trim().is_empty() || *c == terminator) {
                    break;
                }
                token_end += 1;
            }

            if cursor >= i && cursor <= token_end {
                let filter: String = graphemes
                    .get((i + 1)..cursor)
                    .map(|s| s.join(""))
                    .unwrap_or_default();
                return Some((i, filter));
            }
            return None;
        }
        // Hitting whitespace going left means there is no token behind the cursor.
        let g = graphemes.get(i);
        if g.is_some_and(|c| c.trim().is_empty()) {
            return None;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// The `#prompt-token` token under the cursor, if any.
///
/// Terminates on whitespace or a further `#`, so two tokens on one line stay
/// separate.
pub(crate) fn find_hash_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    find_token_at_cursor(input, "#", "#", None)
}

/// The `@path` token under the cursor, if any.
///
/// Terminates on whitespace or a further `@`, and carries the `@@` seam.
pub(crate) fn find_at_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    find_token_at_cursor(input, "@", "@", Some("@"))
}

/// Checks whether a freshly-typed `#` sits where activating is correct.
///
/// Valid at the start of the buffer or directly after a space or newline; a `#`
/// inside a word is literal text.
pub(crate) fn is_valid_hash_trigger_position(input: &ChatInputBoxState) -> bool {
    let pos = input.cursor_pos() - 1;
    if pos == 0 {
        return true;
    }
    matches!(input.grapheme_at(pos - 1), Some(" " | "\n"))
}

/// Checks whether a freshly-typed `@` sits where activating is correct.
///
/// Same boundary rule as [`is_valid_hash_trigger_position`], plus the `@@`
/// seam: an `@` directly after another `@` does not open a popup, so `@@`
/// stays literal.
pub(crate) fn is_valid_at_trigger_position(input: &ChatInputBoxState) -> bool {
    let pos = input.cursor_pos() - 1;
    if pos == 0 {
        return true;
    }
    let prev = input.grapheme_at(pos - 1);
    if prev == Some("@") {
        return false;
    }
    matches!(prev, Some(" " | "\n"))
}

/// Checks whether a freshly-typed `/` sits where activating is correct.
///
/// Valid only as the very first character of the buffer — a `/` anywhere else
/// is a path, not a command.
pub(crate) fn is_valid_slash_trigger_position(input: &ChatInputBoxState) -> bool {
    input.cursor_pos() == 1 && input.text().starts_with('/')
}

/// Computes the grapheme index one past the last character of the token
/// that starts at `token_start` (the trigger glyph's position).
///
/// Scans forward from `token_start + 1` until whitespace, the terminator, or
/// end of buffer.
pub(crate) fn compute_token_end(input: &ChatInputBoxState, token_start: usize) -> usize {
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();
    let mut end = token_start + 1;
    while end < len {
        let g = graphemes.get(end);
        if g.is_none_or(|c| c.trim().is_empty() || *c == "#") {
            break;
        }
        end += 1;
    }
    end
}

/// Scans the buffer to detect if the cursor sits inside a `/command` region at position 0.
///
/// Returns `Some((token_start, filter_text))` if the buffer starts with `/` and the
/// cursor is within the token, where `token_start` is 0 and `filter_text` is the text
/// between position 1 and the cursor.
pub(crate) fn find_slash_token_at_cursor(input: &ChatInputBoxState) -> Option<(usize, String)> {
    use unicode_segmentation::UnicodeSegmentation as _;

    if !input.text().starts_with('/') {
        return None;
    }

    let cursor = input.cursor_pos();
    let graphemes: Vec<&str> = input.text().graphemes(true).collect();
    let len = graphemes.len();

    // The token extends from 1 to the next whitespace or end.
    let mut token_end = 1;
    while token_end < len {
        let g = graphemes.get(token_end);
        if g.is_none_or(|c| c.trim().is_empty()) {
            break;
        }
        token_end += 1;
    }

    // The cursor must be >= 0 and <= token_end.
    if cursor <= token_end {
        let filter: String = graphemes
            .get(1..cursor)
            .map(|s| s.join(""))
            .unwrap_or_default();
        return Some((0, filter));
    }
    None
}
