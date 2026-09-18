//! Retry-policy conversion from the user-preferences schema to provider types.

use jinn_preferences_config::schemas::RequestRetryConfig;

/// Converts the user-preferences retry policy into the provider crate's
/// [`jinn_provider::RetryConfig`].
///
/// A free function (not an inherent method) because the config type lives in
/// `jinn-preferences-config`, which must not depend on `jinn-provider`; this
/// conversion is infra-side behavior over the shared schema.
#[must_use]
pub fn request_retry_to_provider_config(config: &RequestRetryConfig) -> jinn_provider::RetryConfig {
    jinn_provider::RetryConfig {
        max_retries: config.max_retries,
        base_delay: std::time::Duration::from_secs(config.base_delay_secs),
        max_delay: std::time::Duration::from_secs(config.max_delay_secs),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "test code")]

    use super::*;

    #[rstest::rstest]
    fn request_retry_to_provider_config_uses_actual_values_not_defaults() {
        // If the conversion returned Default::default(), all durations would be zero.
        let config = RequestRetryConfig {
            max_retries: 3,
            base_delay_secs: 5,
            max_delay_secs: 120,
        };

        let retry = request_retry_to_provider_config(&config);

        assert_eq!(retry.max_retries, 3);
        assert_eq!(retry.base_delay, std::time::Duration::from_secs(5));
        assert_eq!(retry.max_delay, std::time::Duration::from_mins(2));
    }
}
