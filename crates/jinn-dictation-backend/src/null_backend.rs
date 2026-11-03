//! The null backend — the backend every platform without native support
//! runs.
//!
//! [`NullBackend`] succeeds trivially at everything: the model is always
//! "cached" and loads instantly, feeding audio produces no utterances, and
//! flushing closes nothing. This is what makes the whole dictation
//! lifecycle — state machine, keybind reporting, dashboard row, meter —
//! run identically on platforms without a real engine: the Windows build
//! walks Offline → Fetching → Loading → Ready → Recording and back, with
//! the same hints and the same row, transcribing nothing.
//!
//! Platform *unavailability* is not this type's concern. The gated
//! platform-support query in the dictation slice is the only thing that
//! reports "no real backend here" (to `jinn fetch`, which exits non-zero);
//! the state machine never branches on platform.

use crate::BackendError;
use crate::DictationBackend;
use crate::FetchProgress;
use crate::Utterance;
use error_stack::Report;

/// The do-nothing backend: a full lifecycle that transcribes nothing.
#[derive(Debug, Clone)]
pub struct NullBackend {
    /// The model name handed to [`DictationBackend::model_name`]. The
    /// configured model string, so hints still read "Loading \<model\>"
    /// and the row still names what the user asked for.
    model_name: String,
}

impl NullBackend {
    /// Builds the null backend reporting `model_name`.
    #[must_use]
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
        }
    }
}

impl DictationBackend for NullBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn is_cached(&self) -> bool {
        true
    }

    fn is_loaded(&self) -> bool {
        true
    }

    fn fetch(
        &mut self,
        _on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        Ok(())
    }

    fn load(&mut self) -> Result<(), Report<BackendError>> {
        Ok(())
    }

    fn feed(&mut self, _samples: &[f32]) -> Result<Vec<Utterance>, Report<BackendError>> {
        Ok(Vec::new())
    }

    fn flush(&mut self) -> Result<Option<Utterance>, Report<BackendError>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "null-backend calls are infallible by contract; expect documents that"
    )]

    use super::NullBackend;
    use crate::DictationBackend;
    use crate::FetchProgress;

    #[rstest::rstest]
    #[test]
    fn null_backend_reports_the_model_name_it_was_given() {
        // Given a null backend built with a model name.
        let backend = NullBackend::new("parakeet-tdt-0.6b-v2");

        // When reading the model name.
        // Then it is the configured string, so hints name what the user
        // asked for.
        assert_eq!(backend.model_name(), "parakeet-tdt-0.6b-v2");
    }

    #[rstest::rstest]
    #[test]
    fn null_backend_reports_cached_and_loaded() {
        // Given a null backend.
        let backend = NullBackend::new("any");

        // When the state machine's init consults the cache.
        // Then both checks read true, so init walks Fetching → Loading →
        // Ready without any real work.
        assert!(backend.is_cached());
        assert!(backend.is_loaded());
    }

    #[rstest::rstest]
    #[test]
    fn null_fetch_and_load_succeed_immediately() {
        // Given a null backend.
        let mut backend = NullBackend::new("any");
        let mut progress_seen = false;

        // When fetching (with a progress callback) and loading.
        let fetched = backend.fetch(&mut |_| progress_seen = true);
        let loaded = backend.load();

        // Then both succeed; the callback may or may not fire (nothing is
        // downloading), and neither is an error.
        fetched.expect("null fetch succeeds");
        loaded.expect("null load succeeds");
        let _ = progress_seen;
    }

    #[rstest::rstest]
    #[test]
    fn null_feed_returns_no_utterances() {
        // Given a null backend.
        let mut backend = NullBackend::new("any");

        // When feeding audio.
        let utterances = backend.feed(&[0.0_f32; 1024]).expect("null feed succeeds");

        // Then nothing was transcribed.
        assert!(utterances.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn null_flush_closes_nothing() {
        // Given a null backend.
        let mut backend = NullBackend::new("any");

        // When flushing after feeding audio.
        let flushed = backend.flush().expect("null flush succeeds");

        // Then there was no open utterance to close.
        assert_eq!(flushed, None);
    }

    #[rstest::rstest]
    #[test]
    fn fetch_progress_percent_contract_holds_for_the_null_backend() {
        // Given the progress type the fetch callback carries.
        let progress = FetchProgress::new(640, Some(1000));

        // When reading its percentage.
        // Then the state machine's percent rendering sees a sane value.
        assert_eq!(progress.percent(), Some(64));
    }
}
