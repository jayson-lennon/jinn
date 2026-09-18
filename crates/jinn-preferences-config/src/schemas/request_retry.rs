//! LLM request-retry configuration schema — the `jinn.toml`
//! `[request_retry]` section.
//!
//! Pure serde data; the LLM actor (which converts it to the provider's
//! `RetryConfig` at stream time) stays in the kernel and imports the shape
//! from here.

use serde::{Deserialize, Serialize};

/// Default retry configuration values.
const DEFAULT_RETRY_MAX_RETRIES: u32 = 5;
const DEFAULT_RETRY_BASE_DELAY_SECS: u64 = 2;
const DEFAULT_RETRY_MAX_DELAY_SECS: u64 = 60;

/// Retry configuration for LLM provider requests.
///
/// Serialized as `[request_retry]` in `jinn.toml`.
/// Controls exponential backoff behavior for transient errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestRetryConfig {
    /// Maximum number of retry attempts. Default: 5.
    #[serde(default = "default_retry_max_retries")]
    pub max_retries: u32,
    /// Base delay in seconds for exponential backoff. Default: 2.
    #[serde(default = "default_retry_base_delay_secs")]
    pub base_delay_secs: u64,
    /// Maximum delay cap in seconds. Default: 60.
    /// Overridden by provider-supplied Retry-After / error body hints.
    #[serde(default = "default_retry_max_delay_secs")]
    pub max_delay_secs: u64,
}

fn default_retry_max_retries() -> u32 {
    DEFAULT_RETRY_MAX_RETRIES
}
fn default_retry_base_delay_secs() -> u64 {
    DEFAULT_RETRY_BASE_DELAY_SECS
}
fn default_retry_max_delay_secs() -> u64 {
    DEFAULT_RETRY_MAX_DELAY_SECS
}

impl Default for RequestRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_RETRY_MAX_RETRIES,
            base_delay_secs: DEFAULT_RETRY_BASE_DELAY_SECS,
            max_delay_secs: DEFAULT_RETRY_MAX_DELAY_SECS,
        }
    }
}
