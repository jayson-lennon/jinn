//! The `[dictation]` section — local voice input.
//!
//! Four keys: `enabled` (default false — the feature ships off, and
//! enabling takes a restart because the section is read at slice
//! activation), `device` (an optional exact capture-device name), `model`
//! (the transcription model; an unknown value is an Error-state fact at
//! init, naming the value and the supported set), and `noise_reduction`
//! (default true — the nnnoiseless stage in the capture pipeline).
//!
//! The actor that runs the lifecycle lives in `jinn-dictation`. This
//! module is data: no backend, no audio I/O.

use serde::{Deserialize, Serialize};

/// The `[dictation]` section as configured in `jinn.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

fn default_model() -> String {
    jinn_dictation_default_model().to_owned()
}

fn default_noise_reduction() -> bool {
    true
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
        Self {
            enabled: false,
            device: None,
            model: default_model(),
            noise_reduction: true,
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
        // default, denoise on.
        assert!(!config.enabled);
        assert_eq!(config.device, None);
        assert_eq!(config.model, "parakeet-tdt-0.6b-v2");
        assert!(config.noise_reduction);
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
