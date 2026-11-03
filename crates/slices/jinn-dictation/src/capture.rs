//! The capture arm — the microphone half of the pipeline.
//!
//! The arm opens the configured (or system-default) capture device on
//! cpal's own audio thread (wired with the capture unit), denoises
//! (when `noise_reduction` is on), resamples to 16 kHz mono f32, and
//! feeds the backend through the actor's blocking context. The slice
//! never names cpal: `open` here is the platform-blind seam the device
//! implementation plugs into, and its refusal vocabulary is what the
//! actor's arm-failure path renders.
//!
//! This unit carries the refusal vocabulary and the level report shape;
//! the cpal device open lands with the capture unit, so the
//! platform-blind suite never needs an audio device.

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

/// Opens the capture arm.
///
/// The unit-2 skeleton reports the configured device unavailable when
/// the device layer has not been wired; the capture unit replaces this
/// body with the real device open. A refusal names the configured
/// device and the available inputs (the actor renders it through
/// [`arm_refusal_note`]'s vocabulary), and the machine's arm-failure
/// path pulls the state back to Ready.
///
/// # Errors
///
/// Returns the refusal note when the device cannot be opened.
pub fn open(
    configured: Option<&str>,
    _noise_reduction: bool,
) -> Result<(), String> {
    Err(arm_refusal_note(configured, &[]))
}

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
