//! Response parser for Google Gemini SSE streaming.
//!
//! Maps Gemini's `candidates[0].content.parts[]` structure to
//! [`StreamEvent`] variants. The Gemini API uses the same SSE format
//! as OpenAI (`data: {...}\n\n`) but with a different JSON structure.
//!
//! Text content: `candidates[0].content.parts[0].text`
//! Function calls: `candidates[0].content.parts[0].functionCall`

use crate::StreamEvent;
use crate::stream_event::{StopReason, StreamUsage};
use jinn_core_types::tool_types::ToolCall;

/// Stateful parser for Google Gemini streaming responses.
#[derive(Debug, Default)]
pub struct GeminiStreamParser {
    /// Whether a Done event has been emitted.
    done_emitted: bool,
}

impl GeminiStreamParser {
    /// Create a new parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a single SSE data payload into zero or more `StreamEvent`s.
    #[allow(clippy::manual_let_else, clippy::collapsible_if)]
    pub fn parse_data(&mut self, json: &str) -> Vec<StreamEvent> {
        let mut results = Vec::new();

        let response: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return results,
        };

        let candidates = match response.get("candidates").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => return results,
        };

        let Some(candidate) = candidates.first() else {
            return results;
        };

        let content = match candidate.get("content") {
            Some(c) => c,
            None => return results,
        };

        let parts = match content.get("parts").and_then(|p| p.as_array()) {
            Some(p) => p,
            None => return results,
        };

        for (index, part) in parts.iter().enumerate() {
            // Text content.
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    results.push(StreamEvent::Text(text.to_owned()));
                }
            }

            // Function call.
            if let Some(events) = Self::handle_function_call(index, part) {
                results.extend(events);
            }
        }

        // Check for finish reason.
        let finish_reason = candidate
            .get("finishReason")
            .and_then(|f| f.as_str())
            .unwrap_or("");

        // Extract usage metadata.
        let usage = response.get("usageMetadata").map(|meta| StreamUsage {
            prompt_tokens: meta
                .get("promptTokenCount")
                .and_then(serde_json::Value::as_u64),
            completion_tokens: meta
                .get("completionTokenCount")
                .and_then(serde_json::Value::as_u64),
            cost: None,
            cached_tokens: None,
        });

        if !finish_reason.is_empty() && !self.done_emitted {
            let stop_reason = match finish_reason {
                "STOP" => StopReason::EndTurn,
                _ => StopReason::Other(finish_reason.to_owned()),
            };
            results.push(StreamEvent::Done { stop_reason, usage });
            self.done_emitted = true;
        }

        results
    }

    fn handle_function_call(index: usize, part: &serde_json::Value) -> Option<Vec<StreamEvent>> {
        let fc = part.get("functionCall")?;
        let name = fc
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();
        let args = fc.get("args").cloned().unwrap_or(serde_json::Value::Null);
        let arguments = serde_json::to_string(&args).unwrap_or_default();
        let id = format!("call_{name}");

        Some(vec![
            StreamEvent::ToolUseStart {
                index,
                id: id.clone(),
                name: name.clone(),
            },
            StreamEvent::ToolUseInputDelta {
                index,
                partial_json: arguments.clone(),
            },
            StreamEvent::ToolUseComplete {
                index,
                tool_call: ToolCall {
                    id,
                    name,
                    arguments,
                },
            },
        ])
    }

    /// Handle the `[DONE]` sentinel - emits Done if not already emitted.
    pub fn handle_done(&mut self) -> Vec<StreamEvent> {
        if self.done_emitted {
            return vec![];
        }
        self.done_emitted = true;
        vec![StreamEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: None,
        }]
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    fn parse_single(json: &str) -> Vec<StreamEvent> {
        let mut parser = GeminiStreamParser::new();
        parser.parse_data(json)
    }

    #[rstest::rstest]
    fn text_delta_produces_text_event() {
        let json =
            r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}]},"finishReason":"STOP"}]}"#;
        let events = parse_single(json);

        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::Text(t) if t == "Hello"))
        );
    }

    #[rstest::rstest]
    fn function_call_produces_tool_events() {
        let json = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"Paris"}}}]},"finishReason":"STOP"}]}"#;
        let events = parse_single(json);

        // Should produce ToolUseStart + ToolUseInputDelta + ToolUseComplete + Done.
        assert!(
            events.iter().any(
                |e| matches!(e, StreamEvent::ToolUseStart { name, .. } if name == "get_weather")
            )
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::ToolUseComplete { .. }))
        );
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
    }

    #[rstest::rstest]
    fn empty_text_produces_no_text_event() {
        let json = r#"{"candidates":[{"content":{"parts":[{"text":""}]}}]}"#;
        let events = parse_single(json);
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
    }

    #[rstest::rstest]
    fn invalid_json_produces_no_events() {
        let events = parse_single("not json");
        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn done_sentinel_after_finish_is_noop() {
        let mut parser = GeminiStreamParser::new();
        let json =
            r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}]}"#;
        parser.parse_data(json);

        let events = parser.handle_done();
        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn done_sentinel_without_finish_produces_done() {
        let mut parser = GeminiStreamParser::new();
        let events = parser.handle_done();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ));
    }

    #[rstest::rstest]
    fn exactly_one_done_event_for_multiple_chunks_with_stop() {
        // Given two chunks both with finishReason STOP.
        // Changing `&&` to `||` on the guard would emit duplicate Done events.
        let mut parser = GeminiStreamParser::new();

        let chunk = serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": "hi"}]},
                "finishReason": "STOP"
            }]
        })
        .to_string();
        let events1 = parser.parse_data(&chunk);
        let done_count_1 = events1
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .count();
        assert_eq!(done_count_1, 1, "first chunk should emit exactly one Done");

        let events2 = parser.parse_data(&chunk);
        let done_count_2 = events2
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .count();
        assert_eq!(done_count_2, 0, "second chunk should not emit another Done");
    }

    #[rstest::rstest]
    fn stop_finish_reason_maps_to_end_turn() {
        // Given a chunk with finishReason STOP.
        // Deleting the "STOP" match arm would produce Other("STOP") instead.
        let mut parser = GeminiStreamParser::new();
        let json = serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": "done"}]},
                "finishReason": "STOP"
            }]
        })
        .to_string();
        let events = parser.parse_data(&json);

        let done = events
            .iter()
            .find(|e| matches!(e, StreamEvent::Done { .. }))
            .expect("should have a Done event");
        match done {
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                ..
            } => {}
            other => panic!("expected EndTurn, got {other:?}"),
        }
    }
}
