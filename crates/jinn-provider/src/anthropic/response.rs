//! Response parser for Anthropic SSE streaming.
//!
//! Maps Anthropic's `content_block_start/delta/stop` and `message_delta`
//! events to [`StreamEvent`] variants. Key differences from OpenAI:
//!
//! - `content_block_start` with `type: "tool_use"` → `ToolUseStart`
//! - `content_block_delta` with `type: "text_delta"` → `Text`
//! - `content_block_delta` with `type: "input_json_delta"` → `ToolUseInputDelta`
//! - `content_block_stop` → `ToolUseComplete` (drains state)
//! - `message_delta` with `stop_reason` → `Done`

use std::collections::HashMap;

use crate::StreamEvent;
use crate::stream_event::{StopReason, StreamUsage};
use jinn_core_types::tool_types::ToolCall;

/// State tracked per content block index for tool use.
#[derive(Debug, Default)]
struct ToolUseState {
    id: String,
    name: String,
    json_buffer: String,
}

/// Stateful parser for Anthropic streaming responses.
#[derive(Debug, Default)]
pub struct AnthropicStreamParser {
    /// Per-index tool use state.
    tool_states: HashMap<usize, ToolUseState>,
    /// Accumulated input tokens from `message_start` event.
    input_tokens: Option<u64>,
    /// Accumulated output tokens from `message_delta` event.
    output_tokens: Option<u64>,
}

impl AnthropicStreamParser {
    /// Create a new parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a single SSE data payload into an optional `StreamEvent`.
    pub fn parse_data(&mut self, json: &str) -> Option<StreamEvent> {
        let response: serde_json::Value = serde_json::from_str(json).ok()?;

        let response_type = response.get("type")?.as_str()?;
        let index = response
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;

        match response_type {
            "message_start" => self.handle_message_start(&response),
            "content_block_start" => self.handle_content_block_start(index, &response),
            "content_block_delta" => self.handle_content_block_delta(index, &response),
            "content_block_stop" => self.handle_content_block_stop(index),
            "message_delta" => self.handle_message_delta(&response),
            "error" => Self::handle_error_event(&response),
            _ => None,
        }
    }

    fn handle_message_start(&mut self, response: &serde_json::Value) -> Option<StreamEvent> {
        if let Some(usage) = response.get("message").and_then(|m| m.get("usage")) {
            self.input_tokens = usage
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64);
        }
        None
    }

    fn handle_content_block_start(
        &mut self,
        index: usize,
        response: &serde_json::Value,
    ) -> Option<StreamEvent> {
        let content_block = response.get("content_block")?;
        let block_type = content_block
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        if block_type == "tool_use" {
            let id = content_block
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_owned();
            let name = content_block
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_owned();

            self.tool_states.insert(
                index,
                ToolUseState {
                    id: id.clone(),
                    name: name.clone(),
                    json_buffer: String::new(),
                },
            );

            Some(StreamEvent::ToolUseStart { index, id, name })
        } else {
            None
        }
    }

    fn handle_content_block_delta(
        &mut self,
        index: usize,
        response: &serde_json::Value,
    ) -> Option<StreamEvent> {
        let delta = response.get("delta")?;
        let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match delta_type {
            "text_delta" => delta
                .get("text")
                .and_then(|t| t.as_str())
                .map(|t| StreamEvent::Text(t.to_owned())),
            "input_json_delta" => {
                let partial_json = delta
                    .get("partial_json")
                    .and_then(|p| p.as_str())
                    .unwrap_or("");

                if partial_json.is_empty() {
                    None
                } else {
                    if let Some(state) = self.tool_states.get_mut(&index) {
                        state.json_buffer.push_str(partial_json);
                    }
                    Some(StreamEvent::ToolUseInputDelta {
                        index,
                        partial_json: partial_json.to_owned(),
                    })
                }
            }
            _ => None,
        }
    }

    fn handle_content_block_stop(&mut self, index: usize) -> Option<StreamEvent> {
        self.tool_states.remove(&index).map(|state| {
            let arguments = if state.json_buffer.is_empty() {
                "{}".to_owned()
            } else {
                state.json_buffer
            };
            StreamEvent::ToolUseComplete {
                index,
                tool_call: ToolCall {
                    id: state.id,
                    name: state.name,
                    arguments,
                },
            }
        })
    }

    fn handle_message_delta(&mut self, response: &serde_json::Value) -> Option<StreamEvent> {
        let delta = response.get("delta")?;
        let stop_reason = delta
            .get("stop_reason")
            .and_then(|s| s.as_str())
            .unwrap_or("end_turn");

        if let Some(usage) = response.get("usage") {
            self.output_tokens = usage
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64);
        }

        let stop = match stop_reason {
            "tool_use" => StopReason::ToolUse,
            "end_turn" => StopReason::EndTurn,
            other => StopReason::Other(other.to_owned()),
        };
        let usage = if self.input_tokens.is_some() || self.output_tokens.is_some() {
            Some(StreamUsage {
                prompt_tokens: self.input_tokens,
                completion_tokens: self.output_tokens,
                cost: None,
                cached_tokens: None,
            })
        } else {
            None
        };
        Some(StreamEvent::Done {
            stop_reason: stop,
            usage,
        })
    }
    fn handle_error_event(response: &serde_json::Value) -> Option<StreamEvent> {
        let error_obj = response.get("error")?;
        let error_type = error_obj
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown_error")
            .to_owned();
        let message = error_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown streaming error")
            .to_owned();
        Some(StreamEvent::Error {
            error_type,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    fn parse_single(json: &str) -> Option<StreamEvent> {
        let mut parser = AnthropicStreamParser::new();
        parser.parse_data(json)
    }

    #[rstest::rstest]
    fn text_delta_produces_text_event() {
        let json = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let event = parse_single(json);
        assert_eq!(event, Some(StreamEvent::Text("Hello".to_owned())));
    }

    #[rstest::rstest]
    fn tool_use_start_produces_tool_use_start() {
        let json = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"get_weather","input":{}}}"#;
        let event = parse_single(json);
        assert_eq!(
            event,
            Some(StreamEvent::ToolUseStart {
                index: 1,
                id: "toolu_01".to_owned(),
                name: "get_weather".to_owned(),
            })
        );
    }

    #[rstest::rstest]
    fn input_json_delta_accumulates() {
        let mut parser = AnthropicStreamParser::new();

        // Start.
        parser.parse_data(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"echo","input":{}}}"#,
        );

        // Delta.
        let event = parser.parse_data(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"key\":"}}"#,
        );
        assert!(matches!(
            event,
            Some(StreamEvent::ToolUseInputDelta { partial_json, .. })
            if partial_json == "{\"key\":"
        ));
    }

    #[rstest::rstest]
    fn content_block_stop_produces_tool_use_complete() {
        let mut parser = AnthropicStreamParser::new();

        parser.parse_data(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"echo","input":{}}}"#,
        );
        parser.parse_data(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}"#,
        );

        let event = parser.parse_data(r#"{"type":"content_block_stop","index":1}"#);

        match event {
            Some(StreamEvent::ToolUseComplete { tool_call, .. }) => {
                assert_eq!(tool_call.name, "echo");
                assert_eq!(tool_call.arguments, "{\"x\":1}");
            }
            other => panic!("Expected ToolUseComplete, got {other:?}"),
        }
    }

    #[rstest::rstest]
    fn empty_tool_arguments_default_to_empty_object() {
        let mut parser = AnthropicStreamParser::new();

        parser.parse_data(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"get_time","input":{}}}"#,
        );

        let event = parser.parse_data(r#"{"type":"content_block_stop","index":0}"#);

        match event {
            Some(StreamEvent::ToolUseComplete { tool_call, .. }) => {
                assert_eq!(tool_call.arguments, "{}");
            }
            other => panic!("Expected ToolUseComplete, got {other:?}"),
        }
    }

    #[rstest::rstest]
    fn message_delta_stop_reason_end_turn() {
        let json = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#;
        let event = parse_single(json);
        assert!(matches!(
            event,
            Some(StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            })
        ));
    }

    #[rstest::rstest]
    fn message_delta_stop_reason_tool_use() {
        let json = r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#;
        let event = parse_single(json);
        assert!(matches!(
            event,
            Some(StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            })
        ));
    }

    #[rstest::rstest]
    fn unknown_event_type_produces_none() {
        let json = r#"{"type":"ping"}"#;
        assert!(parse_single(json).is_none());
    }

    #[rstest::rstest]
    fn invalid_json_produces_none() {
        assert!(parse_single("not json").is_none());
    }

    #[rstest::rstest]
    fn error_event_produces_stream_error() {
        // Given an Anthropic error event.
        let json = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;

        // When parsing.
        let event = parse_single(json);

        // Then it produces a StreamEvent::Error with the error details.
        assert!(matches!(
            event,
            Some(StreamEvent::Error { ref error_type, ref message })
            if error_type == "overloaded_error" && message == "Overloaded"
        ));
    }

    #[rstest::rstest]
    fn error_event_with_missing_fields_produces_error_with_defaults() {
        // Given an error event with missing fields.
        let json = r#"{"type":"error","error":{}}"#;

        // When parsing.
        let event = parse_single(json);

        // Then it produces a StreamEvent::Error with default values.
        assert!(matches!(
            event,
            Some(StreamEvent::Error { ref error_type, ref message })
            if error_type == "unknown_error" && message == "Unknown streaming error"
        ));
    }

    #[rstest::rstest]
    fn message_start_captures_input_tokens_for_usage() {
        // Given a message_start event with input_tokens, followed by
        // a message_delta with stop_reason and output_tokens.
        // Deleting the message_start match arm or returning None from
        // handle_message_start would lose input_tokens.
        let mut parser = AnthropicStreamParser::new();

        // message_start with usage.input_tokens.
        parser.parse_data(r#"{"type":"message_start","message":{"usage":{"input_tokens":42}}}"#);

        // message_delta with stop_reason and output_tokens.
        let event = parser.parse_data(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
        );

        // Then the Done event has usage with both token counts.
        match event {
            Some(StreamEvent::Done { usage: Some(u), .. }) => {
                assert_eq!(u.prompt_tokens, Some(42));
                assert_eq!(u.completion_tokens, Some(7));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    #[rstest::rstest]
    fn usage_present_with_only_input_tokens() {
        // Given message_start with input_tokens but message_delta with no
        // output_tokens. The usage condition uses `||` so either field being
        // present should yield usage. Changing `||` to `&&` would break this.
        let mut parser = AnthropicStreamParser::new();

        parser.parse_data(r#"{"type":"message_start","message":{"usage":{"input_tokens":99}}}"#);

        // message_delta with stop_reason but NO usage/output_tokens.
        let event =
            parser.parse_data(r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#);

        // Then usage is still Some because input_tokens was set.
        match event {
            Some(StreamEvent::Done { usage: Some(u), .. }) => {
                assert_eq!(u.prompt_tokens, Some(99));
                assert_eq!(u.completion_tokens, None);
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }
}
