//! Backend tuning options, resolved from `[dictation]` in `jinn.toml`.
//!
//! Pure data: every field carries a default equal to the shipped
//! behavior, so an absent key never changes what runs. The values are
//! engine knobs, not commands — a backend reads them when it builds its
//! decode graph and is free to ignore any that do not apply to its
//! engine. They take effect when the backend loads (a jinn restart
//! re-reads them).

/// The tuning knobs a speech-to-text backend exposes, as configured.
///
/// Defaults mirror the values jinn shipped before this struct existed,
/// so `BackendOptions::default()` is behavior-preserving.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendOptions {
    /// The voice-activity detector's speech-probability gate (0.0–1.0).
    /// Lower catches quieter, mumblier speech; higher demands clearer
    /// speech and ignores background noise. Default 0.5.
    pub vad_threshold: f32,
    /// Seconds of silence that close an utterance. The main
    /// "how long after I stop talking does text land" knob. Default 0.5.
    pub min_silence_duration: f32,
    /// Seconds of speech required before a segment counts as an
    /// utterance; shorter blips are dropped. Default 0.25.
    pub min_speech_duration: f32,
    /// Seconds of continuous speech at which a segment is force-closed
    /// even without a pause. Default 20.0.
    pub max_speech_duration: f32,
    /// The decoder's search strategy: "modified_beam_search" (the
    /// default, more accurate, slower) or "greedy_search" (faster, a
    /// little less accurate). An empty string uses the engine default.
    pub decoding_method: Option<String>,
    /// Beam width for `modified_beam_search`. Ignored by greedy
    /// decoding. Default 4.
    pub max_active_paths: i32,
    /// Penalty applied to blank tokens during decoding; higher biases
    /// against long silences inside words. Default 0.0.
    pub blank_penalty: f32,
    /// ONNX Runtime threads per model. More threads decode faster at
    /// higher CPU cost. Default 2.
    pub num_threads: i32,
    /// Path to a hotwords file (one phrase per line) the decoder boosts
    /// — names, jargon, product terms the base model mangles. `None`
    /// disables boosting.
    pub hotwords_file: Option<String>,
    /// Boost score for hotwords; higher biases harder toward them.
    /// Meaningful only with `hotwords_file` and beam search. Default 1.5.
    pub hotwords_score: f32,
    /// Paths to inverse-text-normalization FSTs (spoken numbers to
    /// digits, currency, dates), colon-separated. `None` disables it.
    pub rule_fsts: Option<String>,
}

impl Default for BackendOptions {
    fn default() -> Self {
        Self {
            vad_threshold: 0.5,
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            max_speech_duration: 20.0,
            decoding_method: Some("modified_beam_search".to_owned()),
            max_active_paths: 4,
            blank_penalty: 0.0,
            num_threads: 2,
            hotwords_file: None,
            hotwords_score: 1.5,
            rule_fsts: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BackendOptions;

    #[rstest::rstest]
    #[test]
    fn defaults_match_the_shipped_hard_coded_values() {
        // Given the default options.
        let options = BackendOptions::default();

        // Then every value is the constant the backend shipped with —
        // an absent config key must not change what runs.
        assert_eq!(options.vad_threshold, 0.5);
        assert_eq!(options.min_silence_duration, 0.5);
        assert_eq!(options.min_speech_duration, 0.25);
        assert_eq!(options.max_speech_duration, 20.0);
        assert_eq!(
            options.decoding_method.as_deref(),
            Some("modified_beam_search")
        );
        assert_eq!(options.max_active_paths, 4);
        assert_eq!(options.blank_penalty, 0.0);
        assert_eq!(options.num_threads, 2);
        assert_eq!(options.hotwords_file, None);
        assert_eq!(options.hotwords_score, 1.5);
        assert_eq!(options.rule_fsts, None);
    }
}
