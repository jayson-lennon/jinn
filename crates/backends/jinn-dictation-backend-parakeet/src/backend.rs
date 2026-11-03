//! The Parakeet backend: model fetch (hf-hub into jinn's cache), load
//! (sherpa-onnx offline transducer, int8), and utterance endpointing
//! (Silero VAD).
//!
//! Unit 6 fills the decode and VAD bodies; the module now carries the
//! fetch (with progress and the jinn-managed cache) and the backend
//! shell so the slice's linux edge compiles.

use std::path::PathBuf;

use error_stack::Report;
use error_stack::ResultExt as _;
use marked_path::MarkedPathAccess as _;

use jinn_dictation_backend::BackendError;
use jinn_dictation_backend::DictationBackend;
use jinn_dictation_backend::FetchProgress;
use jinn_dictation_backend::Utterance;

/// The HuggingFace repo the int8 Parakeet TDT 0.6B v2 package lives in.
const MODEL_REPO: &str = "akashmjn/sherpa-onnx-parakeet-tdt-0.6b-v2-int8";

/// The model files the backend needs from the repo.
const MODEL_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
];

/// The Parakeet backend: sherpa-onnx offline transducer + Silero VAD.
pub struct ParakeetBackend {
    /// The configured model name (the display name the state machine
    /// and hints use).
    model_name: String,
    /// The jinn-managed cache directory the fetch writes into.
    cache_dir: PathBuf,
    /// Whether the model's files are present.
    cached: bool,
    /// Whether the model is resident.
    loaded: bool,
}

impl ParakeetBackend {
    /// Builds the backend for `model_name`, caching under `cache_dir`.
    #[must_use]
    pub fn new(model_name: &str) -> Self {
        Self {
            model_name: model_name.to_owned(),
            cache_dir: jinn_common::app_paths::AppPaths::default()
                .dictation_models_dir()
                .as_path()
                .to_path_buf(),
            cached: false,
            loaded: false,
        }
    }

    /// Fetches every model file into the cache, reporting progress.
    ///
    /// hf-hub's default cache location is overridden: weights land in
    /// jinn's own directory, never `~/.cache/huggingface`.
    fn fetch_files(
        &self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_cache_dir(self.cache_dir.clone())
            .with_progress(false)
            .build()
            .change_context(BackendError)
            .attach("failed to build the HuggingFace client")?;
        let repo = api.model(MODEL_REPO.to_owned());
        for file in MODEL_FILES {
            let path = repo
                .get(file)
                .change_context(BackendError)
                .attach(format!("failed to fetch {file}"))?;
            let total = std::fs::metadata(&path)
                .ok()
                .and_then(|meta| u64::try_from(meta.len()).ok());
            on_progress(FetchProgress::new(total.unwrap_or(0), total));
        }
        Ok(())
    }
}

impl DictationBackend for ParakeetBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn is_cached(&self) -> bool {
        MODEL_FILES.iter().all(|file| {
            let candidate = self.cache_dir.join(MODEL_REPO).join(file);
            candidate.is_file()
        }) || self.cached
    }

    fn is_loaded(&self) -> bool {
        self.loaded
    }

    fn fetch(
        &mut self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        tracing::info!(model = %self.model_name, cache = %self.cache_dir.display(), "fetching dictation model");
        self.fetch_files(on_progress)?;
        self.cached = true;
        Ok(())
    }

    fn load(&mut self) -> Result<(), Report<BackendError>> {
        // The decode graph lands with the capture unit (unit 6): the
        // OfflineRecognizer construction, the VAD frame loop, and the
        // utterance queue. Until then the backend reports loaded — the
        // lifecycle exercises identically, transcribing nothing.
        tracing::warn!("parakeet decode not yet wired; the backend reports loaded and transcribes nothing");
        self.loaded = true;
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
    use super::ParakeetBackend;
    use jinn_dictation_backend::DictationBackend;

    #[rstest::rstest]
    #[test]
    fn backend_reports_the_configured_model_name() {
        // Given a backend built for the default model.
        let backend = ParakeetBackend::new("parakeet-tdt-0.6b-v2");

        // When reading the model name.
        // Then it is the configured string.
        assert_eq!(backend.model_name(), "parakeet-tdt-0.6b-v2");
    }

    #[rstest::rstest]
    #[test]
    fn feed_before_wiring_returns_no_utterances() {
        // Given a backend with no decode graph.
        let mut backend = ParakeetBackend::new("parakeet-tdt-0.6b-v2");

        // When feeding audio.
        let utterances = backend.feed(&[0.0_f32; 512]).expect("feed succeeds");

        // Then nothing was transcribed.
        assert!(utterances.is_empty());
    }
}
