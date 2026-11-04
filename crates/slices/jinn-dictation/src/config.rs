//! The slice's `[dictation]` config resolution.
//!
//! The schema lives in `jinn-preferences-config` beside every other
//! section; this module turns a resolved section into the settings the
//! actor and capture arm carry, naming the model the platform factory
//! will build.

use error_stack::Report;
use jinn_config::ConfigLayer;
use jinn_preferences_config::schemas::DictationConfig;
use wherror::Error;

/// The resolved `[dictation]` settings the actor carries.
#[derive(Debug, Clone)]
pub struct DictationSettings {
    /// Whether the lifecycle runs at all.
    pub enabled: bool,
    /// The configured capture device's exact name, or `None` for the
    /// system default input.
    pub device: Option<String>,
    /// The configured model name.
    pub model: String,
    /// Whether the capture pipeline denoises before transcription.
    pub noise_reduction: bool,
    /// The backend tuning options the decode graph builds with.
    pub options: jinn_dictation_backend::BackendOptions,
}

/// The `[dictation]` section failed to read.
#[derive(Debug, Error)]
#[error(debug)]
pub struct DictationConfigError;

impl DictationSettings {
    /// Resolves the section from the configuration layer.
    ///
    /// An absent section reads as the schema's defaults (disabled), the
    /// same way every other section reads.
    ///
    /// # Errors
    ///
    /// Returns [`DictationConfigError`] when the section cannot be read
    /// from the layer — treated by the activation path as a launch-
    /// blocking wiring failure, the same as a malformed `[canvas]` bind.
    pub fn from_layer(
        config: &ConfigLayer,
    ) -> Result<Self, error_stack::Report<DictationConfigError>> {
        let section = config.get::<DictationConfig>().map_err(|error| {
            Report::new(DictationConfigError).attach(format!(
                "failed to read the [dictation] config section: {error:?}"
            ))
        })?;
        Ok(Self {
            enabled: section.enabled,
            device: section.device.clone(),
            model: section.model.clone(),
            noise_reduction: section.noise_reduction,
            options: jinn_dictation_backend::BackendOptions::from(&section),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code, panics are acceptable")]
    use super::DictationSettings;

    #[rstest::rstest]
    #[test]
    fn settings_resolve_from_an_empty_layer_as_disabled() {
        // Given an empty configuration layer.
        let layer = jinn_slices::empty_config_layer();

        // When resolving the dictation settings.
        let settings = DictationSettings::from_layer(layer).expect("empty layer reads defaults");

        // Then the section's defaults apply: off, default model, denoise
        // on, no device, and the tuning options at their shipped
        // values.
        assert!(!settings.enabled);
        assert_eq!(settings.device, None);
        assert_eq!(settings.model, "parakeet-tdt-0.6b-v2");
        assert!(settings.noise_reduction);
        assert_eq!(
            settings.options,
            jinn_dictation_backend::BackendOptions::default()
        );
    }
}
