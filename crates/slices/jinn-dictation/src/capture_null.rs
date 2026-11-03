//! The null-platform capture body: there is no real backend to
//! transcribe, so arming always refuses with the standard vocabulary.
//! The twin of `capture_linux.rs`, selected by the one `cfg` pair in
//! `capture.rs`.

use super::CaptureArm;
use super::arm_refusal_note;

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
