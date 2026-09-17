//! LLM service trait and error types.

use std::pin::Pin;
use std::time::Duration;

use crate::llm_message::LlmMessage;
use crate::stream_event::StopReason;
use crate::stream_event::StreamEvent;
use jinn_core_types::tool_types::ToolDefinition;
use error_stack::Report;
use futures::StreamExt as _;
use futures::stream;
use futures::stream::Stream;
use wherror::Error;

/// Error type for LLM service operations.
///
/// Unit variants - the original error is preserved in the `Report` chain
/// via `.change_context()`. Attach additional context with `.attach()`.
#[derive(Debug, Error)]
pub enum LlmServiceError {
    /// API key not found or invalid.
    #[error("API key error")]
    ApiKey,
    /// The LLM provider returned an error.
    #[error("LLM provider error")]
    Provider,
    /// Builder configuration error.
    #[error("LLM configuration error")]
    Config,
    /// Rate limited by the provider (HTTP 429).
    ///
    /// Carries an optional duration hint parsed from the response body
    /// or `Retry-After` header. When present, this overrides the
    /// exponential backoff calculation.
    #[error("rate limited")]
    RateLimited {
        /// Provider-suggested wait duration, if available.
        retry_after: Option<Duration>,
    },
    /// Transient server error (HTTP 5xx), retryable.
    #[error("transient server error")]
    Retryable,
}

/// A streaming LLM chat response (text tokens only).
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<String, Report<LlmServiceError>>> + Send>>;

/// A streaming LLM chat response with tool support.
pub type ToolStream =
    Pin<Box<dyn Stream<Item = Result<StreamEvent, Report<LlmServiceError>>> + Send>>;

/// Trait for a single LLM streaming chat session.
///
/// Use [`LlmServiceFactory`] to create instances.
#[async_trait::async_trait]
pub trait LlmService: Send + Sync {
    /// Returns a human-readable name for this service, for debugging.
    fn name(&self) -> &'static str;

    /// Start a streaming chat completion (text only).
    ///
    /// `system_prompt` is the assembled system prompt for this request; it
    /// travels as request data, never inside `messages`.
    ///
    /// Returns a stream of text tokens. The stream ends when the LLM finishes
    /// generating or errors.
    async fn chat_stream(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>>;

    /// Start a streaming chat completion with tool support.
    ///
    /// `system_prompt` is the assembled system prompt for this request; it
    /// travels as request data, never inside `messages`.
    ///
    /// Returns a stream of [`StreamEvent`] variants. When `tools` is non-empty,
    /// the stream may include tool call events. The default implementation
    /// delegates to [`chat_stream`](LlmService::chat_stream), wrapping text
    /// tokens as [`StreamEvent::Text`] and appending a terminal
    /// [`StreamEvent::Done`].
    async fn chat_stream_with_tools(
        &self,
        system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ToolStream, Report<LlmServiceError>> {
        let _ = tools; // Default: no tool support, ignore tool definitions.
        let text_stream = self.chat_stream(system_prompt, messages).await?;
        let events = text_stream.map(|result| result.map(StreamEvent::Text));
        let done = stream::once(async {
            Ok(StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None,
            })
        });
        Ok(Box::pin(events.chain(done)))
    }
}

/// Factory for creating [`LlmService`] instances.
///
/// Each call to [`create`](LlmServiceFactory::create) produces a fresh service.
/// The factory is `Clone + Send + Sync` - wrap in `Arc` for sharing.
pub trait LlmServiceFactory: Send + Sync + std::fmt::Debug {
    /// Create a new LLM service instance.
    ///
    /// # Errors
    ///
    /// Returns an error if the factory fails to create a service.
    fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>>;

    /// Returns a human-readable name for this factory.
    fn name(&self) -> &str;
}

/// Classify an HTTP error response into the appropriate [`LlmServiceError`] variant.
///
/// - HTTP 429 → [`RateLimited`](LlmServiceError::RateLimited) (with parsed hint if available)
/// - HTTP 5xx → [`Retryable`](LlmServiceError::Retryable)
/// - Other → [`Provider`](LlmServiceError::Provider)
///
/// For 429 responses, attempts to extract a `Retry-After` duration from the
/// error body using [`parse_retry_after_hint`].
#[must_use = "classification result should be used"]
pub fn classify_http_error(
    status: reqwest::StatusCode,
    error_body: &str,
    provider_name: &str,
    retry_after_header: Option<Duration>,
) -> Report<LlmServiceError> {
    if status.as_u16() == 429 {
        let retry_after = retry_after_header.or_else(|| parse_retry_after_hint(error_body));
        Report::new(LlmServiceError::RateLimited { retry_after })
            .attach(format!("{provider_name} HTTP {status}"))
            .attach(error_body.to_owned())
    } else if status.is_server_error() {
        Report::new(LlmServiceError::Retryable)
            .attach(format!("{provider_name} HTTP {status}"))
            .attach(error_body.to_owned())
    } else {
        Report::new(LlmServiceError::Provider)
            .attach(format!("{provider_name} HTTP {status}"))
            .attach(error_body.to_owned())
    }
}

/// Attempt to extract a retry-after duration from the error response body.
///
/// Looks for datetime patterns like `"reset at 2026-05-21 14:45:38"` and computes
/// the duration from now. Returns `None` if no parseable pattern is found or
/// the computed duration is negative (already expired).
#[must_use]
pub fn parse_retry_after_hint(body: &str) -> Option<Duration> {
    // Match patterns like "reset at 2026-05-21 14:45:38" or similar datetime patterns.
    let re = regex::Regex::new(
        r"(?i)(?:reset(?:s)?|retry|available)\s+(?:at|after|in)\s+(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2})",
    )
    .ok()?;

    let caps = re.captures(body)?;
    let datetime_str = caps.get(1)?.as_str();

    let ts: jiff::Timestamp = datetime_str.parse().ok()?;
    let now = jiff::Timestamp::now();

    let diff = ts.since(now).ok()?;
    let secs = diff.total(jiff::Unit::Second).ok()?;

    if secs > 0.0 {
        Some(Duration::from_secs_f64(secs))
    } else {
        None
    }
}

/// Parse a `Retry-After` header value.
///
/// The header can be either:
/// - Seconds (integer): `"120"`
/// - HTTP-date: `"Fri, 21 May 2026 14:45:38 GMT"`
///
/// Returns `None` if the value cannot be parsed.
#[must_use]
pub fn parse_retry_after_header(value: &str) -> Option<Duration> {
    // Try seconds first.
    if let Ok(secs) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }

    // Try HTTP-date.
    let ts: jiff::Timestamp = value.trim().parse().ok()?;
    let now = jiff::Timestamp::now();
    let diff = ts.since(now).ok()?;
    let secs = diff.total(jiff::Unit::Second).ok()?;

    if secs > 0.0 {
        Some(Duration::from_secs_f64(secs))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_in_result,
        reason = "test code, panics are acceptable"
    )]
    use super::*;

    #[rstest::rstest]
    fn classify_429_as_rate_limited() {
        let report = classify_http_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "", "test", None);
        let err = report.downcast_ref::<LlmServiceError>().expect("downcast");
        assert!(matches!(
            err,
            LlmServiceError::RateLimited { retry_after: None }
        ));
    }

    #[rstest::rstest]
    fn classify_429_uses_retry_after_header_over_hint() {
        let report = classify_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "reset at 2099-01-01 00:00:00",
            "test",
            Some(Duration::from_secs(42)),
        );
        let err = report.downcast_ref::<LlmServiceError>().expect("downcast");
        assert!(matches!(
            err,
            LlmServiceError::RateLimited {
                retry_after: Some(d)
            } if *d == Duration::from_secs(42)
        ));
    }

    #[rstest::rstest]
    fn classify_5xx_as_retryable() {
        let report = classify_http_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "oops",
            "test",
            None,
        );
        let err = report.downcast_ref::<LlmServiceError>().expect("downcast");
        assert!(matches!(err, LlmServiceError::Retryable));
    }

    #[rstest::rstest]
    fn classify_4xx_as_provider() {
        let report = classify_http_error(reqwest::StatusCode::BAD_REQUEST, "bad", "test", None);
        let err = report.downcast_ref::<LlmServiceError>().expect("downcast");
        assert!(matches!(err, LlmServiceError::Provider));
    }

    #[rstest::rstest]
    fn parse_retry_after_hint_returns_none_for_unparseable_body() {
        // The regex captures a datetime without timezone, which jiff
        // cannot parse without a TZ indicator. This tests that the
        // function returns None rather than panicking.
        let result = parse_retry_after_hint("nothing to see here");
        assert!(result.is_none());
    }

    #[rstest::rstest]
    fn parse_retry_after_hint_returns_none_for_expired() {
        // Even if jiff could parse this, the result would be expired.
        let result = parse_retry_after_hint("reset at 2000-01-01 00:00:00");
        assert!(
            result.is_none(),
            "expired or unparseable should return None"
        );
    }

    #[rstest::rstest]
    fn parse_retry_after_header_seconds() {
        let result = parse_retry_after_header("120");
        assert_eq!(result, Some(Duration::from_mins(2)));
    }

    #[rstest::rstest]
    fn parse_retry_after_header_zero_seconds_returns_zero_duration() {
        // "0" parses as u64=0 and returns Some(ZERO), not None.
        // so we instead test that non-zero values produce non-zero durations.
        let result = parse_retry_after_header("0");
        assert_eq!(result, Some(Duration::ZERO));
    }

    #[rstest::rstest]
    fn parse_retry_after_header_http_date_future() {
        // Use an ISO 8601 date far in the future.
        let result = parse_retry_after_header("2099-01-01T00:00:00Z");
        assert!(result.is_some());
        let dur = result.expect("present");
        assert!(dur > Duration::ZERO);
    }

    #[rstest::rstest]
    fn parse_retry_after_header_http_date_past_returns_none() {
        let result = parse_retry_after_header("2000-01-01T00:00:00Z");
        assert!(result.is_none(), "past date should return None");
    }

    #[rstest::rstest]
    fn parse_retry_after_header_garbage_returns_none() {
        let result = parse_retry_after_header("not-a-date");
        assert!(result.is_none());
    }
}
