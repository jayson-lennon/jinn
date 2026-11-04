//! The linux capture body: the cpal device open and the audio-thread
//! pipeline (denoise, resample, backend feed). Compiled only on linux
//! — the twin of `capture_null.rs`, selected by the one `cfg` pair in
//! `capture.rs`.

//! The linux device open and the audio-thread pipeline.
//!
//! `open` builds the cpal input stream and returns the live arm; the
//! arm's drop closes the device. The audio callback runs entirely off
//! the actor's context: it denoises, resamples, feeds the backend
//! through the shared mutex, and records the level and any stream
//! loss into the shared slot the actor's tick drains.

use std::sync::Arc;

use parking_lot::Mutex;

use cpal::traits::DeviceTrait;
use cpal::traits::HostTrait;
use cpal::traits::StreamTrait;

use jinn_dictation_backend::DictationBackend;

use super::CaptureArm;
use super::CaptureShared;
use super::arm_refusal_note;
use super::push_utterances;
use super::take_utterances;

/// nnnoiseless consumes exactly this many samples per frame.
const DENOISE_FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

/// Lists the available input device names.
fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .into_iter()
        .flatten()
        .filter_map(|device| {
            device
                .description()
                .ok()
                .map(|description| description.name().to_owned())
        })
        .collect()
}

/// The system default input device's name, when the host has one.
pub fn default_input_device() -> Option<String> {
    let host = cpal::default_host();
    host.default_input_device()
        .and_then(|device| device.description().ok())
        .map(|description| description.name().to_owned())
}

/// Lists the capture input devices, for `jinn dictation list-devices`.
///
/// The same enumeration `find_device` matches against — the names
/// printed here are exactly the names a `[dictation] device` value
/// must equal.
pub fn list_input_devices() -> Vec<String> {
    input_device_names()
}

/// Finds the configured device, or the default input.
fn find_device(configured: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    match configured {
        Some(wanted) => {
            let available = input_device_names();
            host.input_devices()
                .into_iter()
                .flatten()
                .find(|device| {
                    device
                        .description()
                        .is_ok_and(|description| description.name() == wanted)
                })
                .ok_or_else(|| arm_refusal_note(Some(wanted), &available))
        }
        None => host
            .default_input_device()
            .ok_or_else(|| arm_refusal_note(None, &input_device_names())),
    }
}

/// The per-arm pipeline state: denoiser frame tail, denoise output
/// tail, and the resampler. Owned by the audio callback.
struct Pipeline {
    denoiser: Option<Box<nnnoiseless::DenoiseState<'static>>>,
    /// Samples waiting for a full denoise frame.
    denoise_tail: Vec<f32>,
    /// Denoised samples waiting for a backend feed.
    feed_tail: Vec<f32>,
    resampler: Option<sherpa_onnx::LinearResampler>,
    backend: Arc<Mutex<Box<dyn DictationBackend>>>,
    shared: Arc<Mutex<CaptureShared>>,
}

impl Pipeline {
    /// Runs one callback's samples through the stages.
    fn process(&mut self, data: &[f32]) {
        if data.is_empty() {
            return;
        }
        let level = data
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        let denoised = match self.denoiser.as_mut() {
            Some(state) => {
                self.denoise_tail.extend_from_slice(data);
                let mut out = Vec::with_capacity(self.denoise_tail.len());
                let mut frame_out = [0.0_f32; DENOISE_FRAME];
                while self.denoise_tail.len() >= DENOISE_FRAME {
                    let frame: Vec<f32> = self.denoise_tail.drain(..DENOISE_FRAME).collect();
                    state.process_frame(&mut frame_out, &frame);
                    out.extend_from_slice(&frame_out);
                }
                out
            }
            None => {
                self.denoise_tail.extend_from_slice(data);
                std::mem::take(&mut self.denoise_tail)
            }
        };
        // Resample to 16 kHz, then park on the feed tail: the
        // backend prefers chunky feeds over per-callback grains.
        let mut samples_16k = match &self.resampler {
            Some(resampler) => resampler.resample(&denoised, false),
            None => denoised,
        };
        self.feed_tail.append(&mut samples_16k);
        if self.feed_tail.len() >= 1_600 {
            let chunk = std::mem::take(&mut self.feed_tail);
            if let Some(mut backend) = self.backend.try_lock() {
                match backend.feed(&chunk) {
                    Ok(utterances) => {
                        drop(backend);
                        // Queue for the actor's tick: each closed
                        // utterance commits to the focused input as
                        // it closes. An empty Vec is the common no-op.
                        push_utterances(&self.shared, utterances);
                    }
                    Err(report) => {
                        drop(backend);
                        // The audio thread cannot surface an error to
                        // the user; the log is the only record. The
                        // feed tail is already consumed — the next
                        // chunk starts fresh, losing this one's audio.
                        tracing::warn!(error = ?report, "dictation backend feed failed");
                    }
                }
            }
            // Backend busy (load in flight, or the disarm flush holds
            // the mutex): drop the chunk silently — the busy path is
            // the normal end-of-utterance overlap, not a fault.
        }
        self.shared.lock().level = level;
    }
}

/// Opens the capture arm.
///
/// The device open and stream build run on the caller's (blocking)
/// context; the audio callback then runs on cpal's own thread. Every
/// step logs: a refused arm otherwise surfaces only as a green flash,
/// with the reason unread anywhere.
///
/// # Errors
///
/// Returns the refusal note when the device cannot be found or the
/// stream cannot be built.
pub fn open(
    configured: Option<&str>,
    noise_reduction: bool,
    backend: &Arc<Mutex<Box<dyn DictationBackend>>>,
) -> Result<CaptureArm, String> {
    tracing::debug!(device = ?configured, "opening the dictation capture device");
    let device = match find_device(configured) {
        Ok(device) => device,
        Err(note) => {
            tracing::warn!(reason = %note, "dictation arm refused: no capture device");
            return Err(note);
        }
    };
    let config = match device.default_input_config() {
        Ok(config) => config,
        Err(error) => {
            let note = format!("capture config failed for the input device: {error}");
            tracing::warn!(reason = %note, "dictation arm refused: config failed");
            return Err(note);
        }
    };
    tracing::debug!(
        sample_rate = config.sample_rate(),
        channels = config.channels(),
        "capture config resolved"
    );
    let sample_rate = i32::try_from(config.sample_rate()).unwrap_or(48_000);

    let shared = Arc::new(Mutex::new(CaptureShared::default()));
    let err_callback = {
        let shared = Arc::clone(&shared);
        move |error| {
            shared.lock().lost = Some(format!("capture stream error: {error}"));
        }
    };

    let pipeline = {
        let denoiser = noise_reduction.then(nnnoiseless::DenoiseState::new);
        let resampler = sherpa_onnx::LinearResampler::create(sample_rate, 16_000)
            .filter(|_| sample_rate != 16_000);
        Pipeline {
            denoiser,
            denoise_tail: Vec::new(),
            feed_tail: Vec::new(),
            resampler,
            backend: Arc::clone(backend),
            shared: Arc::clone(&shared),
        }
    };

    let stream = match device.build_input_stream(
        config.into(),
        {
            let shared = Arc::clone(&shared);
            let mut pipeline = pipeline;
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                pipeline.process(data);
                // The level lands in `pipeline.process`; the
                // local clone keeps the slot's identity alive
                // for the err callback's independent write.
                let _ = &shared;
            }
        },
        err_callback,
        None,
    ) {
        Ok(stream) => stream,
        Err(error) => {
            let note = format!("capture stream open failed: {error}");
            tracing::warn!(reason = %note, "dictation arm refused: stream build failed");
            return Err(note);
        }
    };
    tracing::debug!("capture stream built; starting playback");
    if let Err(error) = stream.play() {
        let note = format!("capture stream start failed: {error}");
        tracing::warn!(reason = %note, "dictation arm refused: stream start failed");
        return Err(note);
    }
    tracing::info!("dictation capture is live");
    Ok(CaptureArm {
        closer: Some(Box::new(stream)),
        shared,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code, panics are acceptable"
    )]
    use super::*;

    use jinn_dictation_backend::BackendError;
    use jinn_dictation_backend::DictationBackend;
    use jinn_dictation_backend::FetchProgress;
    use jinn_dictation_backend::Utterance;

    use error_stack::Report;

    /// A backend the tests script: one canned feed result, success or
    /// failure, returned for every feed call.
    struct ScriptedFeedBackend {
        feed_fails: bool,
        utterances: Vec<Utterance>,
    }

    impl DictationBackend for ScriptedFeedBackend {
        fn model_name(&self) -> &'static str {
            "scripted-feed"
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
            if self.feed_fails {
                return Err(Report::new(BackendError).attach("scripted feed failure"));
            }
            Ok(std::mem::take(&mut self.utterances))
        }

        fn flush(&mut self) -> Result<Option<Utterance>, Report<BackendError>> {
            Ok(None)
        }
    }

    /// A pipeline wired to `backend`, no denoiser or resampler, over a
    /// fresh shared slot.
    fn pipeline_over(
        backend: ScriptedFeedBackend,
    ) -> (
        Arc<Mutex<Box<dyn DictationBackend>>>,
        Arc<Mutex<CaptureShared>>,
        Pipeline,
    ) {
        let backend: Arc<Mutex<Box<dyn DictationBackend>>> =
            Arc::new(Mutex::new(Box::new(backend)));
        let shared = Arc::new(Mutex::new(CaptureShared::default()));
        let pipeline = Pipeline {
            denoiser: None,
            denoise_tail: Vec::new(),
            feed_tail: Vec::new(),
            resampler: None,
            backend: Arc::clone(&backend),
            shared: Arc::clone(&shared),
        };
        (backend, shared, pipeline)
    }

    /// One full backend feed: 1600 samples is the chunk threshold.
    fn one_chunk() -> Vec<f32> {
        vec![0.1_f32; 1_600]
    }

    #[rstest::rstest]
    #[test]
    fn pipeline_queues_decoded_utterances_from_the_backend_feed() {
        // Given a pipeline over a scripted backend that closes one
        // utterance per feed.
        let scripted = ScriptedFeedBackend {
            feed_fails: false,
            utterances: vec![Utterance::new("hello there.".to_owned())],
        };
        let (_backend, shared, mut pipeline) = pipeline_over(scripted);

        // When one chunk of audio runs through the pipeline.
        pipeline.process(&one_chunk());

        // Then the decoded utterance is queued on the shared slot —
        // the actor's tick can commit it; nothing was dropped.
        assert_eq!(
            take_utterances(&shared),
            vec![Utterance::new("hello there.".to_owned())],
            "the pipeline must queue feed output, not discard it"
        );
    }

    /// Records every event's fields, so a test can assert a warn fired.
    #[derive(Clone)]
    struct CapturedEvents(Arc<std::sync::Mutex<Vec<String>>>);

    impl tracing::Subscriber for CapturedEvents {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }

        fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut rendered = String::new();
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    use std::fmt::Write as _;
                    let _ = std::write!(rendered, "{}={:?} ", field.name(), value);
                },
            );
            self.0.lock().expect("event capture lock").push(rendered);
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}

        fn clone_span(&self, id: &tracing::span::Id) -> tracing::span::Id {
            id.clone()
        }

        fn try_close(&self, _id: tracing::span::Id) -> bool {
            true
        }
    }

    #[rstest::rstest]
    #[test]
    fn feed_error_is_logged_not_swallowed() {
        // Given a pipeline over a scripted failing backend, with this
        // thread's events captured.
        let scripted = ScriptedFeedBackend {
            feed_fails: true,
            utterances: Vec::new(),
        };
        let (_backend, shared, mut pipeline) = pipeline_over(scripted);
        let captured = CapturedEvents(Arc::new(std::sync::Mutex::new(Vec::new())));

        // When one chunk of audio runs through the pipeline.
        tracing::subscriber::with_default(captured.clone(), || {
            pipeline.process(&one_chunk());
        });

        // Then the failure was logged as a feed-failed warn, not
        // skipped in silence.
        let events = captured.0.lock().expect("event capture lock");
        assert!(
            events
                .iter()
                .any(|rendered| rendered.contains("dictation backend feed failed")),
            "expected a feed-failed warn among: {events:?}"
        );
        // And nothing was queued — a failed feed yields no utterance.
        assert!(take_utterances(&shared).is_empty());
    }
}
