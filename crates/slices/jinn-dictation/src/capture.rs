//! The capture arm — the microphone half of the pipeline.
//!
//! The arm opens the configured (or system-default) capture device on
//! cpal's own audio thread, denoises (when `noise_reduction` is on),
//! resamples to 16 kHz mono f32, and hands samples to the backend
//! through the shared mutex. The arm reports its level to the meter
//! cell and publishes a `capture-lost` tick when the stream ends
//! without a toggle.
//!
//! Platform shape: on linux and windows this module compiles the real
//! device open (cpal's PipeWire host on linux, its WASAPI host on
//! windows); every other platform compiles a stub whose `open` reports
//! the refusal vocabulary (the null backend transcribes nothing, so
//! there is nothing to arm). This is the integration's one platform
//! split, shared with `platform.rs` — no other `cfg` exists in the
//! slice.
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

/// The shared backend slot: the capture arm's audio callback and the
/// actor's blocking closures pass the backend through this mutex,
/// because `DictationBackend` methods take `&mut self` and the calls
/// run off their callers' contexts.
pub type SharedBackend =
    std::sync::Arc<parking_lot::Mutex<Box<dyn jinn_dictation_backend::DictationBackend>>>;

/// Opens the capture arm — the device seam the actor holds.
///
/// The production opener is [`NativeCaptureOpener`], which delegates
/// to the cfg-swapped `device` module below; tests inject a scripted
/// fake, so no workspace test reaches a real input device. `open` is
/// deliberately synchronous: the actor runs it on the blocking
/// context (a hung driver must not stall the actor's async context),
/// so the trait is not async.
pub trait CaptureOpener: Send + Sync {
    /// Opens the configured (or system-default) input device, feeding
    /// the audio pipeline through the shared backend.
    ///
    /// # Errors
    ///
    /// Returns the rendered refusal note when the device cannot be
    /// found or the stream cannot be built.
    fn open(
        &self,
        configured: Option<&str>,
        noise_reduction: bool,
        backend: &SharedBackend,
    ) -> Result<CaptureArm, String>;

    /// Returns the opener's name, for debugging.
    fn name(&self) -> &'static str;
}

/// The production opener: the cfg-swapped `device::open`.
///
/// Platform-blind by construction — on linux and windows `device` is
/// the native module, elsewhere the null module whose `open` always
/// refuses; this impl names neither platform.
pub struct NativeCaptureOpener;

impl CaptureOpener for NativeCaptureOpener {
    fn open(
        &self,
        configured: Option<&str>,
        noise_reduction: bool,
        backend: &SharedBackend,
    ) -> Result<CaptureArm, String> {
        device::open(configured, noise_reduction, backend)
    }

    fn name(&self) -> &'static str {
        "native"
    }
}

/// What the audio callback writes and the actor's tick drains.
///
/// Platform-blind: every platform carries the same slot; only the
/// native callback ever writes it.
#[derive(Default)]
pub struct CaptureShared {
    /// The latest peak level, `0.0..=1.0`, for the meter cell.
    pub level: f32,
    /// Set once when the stream dies without a toggle; the actor
    /// reads and clears it on its tick.
    pub lost: Option<String>,
    /// Utterances the backend closed during recording, in the order
    /// they were spoken. The audio callback pushes; the actor's tick
    /// drains and commits each to the focused input. Written only
    /// through [`push_utterances`], read only through
    /// [`take_utterances`].
    pub utterances: Vec<jinn_dictation_backend::Utterance>,
}

/// A live capture arm.
///
/// On the native platforms it holds the device closer (dropping the
/// boxed stream ends the audio thread); on the null platform it is
/// never constructed — `open` always refuses. The closer is a boxed
/// `Any` so the shared type names no platform crate.
pub struct CaptureArm {
    /// The device closer (native platforms only; the null platform
    /// never builds one). Never read: its Drop *is* the close — that
    /// is the point.
    #[expect(
        dead_code,
        reason = "the field exists for its Drop; the close is the drop"
    )]
    closer: Option<Box<dyn std::any::Any + Send>>,
    /// The shared level/loss slot.
    shared: std::sync::Arc<parking_lot::Mutex<CaptureShared>>,
}

impl CaptureArm {
    /// The arm's shared level/loss slot, for the actor's tick.
    #[must_use]
    pub fn shared(&self) -> &std::sync::Arc<parking_lot::Mutex<CaptureShared>> {
        &self.shared
    }

    /// Builds an arm over a caller-owned shared slot, for tests.
    ///
    /// No closer: the fake arm has no device to close. Production arms
    /// are always built by `device::open`, which sets the real closer.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn for_tests(shared: std::sync::Arc<parking_lot::Mutex<CaptureShared>>) -> Self {
        Self {
            closer: None,
            shared,
        }
    }
}

/// Writes the slot's level into the meter cell.
pub fn sync_level(
    shared: &parking_lot::Mutex<CaptureShared>,
    level_cell: &jinn_slices::TypedCell<jinn_dictation_msg::DictationLevel>,
) {
    let level = shared.lock().level;
    level_cell.update(|cell| cell.level = level);
}

/// Drains the loss reason, if the stream died without a toggle.
#[must_use]
pub fn take_lost(shared: &parking_lot::Mutex<CaptureShared>) -> Option<String> {
    shared.lock().lost.take()
}

/// Queues the utterances the backend closed on one feed, preserving
/// the order they were spoken. The audio callback's write side.
pub fn push_utterances(
    shared: &parking_lot::Mutex<CaptureShared>,
    utterances: Vec<jinn_dictation_backend::Utterance>,
) {
    shared.lock().utterances.extend(utterances);
}

/// Drains every queued utterance, oldest first. The actor's tick
/// read side: each returned utterance is committed to the focused
/// input and never queued again.
#[must_use]
pub fn take_utterances(
    shared: &parking_lot::Mutex<CaptureShared>,
) -> Vec<jinn_dictation_backend::Utterance> {
    std::mem::take(&mut shared.lock().utterances)
}

// The platform split, same shape as `platform.rs`: the native body in
// `capture_native.rs`, the null body in `capture_null.rs`, swapped by
// one `cfg` pair — the integration's other platform gate, beside
// `platform.rs`'s own. Only `open` differs; the arm, the shared slot,
// and the level/loss vocabulary above are platform-blind.
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[path = "capture_native.rs"]
mod device;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
#[path = "capture_null.rs"]
mod device;

pub use device::default_input_device;
pub use device::list_input_devices;
pub use device::open;

#[cfg(test)]
mod tests {
    use super::CaptureOpener;
    use super::arm_refusal_note;
    use super::push_utterances;
    use super::take_utterances;
    use jinn_dictation_backend::Utterance;
    use parking_lot::Mutex;

    #[rstest::rstest]
    #[test]
    fn native_opener_reports_its_name() {
        // Given the production opener.
        let opener = super::NativeCaptureOpener;

        // When asking for its debug name.
        let name = opener.name();

        // Then it is the native opener.
        assert_eq!(name, "native");
    }

    #[rstest::rstest]
    #[test]
    fn capture_shared_queues_utterances_in_fifo_order() {
        // Given a capture shared slot with two queued utterances.
        let shared = Mutex::new(super::CaptureShared::default());
        push_utterances(
            &shared,
            vec![
                Utterance::new("first.".to_owned()),
                Utterance::new("second.".to_owned()),
            ],
        );

        // When draining the queue, then draining again.
        let first_take = take_utterances(&shared);
        let second_take = take_utterances(&shared);

        // Then the first drain returns both, in the order spoken.
        assert_eq!(
            first_take,
            vec![
                Utterance::new("first.".to_owned()),
                Utterance::new("second.".to_owned()),
            ]
        );
        // And the second drain is empty — the take clears the queue.
        assert_eq!(second_take, [] as [Utterance; 0]);
    }

    #[rstest::rstest]
    #[test]
    fn refusal_names_the_configured_device_and_the_available_inputs() {
        // Given a configured device name that matches nothing.
        let note = arm_refusal_note(
            Some("usb-mic"),
            &["builtin".to_owned(), "webcam".to_owned()],
        );

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
