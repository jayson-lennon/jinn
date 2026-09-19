//! Response parser for OpenAI-compatible SSE streaming.
//!
//! Maps parsed SSE JSON payloads to [`StreamEvent`] variants. Handles
//! tool call state tracking across multiple delta chunks and deduplication
//! of `Done` events (OpenRouter sends `finish_reason` then `[DONE]`).
//!
//! ## Usage enrichment across chunks
//!
//! Some providers (notably OpenRouter with `X-OpenRouter-Experimental-Metadata: enabled`)
//! send `finish_reason` in one SSE chunk and `usage` in a subsequent chunk. The parser
//! defers emitting the `Done` event until `[DONE]` arrives (via [`handle_done`]),
//! allowing usage data from any intermediate chunk to be attached before emission.
//!
//! For providers that send `finish_reason` and `usage` in the same chunk, the enrichment
//! happens inline - the pending `Done` is created and enriched in a single `parse_data` call,
//! then emitted by `handle_done`.

use std::collections::HashMap;

use crate::StreamEvent;
use crate::stream_event::{StopReason, StreamUsage};
use jinn_core_types::url_citation::UrlCitation;
use jinn_core_types::tool_types::ToolCall;

/// State tracked per tool call index during streaming.
#[derive(Debug, Default)]
struct ToolCallState {
    /// Tool call ID (set on first chunk).
    id: String,
    /// Function name (set on first chunk).
    name: String,
    /// Accumulated JSON arguments.
    arguments_buffer: String,
    /// Whether we've emitted a `ToolUseStart` for this tool call.
    started: bool,
}

/// A pending `Done` event awaiting usage enrichment.
#[derive(Debug, Clone, PartialEq)]
struct PendingDone {
    /// Why the stream stopped.
    stop_reason: StopReason,
    /// Usage data collected so far (may be enriched across multiple chunks).
    usage: Option<StreamUsage>,
}

/// Stateful parser that tracks tool call accumulation across SSE chunks.
#[derive(Debug, Default)]
pub struct StreamResponseParser {
    /// Per-index tool call state.
    tool_states: HashMap<usize, ToolCallState>,
    /// Whether a Done event has been finalized (prevents duplicates).
    done_finalized: bool,
    /// A pending Done event, buffered until `[DONE]` arrives.
    /// This allows usage data from subsequent SSE chunks (e.g. OpenRouter's
    /// split-chunk format) to be attached before emission.
    pending_done: Option<PendingDone>,
    /// Accumulated `url_citation` annotations gathered across chunks. Emitted
    /// once as a single [`StreamEvent::Citations`] immediately before `Done`.
    accumulated_citations: Vec<UrlCitation>,
}

impl StreamResponseParser {
    /// Create a new parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a single SSE data payload into zero or more `StreamEvent`s.
    ///
    /// Handles:
    /// - `delta.content` → `StreamEvent::Text`
    /// - `delta.reasoning_content` → `StreamEvent::Reasoning`
    /// - `delta.tool_calls[]` → `ToolUseStart`/`ToolUseInputDelta` (state tracked)
    /// - `finish_reason` → `ToolUseComplete` + pending `Done` (drains pending tool calls)
    ///
    /// The `Done` event is deferred: it is stored internally and emitted
    /// when [`handle_done`] is called. This allows usage data from later
    /// SSE chunks to be attached.
    pub fn parse_data(&mut self, json: &str) -> Vec<StreamEvent> {
        let mut results = Vec::new();

        let chunk: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return results,
        };

        // Top-level error object (e.g. OpenRouter context_length_exceeded).
        if let Some(error) = extract_error(&chunk) {
            results.push(error);
            return results;
        }

        let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) else {
            // No choices - but we can still enrich pending_done with usage.
            self.try_enrich_pending_usage(&chunk);
            return results;
        };

        for choice in choices {
            self.process_choice(choice, &mut results);
        }

        // Enrich the pending Done with usage data from this chunk.
        self.try_enrich_pending_usage(&chunk);

        results
    }

    /// Process a single choice's delta into stream events.
    ///
    /// Each delta can carry text content, reasoning content, tool call deltas,
    /// and a finish reason. Tool calls and finish handling are stateful.
    fn process_choice(&mut self, choice: &serde_json::Value, results: &mut Vec<StreamEvent>) {
        let Some(delta) = choice.get("delta") else {
            return;
        };

        if let Some(event) = extract_text(delta) {
            results.push(event);
        }
        if let Some(event) = extract_reasoning(delta) {
            results.push(event);
        }

        // Tool call deltas.
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tool_calls {
                self.handle_tool_call_delta(tc, results);
            }
        }

        // Annotation deltas (e.g. OpenRouter `url_citation` web-search sources).
        if let Some(annotations) = delta.get("annotations").and_then(|a| a.as_array()) {
            for ann in annotations {
                self.accumulate_citation(ann);
            }
        }

        // Finish reason - only handle the first one we see.
        let Some(finish_reason) = choice.get("finish_reason").and_then(|f| f.as_str()) else {
            return;
        };
        if !finish_reason.is_empty() && self.pending_done.is_none() && !self.done_finalized {
            self.handle_finish_reason(finish_reason, results);
        }
    }

    /// Parse a single annotation entry; if it is a `url_citation`, push it into
    /// the accumulated citations. Non-matching annotation types are ignored.
    fn accumulate_citation(&mut self, annotation: &serde_json::Value) {
        if annotation.get("type").and_then(|t| t.as_str()) != Some("url_citation") {
            return;
        }
        let Some(citation) = annotation.get("url_citation") else {
            return;
        };
        let Some(url) = citation.get("url").and_then(|u| u.as_str()) else {
            return;
        };
        let title = citation
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or(url)
            .to_owned();
        self.accumulated_citations.push(crate::UrlCitation {
            url: url.to_owned(),
            title,
            content: citation
                .get("content")
                .and_then(|c| c.as_str())
                .map(std::string::String::from),
            start_index: citation
                .get("start_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            end_index: citation
                .get("end_index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
        });
    }

    /// Try to attach usage data from a chunk to the pending Done event.
    fn try_enrich_pending_usage(&mut self, chunk: &serde_json::Value) {
        let Some(pending) = &mut self.pending_done else {
            return;
        };
        let Some(usage_val) = chunk.get("usage") else {
            return;
        };
        let usage = StreamUsage {
            prompt_tokens: usage_val
                .get("prompt_tokens")
                .and_then(serde_json::Value::as_u64),
            completion_tokens: usage_val
                .get("completion_tokens")
                .and_then(serde_json::Value::as_u64),
            cost: usage_val.get("cost").and_then(serde_json::Value::as_f64),
            cached_tokens: usage_val
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(serde_json::Value::as_u64),
        };
        pending.usage = Some(usage);
    }

    fn handle_tool_call_delta(&mut self, tc: &serde_json::Value, results: &mut Vec<StreamEvent>) {
        let index = tc
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as usize;

        let state = self.tool_states.entry(index).or_default();

        // First chunk: id and name.
        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
            id.clone_into(&mut state.id);
        }

        let function = tc.get("function");

        if let Some(name) = function
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
        {
            name.clone_into(&mut state.name);

            if !state.started {
                state.started = true;
                results.push(StreamEvent::ToolUseStart {
                    index,
                    id: state.id.clone(),
                    name: state.name.clone(),
                });
            }
        }

        // Arguments delta.
        let arguments = function
            .and_then(|f| f.get("arguments"))
            .and_then(|a| a.as_str())
            .unwrap_or("");

        if !arguments.is_empty() {
            state.arguments_buffer.push_str(arguments);
            results.push(StreamEvent::ToolUseInputDelta {
                index,
                partial_json: arguments.to_owned(),
            });
        }
    }

    fn drain_pending_tool_calls(&mut self) -> Vec<StreamEvent> {
        let mut results = Vec::new();
        let mut pending_indices: Vec<usize> = self.tool_states.keys().copied().collect();
        pending_indices.sort_unstable();

        for idx in pending_indices {
            if let Some(state) = self.tool_states.remove(&idx).filter(|s| s.started) {
                results.push(StreamEvent::ToolUseComplete {
                    index: idx,
                    tool_call: ToolCall {
                        id: state.id,
                        name: state.name,
                        arguments: state.arguments_buffer,
                    },
                });
            }
        }

        results
    }

    fn handle_finish_reason(&mut self, finish_reason: &str, results: &mut Vec<StreamEvent>) {
        // Drain all pending tool calls.
        let drained = self.drain_pending_tool_calls();
        results.extend(drained);

        let stop_reason = match finish_reason {
            "tool_calls" => StopReason::ToolUse,
            "stop" => StopReason::EndTurn,
            other => StopReason::Other(other.to_string()),
        };

        // Buffer the Done event instead of emitting immediately.
        // It will be enriched with usage data from subsequent chunks
        // and emitted when handle_done() is called.
        self.pending_done = Some(PendingDone {
            stop_reason,
            usage: None,
        });
    }

    /// Handle the `[DONE]` sentinel from SSE.
    ///
    /// Finalizes the stream: emits any pending `Done` event (enriched with
    /// usage data from prior chunks), or creates a fallback `Done(EndTurn)`
    /// if no `finish_reason` was ever received.
    pub fn handle_done(&mut self) -> Vec<StreamEvent> {
        if self.done_finalized {
            return vec![];
        }

        let mut done_events = self.emit_citations_if_any();

        // If we have a pending Done (from finish_reason), emit it now.
        if let Some(pending) = self.pending_done.take() {
            self.done_finalized = true;
            done_events.push(StreamEvent::Done {
                stop_reason: pending.stop_reason,
                usage: pending.usage,
            });
            return done_events;
        }

        // No finish_reason was ever received - create a fallback Done.
        let mut results = self.drain_pending_tool_calls();
        let stop_reason = if results.is_empty() {
            StopReason::EndTurn
        } else {
            StopReason::ToolUse
        };

        // Emit accumulated citations before the terminal Done, mirroring the
        // pending_done branch.
        results.splice(0..0, self.emit_citations_if_any());

        results.push(StreamEvent::Done {
            stop_reason,
            usage: None,
        });
        self.done_finalized = true;
        results
    }

    /// Drain accumulated citations into a single `StreamEvent::Citations`, if any.
    ///
    /// OpenRouter may spread `url_citation` annotations across many delta chunks;
    /// we accumulate them and emit once, immediately before the terminal `Done`.
    fn emit_citations_if_any(&mut self) -> Vec<StreamEvent> {
        let citations = std::mem::take(&mut self.accumulated_citations);
        if citations.is_empty() {
            vec![]
        } else {
            vec![StreamEvent::Citations(citations)]
        }
    }
}

/// Extract a top-level error object (e.g. OpenRouter `context_length_exceeded`)
/// into a `StreamEvent::Error`. Returns `None` when the chunk has no `error` field.
///
/// Missing `type`/`message` fields fall back to documented defaults.
fn extract_error(chunk: &serde_json::Value) -> Option<StreamEvent> {
    let error_obj = chunk.get("error")?;
    let error_type = error_obj
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown_error")
        .to_owned();
    let message = error_obj
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("Unknown error")
        .to_owned();
    Some(StreamEvent::Error {
        error_type,
        message,
    })
}

/// Extract a non-empty `delta.content` into a `Text` event.
///
/// Returns `None` when the field is absent or empty.
fn extract_text(delta: &serde_json::Value) -> Option<StreamEvent> {
    let content = delta.get("content")?.as_str()?;
    (!content.is_empty()).then(|| StreamEvent::Text(content.to_owned()))
}

/// Extract reasoning/thinking content from a delta.
///
/// Different providers use different field names:
/// - DeepSeek R1/V3: `reasoning_content`
/// - DeepSeek V4: `thinking_content`
/// - OpenRouter: `reasoning`
///
/// Only the first matching field is considered. An empty value suppresses
/// emission and stops the search, matching provider behavior.
fn extract_reasoning(delta: &serde_json::Value) -> Option<StreamEvent> {
    let mut emitted = None;
    for field in ["reasoning_content", "thinking_content", "reasoning"] {
        if let Some(reasoning) = delta.get(field).and_then(|c| c.as_str()) {
            if !reasoning.is_empty() {
                tracing::debug!(
                    field,
                    len = reasoning.len(),
                    preview = %&reasoning[..reasoning.len().min(30)],
                    "response parser: reasoning field matched"
                );
                emitted = Some(StreamEvent::Reasoning(reasoning.to_owned()));
            }
            break;
        }
    }
    if emitted.is_none() {
        let delta_keys: Vec<&str> = delta
            .as_object()
            .map(|o| o.keys().map(std::string::String::as_str).collect())
            .unwrap_or_default();
        tracing::trace!(
            delta_keys = ?delta_keys,
            "response parser: no reasoning field found in delta"
        );
    }
    emitted
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    fn parse_single(json: &str) -> Vec<StreamEvent> {
        let mut parser = StreamResponseParser::new();
        parser.parse_data(json)
    }

    #[rstest::rstest]
    fn text_delta_produces_text_event() {
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Text("Hello".to_owned()));
    }

    #[rstest::rstest]
    fn empty_content_produces_no_event() {
        let json =
            r#"{"id":"x","choices":[{"index":0,"delta":{"content":""},"finish_reason":null}]}"#;
        let events = parse_single(json);
        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn reasoning_delta_produces_reasoning_event() {
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"reasoning_content":"thinking..."},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Reasoning("thinking...".to_owned()));
    }

    #[rstest::rstest]
    fn thinking_content_delta_produces_reasoning_event() {
        // DeepSeek V4 uses `thinking_content` instead of `reasoning_content`.
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"thinking_content":"reasoning..."},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Reasoning("reasoning...".to_owned()));
    }

    #[rstest::rstest]
    fn reasoning_field_delta_produces_reasoning_event() {
        // OpenRouter uses `reasoning` instead of `reasoning_content`.
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"","reasoning":"thinking..."},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Reasoning("thinking...".to_owned()));
    }

    #[rstest::rstest]
    fn reasoning_content_takes_precedence_over_reasoning_field() {
        // Given a delta that carries both `reasoning_content` and `reasoning`.
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"reasoning_content":"primary","reasoning":"secondary"},"finish_reason":null}]}"#;
        let events = parse_single(json);

        // Then the first-listed field (`reasoning_content`) wins.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], StreamEvent::Reasoning("primary".to_owned()));
    }

    #[rstest::rstest]
    fn empty_reasoning_field_suppresses_search() {
        // Given a delta where the first-listed reasoning field is empty,
        // even though a later-listed field carries content. The search
        // stops at the first match, so no event is emitted.
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"reasoning_content":"","reasoning":"ignored"},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn tool_call_start_produces_tool_use_start() {
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::ToolUseStart {
                index: 0,
                id: "call_1".to_owned(),
                name: "get_weather".to_owned(),
            }
        );
    }

    #[rstest::rstest]
    fn tool_call_arguments_delta_accumulates() {
        let mut parser = StreamResponseParser::new();

        // Start.
        let start_json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":""}}]},"finish_reason":null}]}"#;
        let events1 = parser.parse_data(start_json);
        assert!(matches!(&events1[0], StreamEvent::ToolUseStart { name, .. } if name == "echo"));

        // Arguments delta.
        let delta_json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"key\":"}}]},"finish_reason":null}]}"#;
        let events2 = parser.parse_data(delta_json);
        assert_eq!(events2.len(), 1);
        assert!(matches!(
            &events2[0],
            StreamEvent::ToolUseInputDelta { partial_json, .. }
            if partial_json == "{\"key\":"
        ));
    }

    #[rstest::rstest]
    fn finish_reason_stop_produces_done_end_turn() {
        // Given an SSE chunk with finish_reason stop.
        let mut parser = StreamResponseParser::new();
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let events_data = parser.parse_data(json);
        // Done is deferred - parse_data returns no Done event.
        assert!(events_data.is_empty());

        // When [DONE] sentinel arrives.
        let events_done = parser.handle_done();

        // Then Done is emitted with EndTurn.
        assert_eq!(events_done.len(), 1);
        assert!(matches!(
            &events_done[0],
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None
            }
        ));
    }

    #[rstest::rstest]
    fn finish_reason_tool_calls_produces_complete_and_done() {
        let mut parser = StreamResponseParser::new();

        // Set up a tool call.
        let start_json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":""}}]},"finish_reason":null}]}"#;
        parser.parse_data(start_json);

        // Simulate some arguments.
        let args_json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"x\":1}"}}]},"finish_reason":null}]}"#;
        parser.parse_data(args_json);

        // Finish.
        let finish_json =
            r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let events = parser.parse_data(finish_json);

        // ToolUseComplete is emitted immediately; Done is deferred.
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], StreamEvent::ToolUseComplete { index: 0, tool_call } if tool_call.name == "echo" && tool_call.arguments == "{\"x\":1}")
        );

        // [DONE] flushes the pending Done.
        let done_events = parser.handle_done();
        assert_eq!(done_events.len(), 1);
        assert!(matches!(
            &done_events[0],
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[rstest::rstest]
    fn done_sentinel_after_finish_is_noop() {
        let mut parser = StreamResponseParser::new();

        // Finish with stop.
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        parser.parse_data(json);

        // [DONE] sentinel - flushes pending Done.
        let events = parser.handle_done();
        assert_eq!(events.len(), 1);

        // Second [DONE] is a no-op.
        let events2 = parser.handle_done();
        assert!(events2.is_empty());
    }

    #[rstest::rstest]
    fn done_sentinel_without_finish_produces_done() {
        let mut parser = StreamResponseParser::new();
        let events = parser.handle_done();

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: None
            }
        ));
    }

    #[rstest::rstest]
    fn parallel_tool_calls_start() {
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"f1","arguments":""}},{"index":1,"id":"c2","function":{"name":"f2","arguments":""}}]},"finish_reason":null}]}"#;
        let events = parse_single(json);

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolUseStart { name, .. } if name == "f1"));
        assert!(matches!(&events[1], StreamEvent::ToolUseStart { name, .. } if name == "f2"));
    }

    #[rstest::rstest]
    fn invalid_json_produces_no_events() {
        let events = parse_single("not json");
        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn no_choices_produces_no_events() {
        let events = parse_single(r#"{"id":"x"}"#);
        assert!(events.is_empty());
    }

    #[rstest::rstest]
    fn done_sentinel_with_pending_tool_calls() {
        let mut parser = StreamResponseParser::new();

        // Start a tool call.
        let start_json = r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"echo","arguments":""}}]},"finish_reason":null}]}"#;
        parser.parse_data(start_json);

        // [DONE] without finish_reason.
        let events = parser.handle_done();

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], StreamEvent::ToolUseComplete { .. }));
        assert!(matches!(
            &events[1],
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[rstest::rstest]
    fn finish_reason_stop_with_usage_cost_extracts_cost() {
        // Given an SSE chunk with finish_reason stop and usage.cost in the same chunk.
        let mut parser = StreamResponseParser::new();
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":194,"completion_tokens":2,"cost":0.95}}"#;

        // When parsing the chunk then calling handle_done.
        let events_data = parser.parse_data(json);
        // Done is deferred.
        assert!(events_data.is_empty());

        let events_done = parser.handle_done();

        // Then the Done event contains the cost from usage.
        assert_eq!(events_done.len(), 1);
        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cost, Some(0.95));
        assert_eq!(usage.prompt_tokens, Some(194));
        assert_eq!(usage.completion_tokens, Some(2));
    }

    #[rstest::rstest]
    fn finish_reason_stop_with_usage_but_no_cost_returns_none() {
        // Given an SSE chunk with finish_reason stop and usage without cost.
        let mut parser = StreamResponseParser::new();
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":50}}"#;

        // When parsing the chunk then calling handle_done.
        let events_data = parser.parse_data(json);
        assert!(events_data.is_empty());

        let events_done = parser.handle_done();

        // Then the Done event has usage with cost as None.
        assert_eq!(events_done.len(), 1);
        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cost, None);
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.completion_tokens, Some(50));
    }

    #[rstest::rstest]
    fn cached_tokens_parsed_from_prompt_tokens_details() {
        // Given a usage chunk carrying prompt_tokens_details.cached_tokens.
        let mut parser = StreamResponseParser::new();
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1000,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":400}}}"#;

        // When parsing then finalizing.
        parser.parse_data(json);
        let events_done = parser.handle_done();

        // Then the Done usage reports 400 cached tokens.
        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cached_tokens, Some(400));
    }

    #[rstest::rstest]
    fn cached_tokens_none_when_prompt_tokens_details_absent() {
        // Given a usage chunk without prompt_tokens_details.
        let mut parser = StreamResponseParser::new();
        let json = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1000,"completion_tokens":50}}"#;

        // When parsing then finalizing.
        parser.parse_data(json);
        let events_done = parser.handle_done();

        // Then the Done usage reports cached_tokens as None.
        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cached_tokens, None);
    }

    #[rstest::rstest]
    fn cached_tokens_survives_split_chunk_usage() {
        // Given OpenRouter-style split: finish_reason in chunk 1, usage (with
        // prompt_tokens_details) in a second chunk.
        let mut parser = StreamResponseParser::new();

        let chunk1 = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        parser.parse_data(chunk1);

        let chunk2 = r#"{"id":"x","usage":{"prompt_tokens":1000,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":250}}}"#;
        parser.parse_data(chunk2);

        // When [DONE] arrives.
        let events_done = parser.handle_done();

        // Then the enriched Done usage reports the cache hits from chunk 2.
        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cached_tokens, Some(250));
        assert_eq!(usage.prompt_tokens, Some(1000));
    }

    #[rstest::rstest]
    fn full_tool_call_sequence() {
        let mut parser = StreamResponseParser::new();

        // 1. Start.
        let e1 = parser.parse_data(
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
        );
        assert!(matches!(&e1[0], StreamEvent::ToolUseStart { name, .. } if name == "get_weather"));

        // 2. Arguments delta.
        let e2 = parser.parse_data(
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]},"finish_reason":null}]}"#,
        );
        assert!(
            matches!(&e2[0], StreamEvent::ToolUseInputDelta { partial_json, .. } if partial_json == "{\"city\":")
        );

        // 3. More arguments.
        let e3 = parser.parse_data(
            r#"{"id":"x","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]},"finish_reason":null}]}"#,
        );
        assert!(
            matches!(&e3[0], StreamEvent::ToolUseInputDelta { partial_json, .. } if partial_json == "\"Paris\"}")
        );

        // 4. Finish.
        let e4 = parser.parse_data(
            r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        assert_eq!(e4.len(), 1);
        let tc = match &e4[0] {
            StreamEvent::ToolUseComplete { tool_call, .. } => tool_call.clone(),
            _ => panic!("expected ToolUseComplete"),
        };
        assert_eq!(tc.name, "get_weather");
        assert_eq!(tc.arguments, "{\"city\":\"Paris\"}");

        // 5. [DONE] flushes the pending Done.
        let e5 = parser.handle_done();
        assert_eq!(e5.len(), 1);
        assert!(matches!(
            &e5[0],
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                ..
            }
        ));
    }

    #[rstest::rstest]
    fn openrouter_split_chunk_usage_in_separate_chunk_from_finish_reason() {
        // Given OpenRouter-style SSE: finish_reason in chunk 1, usage+cost in chunk 2.
        let mut parser = StreamResponseParser::new();

        // Chunk 1: finish_reason "stop" without usage.
        let chunk1 = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"","role":"assistant","reasoning":null},"finish_reason":"stop","native_finish_reason":"stop"}]}"#;
        let events1 = parser.parse_data(chunk1);
        // Done is deferred - no events from parse_data.
        assert!(events1.is_empty());

        // Chunk 2: second finish_reason with usage.cost (OpenRouter sends both).
        let chunk2 = r#"{"id":"x","choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"stop","native_finish_reason":"stop"}],"usage":{"prompt_tokens":6,"completion_tokens":56,"total_tokens":62,"cost":0.00001308384}}"#;
        let events2 = parser.parse_data(chunk2);
        // No new events - pending_done is enriched with usage from this chunk.
        assert!(events2.is_empty());

        // [DONE] sentinel flushes the enriched pending Done.
        let events_done = parser.handle_done();
        assert_eq!(events_done.len(), 1);

        let usage = match &events_done[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage, got: {:?}", events_done[0]),
        };
        assert_eq!(usage.cost, Some(0.000_013_083_84));
        assert_eq!(usage.prompt_tokens, Some(6));
        assert_eq!(usage.completion_tokens, Some(56));
    }

    #[rstest::rstest]
    fn openrouter_split_chunk_usage_only_in_second_chunk() {
        // Given usage arrives in a chunk WITHOUT a second finish_reason.
        let mut parser = StreamResponseParser::new();

        // Chunk 1: finish_reason without usage.
        let chunk1 = r#"{"id":"x","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        parser.parse_data(chunk1);

        // Chunk 2: usage only (no choices, no finish_reason).
        let chunk2 =
            r#"{"id":"x","usage":{"prompt_tokens":10,"completion_tokens":20,"cost":0.005}}"#;
        parser.parse_data(chunk2);

        // [DONE] flushes with usage from chunk 2.
        let events = parser.handle_done();
        assert_eq!(events.len(), 1);
        let usage = match &events[0] {
            StreamEvent::Done { usage: Some(u), .. } => u.clone(),
            _ => panic!("expected Done with usage"),
        };
        assert_eq!(usage.cost, Some(0.005));
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(20));
    }

    #[rstest::rstest]
    fn top_level_error_object_produces_stream_error() {
        // Given an OpenRouter-style error response.
        let json =
            r#"{"error":{"type":"invalid_request_error","message":"context_length_exceeded"}}"#;

        // When parsing.
        let events = parse_single(json);

        // Then it produces a single StreamEvent::Error.
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Error { error_type, message }
            if error_type == "invalid_request_error" && message == "context_length_exceeded"
        ));
    }

    #[rstest::rstest]
    fn error_object_with_missing_fields_produces_error_with_defaults() {
        // Given an error object with no type or message.
        let json = r#"{"error":{}}"#;

        // When parsing.
        let events = parse_single(json);

        // Then it produces a StreamEvent::Error with defaults.
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Error { error_type, message }
            if error_type == "unknown_error" && message == "Unknown error"
        ));
    }

    #[rstest::rstest]
    fn only_one_done_event_emitted_for_repeated_finish_reasons() {
        // Given a stream that sends finish_reason in two consecutive chunks
        // (OpenRouter does this). The guard `!finish_reason.is_empty() &&
        // self.pending_done.is_none() && !self.done_finalized` must prevent
        // duplicates.
        let mut parser = StreamResponseParser::new();

        // First chunk with finish_reason.
        let chunk1 = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
        .to_string();
        let events1 = parser.parse_data(&chunk1);
        // Done is deferred - no events from parse_data.
        assert!(events1.is_empty());

        // Second chunk also has finish_reason (should be ignored).
        let chunk2 = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
        .to_string();
        let events2 = parser.parse_data(&chunk2);
        assert!(
            events2.is_empty(),
            "duplicate finish_reason should not emit events"
        );

        // [DONE] sentinel flushes exactly one Done.
        let done_events = parser.handle_done();
        assert_eq!(done_events.len(), 1, "should emit exactly one Done event");
    }

    #[rstest::rstest]
    fn single_url_citation_annotation_accumulates_until_done() {
        // Given a stream chunk carrying one url_citation annotation.
        let mut parser = StreamResponseParser::new();
        let json = serde_json::json!({
            "id": "x",
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "See [1].",
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {
                            "url": "https://example.com/a",
                            "title": "Source A",
                            "content": "snippet A",
                            "start_index": 4,
                            "end_index": 7
                        }
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string();
        let events = parser.parse_data(&json);

        // Citations are accumulated; none emitted mid-stream.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, StreamEvent::Citations(_)))
        );

        // When the stream finishes.
        let finish = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
        .to_string();
        parser.parse_data(&finish);
        let done_events = parser.handle_done();

        // Then exactly one Citations event precedes the terminal Done.
        assert_eq!(done_events.len(), 2);
        assert!(matches!(&done_events[0], StreamEvent::Citations(c) if c.len() == 1));
        let StreamEvent::Citations(citations) = &done_events[0] else {
            unreachable!()
        };
        assert_eq!(citations[0].url, "https://example.com/a");
        assert_eq!(citations[0].title, "Source A");
        assert_eq!(citations[0].content.as_deref(), Some("snippet A"));
        assert_eq!(citations[0].start_index, Some(4));
        assert_eq!(citations[0].end_index, Some(7));
    }

    #[rstest::rstest]
    fn multiple_annotation_chunks_merge_into_one_citations_event() {
        // Given two chunks, each carrying one annotation.
        let mut parser = StreamResponseParser::new();
        let chunk1 = serde_json::json!({
            "id": "x",
            "choices": [{
                "index": 0,
                "delta": {
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {"url": "https://example.com/a", "title": "A"}
                    }]
                }
            }]
        })
        .to_string();
        let chunk2 = serde_json::json!({
            "id": "x",
            "choices": [{
                "index": 0,
                "delta": {
                    "annotations": [{
                        "type": "url_citation",
                        "url_citation": {"url": "https://example.com/b", "title": "B"}
                    }]
                }
            }]
        })
        .to_string();
        parser.parse_data(&chunk1);
        parser.parse_data(&chunk2);

        // When the stream finishes.
        let finish = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })
        .to_string();
        parser.parse_data(&finish);
        let done_events = parser.handle_done();

        // Then a single Citations event holds both, in arrival order.
        assert_eq!(done_events.len(), 2);
        let StreamEvent::Citations(citations) = &done_events[0] else {
            panic!("expected Citations event, got {:?}", done_events[0])
        };
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].url, "https://example.com/a");
        assert_eq!(citations[1].url, "https://example.com/b");
    }

    #[rstest::rstest]
    fn no_annotations_emits_no_citations_event() {
        // Given a plain stream with no annotations.
        let mut parser = StreamResponseParser::new();
        let json = serde_json::json!({
            "id": "x",
            "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": "stop"}]
        })
        .to_string();
        parser.parse_data(&json);
        let done_events = parser.handle_done();

        // Then only the Done event is emitted; no Citations.
        assert_eq!(done_events.len(), 1);
        assert!(matches!(done_events[0], StreamEvent::Done { .. }));
    }

    #[rstest::rstest]
    fn non_url_citation_annotations_are_ignored() {
        // Given a chunk carrying an annotation of a different type.
        let mut parser = StreamResponseParser::new();
        let json = serde_json::json!({
            "id": "x",
            "choices": [{
                "index": 0,
                "delta": {
                    "annotations": [{
                        "type": "file_citation",
                        "file_citation": {"file_id": "file_123"}
                    }]
                },
                "finish_reason": "stop"
            }]
        })
        .to_string();
        parser.parse_data(&json);
        let done_events = parser.handle_done();

        // Then no Citations event is emitted.
        assert_eq!(done_events.len(), 1);
        assert!(matches!(done_events[0], StreamEvent::Done { .. }));
    }
}
