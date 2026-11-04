//! The `[dictation]` section — local voice input.
//!
//! Core keys: `enabled` (default false — the feature ships off, and
//! enabling takes a restart because the section is read at slice
//! activation), `device` (an optional exact capture-device name), `model`
//! (the transcription model; an unknown value is an Error-state fact at
//! init, naming the value and the supported set), and `noise_reduction`
//! (default true — the nnnoiseless stage in the capture pipeline).
//!
//! Tuning keys (all defaulted to the shipped behavior, all applied when
//! the backend builds its decode graph — a restart re-reads them):
//! `vad_threshold` and the three speech-duration keys shape endpointing
//! (when a spoken segment closes into text); `decoding_method`,
//! `max_active_paths`, `blank_penalty`, `num_threads`, `hotwords_file`,
//! `hotwords_score`, and `rule_fsts` shape the decode itself.
//!
//! The actor that runs the lifecycle lives in `jinn-dictation`. This
//! module is data: no backend, no audio I/O.

use jinn_dictation_backend::BackendOptions;
use serde::{Deserialize, Serialize};

/// The `[dictation]` section as configured in `jinn.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DictationConfig {
    /// Whether dictation loads its backend at startup. The shipped
    /// default is `false`: enabling fetches a ~650 MB model on first
    /// launch and takes effect on the next jinn start.
    #[serde(default)]
    pub enabled: bool,
    /// The capture device's exact name (from `pw-record --list-targets`
    /// or your sound server's device list). `None` uses the system
    /// default input.
    #[serde(default)]
    pub device: Option<String>,
    /// The transcription model. Unknown values fail at init into the
    /// Error state, naming the value and the supported set.
    #[serde(default = "default_model")]
    pub model: String,
    /// Whether the capture pipeline denoises (nnnoiseless) before
    /// transcription. On by default; the resample-to-16k stage runs
    /// either way.
    #[serde(default = "default_noise_reduction")]
    pub noise_reduction: bool,
    /// Voice-activity gate (0.0–1.0): how certain the detector must be
    /// that a sound is speech. Lower catches quieter speech; higher
    /// ignores more background noise.
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,
    /// Seconds of silence that close an utterance — the main "how long
    /// after I stop talking does text land" knob.
    #[serde(default = "default_min_silence_duration")]
    pub min_silence_duration: f32,
    /// Seconds of speech required before a segment counts as an
    /// utterance; shorter blips are dropped.
    #[serde(default = "default_min_speech_duration")]
    pub min_speech_duration: f32,
    /// Seconds of continuous speech at which a segment force-closes
    /// even without a pause.
    #[serde(default = "default_max_speech_duration")]
    pub max_speech_duration: f32,
    /// The decoder's search strategy: `modified_beam_search` (default,
    /// more accurate, slower) or `greedy_search` (faster, a little less
    /// accurate).
    #[serde(default = "default_decoding_method")]
    pub decoding_method: Option<String>,
    /// Beam width for `modified_beam_search`; ignored by greedy
    /// decoding.
    #[serde(default = "default_max_active_paths")]
    pub max_active_paths: i32,
    /// Penalty on blank tokens during decoding; higher biases against
    /// long silences inside words.
    #[serde(default = "default_blank_penalty")]
    pub blank_penalty: f32,
    /// ONNX Runtime threads per model: more decodes faster at higher
    /// CPU cost.
    #[serde(default = "default_num_threads")]
    pub num_threads: i32,
    /// Path to a hotwords file (one phrase per line) the decoder boosts
    /// — names, jargon, product terms the base model mangles.
    #[serde(default)]
    pub hotwords_file: Option<String>,
    /// Boost score for hotwords; higher biases harder toward them.
    /// Needs `hotwords_file` and beam search to matter.
    #[serde(default = "default_hotwords_score")]
    pub hotwords_score: f32,
    /// Paths to inverse-text-normalization FSTs (spoken numbers to
    /// digits, currency, dates), colon-separated.
    #[serde(default)]
    pub rule_fsts: Option<String>,
}

fn default_model() -> String {
    jinn_dictation_default_model().to_owned()
}

fn default_noise_reduction() -> bool {
    true
}

// The tuning defaults are spelled as functions rather than constants
// so every field's `#[serde(default = ...)]` names its source; each
// mirrors `BackendOptions::default()` so an absent key never changes
// what runs.
fn default_vad_threshold() -> f32 {
    BackendOptions::default().vad_threshold
}

fn default_min_silence_duration() -> f32 {
    BackendOptions::default().min_silence_duration
}

fn default_min_speech_duration() -> f32 {
    BackendOptions::default().min_speech_duration
}

fn default_max_speech_duration() -> f32 {
    BackendOptions::default().max_speech_duration
}

fn default_decoding_method() -> Option<String> {
    BackendOptions::default().decoding_method
}

fn default_max_active_paths() -> i32 {
    BackendOptions::default().max_active_paths
}

fn default_blank_penalty() -> f32 {
    BackendOptions::default().blank_penalty
}

fn default_num_threads() -> i32 {
    BackendOptions::default().num_threads
}

fn default_hotwords_score() -> f32 {
    BackendOptions::default().hotwords_score
}

/// The default model name.
///
/// Indirected through a function rather than inlined so the schema crate
/// stays platform-blind: the name itself comes from the dictation
/// slice's platform module, re-exported here at compile time via the
/// constant below to avoid a dependency arrow from config → slice.
pub const DICTATION_DEFAULT_MODEL: &str = "parakeet-tdt-0.6b-v2";

fn jinn_dictation_default_model() -> &'static str {
    DICTATION_DEFAULT_MODEL
}

impl Default for DictationConfig {
    fn default() -> Self {
        let options = BackendOptions::default();
        Self {
            enabled: false,
            device: None,
            model: default_model(),
            noise_reduction: true,
            vad_threshold: options.vad_threshold,
            min_silence_duration: options.min_silence_duration,
            min_speech_duration: options.min_speech_duration,
            max_speech_duration: options.max_speech_duration,
            decoding_method: options.decoding_method,
            max_active_paths: options.max_active_paths,
            blank_penalty: options.blank_penalty,
            num_threads: options.num_threads,
            hotwords_file: options.hotwords_file,
            hotwords_score: options.hotwords_score,
            rule_fsts: options.rule_fsts,
        }
    }
}

impl From<&DictationConfig> for BackendOptions {
    fn from(config: &DictationConfig) -> Self {
        Self {
            vad_threshold: config.vad_threshold,
            min_silence_duration: config.min_silence_duration,
            min_speech_duration: config.min_speech_duration,
            max_speech_duration: config.max_speech_duration,
            decoding_method: config.decoding_method.clone(),
            max_active_paths: config.max_active_paths,
            blank_penalty: config.blank_penalty,
            num_threads: config.num_threads,
            hotwords_file: config.hotwords_file.clone(),
            hotwords_score: config.hotwords_score,
            rule_fsts: config.rule_fsts.clone(),
        }
    }
}

impl jinn_config::Configurable for DictationConfig {
    const KEY: &'static str = "dictation";
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code, panics are acceptable")]

    use super::DictationConfig;
    use serde::Deserialize;

    fn from_table(raw: &str) -> DictationConfig {
        #[derive(Deserialize)]
        struct Wrapper {
            dictation: DictationConfig,
        }
        toml::from_str::<Wrapper>(raw)
            .expect("dictation table parses")
            .dictation
    }

    #[rstest::rstest]
    #[test]
    fn absent_section_reads_as_disabled_with_defaults() {
        // Given an empty `[dictation]` table.
        let config = from_table("[dictation]\n");

        // Then every default applies: off, no device, the parakeet
        // default, denoise on, and every tuning knob at its shipped
        // value.
        assert!(!config.enabled);
        assert_eq!(config.device, None);
        assert_eq!(config.model, "parakeet-tdt-0.6b-v2");
        assert!(config.noise_reduction);
        let default_options = jinn_dictation_backend::BackendOptions::default();
        assert_eq!(config.vad_threshold, default_options.vad_threshold);
        assert_eq!(
            config.min_silence_duration,
            default_options.min_silence_duration
        );
        assert_eq!(
            config.min_speech_duration,
            default_options.min_speech_duration
        );
        assert_eq!(
            config.max_speech_duration,
            default_options.max_speech_duration
        );
        assert_eq!(config.decoding_method, default_options.decoding_method);
        assert_eq!(config.max_active_paths, default_options.max_active_paths);
        assert_eq!(config.blank_penalty, default_options.blank_penalty);
        assert_eq!(config.num_threads, default_options.num_threads);
        assert_eq!(config.hotwords_file, None);
        assert_eq!(config.hotwords_score, default_options.hotwords_score);
        assert_eq!(config.rule_fsts, None);
    }

    #[rstest::rstest]
    #[test]
    fn tuning_keys_round_trip_through_toml() {
        // Given a section with every tuning key set.
        let config = from_table(
            "[dictation]\n\
             vad_threshold = 0.7\n\
             min_silence_duration = 0.3\n\
             min_speech_duration = 0.4\n\
             max_speech_duration = 30.0\n\
             decoding_method = \"greedy_search\"\n\
             max_active_paths = 8\n\
             blank_penalty = 0.2\n\
             num_threads = 4\n\
             hotwords_file = \"/home/me/words.txt\"\n\
             hotwords_score = 2.5\n\
             rule_fsts = \"/home/me/itn.fst\"\n",
        );

        // When reading the keys back.
        // Then every knob survives verbatim.
        assert_eq!(config.vad_threshold, 0.7);
        assert_eq!(config.min_silence_duration, 0.3);
        assert_eq!(config.min_speech_duration, 0.4);
        assert_eq!(config.max_speech_duration, 30.0);
        assert_eq!(config.decoding_method.as_deref(), Some("greedy_search"));
        assert_eq!(config.max_active_paths, 8);
        assert_eq!(config.blank_penalty, 0.2);
        assert_eq!(config.num_threads, 4);
        assert_eq!(config.hotwords_file.as_deref(), Some("/home/me/words.txt"));
        assert_eq!(config.hotwords_score, 2.5);
        assert_eq!(config.rule_fsts.as_deref(), Some("/home/me/itn.fst"));

        // And the conversion into the backend options carries the same
        // values — config is the source, the options are the shape the
        // backend reads.
        let options = jinn_dictation_backend::BackendOptions::from(&config);
        assert_eq!(
            options,
            jinn_dictation_backend::BackendOptions {
                vad_threshold: 0.7,
                min_silence_duration: 0.3,
                min_speech_duration: 0.4,
                max_speech_duration: 30.0,
                decoding_method: Some("greedy_search".to_owned()),
                max_active_paths: 8,
                blank_penalty: 0.2,
                num_threads: 4,
                hotwords_file: Some("/home/me/words.txt".to_owned()),
                hotwords_score: 2.5,
                rule_fsts: Some("/home/me/itn.fst".to_owned()),
            }
        );
    }

    #[rstest::rstest]
    #[test]
    fn enabled_section_carries_every_key() {
        // Given a fully configured section.
        let config = from_table(
            "[dictation]\nenabled = true\ndevice = \"usb-mic\"\nmodel = \"parakeet-tdt-0.6b-v2\"\nnoise_reduction = false\n",
        );

        // When reading it back.
        // Then every key survives the round trip.
        assert!(config.enabled);
        assert_eq!(config.device.as_deref(), Some("usb-mic"));
        assert_eq!(config.model, "parakeet-tdt-0.6b-v2");
        assert!(!config.noise_reduction);
    }

    #[rstest::rstest]
    #[test]
    fn serializing_the_default_round_trips() {
        #[derive(Deserialize)]
        struct Repr {
            dictation: DictationConfig,
        }

        // Given the default config.
        let config = DictationConfig::default();

        // When serializing then re-parsing.
        let s = toml::to_string(&config).expect("serialize");
        let reparsed: DictationConfig = toml::from_str::<Repr>(&format!("[dictation]\n{s}"))
            .expect("reparse")
            .dictation;

        // Then it equals the original.
        assert_eq!(config, reparsed);
    }
}
