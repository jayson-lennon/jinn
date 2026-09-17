//! Retry decorator for [`LlmService`] with exponential backoff.

use std::time::Duration;

use error_stack::Report;

use crate::llm_message::LlmMessage;
use crate::service::{ChatStream, LlmService, LlmServiceError, ToolStream};
use jinn_core_types::tool_types::ToolDefinition;

/// Callback invoked when a retry is about to happen.
///
/// Implementors can use this to surface retry status to the user.
pub trait OnRetry: Send + Sync {
    /// Called before sleeping before a retry attempt.
    ///
    /// - `attempt`: 1-indexed retry number (first retry = 1)
    /// - `max_retries`: maximum number of retries
    /// - `wait_duration`: how long we'll wait before retrying
    /// - `error`: the error that triggered the retry
    fn on_retry(
        &self,
        attempt: u32,
        max_retries: u32,
        wait_duration: Duration,
        error: &Report<LlmServiceError>,
    );
}

/// A no-op retry callback that does nothing.
pub struct NoOpOnRetry;

impl OnRetry for NoOpOnRetry {
    fn on_retry(&self, _: u32, _: u32, _: Duration, _: &Report<LlmServiceError>) {}
}

/// Configuration for retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Base delay for exponential backoff.
    pub base_delay: Duration,
    /// Maximum delay cap for exponential backoff.
    /// Overridden by provider-supplied hints (Retry-After / error body).
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_mins(1),
        }
    }
}

/// Decorator that wraps an [`LlmService`] and retries on retryable errors.
///
/// Retries [`LlmServiceError::RateLimited`] and [`LlmServiceError::Retryable`]
/// errors with exponential backoff. Provider-supplied timing hints
/// (from the `RateLimited` variant) override the backoff calculation.
pub struct RetryingLlmService {
    inner: Box<dyn LlmService>,
    config: RetryConfig,
    on_retry: Box<dyn OnRetry>,
}

impl RetryingLlmService {
    /// Create a new retrying decorator.
    #[must_use]
    pub fn new(
        inner: Box<dyn LlmService>,
        config: RetryConfig,
        on_retry: Box<dyn OnRetry>,
    ) -> Self {
        Self {
            inner,
            config,
            on_retry,
        }
    }

    /// Compute the delay for the next retry attempt.
    ///
    /// Returns `None` if the error is not retryable.
    fn compute_delay(&self, attempt: u32, error: &Report<LlmServiceError>) -> Option<Duration> {
        let (is_retryable, provider_hint) = match error.downcast_ref::<LlmServiceError>() {
            Some(LlmServiceError::RateLimited { retry_after }) => (true, *retry_after),
            Some(LlmServiceError::Retryable) => (true, None),
            _ => (false, None),
        };

        if !is_retryable {
            return None;
        }

        // If provider gave us a hint, always use it (even if > max_delay).
        if let Some(hint) = provider_hint {
            return Some(hint);
        }

        // Exponential backoff: base_delay * 2^attempt, capped at max_delay.
        let base_secs = self.config.base_delay.as_secs_f64();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let exponential = base_secs * 2_f64.powi(i32::try_from(attempt).unwrap_or(i32::MAX));
        let capped = exponential.min(self.config.max_delay.as_secs_f64());

        if capped <= 0.0 {
            return Some(Duration::ZERO);
        }

        Some(Duration::from_secs_f64(capped))
    }

    /// Run a retry loop for a fallible async operation.
    async fn retry_loop<F, Fut, T>(&self, f: F) -> Result<T, Report<LlmServiceError>>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, Report<LlmServiceError>>>,
    {
        let mut attempt = 0u32;
        loop {
            match f().await {
                Ok(result) => return Ok(result),
                Err(report) => {
                    let delay = self.compute_delay(attempt, &report);
                    if attempt >= self.config.max_retries {
                        return Err(report);
                    }
                    let Some(wait) = delay else {
                        return Err(report);
                    };
                    attempt += 1;
                    self.on_retry
                        .on_retry(attempt, self.config.max_retries, wait, &report);
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }
}

impl std::fmt::Debug for RetryingLlmService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryingLlmService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl LlmService for RetryingLlmService {
    fn name(&self) -> &'static str {
        "retrying"
    }

    async fn chat_stream(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>> {
        let system = system_prompt.map(std::borrow::ToOwned::to_owned);
        self.retry_loop(|| self.inner.chat_stream(system.as_deref(), messages.clone()))
            .await
    }

    async fn chat_stream_with_tools(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolStream, Report<LlmServiceError>> {
        let system = system_prompt.map(std::borrow::ToOwned::to_owned);
        self.retry_loop(|| {
            self.inner
                .chat_stream_with_tools(system.as_deref(), messages.clone(), tools.clone())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use parking_lot::Mutex;

    /// A service that fails N times then succeeds.
    struct FlakyService {
        fail_count: Mutex<u32>,
        fail_with: LlmServiceError,
    }

    impl FlakyService {
        fn new(fail_count: u32, fail_with: LlmServiceError) -> Self {
            Self {
                fail_count: Mutex::new(fail_count),
                fail_with,
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmService for FlakyService {
        fn name(&self) -> &'static str {
            "flaky"
        }

        async fn chat_stream(
            &self,
            _system_prompt: Option<&str>,
            _messages: Vec<LlmMessage>,
        ) -> Result<ChatStream, Report<LlmServiceError>> {
            let mut count = self.fail_count.lock();
            if *count > 0 {
                *count -= 1;
                return Err(Report::new(self.fail_with.clone()));
            }
            // Return an empty stream.
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    impl std::fmt::Debug for FlakyService {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FlakyService").finish_non_exhaustive()
        }
    }

    /// Clone implementation for test error types.
    /// We need this because FlakyService::fail_with needs to be Clone-able.
    impl Clone for LlmServiceError {
        fn clone(&self) -> Self {
            match self {
                Self::ApiKey => Self::ApiKey,
                Self::Provider => Self::Provider,
                Self::Config => Self::Config,
                Self::RateLimited { retry_after } => Self::RateLimited {
                    retry_after: *retry_after,
                },
                Self::Retryable => Self::Retryable,
            }
        }
    }

    /// A recording callback that captures retry events.
    struct RecordingOnRetry {
        calls: std::sync::Arc<Mutex<Vec<(u32, u32, Duration)>>>,
    }

    impl RecordingOnRetry {
        fn new() -> Self {
            Self {
                calls: std::sync::Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl OnRetry for RecordingOnRetry {
        fn on_retry(
            &self,
            attempt: u32,
            max_retries: u32,
            wait: Duration,
            _error: &Report<LlmServiceError>,
        ) {
            self.calls.lock().push((attempt, max_retries, wait));
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn retry_succeeds_after_transient_failure() {
        // Given a service that fails once with Retryable.
        let inner = FlakyService::new(1, LlmServiceError::Retryable);
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it succeeds after one retry.
        assert!(result.is_ok());
        let calls = calls.lock();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 1); // attempt 1
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn retry_gives_up_after_max_retries() {
        // Given a service that always fails with Retryable.
        let inner = FlakyService::new(100, LlmServiceError::Retryable);
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 3,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it fails after max retries.
        assert!(result.is_err());
        let calls = calls.lock();
        assert_eq!(calls.len(), 3);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn retry_does_not_retry_non_retryable_error() {
        // Given a service that fails with Provider error.
        let inner = FlakyService::new(1, LlmServiceError::Provider);
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it fails immediately without retry.
        assert!(result.is_err());
        let calls = calls.lock();
        assert!(calls.is_empty());
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn retry_uses_provider_hint_over_exponential() {
        // Given a service that fails with RateLimited and a 50ms hint.
        let inner = FlakyService::new(
            1,
            LlmServiceError::RateLimited {
                retry_after: Some(Duration::from_millis(50)),
            },
        );
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it succeeds.
        assert!(result.is_ok());
        let calls = calls.lock();
        assert_eq!(calls.len(), 1);
        // The wait duration should be the provider hint, not exponential backoff.
        assert_eq!(calls[0].2, Duration::from_millis(50));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn provider_hint_exceeds_max_delay() {
        // Given a service that fails with RateLimited and a hint exceeding max_delay.
        let inner = FlakyService::new(
            1,
            LlmServiceError::RateLimited {
                retry_after: Some(Duration::from_millis(200)),
            },
        );
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(50),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it succeeds and the wait is 200ms (provider hint overrides max_delay).
        assert!(result.is_ok());
        let calls = calls.lock();
        assert_eq!(calls[0].2, Duration::from_millis(200));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn no_retry_when_max_retries_is_zero() {
        // Given a service that fails with Retryable but max_retries is 0.
        let inner = FlakyService::new(1, LlmServiceError::Retryable);
        let callback = RecordingOnRetry::new();
        let calls = callback.calls.clone();
        let svc = RetryingLlmService::new(
            Box::new(inner),
            RetryConfig {
                max_retries: 0,
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(10),
            },
            Box::new(callback),
        );

        // When calling chat_stream.
        let result = svc.chat_stream(None, vec![]).await;

        // Then it fails immediately.
        assert!(result.is_err());
        let calls = calls.lock();
        assert!(calls.is_empty());
    }

    #[rstest::rstest]
    #[test]
    fn compute_delay_returns_none_for_non_retryable() {
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig::default(),
            Box::new(NoOpOnRetry),
        );

        let d = svc.compute_delay(0, &Report::new(LlmServiceError::Provider));
        assert!(d.is_none());

        let d = svc.compute_delay(0, &Report::new(LlmServiceError::ApiKey));
        assert!(d.is_none());

        let d = svc.compute_delay(0, &Report::new(LlmServiceError::Config));
        assert!(d.is_none());
    }

    #[rstest::rstest]
    #[test]
    fn compute_delay_uses_provider_hint() {
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_secs(2),
                max_delay: Duration::from_secs(10),
            },
            Box::new(NoOpOnRetry),
        );

        let d = svc.compute_delay(
            0,
            &Report::new(LlmServiceError::RateLimited {
                retry_after: Some(Duration::from_secs(100)),
            }),
        );
        // Provider hint overrides max_delay.
        assert_eq!(d, Some(Duration::from_secs(100)));
    }

    #[rstest::rstest]
    #[test]
    fn first_retry_delay_equals_base_delay() {
        // Given a retry config with base_delay=2s, max_delay=60s.
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_secs(2),
                max_delay: Duration::from_mins(1),
            },
            Box::new(NoOpOnRetry),
        );

        // When computing the delay for attempt 0.
        let delay = svc
            .compute_delay(0, &Report::new(LlmServiceError::Retryable))
            .expect("should return Some for Retryable");

        // Then the delay equals base_delay exactly.
        assert_eq!(delay, Duration::from_secs(2));
    }

    #[rstest::rstest]
    #[case(0u32, Duration::from_secs(2))]
    #[case(1u32, Duration::from_secs(4))]
    #[case(2u32, Duration::from_secs(8))]
    #[case(3u32, Duration::from_secs(16))]
    #[case(4u32, Duration::from_secs(32))]
    fn retry_delay_doubles_each_attempt(#[case] attempt: u32, #[case] expected: Duration) {
        // Given a retry config with base_delay=2s, max_delay=60s.
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_secs(2),
                max_delay: Duration::from_mins(1),
            },
            Box::new(NoOpOnRetry),
        );

        // When computing the delay for the given attempt.
        let delay = svc
            .compute_delay(attempt, &Report::new(LlmServiceError::Retryable))
            .expect("should return Some for Retryable");

        // Then the delay equals base_delay * 2^attempt.
        assert_eq!(delay, expected);
    }

    #[rstest::rstest]
    #[test]
    fn retry_delay_capped_at_max_delay() {
        // Given a retry config where base*2^attempt exceeds max_delay.
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::from_secs(10),
                max_delay: Duration::from_secs(1),
            },
            Box::new(NoOpOnRetry),
        );

        // When computing the delay for attempt 0.
        let delay = svc
            .compute_delay(0, &Report::new(LlmServiceError::Retryable))
            .expect("should return Some for Retryable");

        // Then the delay is capped at max_delay (not base_delay).
        assert_eq!(delay, Duration::from_secs(1));
    }

    #[rstest::rstest]
    #[test]
    fn retry_delay_zero_base_returns_zero() {
        // Given a retry config with base_delay=0.
        let svc = RetryingLlmService::new(
            Box::new(FlakyService::new(0, LlmServiceError::Retryable)),
            RetryConfig {
                max_retries: 5,
                base_delay: Duration::ZERO,
                max_delay: Duration::from_secs(1),
            },
            Box::new(NoOpOnRetry),
        );

        // When computing the delay for attempt 0.
        let delay = svc
            .compute_delay(0, &Report::new(LlmServiceError::Retryable))
            .expect("should return Some for Retryable");

        // Then the delay is zero.
        assert_eq!(delay, Duration::ZERO);
    }
}
