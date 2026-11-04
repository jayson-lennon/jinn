//! The decodable-audio guarantee — recognizer input that cannot be
//! too short to transcribe.
//!
//! Speech recognizers extract fbank features in fixed frames (sherpa's
//! defaults: 25 ms at 16 kHz = 400 samples per frame). Audio shorter
//! than one frame yields zero frames, and a zero-frame batch through a
//! quantized encoder aborts the process from inside the native engine
//! — a C++ exception no Rust layer can catch. [`DecodableAudio`] makes
//! that input unrepresentable: construction is the validation, and
//! every decode path in jinn takes the type.

/// Mono 16 kHz audio ready for recognition: guaranteed to hold at
/// least one fbank feature frame ([`DecodableAudio::MIN_SAMPLES`]).
///
/// Construction is the validation: [`DecodableAudio::new`] refuses
/// shorter audio, so no instance wrapping too-few samples can exist.
/// The caller decides what short audio means — park it, drop it —
/// never the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodableAudio {
    samples: Vec<f32>,
}

impl DecodableAudio {
    /// The one-feature-frame floor, derived from sherpa-onnx's
    /// `FeatureExtractorConfig` defaults: `frame_length_ms = 25.0` at
    /// 16 kHz → 400 samples per frame. Below any engine's useful
    /// input, and 10× under the default `min_speech_duration`
    /// (250 ms), so no real utterance is ever this short.
    pub const MIN_SAMPLES: usize = 400;

    /// Wraps `samples` for recognition, refusing audio shorter than
    /// [`Self::MIN_SAMPLES`].
    ///
    /// Takes ownership rather than borrowing so a rejected slice is
    /// simply dropped with the rejected wrapper — the hot audio path
    /// stays allocation-free.
    #[must_use]
    pub fn new(samples: Vec<f32>) -> Option<Self> {
        (samples.len() >= Self::MIN_SAMPLES).then_some(Self { samples })
    }

    /// Appends more audio. Length only ever grows, so a valid instance
    /// stays valid — no re-validation, and none possible to need.
    pub fn extend(&mut self, samples: &[f32]) {
        self.samples.extend_from_slice(samples);
    }

    /// The wrapped audio, for the backend's engine.
    #[must_use]
    pub fn as_samples(&self) -> &[f32] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "test code, panics are acceptable"
    )]
    use super::DecodableAudio;

    #[rstest::rstest]
    #[case(0)]
    #[case(399)]
    fn new_refuses_audio_below_one_feature_frame(#[case] len: usize) {
        // Given audio shorter than one feature frame.
        let samples = vec![0.0_f32; len];

        // When constructing decodable audio.
        let audio = DecodableAudio::new(samples);

        // Then construction refuses it — the recognizer can never see it.
        assert_eq!(audio, None);
    }

    #[rstest::rstest]
    #[case(DecodableAudio::MIN_SAMPLES)]
    #[case(1_600)]
    fn new_accepts_audio_at_or_above_one_feature_frame(#[case] len: usize) {
        // Given audio of at least one feature frame.
        let samples = vec![0.0_f32; len];

        // When constructing decodable audio.
        let audio = DecodableAudio::new(samples).expect("audio at or above the floor constructs");

        // Then the wrapper holds exactly the given samples.
        assert_eq!(audio.as_samples().len(), len);
    }

    #[rstest::rstest]
    #[test]
    fn extend_preserves_a_valid_instance() {
        // Given valid decodable audio.
        let mut audio = DecodableAudio::new(vec![0.0_f32; DecodableAudio::MIN_SAMPLES])
            .expect("floor-length audio constructs");

        // When appending more audio — even nothing at all.
        audio.extend(&[0.5_f32; 100]);

        // Then the instance stays valid and grows, with the appended
        // samples at the tail.
        assert_eq!(audio.as_samples().len(), DecodableAudio::MIN_SAMPLES + 100);
        assert_eq!(audio.as_samples()[DecodableAudio::MIN_SAMPLES], 0.5);
    }

    #[rstest::rstest]
    #[test]
    fn as_samples_returns_the_wrapped_slice() {
        // Given decodable audio over known samples.
        let samples = vec![0.25_f32; DecodableAudio::MIN_SAMPLES];
        let audio = DecodableAudio::new(samples.clone()).expect("floor-length audio constructs");

        // When reading the wrapped slice.
        // Then it is exactly the samples the instance was built from.
        assert_eq!(audio.as_samples(), samples.as_slice());
    }
}
