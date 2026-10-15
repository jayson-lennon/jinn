//! Terminal suspend/resume via RAII guard.
//!
//! [`TerminalGuard`] suspends the TUI when created (exits raw mode, leaves
//! alternate screen) and automatically restores it when dropped - even if
//! the closure passed to [`suspend_and_run`] panics.

use std::io;

#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{
    event::{DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use error_stack::{Report, ResultExt as _};
use ratatui::{Terminal, backend::CrosstermBackend};
use wherror::Error;

/// Error type for terminal suspend/resume operations.
#[derive(Debug, Error)]
#[error(debug)]
pub struct TerminalSuspendError;

/// RAII guard for terminal suspend/resume.
///
/// Suspends the TUI when created (exits raw mode, leaves alternate screen)
/// and automatically restores it when dropped. Used to temporarily return
/// to the normal terminal for external editor sessions.
pub struct TerminalGuard<'a> {
    /// Reference to the terminal being suspended and restored.
    terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
}

impl<'a> TerminalGuard<'a> {
    /// Creates a new guard, suspending the TUI.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal cannot be suspended.
    pub fn new(
        terminal: &'a mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<Self, Report<TerminalSuspendError>> {
        disable_raw_mode()
            .change_context(TerminalSuspendError)
            .attach("failed to disable raw mode")?;
        execute!(terminal.backend_mut(), DisableMouseCapture)
            .change_context(TerminalSuspendError)
            .attach("failed to disable mouse capture")?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)
            .change_context(TerminalSuspendError)
            .attach("failed to leave alternate screen")?;
        let _ = disable_keyboard_enhancement(terminal.backend_mut());
        terminal
            .show_cursor()
            .change_context(TerminalSuspendError)
            .attach("failed to show cursor")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard<'_> {
    fn drop(&mut self) {
        let _ = enable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), EnterAlternateScreen);
        let _ = enable_keyboard_enhancement(self.terminal.backend_mut());
        let _ = enable_bracketed_paste(self.terminal.backend_mut());
        let _ = execute!(self.terminal.backend_mut(), EnableMouseCapture);
        let _ = self.terminal.hide_cursor();
        let _ = self.terminal.clear();
    }
}

/// Suspends the TUI, runs the closure, then resumes the TUI.
///
/// Automatically handles cleanup on drop, even if the closure panics.
///
/// # Errors
///
/// Returns an error if the terminal cannot be suspended.
pub fn suspend_and_run<F, T>(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    f: F,
) -> Result<T, Report<TerminalSuspendError>>
where
    F: FnOnce() -> T,
{
    let _guard = TerminalGuard::new(terminal)?;
    Ok(f())
}

/// Enables the Kitty keyboard protocol's disambiguated escape codes so
/// crossterm can distinguish modified special keys (e.g. Shift+Enter,
/// Ctrl+Enter). Terminals that don't implement the protocol ignore the
/// sequence.
///
/// # Errors
///
/// Returns an error if writing the sequence to the terminal fails.
#[cfg(not(windows))]
pub(crate) fn enable_keyboard_enhancement<W>(writer: &mut W) -> io::Result<()>
where
    W: io::Write,
{
    execute!(
        writer,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
}

/// Windows twin of the Unix [`enable_keyboard_enhancement`]: never emits
/// the protocol sequence.
///
/// crossterm's Windows input path reads Win32 console records, which carry
/// modifier state natively, and crossterm rejects the push/pop commands on
/// Windows with an `Unsupported` error regardless of terminal — so jinn
/// must not emit them there.
///
/// # Errors
///
/// Never fails; the signature mirrors the Unix twin for uniform call sites.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the Unix twin so call sites are uniform"
)]
pub(crate) fn enable_keyboard_enhancement<W>(_writer: &mut W) -> io::Result<()>
where
    W: io::Write,
{
    Ok(())
}

/// Disables the Kitty keyboard protocol by popping the flags pushed by
/// [`enable_keyboard_enhancement`].
///
/// # Errors
///
/// Returns an error if writing the sequence to the terminal fails.
#[cfg(not(windows))]
pub(crate) fn disable_keyboard_enhancement<W>(writer: &mut W) -> io::Result<()>
where
    W: io::Write,
{
    execute!(writer, PopKeyboardEnhancementFlags)
}

/// Windows twin of the Unix [`disable_keyboard_enhancement`]: never emits
/// the protocol sequence. See the Windows [`enable_keyboard_enhancement`]
/// twin for why.
///
/// # Errors
///
/// Never fails; the signature mirrors the Unix twin for uniform call sites.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature mirrors the Unix twin so call sites are uniform"
)]
pub(crate) fn disable_keyboard_enhancement<W>(_writer: &mut W) -> io::Result<()>
where
    W: io::Write,
{
    Ok(())
}

/// Re-enables bracketed paste after a terminal suspension, restoring the
/// mode [`crate::run`] sets at startup.
///
/// A terminal suspension exits raw mode, and most terminals reset DECSET
/// modes when raw mode ends — so after the guard tears down, paste bytes
/// arrive as individual key events instead of one `Event::Paste`. Re-enabling
/// here keeps multi-line pastes intact across an external-editor session.
///
/// Emitted unconditionally on every platform, mirroring the startup and
/// teardown sequence in [`crate::run`]: crossterm routes the command through
/// the ANSI writer or the Windows console API on its own, so unlike the
/// kitty keyboard flags this command needs no cfg split.
///
/// # Errors
///
/// Returns an error if writing the sequence to the terminal fails.
pub(crate) fn enable_bracketed_paste<W>(writer: &mut W) -> io::Result<()>
where
    W: io::Write,
{
    execute!(writer, EnableBracketedPaste)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "test code"
    )]
    use super::*;

    /// Writer that records bytes without touching a real terminal.
    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
    }

    impl io::Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    #[rstest::rstest]
    fn enable_keyboard_enhancement_writes_kitty_push_sequence() {
        // Given a recording writer.
        let mut writer = RecordingWriter::default();

        // When enabling keyboard enhancement.
        let result = enable_keyboard_enhancement(&mut writer);

        // Then the kitty push sequence for DISAMBIGUATE_ESCAPE_CODES is written.
        assert!(result.is_ok());
        assert_eq!(writer.bytes, b"\x1b[>1u");
    }

    #[cfg(not(windows))]
    #[rstest::rstest]
    fn disable_keyboard_enhancement_writes_kitty_pop_sequence() {
        // Given a recording writer.
        let mut writer = RecordingWriter::default();

        // When disabling keyboard enhancement.
        let result = disable_keyboard_enhancement(&mut writer);

        // Then the kitty pop sequence is written.
        assert!(result.is_ok());
        assert_eq!(writer.bytes, b"\x1b[<1u");
    }

    #[rstest::rstest]
    fn enable_bracketed_paste_writes_paste_enable_sequence() {
        // Given a recording writer.
        let mut writer = RecordingWriter::default();

        // When enabling bracketed paste.
        let result = enable_bracketed_paste(&mut writer);

        // Then the DECSET sequence that turns bracketed paste on is written.
        assert!(result.is_ok());
        assert_eq!(writer.bytes, b"\x1b[?2004h");
    }

    #[cfg(windows)]
    #[rstest::rstest]
    fn enable_keyboard_enhancement_writes_nothing() {
        // Given a recording writer.
        let mut writer = RecordingWriter::default();

        // When enabling keyboard enhancement.
        let result = enable_keyboard_enhancement(&mut writer);

        // Then nothing is written and the call succeeds.
        assert!(result.is_ok());
        assert_eq!(writer.bytes, [] as [u8; 0]);
    }

    #[cfg(windows)]
    #[rstest::rstest]
    fn disable_keyboard_enhancement_writes_nothing() {
        // Given a recording writer.
        let mut writer = RecordingWriter::default();

        // When disabling keyboard enhancement.
        let result = disable_keyboard_enhancement(&mut writer);

        // Then nothing is written and the call succeeds.
        assert!(result.is_ok());
        assert_eq!(writer.bytes, [] as [u8; 0]);
    }
}
