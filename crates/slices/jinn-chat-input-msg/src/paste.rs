//! The shared paste-append implementation.
//!
//! One function owns what "append text to the composing message" means:
//! deactivate any active autocomplete, then insert the text at the
//! cursor in one O(n) operation. Both callers — the kernel's
//! bracketed-paste intent path and the dictation actor's utterance
//! commits — go through this helper, so the two paths can never drift
//! (an edit here changes both behaviors identically).
//!
//! The helper takes the buffer (`ChatInputBoxState`), not `AppState`:
//! this crate sits below the state layer, and the buffer is where the
//! mutation lives. Callers resolve the *focused* session's buffer
//! through their own state handle (`update_active_input`), which is
//! what makes a commit land in whichever session holds focus at commit
//! time — the same routing rule a bracketed paste follows.

use crate::chat_input_state::ChatInputBoxState;

/// Appends pasted text to the composing buffer.
///
/// Pastes bypass the per-character insertion pipeline entirely,
/// inserting the full string in one O(n) operation. Autocomplete is
/// always deactivated on paste since the appended content may span
/// multiple lines or tokens.
pub fn append_paste_to_buffer(buffer: &mut ChatInputBoxState, text: &str) {
    buffer.deactivate_autocomplete();
    buffer.insert_text(text);
}

#[cfg(test)]
mod tests {
    use super::append_paste_to_buffer;
    use crate::chat_input_state::ChatInputBoxState;

    #[rstest::rstest]
    #[test]
    fn append_lands_text_in_an_empty_buffer() {
        // Given an empty composing buffer.
        let mut buffer = ChatInputBoxState::default();

        // When appending pasted text.
        append_paste_to_buffer(&mut buffer, "hello\nworld");

        // Then the text is in the buffer, newlines intact.
        assert_eq!(buffer.text(), "hello\nworld");
    }

    #[rstest::rstest]
    #[test]
    fn append_joins_onto_existing_text() {
        // Given a buffer the user already typed into.
        let mut buffer = ChatInputBoxState::default();
        for ch in "say ".chars() {
            buffer.insert_grapheme_at_cursor(ch);
        }

        // When appending pasted text.
        append_paste_to_buffer(&mut buffer, "hello");

        // Then it joins onto the existing content.
        assert_eq!(buffer.text(), "say hello");
    }

    #[rstest::rstest]
    #[test]
    fn append_deactivates_an_active_autocomplete() {
        // Given a buffer with the `@` file popup active.
        let mut buffer = ChatInputBoxState::default();
        buffer.activate_autocomplete(
            0,
            crate::chat_input_state::AutocompleteTrigger::At,
            Vec::new(),
        );
        assert!(buffer.autocomplete().is_some());

        // When appending pasted text.
        append_paste_to_buffer(&mut buffer, "path");

        // Then the popup is gone and the text is in the buffer.
        assert!(buffer.autocomplete().is_none());
        assert_eq!(buffer.text(), "path");
    }
}
