//! The clipboard writer — the workspace's one system-clipboard writer.
//!
//! Both clipboard write paths converge here: the intent-signal drain (chat
//! entry `y`, sidebar session-ID `y`, term overlay `y`) and the mouse
//! selection flush. A write is staged as a [`ClipboardRequest`] (payload
//! text + caller-chosen label); [`ClipboardWriter`] hands it to a backend
//! on a detached thread and — this is the point of the module — raises the
//! status hint **only after the write resolves**, so the bar never claims a
//! copy that has not happened:
//!
//! - success → `copied {label} to the clipboard`
//! - failure → `could not copy {label} to the clipboard`
//!
//! The hint lands the moment the write resolves — *before* the hold — so
//! the copy feedback is not delayed by clipboard-manager bookkeeping. The
//! detached thread then holds the clipboard open for ~2 seconds: on X11,
//! clipboard data is only available while the clipboard instance is alive
//! — dropping it immediately prevents clipboard managers from syncing. The
//! hold is load-bearing, not waste.
//!
//! Tests swap the backend through [`ClipboardBackend`]; a fake makes both
//! the write and the hint deterministic without touching a real clipboard
//! or waiting on wall-clock timing.

use std::sync::Arc;

use wherror::Error;

use jinn_slices::ClipboardRequest;
use jinn_slices::cell::TypedCell;
use jinn_status_bar_msg::{StatusBarState, status_bar_slot};

/// How long the writer's thread holds the clipboard open after writing.
///
/// X11 clipboard managers can only sync while the clipboard instance is
/// alive; both write sites kept this hold before the writer existed, and
/// the writer preserves it.
const HOLD_DURATION: std::time::Duration = std::time::Duration::from_secs(2);

/// Why a clipboard write can fail.
#[derive(Debug, Clone, Error)]
pub enum ClipboardBackendError {
    /// The backend could not open the system clipboard.
    #[error("failed to open the clipboard")]
    Open(String),
    /// The backend could not place the text on the clipboard.
    #[error("failed to write to the clipboard")]
    Write(String),
}

/// The write-outcome callback a backend invokes the moment its write
/// resolves — boxed so the backend trait stays object-safe behind
/// [`ClipboardWriter`]'s `Arc<dyn ClipboardBackend>`.
pub type Resolved = Box<dyn FnOnce(Result<(), ClipboardBackendError>) + Send>;

/// The capability the writer needs: put text on the system clipboard.
///
/// The production backend is arboard (see [`ArboardBackend`]); tests
/// install a fake via [`ClipboardWriter::with_backend`] so a write's
/// outcome — and therefore the hint — is deterministic.
pub trait ClipboardBackend: Send + Sync + 'static {
    /// Places `text` on the system clipboard, reports the outcome the
    /// moment it resolves, then holds the clipboard open.
    ///
    /// `on_resolved` must run as soon as the write's outcome is known and
    /// *before* any hold: the status hint rides this callback, and copy
    /// feedback must not wait on clipboard-manager bookkeeping. The hold
    /// itself — [`HOLD_DURATION`] on the production backend, a no-op on
    /// fakes — keeps X11 clipboard managers able to sync, so it runs after
    /// the callback with the writing instance still alive.
    fn write_with_hold(&self, text: &str, on_resolved: Resolved);
}

/// The production backend: arboard.
struct ArboardBackend;

impl ClipboardBackend for ArboardBackend {
    fn write_with_hold(&self, text: &str, on_resolved: Resolved) {
        // The `Clipboard` instance is created, written, reported, and held
        // in ONE scope: X11 clipboard data is only available while the
        // writing instance is alive, so `cb` must live through the sleep.
        let mut cb = match arboard::Clipboard::new() {
            Ok(cb) => cb,
            Err(e) => {
                on_resolved(Err(ClipboardBackendError::Open(e.to_string())));
                return;
            }
        };
        let outcome = cb
            .set_text(text)
            .map_err(|e| ClipboardBackendError::Write(e.to_string()));
        // Report before the hold: the status hint rides `on_resolved`, and
        // feedback must not wait out the clipboard-manager window.
        on_resolved(outcome);
        // Hold the clipboard open so clipboard managers can sync. `cb`
        // must live through the sleep.
        std::thread::sleep(HOLD_DURATION);
    }
}

impl<B: ClipboardBackend + ?Sized> ClipboardBackend for std::sync::Arc<B> {
    fn write_with_hold(&self, text: &str, on_resolved: Resolved) {
        (**self).write_with_hold(text, on_resolved);
    }
}

/// Writes staged clipboard requests and raises the status hint afterwards.
///
/// Cheap to clone: every clone shares the same backend and resolves the
/// status-bar cell through the shared `Slices` registry at write time, so
/// a clone handed to a detached thread observes the live hint cell.
#[derive(Clone)]
pub struct ClipboardWriter {
    /// `None` on the production writer: the arboard write runs with the
    /// hold in scope (see [`write_and_hold`]). `Some` when built over an
    /// explicit backend — the test seam.
    backend: Option<Arc<dyn ClipboardBackend>>,
}

impl std::fmt::Debug for ClipboardWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.backend {
            Some(_) => f.write_str("ClipboardWriter(fake)"),
            None => f.write_str("ClipboardWriter(arboard)"),
        }
    }
}

impl Default for ClipboardWriter {
    fn default() -> Self {
        Self::arboard()
    }
}

impl ClipboardWriter {
    /// The production writer: arboard, holding the clipboard open ~2s
    /// after each write.
    #[must_use]
    pub fn arboard() -> Self {
        Self { backend: None }
    }

    /// A writer over an explicit backend — the test seam.
    #[must_use]
    pub fn with_backend<B>(backend: B) -> Self
    where
        B: ClipboardBackend,
    {
        Self {
            backend: Some(Arc::new(backend)),
        }
    }

    /// Runs one write against the chosen backend, calling `on_resolved`
    /// with the write's outcome the moment it is known — the
    /// arboard-with-hold path when no explicit backend was installed.
    fn run_write<R>(&self, text: &str, label: String, on_resolved: R)
    where
        R: FnOnce(String, Result<(), ClipboardBackendError>) + Send + 'static,
    {
        let on_resolved: Resolved = Box::new(move |outcome| on_resolved(label, outcome));
        match &self.backend {
            Some(backend) => backend.write_with_hold(text, on_resolved),
            None => ArboardBackend.write_with_hold(text, on_resolved),
        }
    }

    /// Writes `request` on a detached thread and raises the status hint
    /// from that thread, only after the write resolves: success →
    /// `copied {label} to the clipboard`, failure → `could not copy
    /// {label} to the clipboard`.
    ///
    /// Returns immediately; the hint arrives asynchronously, exactly when
    /// the write's outcome is known and not before — and before the
    /// backend's clipboard-manager hold begins, so feedback is instant.
    pub fn write_async(&self, request: ClipboardRequest, slices: &jinn_slices::Slices) {
        // The write runs on the thread; the hint is computed from its
        // outcome there, never before it resolves.
        let writer = self.clone();
        let status = slices.reader::<StatusBarState>(&status_bar_slot());
        // Detached by design: the write (and its ~2s hold on the
        // production backend) outlives the keypress that staged it.
        std::thread::spawn(move || {
            writer.run_write(&request.text, request.label, |label, outcome| {
                let hint = hint_for(&label, outcome);
                if let Some(status) = status {
                    status.update(|state| state.hint = Some(hint));
                }
            });
        });
    }

    /// Writes `request` synchronously and raises the status hint after the
    /// write resolves, with the same phrasing [`Self::write_async`] uses.
    ///
    /// The test path: a fake backend plus a seeded status-bar cell makes
    /// both the write and the hint fully deterministic, with no wall-clock
    /// wait and no detached thread to synchronize against.
    pub fn write(&self, request: &ClipboardRequest, status: &TypedCell<StatusBarState>) {
        let status = status.clone();
        let label = request.label.clone();
        self.run_write(&request.text, label, move |label, outcome| {
            let hint = hint_for(&label, outcome);
            status.update(|state| state.hint = Some(hint));
        });
    }
}

/// The hint phrasing for one write outcome — the single place the two
/// copied/could-not-copy sentences are spelled.
fn hint_for(label: &str, outcome: Result<(), ClipboardBackendError>) -> String {
    match outcome {
        Ok(()) => format!("copied {label} to the clipboard"),
        Err(error) => {
            tracing::warn!(%error, label, "clipboard write failed");
            format!("could not copy {label} to the clipboard")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]
    use super::*;
    use jinn_slices::Slices;

    /// A fake backend whose writes all succeed, recording what it was
    /// given so a test can assert the payload verbatim.
    #[derive(Debug, Default)]
    struct FakeSuccessBackend {
        written: std::sync::Mutex<Vec<String>>,
    }

    impl FakeSuccessBackend {
        fn written_text(&self) -> String {
            self.written.lock().expect("backend lock").join("\n")
        }
    }

    impl ClipboardBackend for FakeSuccessBackend {
        fn write_with_hold(&self, text: &str, on_resolved: Resolved) {
            let outcome = match self.written.lock() {
                Ok(mut written) => {
                    written.push(text.to_owned());
                    Ok(())
                }
                Err(_) => Err(ClipboardBackendError::Write("backend poisoned".to_owned())),
            };
            // Report before the hold — the contract the hint latency rides.
            on_resolved(outcome);
        }
    }

    /// A fake backend whose writes always fail.
    #[derive(Debug, Default)]
    struct FakeFailingBackend;

    impl ClipboardBackend for FakeFailingBackend {
        fn write_with_hold(&self, _text: &str, on_resolved: Resolved) {
            on_resolved(Err(ClipboardBackendError::Write(
                "backend refused".to_owned(),
            )));
        }
    }

    /// A fake whose hold phase blocks until the test releases it, so a
    /// test can observe state *while* the hold is in progress.
    struct HoldBlockedBackend {
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl ClipboardBackend for HoldBlockedBackend {
        fn write_with_hold(&self, _text: &str, on_resolved: Resolved) {
            // Report first, then block in the hold phase.
            on_resolved(Ok(()));
            if let Ok(rx) = self.release.lock() {
                let _ = rx.recv();
            }
        }
    }

    fn status_slices() -> (Slices, TypedCell<StatusBarState>) {
        let slices = Slices::new();
        let cell = jinn_kernel::common::render_test::seed_cell(
            &slices,
            &status_bar_slot(),
            StatusBarState::default(),
        );
        (slices, cell)
    }

    fn hint_of(status: &TypedCell<StatusBarState>) -> Option<String> {
        status.read().hint.clone()
    }

    #[rstest::rstest]
    fn write_success_sets_the_copied_hint_with_the_request_label() {
        // Given a writer over a succeeding fake and a seeded status bar.
        let writer = ClipboardWriter::with_backend(FakeSuccessBackend::default());
        let (_slices, status) = status_slices();

        // When writing an "entry" request.
        writer.write(&ClipboardRequest::new("payload text", "entry"), &status);

        // Then the hint names the label, not the payload.
        assert_eq!(
            hint_of(&status),
            Some("copied entry to the clipboard".to_owned())
        );
    }

    #[rstest::rstest]
    fn write_success_carries_the_payload_to_the_backend() {
        // Given a writer over a recording fake.
        let backend = std::sync::Arc::new(FakeSuccessBackend::default());
        let writer = ClipboardWriter::with_backend(backend.clone());
        let (_slices, status) = status_slices();

        // When writing a request.
        writer.write(&ClipboardRequest::new("payload text", "entry"), &status);

        // Then the backend received exactly the payload text.
        assert_eq!(backend.written_text(), "payload text");
        // And the hint is the copied sentence (payload stays off the bar).
        assert_eq!(
            hint_of(&status),
            Some("copied entry to the clipboard".to_owned())
        );
    }

    #[rstest::rstest]
    fn write_failure_sets_the_could_not_copy_hint() {
        // Given a writer over a failing fake and a seeded status bar.
        let writer = ClipboardWriter::with_backend(FakeFailingBackend);
        let (_slices, status) = status_slices();

        // When writing a "selection" request.
        writer.write(&ClipboardRequest::new("text", "selection"), &status);

        // Then the hint names the failure with the label.
        assert_eq!(
            hint_of(&status),
            Some("could not copy selection to the clipboard".to_owned())
        );
    }

    #[rstest::rstest]
    fn write_failure_overwrites_a_prior_hint() {
        // Given a status bar already carrying a hint.
        let writer = ClipboardWriter::with_backend(FakeSuccessBackend::default());
        let (_slices, status) = status_slices();
        status.update(|state| state.hint = Some("stale".to_owned()));

        // When writing a session-id request that succeeds.
        writer.write(&ClipboardRequest::new("uuid", "session id"), &status);

        // Then the writer's hint replaced the stale one.
        assert_eq!(
            hint_of(&status),
            Some("copied session id to the clipboard".to_owned())
        );
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn write_async_raises_the_hint_while_the_hold_is_still_running() {
        // Given a backend whose hold blocks until the test releases it.
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = ClipboardWriter::with_backend(HoldBlockedBackend {
            release: std::sync::Mutex::new(rx),
        });
        let (slices, status) = status_slices();

        // When dispatching an async write and waiting for the hint.
        writer.write_async(ClipboardRequest::new("text", "selection"), &slices);
        jinn_testutil::wait::await_cell(&status, std::time::Duration::from_secs(5), |state| {
            state.hint.as_deref() == Some("copied selection to the clipboard")
        })
        .await;

        // Then the hint arrived while the hold phase was still running —
        // copy feedback does not wait out the clipboard-manager window.
        // (Releasing is best-effort: the detached thread exits regardless.)
        let _ = tx.send(());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn write_async_raises_the_hint_only_after_the_write_resolves() {
        // Given a writer over a succeeding fake and a seeded status bar.
        let backend = FakeSuccessBackend::default();
        let writer = ClipboardWriter::with_backend(backend);
        let (slices, status) = status_slices();

        // When dispatching an async write of a counted terminal label.
        writer.write_async(ClipboardRequest::new("screen", "3 terminal lines"), &slices);

        // Then the hint — raised from the write thread, after the write —
        // reports the label.
        jinn_testutil::wait::await_cell(&status, std::time::Duration::from_secs(5), |state| {
            state.hint.as_deref() == Some("copied 3 terminal lines to the clipboard")
        })
        .await;
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn write_async_reports_a_failed_write_from_the_thread() {
        // Given a writer over a failing fake and a seeded status bar.
        let writer = ClipboardWriter::with_backend(FakeFailingBackend);
        let (slices, status) = status_slices();

        // When dispatching an async write.
        writer.write_async(ClipboardRequest::new("screen", "selection"), &slices);

        // Then the failure hint arrives from the write thread.
        jinn_testutil::wait::await_cell(&status, std::time::Duration::from_secs(5), |state| {
            state.hint.as_deref() == Some("could not copy selection to the clipboard")
        })
        .await;
    }

    #[rstest::rstest]
    fn write_async_without_a_status_bar_writes_without_panicking() {
        // Given a registry with no status-bar cell seeded.
        let slices = Slices::new();
        let writer = ClipboardWriter::with_backend(FakeSuccessBackend::default());

        // When dispatching an async write.
        writer.write_async(ClipboardRequest::new("text", "entry"), &slices);

        // Then the detached write completes without a hint landing anywhere
        // — a hint with nowhere to land is not an error of its own. The
        // fake's writes are synchronous-enough; joining nothing, the test
        // simply must not hang or panic on the missing cell.
    }
}
