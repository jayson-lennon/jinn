//! The Parakeet backend: model fetch (hf-hub into jinn's cache), load
//! (sherpa-onnx offline transducer, int8), and utterance endpointing
//! (Silero VAD at 16 kHz).

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

/// The Silero VAD model file, fetched from the same repo.
const VAD_FILE: &str = "silero_vad.onnx";

/// Every file the backend needs, model and VAD together.
const ALL_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
    VAD_FILE,
];

/// Silero VAD consumes exactly this many samples per frame at 16 kHz.
const VAD_WINDOW_SAMPLES: usize = 512;

/// The offline transducer + Silero VAD, both resident once loaded.
struct DecodeGraph {
    recognizer: sherpa_onnx::OfflineRecognizer,
    vad: sherpa_onnx::VoiceActivityDetector,
    /// The VAD consumes fixed windows; audio shorter than a window
    /// parks here until the next feed completes it.
    window_tail: Vec<f32>,
    /// Speech samples of the utterance the VAD is currently inside.
    open_utterance: Vec<f32>,
}

/// One decode session over a resident graph: the recognizer shared
/// behind its interior pointer, the VAD state and window tails mutable.
struct DecodeSession<'a> {
    recognizer: &'a sherpa_onnx::OfflineRecognizer,
    vad: &'a sherpa_onnx::VoiceActivityDetector,
    window_tail: &'a mut Vec<f32>,
    open_utterance: &'a mut Vec<f32>,
}

impl DecodeSession<'_> {
    /// Feeds samples through the VAD windows, decoding every utterance
    /// the VAD closes.
    fn feed(&mut self, samples: &[f32]) -> Vec<Utterance> {
        let mut utterances = Vec::new();
        let mut available = samples;
        while !available.is_empty() {
            let tail = self.window_tail.len();
            let take = (VAD_WINDOW_SAMPLES - tail).min(available.len());
            if take == 0 {
                break;
            }
            self.window_tail.extend_from_slice(&available[..take]);
            available = &available[take..];
            if self.window_tail.len() < VAD_WINDOW_SAMPLES {
                break;
            }
            let window = std::mem::take(self.window_tail);
            self.vad.accept_waveform(&window);
            self.drain_closed(&mut utterances);
        }
        utterances
    }

    /// Decodes every VAD-closed segment, appending to the open
    /// utterance's samples as speech continues.
    fn drain_closed(&mut self, utterances: &mut Vec<Utterance>) {
        while let Some(segment) = self.vad.front() {
            let samples = segment.samples().to_vec();
            self.vad.pop();
            self.open_utterance.extend_from_slice(&samples);
            let text = self.decode(&samples);
            if !text.is_empty() {
                utterances.push(Utterance::new(text));
            }
        }
    }

    /// Force-closes the VAD's open speech, decoding whatever it hands
    /// back; `None` when no utterance was open.
    fn flush(&mut self) -> Option<Utterance> {
        self.vad.flush();
        let mut closed = Vec::new();
        self.drain_closed(&mut closed);
        self.open_utterance.clear();
        self.vad.clear();
        closed.into_iter().next()
    }

    /// Batch-decodes one utterance's samples.
    fn decode(&self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        self.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|result| result.text)
            .unwrap_or_default()
    }
}

/// The Parakeet backend: sherpa-onnx offline transducer + Silero VAD.
pub struct ParakeetBackend {
    /// The configured model name (the display name the state machine
    /// and hints use).
    model_name: String,
    /// The jinn-managed cache directory the fetch writes into.
    cache_dir: PathBuf,
    /// The decode graph, resident after a successful load.
    graph: Option<DecodeGraph>,
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
            graph: None,
        }
    }

    /// The absolute path of one fetched model file.
    fn file_path(&self, file: &str) -> PathBuf {
        self.cache_dir.join(MODEL_REPO).join(file)
    }

    /// Whether every model file is present.
    fn all_files_present(&self) -> bool {
        ALL_FILES.iter().all(|file| self.file_path(file).is_file())
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
            .build()
            .change_context(BackendError)
            .attach("failed to build the HuggingFace client")?;
        let repo = api.model(MODEL_REPO.to_owned());
        let total: usize = ALL_FILES.len();
        for (index, file) in ALL_FILES.iter().enumerate() {
            let path = repo
                .get(file)
                .change_context(BackendError)
                .attach(format!("failed to fetch {file}"))?;
            let bytes = std::fs::metadata(&path)
                .ok()
                .and_then(|meta| u64::try_from(meta.len()).ok())
                .unwrap_or(0);
            on_progress(FetchProgress::new(
                bytes,
                Some(bytes),
            ));
            let _ = index;
            let _ = total;
        }
        Ok(())
    }

    /// Builds the decode graph from the fetched files.
    fn build_graph(&self) -> Result<DecodeGraph, Report<BackendError>> {
        let model_dir = self.cache_dir.join(MODEL_REPO);
        let transducer = sherpa_onnx::OfflineTransducerModelConfig {
            encoder: Some(
                model_dir
                    .join("encoder.int8.onnx")
                    .to_string_lossy()
                    .into_owned(),
            ),
            decoder: Some(
                model_dir
                    .join("decoder.int8.onnx")
                    .to_string_lossy()
                    .into_owned(),
            ),
            joiner: Some(
                model_dir
                    .join("joiner.int8.onnx")
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        let model_config = sherpa_onnx::OfflineModelConfig {
            transducer,
            tokens: Some(model_dir.join("tokens.txt").to_string_lossy().into_owned()),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_owned()),
            ..sherpa_onnx::OfflineModelConfig::default()
        };
        let config = sherpa_onnx::OfflineRecognizerConfig {
            feat_config: sherpa_onnx_sys::FeatureConfig {
                sample_rate: 16_000,
                feature_dim: 80,
            },
            model_config,
            ..sherpa_onnx::OfflineRecognizerConfig::default()
        };
        let recognizer = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or(Report::new(BackendError).attach("failed to build the offline recognizer"))?;

        let vad_config = sherpa_onnx::VadModelConfig {
            silero_vad: sherpa_onnx::SileroVadModelConfig {
                model: Some(model_dir.join(VAD_FILE).to_string_lossy().into_owned()),
                threshold: 0.5,
                min_silence_duration: 0.5,
                min_speech_duration: 0.25,
                window_size: i32::try_from(VAD_WINDOW_SAMPLES).unwrap_or(512),
                max_speech_duration: 20.0,
            },
            ten_vad: sherpa_onnx::TenVadModelConfig::default(),
            sample_rate: 16_000,
            num_threads: 1,
            debug: false,
            provider: Some("cpu".to_owned()),
        };
        let vad = sherpa_onnx::VoiceActivityDetector::create(&vad_config, 30.0)
            .ok_or(Report::new(BackendError).attach("failed to build the voice activity detector"))?;

        Ok(DecodeGraph {
            recognizer,
            vad,
            window_tail: Vec::new(),
            open_utterance: Vec::new(),
        })
    }
}

impl DictationBackend for ParakeetBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn is_cached(&self) -> bool {
        self.all_files_present()
    }

    fn is_loaded(&self) -> bool {
        self.graph.is_some()
    }

    fn fetch(
        &mut self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        tracing::info!(model = %self.model_name, cache = %self.cache_dir.display(), "fetching dictation model");
        self.fetch_files(on_progress)
    }

    fn load(&mut self) -> Result<(), Report<BackendError>> {
        if self.graph.is_none() {
            let graph = self.build_graph()?;
            self.graph = Some(graph);
        }
        Ok(())
    }

    fn feed(&mut self, samples: &[f32]) -> Result<Vec<Utterance>, Report<BackendError>> {
        let Some(graph) = self.graph.as_mut() else {
            return Ok(Vec::new());
        };
        let DecodeGraph {
            recognizer,
            vad,
            window_tail,
            open_utterance,
        } = graph;
        let mut session = DecodeSession {
            recognizer: &*recognizer,
            vad: &*vad,
            window_tail,
            open_utterance,
        };
        Ok(session.feed(samples))
    }

    fn flush(&mut self) -> Result<Option<Utterance>, Report<BackendError>> {
        let Some(graph) = self.graph.as_mut() else {
            return Ok(None);
        };
        let DecodeGraph {
            recognizer,
            vad,
            window_tail,
            open_utterance,
        } = graph;
        let mut session = DecodeSession {
            recognizer: &*recognizer,
            vad: &*vad,
            window_tail,
            open_utterance,
        };
        Ok(session.flush())
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
    fn an_unloaded_backend_reports_not_loaded_and_transcribes_nothing() {
        // Given a backend with no decode graph (no model fetched here —
        // hermetic).
        let mut backend = ParakeetBackend::new("parakeet-tdt-0.6b-v2");

        // When feeding audio and flushing.
        // Then neither produces utterances and nothing reports loaded.
        assert!(!backend.is_loaded());
        assert!(
            backend
                .feed(&[0.0_f32; 512])
                .expect("feed succeeds")
                .is_empty()
        );
        assert!(backend.flush().expect("flush succeeds").is_none());
    }

    #[rstest::rstest]
    #[test]
    fn a_feed_after_flush_returns_nothing_from_an_empty_flush() {
        // Given a loaded backend fed a full silent window.
        let mut backend = ParakeetBackend::new("parakeet-tdt-0.6b-v2");
        if backend.load().is_err() {
            // No fetched model in the test environment: the load is a
            // legitimate failure, and the graph stays absent.
            return;
        }

        // When feeding silence (below any VAD threshold) and flushing.
        let utterances = backend.feed(&[0.0_f32; 512]).expect("feed succeeds");
        let flushed = backend.flush().expect("flush succeeds");

        // Then silence closes no utterance, and the flush finds none.
        assert!(utterances.is_empty());
        assert!(flushed.is_none());
    }
}
