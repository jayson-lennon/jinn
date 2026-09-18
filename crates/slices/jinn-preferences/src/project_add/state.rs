//! State for the project-add input popup — editing a directory path.

use jinn_slices::LineInput;

/// State for the project-add input popup — editing a directory path.
///
/// The editable text and cursor live in [`LineInput`] (shared with the cwd,
/// arg, and rename session inputs) under the [`ProjectAddInputState::text`]
/// field; access via `.text.input` / `.text.cursor_pos`.
#[derive(Debug, Clone, Default)]
pub struct ProjectAddInputState {
    /// The editable text + cursor.
    pub text: LineInput,
}
