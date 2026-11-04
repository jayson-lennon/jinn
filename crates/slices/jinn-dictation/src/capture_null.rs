//! The null-platform capture body: there is no real backend to
//! transcribe, so arming always refuses with the standard vocabulary.
//! The twin of `capture_native.rs`, selected by the one `cfg` pair in
//! `capture.rs`.

use super::CaptureArm;
use super::arm_refusal_note;

/// Lists the capture input devices: always empty on the null platform.
///
/// The null path never opens a device (`open` refuses unconditionally),
/// so there is nothing to enumerate; the CLI prints the unavailability
/// sentence when the list comes back empty.
pub fn list_input_devices() -> Vec<String> {
    Vec::new()
}

/// The default input device's name: never on the null platform.
pub fn default_input_device() -> Option<String> {
    None
}

/// Opens the capture arm: always refuses on the null platform.
///
/// No `CaptureArm` is ever constructed here — the null backend
/// transcribes nothing, so there is nothing to arm.
///
/// # Errors
///
/// Always returns the refusal note naming the configured device and
/// the (empty) device list.
pub fn open(
    configured: Option<&str>,
    _noise_reduction: bool,
    _backend: &std::sync::Arc<
        parking_lot::Mutex<Box<dyn jinn_dictation_backend::DictationBackend>>,
    >,
) -> Result<CaptureArm, String> {
    Err(arm_refusal_note(configured, &[]))
}
