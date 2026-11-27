//! Sample LLM service for manual UI testing.
//!
//! Responds to special commands in the user's message with canned,
//! streamed output. Use `--fake-llm` to activate.
//!
//! # Commands
//!
//! - `!response` - streams a plain canned response with inter-token delays.
//! - `!think` - streams a thinking block followed by a canned response,
//!   both with inter-token delays.
//! - anything else - streams a help message instantly (no delays).

use std::time::Duration;

use error_stack::Report;
use futures::stream;
use jinn_core_types::llm_message::LlmMessage;
use rand::Rng;

use crate::service::{ChatStream, LlmService, LlmServiceError, LlmServiceFactory};

/// Canned response for `!response`.
const RESPONSE_TEXT: &str = "This is a sample response from the sample LLM provider. \
    It streams tokens with a small random delay to simulate a real response.";

/// Canned thinking content for `!think`.
const THINK_TEXT: &str = "The user triggered the thinking command. \
    Let me compose a thoughtful response to demonstrate the streaming experience.";

/// Canned response body for `!think` (after the thinking block).
const THINK_RESPONSE_TEXT: &str = "Here is my response after thinking. \
    This demonstrates how a model with a thinking phase would stream output.";

/// Help text shown for unrecognized commands.
const HELP_TEXT: &str = "Sample LLM Provider - Available commands:\n\
    \n\
    !response - Stream a sample response with delays\n\
    !think    - Stream a thinking block followed by a response\n\
    !markdown - Stream a markdown document exercising the renderer\n\
    \n\
    Any other input shows this help message.";

/// Canned markdown document for `!markdown`.
///
/// Shaped like a real web-search reply, carrying every construct the
/// web UI renderer must handle: a footnote pair, a task list, a table, a
/// bare address in each punctuation shape the linkifier handles
/// differently, an address that must stay text, and an email. The prose
/// between them is a model's own, so a failure reads as a sentence and not
/// as a fixture.
///
/// The document is a file rather than a literal because the web UI client's
/// unit tests read the same bytes: a rule proven there and the page a user
/// sees are checked against one document, so they cannot drift apart.
const MARKDOWN_TEXT: &str = include_str!("resources/markdown_showcase.md");

/// Minimum inter-token delay in milliseconds.
const DELAY_MIN_MS: u64 = 10;
/// Maximum inter-token delay in milliseconds.
const DELAY_MAX_MS: u64 = 50;

/// Factory that creates sample LLM service instances.
///
/// Each service inspects the last user message and responds
/// with the appropriate canned stream. Use this for manual UI
/// testing via the `--fake-llm` flag.
#[derive(Debug, Clone)]
pub struct SampleLlmServiceFactory;

impl LlmServiceFactory for SampleLlmServiceFactory {
    fn create(&self) -> Result<Box<dyn LlmService>, Report<LlmServiceError>> {
        Ok(Box::new(SampleLlmService))
    }

    fn name(&self) -> &'static str {
        "Sample"
    }
}

/// A sample LLM service that parses commands from user input.
struct SampleLlmService;

#[async_trait::async_trait]
impl LlmService for SampleLlmService {
    fn name(&self) -> &'static str {
        "sample"
    }

    async fn chat_stream(
        &self,
        _system_prompt: Option<&str>,
        messages: Vec<LlmMessage>,
    ) -> Result<ChatStream, Report<LlmServiceError>> {
        let last_user_msg = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                LlmMessage::User { content, .. } => Some(content.trim().to_lowercase()),
                _ => None,
            })
            .unwrap_or_default();

        let (tokens, with_delay) = match last_user_msg.as_str() {
            "!response" => (tokenize(RESPONSE_TEXT), true),
            "!think" => (tokenize_think(), true),
            "!markdown" => (tokenize(MARKDOWN_TEXT), true),
            _ => (tokenize(HELP_TEXT), false),
        };

        let stream = if with_delay {
            delayed_token_stream(tokens)
        } else {
            instant_token_stream(tokens)
        };

        Ok(stream)
    }
}

/// Splits text into word-level tokens (first word has no leading space, rest do).
fn tokenize(text: &str) -> Vec<String> {
    text.split(' ')
        .enumerate()
        .map(|(i, word)| {
            if i == 0 {
                word.to_owned()
            } else {
                format!(" {word}")
            }
        })
        .collect()
}

/// Produces tokens for the `!think` command: thinking block then response.
fn tokenize_think() -> Vec<String> {
    let mut tokens = Vec::new();
    tokens.extend(tokenize("<think"));
    tokens.push(" ".to_owned());
    tokens.extend(tokenize(THINK_TEXT));
    tokens.extend(tokenize("</think"));
    tokens.push("> ".to_owned());
    tokens.extend(tokenize(THINK_RESPONSE_TEXT));
    tokens
}

/// Creates a stream that yields tokens instantly (no delays).
fn instant_token_stream(tokens: Vec<String>) -> ChatStream {
    Box::pin(stream::iter(tokens.into_iter().map(Ok)))
}

/// Creates a stream that yields tokens with a random delay (10–50ms) between each.
fn delayed_token_stream(tokens: Vec<String>) -> ChatStream {
    let mut rng = rand::rng();
    let items: Vec<(String, u64)> = tokens
        .into_iter()
        .map(|token| {
            let delay = Rng::random_range(&mut rng, DELAY_MIN_MS..=DELAY_MAX_MS);
            (token, delay)
        })
        .collect();

    let stream = stream::unfold(items.into_iter(), |mut iter| async move {
        match iter.next() {
            Some((token, delay)) => {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                Some((Ok::<_, Report<LlmServiceError>>(token), iter))
            }
            None => None,
        }
    });

    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use futures::StreamExt;

    use super::*;

    #[rstest::rstest]
    #[tokio::test]
    async fn response_command_streams_canned_text() {
        tokio::time::pause();

        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");
        let messages = vec![LlmMessage::User {
            content: "!response".to_owned(),
            attachments: Vec::new(),
        }];

        // When streaming.
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = StreamExt::map(stream, |r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output matches the canned response.
        assert_eq!(output, RESPONSE_TEXT);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn markdown_command_streams_the_markdown_document() {
        tokio::time::pause();

        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");
        let messages = vec![LlmMessage::User {
            content: "!markdown".to_owned(),
            attachments: Vec::new(),
        }];

        // When streaming.
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = StreamExt::map(stream, |r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output is the canned markdown, with its newlines intact.
        assert_eq!(output, MARKDOWN_TEXT);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn think_output_starts_with_think_tag() {
        tokio::time::pause();

        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");
        let messages = vec![LlmMessage::User {
            content: "!think".to_owned(),
            attachments: Vec::new(),
        }];

        // When streaming.
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = stream
            .map(|r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output starts with <think and contains the thinking text.
        assert!(output.starts_with("<think"));
        assert!(output.contains(THINK_TEXT));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn think_output_contains_closing_tag() {
        tokio::time::pause();

        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");
        let messages = vec![LlmMessage::User {
            content: "!think".to_owned(),
            attachments: Vec::new(),
        }];

        // When streaming.
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = stream
            .map(|r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output contains the closing tag and response text.
        assert!(output.contains("</think"));
        assert!(output.contains(THINK_RESPONSE_TEXT));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn unknown_input_streams_help() {
        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");
        let messages = vec![LlmMessage::User {
            content: "hello".to_owned(),
            attachments: Vec::new(),
        }];

        // When streaming.
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = stream
            .map(|r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output is the help text.
        assert_eq!(output, HELP_TEXT);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn empty_input_streams_help() {
        // Given a sample service with no messages.
        let service = SampleLlmServiceFactory.create().expect("create service");

        // When streaming with no messages.
        let stream = service
            .chat_stream(None, vec![])
            .await
            .expect("chat_stream");
        let output: String = stream
            .map(|r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the help text is streamed.
        assert_eq!(output, HELP_TEXT);
    }

    #[rstest::rstest]
    fn tokenize_splits_on_spaces() {
        // Given a simple sentence.
        // When tokenizing.
        let tokens = tokenize("Hello world!");

        // Then tokens are word-level with leading spaces on subsequent words.
        assert_eq!(tokens, vec!["Hello", " world!"]);
    }

    #[rstest::rstest]
    fn tokenize_single_word() {
        // Given a single word.
        // When tokenizing.
        let tokens = tokenize("Hello");

        // Then a single token is produced.
        assert_eq!(tokens, vec!["Hello"]);
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn command_matching_is_case_insensitive() {
        tokio::time::pause();

        // Given a sample service.
        let service = SampleLlmServiceFactory.create().expect("create service");

        // When sending "!Response" (mixed case) as a message.
        let messages = vec![LlmMessage::User {
            content: "!Response".to_owned(),
            attachments: Vec::new(),
        }];
        let stream = service
            .chat_stream(None, messages)
            .await
            .expect("chat_stream");
        let output: String = stream
            .map(|r| r.expect("token"))
            .collect::<Vec<_>>()
            .await
            .join("");

        // Then the output matches the canned response (case-insensitive match).
        assert_eq!(output, RESPONSE_TEXT);
    }

    #[rstest::rstest]
    fn factory_name_returns_sample_llm() {
        // Given a sample LLM factory.
        let factory = SampleLlmServiceFactory;

        // When reading its name.
        let name = factory.name();

        // Then the name is "Sample".
        assert_eq!(name, "Sample");
    }
}
