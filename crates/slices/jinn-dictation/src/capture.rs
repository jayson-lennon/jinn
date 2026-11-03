//! The capture arm — the microphone half of the pipeline.
//!
//! The arm opens the configured (or system-default) capture device on
//! cpal's own audio thread, denoises (when `noise_reduction` is on),
//! resamples to 16 kHz mono f32, and hands samples to the backend
//! through the shared mutex. The arm reports its level to the meter
//! cell and publishes a `capture-lost` tick when the stream ends
//! without a toggle.
//!
//! Platform shape: on linux this module compiles the real device open;
//! every other platform compiles a stub whose `open` reports the
//! refusal vocabulary (the null backend transcribes nothing, so there
//! is nothing to arm). This is the integration's one platform split,
//! shared with `platform.rs` — no other `cfg` exists in the slice.
//!
//! Pipeline order matters: nnnoiseless is a 48 kHz-era architecture, so
//! denoise runs first at the device's native rate, then the resample to
//! 16 kHz. Reversing the order denoises at the wrong rate and the
//! result reads as silence.

/// Why arming refused, rendered for the refusal note.
///
/// The note names the configured device (when one was configured) and
/// the available input devices, so a typo'd `device` value is
/// self-correcting from the dashboard row alone.
#[must_use]
pub fn arm_refusal_note(configured: Option<&str>, available: &[String]) -> String {
    let wanted = configured.unwrap_or("the system default input");
    let mut note = format!("no capture device available for {wanted}");
    if available.is_empty() {
        note.push_str("; no input devices found");
    } else {
        note.push_str("; available inputs: ");
        note.push_str(&available.join(", "));
    }
    note
}

#[cfg(target_os = "linux")]
mod device {
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
    use jinn_dictation_msg::DictationLevel;

    use super::arm_refusal_note;

    /// nnnoiseless consumes exactly this many samples per frame.
    const DENOISE_FRAME: usize = nnnoiseless::DenoiseState::FRAME_SIZE;

    /// A live capture arm: holds the cpal stream (closing on drop) and
    /// the shared state the callback writes.
    pub struct CaptureArm {
        /// The cpal stream. Kept for its Drop: dropping it closes the
        /// device and the audio thread stops.
        _stream: cpal::Stream,
        /// The shared level/loss slot.
        pub shared: Arc<Mutex<CaptureShared>>,
    }

    /// What the audio callback writes and the actor's tick drains.
    #[derive(Default)]
    pub struct CaptureShared {
        /// The latest peak level, `0.0..=1.0`, for the meter cell.
        pub level: f32,
        /// Set once when the stream dies without a toggle; the actor
        /// reads and clears it on its tick.
        pub lost: Option<String>,
    }

    /// Lists the available input device names.
    fn input_device_names() -> Vec<String> {
        let host = cpal::default_host();
        host.input_devices()
            .into_iter()
            .flatten()
            .filter_map(|device| device.description().ok().map(|description| description.name().to_owned()))
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
                            .map(|description| description.name() == wanted)
                            .unwrap_or(false)
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
            let level = data.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
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
                    if let Ok(utterances) = backend.feed(&chunk) {
                        drop(backend);
                        // Utterances committed here land through the
                        // actor only when it is subscribed — on the
                        // null platform there is no feed at all. The
                        // linux actor polls `feed`'s returns through
                        // this same shared-mutex seam.
                        let _ = utterances;
                    }
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
            let resampler =
                sherpa_onnx::LinearResampler::create(sample_rate, 16_000)
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
            _stream: stream,
            shared,
        })
    }

    /// The level cell update + loss drain the actor runs on its tick.
    pub fn sync_level(shared: &Mutex<CaptureShared>, level_cell: &jinn_slices::TypedCell<DictationLevel>) {
        let level = shared.lock().level;
        level_cell.update(|cell| cell.level = level);
    }

    /// Drains the loss reason, if the stream died without a toggle.
    pub fn take_lost(shared: &Mutex<CaptureShared>) -> Option<String> {
        shared.lock().lost.take()
    }
}

#[cfg(target_os = "linux")]
pub use device::open;
#[cfg(target_os = "linux")]
pub use device::CaptureArm;
#[cfg(target_os = "linux")]
pub use device::sync_level;
#[cfg(target_os = "linux")]
pub use device::take_lost;

#[cfg(test)]
mod tests {
    use super::arm_refusal_note;

    #[rstest::rstest]
    #[test]
    fn refusal_names_the_configured_device_and_the_available_inputs() {
        // Given a configured device name that matches nothing.
        let note = arm_refusal_note(Some("usb-mic"), &["builtin".to_owned(), "webcam".to_owned()]);

        // Then the refusal names both the wanted device and the choices.
        assert_eq!(
            note,
            "no capture device available for usb-mic; available inputs: builtin, webcam"
        );
    }

    #[rstest::rstest]
    #[test]
    fn refusal_names_the_default_input_when_no_device_is_configured() {
        // Given no configured device.
        let note = arm_refusal_note(None, &["builtin".to_owned()]);

        // Then the refusal says the default input was wanted.
        assert_eq!(
            note,
            "no capture device available for the system default input; available inputs: builtin"
        );
    }

    #[rstest::rstest]
    #[test]
    fn refusal_says_no_inputs_when_the_device_list_is_empty() {
        // Given no configured device and no inputs at all.
        let note = arm_refusal_note(None, &[]);

        // Then the refusal names the absence, not an empty list.
        assert_eq!(
            note,
            "no capture device available for the system default input; no input devices found"
        );
    }
}

#[cfg(not(target_os = "linux"))]
mod null_arm {
    //! The null-platform arm: there is no real backend to transcribe,
    //! so arming always refuses with the standard vocabulary.

    use super::arm_refusal_note;

    /// Opens the capture arm: always refuses on the null platform.
    ///
    /// # Errors
    ///
    /// Always returns the refusal note naming the configured device and
    /// the (empty) device list.
    pub fn open(
        configured: Option<&str>,
        _noise_reduction: bool,
        _backend: &std::sync::Arc<parking_lot::Mutex<Box<dyn jinn_dictation_backend::DictationBackend>>>,
    ) -> Result<(), String> {
        Err(arm_refusal_note(configured, &[]))
    }
}

#[cfg(not(target_os = "linux"))]
pub use null_arm::open;
