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
            if let Some(mut backend) = self.backend.try_lock()
                && let Ok(utterances) = backend.feed(&chunk)
            {
                drop(backend);
                // Utterances committed here land through the actor on
                // its tick; the shared-mutex seam is the same one the
                // disarm flush drains.
                let _ = utterances;
            }
        }
        self.shared.lock().level = level;
    }
}

/// Opens the capture arm.
///
/// The device open and stream build run on the caller's (blocking)
/// context; the audio callback then runs on cpal's own thread.
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
    let device = find_device(configured)?;
    let config = device
        .default_input_config()
        .map_err(|error| format!("capture config failed for the input device: {error}"))?;
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

    let stream = device
        .build_input_stream(
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
        )
        .map_err(|error| format!("capture stream open failed: {error}"))?;
    stream
        .play()
        .map_err(|error| format!("capture stream start failed: {error}"))?;
    Ok(CaptureArm {
        closer: Some(Box::new(stream)),
        shared,
    })
}
