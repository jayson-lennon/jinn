//! The real dictation backend: NVIDIA Parakeet TDT 0.6B v2 (int8 ONNX)
//! via sherpa-onnx.
//!
//! The backend owns its own utterance endpointing, per the
//! [`DictationBackend`] contract: Silero VAD gates the audio into
//! utterances (512-sample frames at 16 kHz), each closed utterance is
//! batch-decoded by the offline transducer, and
//! [`DictationBackend::feed`] returns every utterance closed since the
//! previous call. [`DictationBackend::flush`] force-closes the VAD's
//! open speech so a disarm inserts the trailing fragment.
//!
//! The capture pipeline's denoise (nnnoiseless) and resample stages
//! run *before* the backend sees audio, in the dictation slice's
//! capture arm; the VAD here is the endpointing decision, not a noise
//! filter.
//!
//! The model fetch goes through hf-hub with the cache redirected to
//! jinn's own AppPaths-derived directory — nothing lands in
//! `~/.cache/huggingface`.
//!
//! This crate compiles on linux and windows targets only; the
//! dictation slice's target-scoped dependency tables are the only edge
//! into it.

mod backend;

pub use backend::ParakeetBackend;
