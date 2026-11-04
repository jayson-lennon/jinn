//! The Parakeet backend: model fetch (hf-hub into jinn's cache), load
//! (sherpa-onnx offline transducer, int8), and utterance endpointing
//! (Silero VAD at 16 kHz).

use error_stack::Report;
use error_stack::ResultExt as _;
use marked_path::MarkedPathAccess as _;

use jinn_dictation_backend::BackendError;
use jinn_dictation_backend::BackendOptions;
use jinn_dictation_backend::DictationBackend;
use jinn_dictation_backend::FetchProgress;
use jinn_dictation_backend::Utterance;

/// The HuggingFace repo the int8 Parakeet TDT 0.6B v2 package lives in.
/// The sherpa-onnx maintainer's mirror of the released int8 package.
const MODEL_REPO: &str = "csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8";

/// The Silero VAD model file, fetched from the sherpa-onnx release
/// assets (the model repo does not carry it).
const VAD_FILE: &str = "silero_vad_v4.onnx";

/// The release asset URL the VAD file downloads from.
const VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad_v4.onnx";

/// Every HuggingFace file the backend needs (the VAD is fetched
/// separately from the release asset above).
const ALL_FILES: &[&str] = &[
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
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
            let (chunk, rest) = available.split_at(take);
            self.window_tail.extend_from_slice(chunk);
            available = rest;
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

/// The absolute path of one model file under `model_dir`.
///
/// The join is a name join on an absolute base, so it cannot fail; the
/// Result shape satisfies the no-expect lint in fallible callers.
fn model_file(
    model_dir: &marked_path::MarkedPathBuf<marked_path::Absolute>,
    name: &str,
) -> Result<String, Report<BackendError>> {
    let path = model_dir
        .join_all([name])
        .change_context(BackendError)
        .attach("failed to join a model file name onto the cache dir")?;
    Ok(path.as_path().to_string_lossy().into_owned())
}

/// The Parakeet backend: sherpa-onnx offline transducer + Silero VAD.
pub struct ParakeetBackend {
    /// The configured model name (the display name the state machine
    /// and hints use).
    model_name: String,
    /// The jinn-managed cache directory the fetch writes into.
    cache_dir: marked_path::MarkedPathBuf<marked_path::Absolute>,
    /// The tuning knobs the graph builds with.
    options: BackendOptions,
    /// The decode graph, resident after a successful load.
    graph: Option<DecodeGraph>,
}

impl ParakeetBackend {
    /// Builds the backend for `model_name`, caching under `cache_dir`.
    #[must_use]
    pub fn new(model_name: &str) -> Self {
        Self::with_options(model_name, BackendOptions::default())
    }

    /// Builds the backend with explicit tuning options; the values
    /// apply when the graph builds (on `load`).
    #[must_use]
    pub fn with_options(model_name: &str, options: BackendOptions) -> Self {
        Self {
            model_name: model_name.to_owned(),
            cache_dir: jinn_common::app_paths::AppPaths::default().dictation_models_dir(),
            options,
            graph: None,
        }
    }

    /// The absolute path of one fetched model file, under the
    /// HuggingFace snapshot layout the fetch writes.
    fn file_path(
        &self,
        file: &str,
    ) -> Result<marked_path::MarkedPathBuf<marked_path::Absolute>, Report<BackendError>> {
        let snapshot = self.hf_snapshot_dir()?;
        snapshot
            .join_all([file])
            .change_context(BackendError)
            .attach("failed to join a model file name onto the snapshot dir")
    }

    /// Whether one model file is on disk under the model directory.
    fn file_present(&self, file: &str) -> bool {
        self.file_path(file)
            .is_ok_and(|path| path.as_path().is_file())
    }

    /// The HuggingFace snapshot directory the fetch wrote the model
    /// files into, discovered from the cache's `refs/main` commit.
    fn hf_snapshot_dir(
        &self,
    ) -> Result<marked_path::MarkedPathBuf<marked_path::Absolute>, Report<BackendError>> {
        let repo_component = format!("models--{}", MODEL_REPO.replace('/', "--"));
        let repo_dir = self
            .cache_dir
            .join_all([repo_component.as_str()])
            .change_context(BackendError)
            .attach("failed to join the model repo name onto the cache dir")?;
        let refs_main = repo_dir
            .join_all(["refs", "main"])
            .change_context(BackendError)
            .attach("failed to join the refs path onto the repo dir")?;
        let commit = std::fs::read_to_string(refs_main.as_path())
            .change_context(BackendError)
            .attach("the model is not fetched yet (no refs/main)")?;
        repo_dir
            .join_all(["snapshots", commit.trim()])
            .change_context(BackendError)
            .attach("failed to join the snapshot commit onto the repo dir")
    }

    /// Whether every model file is present.
    fn all_files_present(&self) -> bool {
        ALL_FILES.iter().all(|file| self.file_present(file)) && self.file_present(VAD_FILE)
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
            .with_cache_dir(self.cache_dir.as_path().to_path_buf())
            // hf-hub draws indicatif progress bars on the terminal by
            // default; jinn's TUI owns that surface, and fetch progress
            // already reports through the dictation state cell (the
            // dashboard row). The bars would corrupt the frame.
            .with_progress(false)
            .build()
            .change_context(BackendError)
            .attach("failed to build the HuggingFace client")?;
        let repo = api.model(MODEL_REPO.to_owned());
        for file in ALL_FILES {
            let path = repo
                .get(file)
                .change_context(BackendError)
                .attach(format!("failed to fetch {file}"))?;
            let bytes = std::fs::metadata(&path).map_or(0, |meta| meta.len());
            on_progress(FetchProgress::new(bytes, Some(bytes)));
        }
        self.fetch_vad(on_progress)
    }

    /// Fetches the Silero VAD file from the sherpa-onnx release
    /// assets, into the model directory beside the transducer files.
    fn fetch_vad(
        &self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>> {
        let destination = self.file_path(VAD_FILE)?;
        if destination.as_path().is_file() {
            return Ok(());
        }
        // The model directory may not exist yet on a first fetch; the
        // HF cache layout creates its own tree, this one must too.
        if let Some(parent) = destination.as_path().parent() {
            std::fs::create_dir_all(parent)
                .change_context(BackendError)
                .attach("failed to create the model directory")?;
        }
        let response = ureq::get(VAD_URL)
            .call()
            .change_context(BackendError)
            .attach(format!("failed to fetch {VAD_FILE}"))?;
        let mut body = response.into_body().into_reader();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut file = std::fs::File::create(destination.as_path())
            .change_context(BackendError)
            .attach("failed to create the VAD model file")?;
        let mut written: u64 = 0;
        loop {
            let read = std::io::Read::read(&mut body, &mut buffer)
                .change_context(BackendError)
                .attach(format!("failed to read {VAD_FILE}"))?;
            if read == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, buffer.get(..read).unwrap_or(&buffer))
                .change_context(BackendError)
                .attach("failed to write the VAD model file")?;
            written += u64::try_from(read).unwrap_or(0);
            on_progress(FetchProgress::new(written, None));
        }
        Ok(())
    }

    /// Builds the decode graph from the fetched files, tuned by the
    /// configured options.
    fn build_graph(&self) -> Result<DecodeGraph, Report<BackendError>> {
        // HF stores the blobs under its own `models--<org>--<repo>`
        // layout; `repo.get` returns the snapshot path the fetch
        // resolved to, so the graph reads the files from there.
        let model_dir = self.hf_snapshot_dir()?;
        let options = &self.options;
        let transducer = sherpa_onnx::OfflineTransducerModelConfig {
            encoder: Some(model_file(&model_dir, "encoder.int8.onnx")?),
            decoder: Some(model_file(&model_dir, "decoder.int8.onnx")?),
            joiner: Some(model_file(&model_dir, "joiner.int8.onnx")?),
        };
        let model_config = sherpa_onnx::OfflineModelConfig {
            transducer,
            tokens: Some(model_file(&model_dir, "tokens.txt")?),
            num_threads: options.num_threads,
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
            decoding_method: options.decoding_method.clone(),
            max_active_paths: options.max_active_paths,
            hotwords_file: options.hotwords_file.clone(),
            hotwords_score: options.hotwords_score,
            blank_penalty: options.blank_penalty,
            rule_fsts: options.rule_fsts.clone(),
            ..sherpa_onnx::OfflineRecognizerConfig::default()
        };
        let recognizer = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or(Report::new(BackendError).attach("failed to build the offline recognizer"))?;

        let vad_config = sherpa_onnx::VadModelConfig {
            silero_vad: sherpa_onnx::SileroVadModelConfig {
                model: Some(model_file(&model_dir, VAD_FILE)?),
                threshold: options.vad_threshold,
                min_silence_duration: options.min_silence_duration,
                min_speech_duration: options.min_speech_duration,
                window_size: i32::try_from(VAD_WINDOW_SAMPLES).unwrap_or(512),
                max_speech_duration: options.max_speech_duration,
            },
            ten_vad: sherpa_onnx::TenVadModelConfig::default(),
            sample_rate: 16_000,
            num_threads: 1,
            debug: false,
            provider: Some("cpu".to_owned()),
        };
        // The detector buffers the open segment in a ring of this many
        // seconds; a max_speech_duration beyond the shipped 30 s
        // default must grow it or the ring overflows mid-utterance.
        let buffer_seconds = f32::max(30.0, options.max_speech_duration + 10.0);
        let vad = sherpa_onnx::VoiceActivityDetector::create(&vad_config, buffer_seconds).ok_or(
            Report::new(BackendError).attach("failed to build the voice activity detector"),
        )?;

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
        tracing::info!(model = %self.model_name, cache = %self.cache_dir.as_path().display(), "fetching dictation model");
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
    #![allow(
        clippy::expect_used,
        clippy::assertions_on_constants,
        reason = "test code, panics are acceptable"
    )]
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
        assert_eq!(
            backend.feed(&[0.0_f32; 512]).expect("feed succeeds").len(),
            0
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
        assert_eq!(utterances.len(), 0);
        assert!(flushed.is_none());
    }
}
