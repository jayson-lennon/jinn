//! The dictation backend contract — one trait any speech-to-text engine
//! implements.
//!
//! This crate is deliberately pure: no actor, no cell, no kernel, no TUI,
//! no native audio or ML dependency. A future backend (whisper.cpp, a
//! streaming Parakeet, a cloud engine) compiles against this trait on any
//! platform, which is what keeps the rest of the dictation integration
//! platform-blind.
//!
//! # The contract
//!
//! A backend owns its model lifecycle and its own utterance endpointing:
//!
//! - [`DictationBackend::model_name`] — the display name, for hints and
//!   the dashboard row.
//! - [`DictationBackend::is_cached`] / [`DictationBackend::is_loaded`] —
//!   the two checks the state machine's init consults.
//! - [`DictationBackend::fetch`] — download the model, reporting progress
//!   through the callback the caller supplies.
//! - [`DictationBackend::load`] — make the model resident.
//! - [`DictationBackend::feed`] — hand the backend 16 kHz mono f32 PCM,
//!   wrapped in a [`DecodableAudio`] that guarantees at least one
//!   feature frame (construction is the validation — an engine can
//!   never receive the empty/short batch that aborts a quantized
//!   encoder); it returns every utterance its own endpointing has
//!   *closed* since the previous call. Nothing here detects silence or
//!   utterance boundaries — that decision belongs to the backend,
//!   because the right endpointing differs per engine.
//! - [`DictationBackend::flush`] — force-close any open utterance and
//!   return its text if one was open. This is the disarm path's trailing
//!   fragment.
//! - [`DictationBackend::reset`] — wipe every trace of the current
//!   session's endpointing state, so the next session behaves as if the
//!   graph were freshly loaded.
//!
//! Every fallible method returns `error_stack::Report<BackendError>`.

mod decodable_audio;
mod null_backend;
mod options;
mod utterance;

pub use decodable_audio::DecodableAudio;
pub use null_backend::NullBackend;
pub use options::BackendOptions;
pub use utterance::{FetchProgress, Utterance};

use error_stack::Report;

/// Why a backend operation failed.
///
/// A backend is a local component: every failure is one of a missing or
/// broken model artifact, a failed download, or a decode error. The
/// report's attachments carry the specifics; the type exists so callers
/// have one error to name.
#[derive(Debug, wherror::Error)]
#[error(debug)]
pub struct BackendError;

/// A local speech-to-text backend.
///
/// Implementors keep model state inside themselves; the trait object is
/// held by the dictation actor and driven from a blocking context (the
/// actor hops calls onto `spawn_blocking`). One instance serves one
/// process run.
pub trait DictationBackend: Send {
    /// The backend's model display name (e.g. `"parakeet-tdt-0.6b-v2"`).
    ///
    /// Surfaces on hints ("Downloading …") and the dashboard row.
    fn model_name(&self) -> &str;

    /// Whether the model's files are already in the local cache.
    ///
    /// `true` lets the state machine's init skip fetching and go straight
    /// to loading.
    fn is_cached(&self) -> bool;

    /// Whether the model is currently resident and ready to transcribe.
    ///
    /// Re-checking before every load is the idempotence seam: loading an
    /// already-loaded backend must be cheap and must succeed.
    fn is_loaded(&self) -> bool;

    /// Downloads the model into the backend's cache.
    ///
    /// `on_progress` receives periodic updates (bytes done / bytes total);
    /// `total` is `None` while the size is unknown. Progress may be called
    /// from any thread the backend chooses; the callback must be cheap and
    /// must not block.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the download fails or is
    /// unrecoverable (network, checksum, disk). A partially downloaded
    /// model is not cached: [`Self::is_cached`] stays `false` after a
    /// failed fetch.
    fn fetch(
        &mut self,
        on_progress: &mut dyn FnMut(FetchProgress),
    ) -> Result<(), Report<BackendError>>;

    /// Loads the model into memory, ready to transcribe.
    ///
    /// Loading a cached-but-not-loaded backend reads every model file
    /// once; loading an already-loaded backend is a no-op that succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the cached files are missing or
    /// cannot be decoded.
    fn load(&mut self) -> Result<(), Report<BackendError>>;

    /// Feeds the backend more audio and collects closed utterances.
    ///
    /// `audio` is 16 kHz mono f32 PCM, unit range (-1.0..1.0), and
    /// carries at least one feature frame by construction — a
    /// [`DecodableAudio`] cannot wrap fewer samples, so a backend may
    /// hand `audio.as_samples()` straight to its engine unchecked. The
    /// backend decides when an utterance has ended (its own endpointing
    /// — VAD, silence windows, engine-native end-of-utterance) and
    /// returns every utterance closed *since the previous call* in
    /// order. An utterance still open is not returned here;
    /// [`Self::flush`] closes it.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot process audio in
    /// its current state (typically: not loaded).
    fn feed(&mut self, audio: DecodableAudio) -> Result<Vec<Utterance>, Report<BackendError>>;

    /// Force-closes any open utterance and returns its text.
    ///
    /// `None` when nothing was open (or the open audio was empty). The
    /// disarm path calls this so the words spoken between the last natural
    /// endpoint and the keypress still land in the input buffer.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the backend cannot process audio in
    /// its current state.
    fn flush(&mut self) -> Result<Option<Utterance>, Report<BackendError>>;

    /// Clears every trace of the current dictation session's endpointing
    /// state — segment queue, open-utterance buffers, trailing-window
    /// tails, and the engine's internal counters — so the next session
    /// behaves as if the graph were freshly loaded.
    ///
    /// Must not unload the model or touch cached files, and must succeed
    /// as a no-op when the model is not loaded. Runs between sessions
    /// (the owner calls it on arm, and a flush leaves the backend
    /// already reset); never concurrently with [`Self::feed`].
    fn reset(&mut self);
}
