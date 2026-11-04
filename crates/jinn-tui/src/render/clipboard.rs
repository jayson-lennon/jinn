//! Clipboard flush - extracts selected text from the buffer and hands the
//! write to the app's clipboard writer.

use ratatui::buffer::Buffer;

use jinn_slices::ClipboardRequest;

use crate::TuiApp;
use crate::selection::SelectionState;

/// If a clipboard copy is pending, extracts the selected text from the
/// buffer and hands it to [`TuiApp::clipboard_writer`]. Clears the pending
/// flag regardless of success or failure.
///
/// This module owns extraction only. The write itself — thread, backend,
/// ~2-second hold, and the copied/could-not-copy status hint raised after
/// the write resolves — belongs to the clipboard writer.
pub(super) fn flush_pending_clipboard(app: &mut TuiApp, buf: &Buffer) {
    if !app.pending_clipboard {
        return;
    }
    app.pending_clipboard = false;

    let text = match app.selection.extract_text(buf) {
        Some(text) if !text.is_empty() => text,
        _ => {
            // Empty selection - clear highlight silently.
            app.selection = SelectionState::Idle;
            return;
        }
    };

    // Clear selection highlight immediately.
    app.selection = SelectionState::Idle;

    app.clipboard_writer.write_async(
        ClipboardRequest::new(text, "selection"),
        &app.services.slices,
    );
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test code, panics are acceptable"
    )]
    use super::*;
    use crate::clipboard_writer::ClipboardBackend;
    use crate::selection::SelectionState;
    use jinn_status_bar_msg::StatusBarState;
    use jinn_status_bar_msg::status_bar_slot;
    use ratatui::layout::Rect;

    /// Creates a minimal `TuiApp` for render testing.
    async fn render_test_app() -> crate::TuiApp {
        crate::TuiApp::test_builder().build().await
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clipboard_copy_clears_pending_flag_on_idle_selection() {
        // Given an app with pending_clipboard set but Idle selection.
        let mut app = render_test_app().await;
        app.selection = SelectionState::Idle;
        app.pending_clipboard = true;

        let area = Rect::new(0, 0, 20, 5);
        let buf = ratatui::buffer::Buffer::empty(area);

        // When flushing the pending clipboard.
        flush_pending_clipboard(&mut app, &buf);

        // Then the pending flag is cleared (even though there was nothing to copy).
        assert!(!app.pending_clipboard);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clipboard_copy_skips_empty_selection() {
        // Given an app with pending_clipboard and an Active selection over empty cells.
        let area = Rect::new(0, 0, 20, 5);
        let buf = ratatui::buffer::Buffer::empty(area);

        let mut app = render_test_app().await;
        app.selection = SelectionState::Active {
            anchor: (0, 0),
            focus: (3, 0),
            bounds: area,
        };
        app.pending_clipboard = true;

        // When flushing the pending clipboard.
        flush_pending_clipboard(&mut app, &buf);

        // Then the pending flag is cleared.
        assert!(!app.pending_clipboard);
        // And the selection is cleared to Idle (no highlight persists).
        assert_eq!(app.selection, SelectionState::Idle);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn clipboard_clears_pending_flag_immediately() {
        // Given a buffer with known text and an active selection.
        let area = Rect::new(0, 0, 20, 5);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        // Write "Hello" on row 2.
        for (i, ch) in "Hello".chars().enumerate() {
            buf.cell_mut((2 + i as u16, 2))
                .unwrap()
                .set_symbol(&ch.to_string());
        }

        let mut app = render_test_app().await;
        app.selection = SelectionState::Active {
            anchor: (2, 2),
            focus: (6, 2),
            bounds: area,
        };
        app.pending_clipboard = true;

        // When flushing the pending clipboard.
        flush_pending_clipboard(&mut app, &buf);

        // Then the pending flag is cleared immediately.
        assert!(!app.pending_clipboard);
        // And the selection is cleared to Idle.
        assert_eq!(app.selection, SelectionState::Idle);
    }

    /// A fake backend recording what it was handed, standing in for the
    /// system clipboard so the flush's write is deterministic.
    #[derive(Debug, Default)]
    struct RecordingBackend {
        written: std::sync::Mutex<Vec<String>>,
    }

    impl ClipboardBackend for RecordingBackend {
        fn set_text(
            &self,
            text: &str,
        ) -> Result<(), crate::clipboard_writer::ClipboardBackendError> {
            if let Ok(mut written) = self.written.lock() {
                written.push(text.to_owned());
            }
            Ok(())
        }

        fn hold_open(&self) {}
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn flush_writes_the_selection_through_the_app_writer_and_reports_it() {
        // Given a buffer with known text, an active selection, and an app
        // whose writer runs on a recording backend.
        let area = Rect::new(0, 0, 20, 5);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (i, ch) in "Hello".chars().enumerate() {
            buf.cell_mut((2 + i as u16, 2))
                .unwrap()
                .set_symbol(&ch.to_string());
        }

        let mut app = render_test_app().await;
        let backend = std::sync::Arc::new(RecordingBackend::default());
        app.clipboard_writer =
            crate::clipboard_writer::ClipboardWriter::with_backend(backend.clone());
        // The hint resolves through the app's own registry — the same path
        // production takes — so read it from there, not a parallel registry.
        let status = app
            .services
            .slices
            .reader::<StatusBarState>(&status_bar_slot())
            .expect("the cell catalog seeded the status-bar cell");
        app.selection = SelectionState::Active {
            anchor: (2, 2),
            focus: (6, 2),
            bounds: area,
        };
        app.pending_clipboard = true;

        // When flushing the pending clipboard.
        flush_pending_clipboard(&mut app, &buf);

        // Then the copied hint — raised from the write thread, after the
        // write resolves — names the selection label. The hint is the
        // completion signal: once it is up, the write is known to be done.
        jinn_testutil::wait::await_cell(&status, std::time::Duration::from_secs(5), |state| {
            state.hint.as_deref() == Some("copied selection to the clipboard")
        })
        .await;
        // And the app's writer received the selection verbatim.
        assert_eq!(
            backend.written.lock().expect("backend lock").as_slice(),
            ["Hello"]
        );
    }
}
