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
//!
//! The gated twin of this module is `platform_linux.rs`, swapped in by
//! the single `cfg` attribute in `lib.rs`.
#[cfg(not(target_os = "linux"))]
mod imp {
    /// The model names the platform's backend can run, for the
    /// unknown-model validation at init.
    ///
    /// The real backend names Parakeet TDT 0.6B v2 (int8); the null path
    /// carries the same name so a configured default validates
    /// identically on every platform.
    pub const SUPPORTED_MODELS: &[&str] = &["parakeet-tdt-0.6b-v2"];

    /// The `[dictation] model` value the shipped default names.
    pub const DEFAULT_MODEL: &str = "parakeet-tdt-0.6b-v2";

    /// Whether this platform runs a real transcription backend.
    ///
    /// `false` on the null path; `jinn fetch dictation` reads this to
    /// print "unavailable" and exit non-zero.
    #[must_use]
    pub fn real_backend_available() -> bool {
        false
    }

    /// Builds the platform's backend for the configured model name.
    ///
    /// The null build returns [`jinn_dictation_backend::NullBackend`],
    /// which succeeds trivially at everything: the lifecycle runs
    /// identically and transcribes nothing.
    #[must_use]
    pub fn build_backend(model: &str) -> Box<dyn jinn_dictation_backend::DictationBackend> {
        Box::new(jinn_dictation_backend::NullBackend::new(model))
    }
}

#[cfg(target_os = "linux")]
#[path = "platform_linux.rs"]
mod imp;

pub use imp::{DEFAULT_MODEL, SUPPORTED_MODELS, build_backend, real_backend_available};

/// Whether this platform runs a real transcription backend.
///
/// `jinn fetch dictation-model` reads this for its non-zero exit on
/// platforms without one.
#[must_use]
pub fn platform_supports_real_backend() -> bool {
    real_backend_available()
}

/// The default model name this platform's backend understands.
#[must_use]
pub fn platform_default_model() -> String {
    DEFAULT_MODEL.to_owned()
}

/// Fetches the named model into jinn's cache through the platform
/// backend: the same `fetch` the actor runs, driven from the CLI.
///
/// # Errors
///
/// Returns the backend's fetch report on network or disk failure.
pub fn fetch_model(
    model: &str,
) -> Result<(), error_stack::Report<jinn_dictation_backend::BackendError>> {
    let mut backend = build_backend(model);
    let mut no_progress = |_: jinn_dictation_backend::FetchProgress| {};
    backend.fetch(&mut no_progress)
}
