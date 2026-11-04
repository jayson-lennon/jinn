//! The platform split — the dictation integration's ONE `cfg`-gated
//! module.
//!
//! Everything platform-specific about dictation lives here and only
//! here: which backend the factory builds, and whether a real backend
//! exists at all (the query `jinn fetch` consults for its non-zero
//! exit). The state machine, the actor, the route rows, the meter, and
//! the config schema are all platform-blind; this module is the seam
//! between them and the native world.
//!
//! The manifest side of the same gate is the linux target-scoped
//! dependency table in this crate's `Cargo.toml`.

/// The model names the platform's backend can run, for the
/// unknown-model validation at init.
pub const SUPPORTED_MODELS: &[&str] = &["parakeet-tdt-0.6b-v2"];

/// The `[dictation] model` value the shipped default names.
pub const DEFAULT_MODEL: &str = "parakeet-tdt-0.6b-v2";

/// Whether this platform runs a real transcription backend.
#[must_use]
pub fn real_backend_available() -> bool {
    true
}

/// Builds the platform's backend for the configured model name and
/// tuning options: the real Parakeet backend, backed by sherpa-onnx,
/// Silero VAD, and jinn's own model cache. The options apply when the
/// graph builds (on load), so they take effect on jinn's next start.
#[must_use]
pub fn build_backend(
    model: &str,
    options: jinn_dictation_backend::BackendOptions,
) -> Box<dyn jinn_dictation_backend::DictationBackend> {
    Box::new(jinn_dictation_backend_parakeet::ParakeetBackend::with_options(model, options))
}
