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

/// What the audio callback writes and the actor's tick drains.
///
/// Platform-blind: both platforms carry the same slot; only the linux
/// callback ever writes it.
#[derive(Default)]
pub struct CaptureShared {
    /// The latest peak level, `0.0..=1.0`, for the meter cell.
    pub level: f32,
    /// Set once when the stream dies without a toggle; the actor
    /// reads and clears it on its tick.
    pub lost: Option<String>,
}

/// A live capture arm.
///
/// On linux it holds the device closer (dropping the boxed stream ends
/// the audio thread); on the null platform it is never constructed —
/// `open` always refuses. The closer is a boxed `Any` so the shared
/// type names no platform crate.
pub struct CaptureArm {
    /// The device closer (linux only; the null platform never builds
    /// one). Never read: its Drop *is* the close — that is the point.
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

// The platform split, same shape as `platform.rs`: the linux body in
// `capture_linux.rs`, the null body in `capture_null.rs`, swapped by
// one `cfg` pair — the integration's other platform gate, beside
// `platform.rs`'s own. Only `open` differs; the arm, the shared slot,
// and the level/loss vocabulary above are platform-blind.
#[cfg(target_os = "linux")]
#[path = "capture_linux.rs"]
mod device;

#[cfg(not(target_os = "linux"))]
#[path = "capture_null.rs"]
mod device;

pub use device::open;

#[cfg(test)]
mod tests {
    use super::arm_refusal_note;

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
