//! Request body builder for OpenAI-compatible chat completions.
//!
//! Converts [`LlmMessage`] and [`ToolDefinition`] into the JSON body
//! expected by the OpenAI chat completions endpoint.

use serde::Serialize;

use crate::Attachment;
use crate::LlmMessage;
use jinn_core_types::tool_types::ToolDefinition;

/// Top-level request body for OpenAI-compatible chat completions.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub stream: bool,
    /// Request usage data (token counts and cost) in streaming responses.
    pub stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoiceValue>,
    /// Extra body fields merged from config (e.g., `enable_thinking`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Stream options for requesting usage data in streaming responses.
///
/// When `include_usage` is `true`, OpenAI-compatible providers include
/// `usage` (with token counts and cost) in the final SSE chunk.
#[derive(Debug, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// Tool choice parameter.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceValue {
    Auto,
    #[allow(dead_code)]
    None,
    #[allow(dead_code)]
    Required,
}

/// Builds a [`ChatCompletionRequest`] from protocol types.
///
/// The system prompt comes exclusively from the `system_prompt` parameter;
/// when present it becomes a single leading system-role message. The message
/// array is pure conversation and is never inspected for system content.
pub fn build_request(
    model: &str,
    system_prompt: Option<&str>,
    messages: &[LlmMessage],
    tools: &[ToolDefinition],
    extra_body: &serde_json::Map<String, serde_json::Value>,
) -> ChatCompletionRequest {
    // Coalesce consecutive same-role messages (user, assistant without tool_calls)
    // into single messages. Many OpenAI-compatible providers (e.g. ZAI) reject
    // consecutive messages with the same role.
    let borrowed: Vec<&LlmMessage> = messages.iter().collect();
    let coalesced = coalesce_messages(&borrowed);

    // Prepend exactly one system-role message from the explicit parameter,
    // after coalescing, so it stays a distinct leading message.
    let openai_messages = {
        let mut result = Vec::with_capacity(coalesced.len() + 1);
        if let Some(system) = system_prompt {
            result.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }
        result.extend(coalesced);
        result
    };

    let openai_tools = if tools.is_empty() {
        None
    } else {
        Some(tools.iter().map(tool_definition_to_json).collect())
    };

    let tool_choice = if openai_tools.is_some() {
        Some(ToolChoiceValue::Auto)
    } else {
        None
    };

    ChatCompletionRequest {
        model: model.to_owned(),
        messages: openai_messages,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        tools: openai_tools,
        tool_choice,
        extra: extra_body.clone(),
    }
}

/// Coalesce consecutive messages of the same coalescable role into one.
///
/// Only `user` and `assistant` (without tool_calls) messages are coalescable.
/// Messages with tool_calls, tool results, and other role-specific fields
/// are never merged because they carry distinct semantic meaning.
fn coalesce_messages(messages: &[&LlmMessage]) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        let json = message_to_json(msg);
        let can_coalesce = matches!(msg, LlmMessage::User { attachments, .. } if attachments.is_empty())
            || matches!(
                msg,
                LlmMessage::Assistant {
                    tool_calls: None,
                    ..
                }
            );

        if can_coalesce && let Some(last) = result.last_mut() {
            // Only merge if the previous message is also a plain user or
            // assistant (no tool_calls, no tool_call_id) and has the same role.
            let last_is_coalescable = (last["role"] == "user" || last["role"] == "assistant")
                && last.get("tool_calls").is_none()
                && last.get("tool_call_id").is_none();

            if last_is_coalescable && last["role"] == json["role"] {
                // Append content to the previous message of the same role.
                let existing = last["content"].as_str().unwrap_or("");
                let incoming = json["content"].as_str().unwrap_or("");
                last["content"] = serde_json::Value::String(format!("{existing}\n\n{incoming}"));
                continue;
            }
        }

        result.push(json);
    }

    result
}

/// Convert an [`LlmMessage`] to an OpenAI-format JSON object.
fn message_to_json(msg: &LlmMessage) -> serde_json::Value {
    match msg {
        LlmMessage::User {
            content,
            attachments,
        } if attachments.is_empty() => serde_json::json!({
            "role": "user",
            "content": content,
        }),
        LlmMessage::User {
            content,
            attachments,
        } => {
            // Build a content array of image_url blocks followed by text.
            let mut parts: Vec<serde_json::Value> =
                attachments.iter().map(attachment_to_openai_block).collect();
            if !content.is_empty() {
                parts.push(serde_json::json!({
                    "type": "text",
                    "text": content,
                }));
            }
            serde_json::json!({
                "role": "user",
                "content": parts,
            })
        }
        LlmMessage::Assistant {
            content,
            tool_calls: None,
        } => serde_json::json!({
            "role": "assistant",
            "content": content,
        }),
        LlmMessage::Assistant {
            content,
            tool_calls: Some(calls),
        } => {
            let json_calls: Vec<serde_json::Value> = calls.iter().map(tool_call_to_json).collect();
            serde_json::json!({
                "role": "assistant",
                "content": content,
                "tool_calls": json_calls,
            })
        }
        LlmMessage::Tool {
            tool_call_id,
            content,
            ..
        } => serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
}

/// Convert a [`jinn_core_types::tool_types::ToolCall`] to OpenAI-format JSON.
fn tool_call_to_json(tc: &jinn_core_types::tool_types::ToolCall) -> serde_json::Value {
    serde_json::json!({
        "id": tc.id,
        "type": "function",
        "function": {
            "name": tc.name,
            "arguments": tc.arguments,
        },
    })
}

/// Renders an [`Attachment`] as an OpenAI image content part:
/// `{ type: "image_url", image_url: { url: "data:..." } }`.
fn attachment_to_openai_block(attachment: &Attachment) -> serde_json::Value {
    serde_json::json!({
        "type": "image_url",
        "image_url": {
            "url": attachment.data_url(),
        }
    })
}

/// Convert a [`ToolDefinition`] to OpenAI-format JSON.
fn tool_definition_to_json(def: &ToolDefinition) -> serde_json::Value {
    if let Some(ref tool_type) = def.server_tool_type {
        serde_json::json!({
            "type": tool_type.as_str(),
            "parameters": def.parameters,
        })
    } else {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": def.name,
                "description": def.description,
                "parameters": def.parameters,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;
    use jinn_core_types::tool_types::ServerToolType;

    #[rstest::rstest]
    fn build_request_includes_model_and_stream() {
        // Given messages and no tools.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then model and stream are correct.
        assert_eq!(req.model, "gpt-4");
        assert!(req.stream);
        assert!(req.tools.is_none());
    }

    #[rstest::rstest]
    fn build_request_includes_tools() {
        // Given messages and tool definitions.
        let messages = vec![LlmMessage::User {
            content: "what's the weather?".into(),
            attachments: Vec::new(),
        }];
        let tools = vec![ToolDefinition {
            name: "get_weather".into(),
            description: "Get weather".into(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters: serde_json::json!({"type": "object"}),
            server_tool_type: None,
        }];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &tools, &serde_json::Map::new());

        // Then tools are included and tool_choice is auto.
        assert!(req.tools.is_some());
        assert!(matches!(req.tool_choice, Some(ToolChoiceValue::Auto)));
    }

    #[rstest::rstest]
    fn build_request_merges_extra_body() {
        // Given extra_body with a custom field.
        let mut extra = serde_json::Map::new();
        extra.insert("enable_thinking".into(), serde_json::json!(true));

        // When building request.
        let req = build_request("gpt-4", None, &[], &[], &extra);

        // Then extra_body is merged.
        assert_eq!(req.extra.get("enable_thinking").unwrap(), true);
    }

    #[rstest::rstest]
    fn explicit_system_prompt_prepends_single_system_message() {
        // Given conversation messages and an explicit system prompt.
        let messages = vec![
            LlmMessage::User {
                content: "first".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "second".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request with the system prompt parameter.
        let req = build_request(
            "gpt-4",
            Some("You are helpful."),
            &messages,
            &[],
            &serde_json::Map::new(),
        );

        // Then exactly one system message leads and user messages stay coalesced after it.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0]["role"], "system");
        assert_eq!(req.messages[0]["content"], "You are helpful.");
        assert_eq!(req.messages[1]["role"], "user");
        assert_eq!(
            req.messages[1]["content"].as_str().unwrap(),
            "first\n\nsecond"
        );
    }

    #[rstest::rstest]
    fn no_system_prompt_produces_no_system_message() {
        // Given conversation messages and no system prompt.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request with None.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then no system-role message appears.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
    }

    #[rstest::rstest]
    fn tool_result_message_serializes_correctly() {
        let json = message_to_json(&LlmMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "echo".into(),
            content: "result".into(),
        });
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_1");
        assert_eq!(json["content"], "result");
    }

    #[rstest::rstest]
    fn build_request_includes_stream_options() {
        // Given a basic request.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then stream_options requests usage data.
        assert!(req.stream_options.include_usage);
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_serializes_correctly() {
        let json = message_to_json(&LlmMessage::Assistant {
            content: String::new(),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: r#"{"x":1}"#.into(),
            }]),
        });
        assert_eq!(json["role"], "assistant");
        let calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "echo");
    }

    #[rstest::rstest]
    fn consecutive_user_messages_are_coalesced() {
        // Given two consecutive user messages.
        let messages = vec![
            LlmMessage::User {
                content: "first".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "second".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then they are merged into one user message.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(
            req.messages[0]["content"].as_str().unwrap(),
            "first\n\nsecond"
        );
    }

    #[rstest::rstest]
    fn consecutive_assistant_messages_are_coalesced() {
        // Given two consecutive assistant messages without tool calls.
        let messages = vec![
            LlmMessage::Assistant {
                content: "hello".into(),
                tool_calls: None,
            },
            LlmMessage::Assistant {
                content: "world".into(),
                tool_calls: None,
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then they are merged into one assistant message.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "assistant");
        assert_eq!(
            req.messages[0]["content"].as_str().unwrap(),
            "hello\n\nworld"
        );
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_is_not_coalesced() {
        // Given an assistant with tool calls followed by another assistant.
        let messages = vec![
            LlmMessage::Assistant {
                content: "checking".into(),
                tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                }]),
            },
            LlmMessage::Assistant {
                content: "result".into(),
                tool_calls: None,
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then both messages are kept separate.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0]["role"], "assistant");
        assert_eq!(req.messages[1]["role"], "assistant");
    }

    #[rstest::rstest]
    fn tool_messages_are_not_coalesced() {
        // Given two consecutive tool result messages.
        let messages = vec![
            LlmMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "echo".into(),
                content: "result1".into(),
            },
            LlmMessage::Tool {
                tool_call_id: "call_2".into(),
                name: "ls".into(),
                content: "result2".into(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then they are kept as separate messages.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0]["tool_call_id"], "call_1");
        assert_eq!(req.messages[1]["tool_call_id"], "call_2");
    }

    #[rstest::rstest]
    fn alternating_roles_are_not_coalesced() {
        // Given alternating user/assistant messages.
        let messages = vec![
            LlmMessage::User {
                content: "hello".into(),
                attachments: Vec::new(),
            },
            LlmMessage::Assistant {
                content: "hi".into(),
                tool_calls: None,
            },
            LlmMessage::User {
                content: "how are you?".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then all messages are kept separate.
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[1]["role"], "assistant");
        assert_eq!(req.messages[2]["role"], "user");
    }

    #[rstest::rstest]
    fn three_consecutive_user_messages_are_coalesced_into_one() {
        // Given three consecutive user messages.
        let messages = vec![
            LlmMessage::User {
                content: "first".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "second".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "third".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then all three are merged into a single user message.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(
            req.messages[0]["content"].as_str().unwrap(),
            "first\n\nsecond\n\nthird"
        );
    }

    #[rstest::rstest]
    fn user_messages_separated_by_tool_result_are_not_coalesced() {
        // Given user → tool → user.
        let messages = vec![
            LlmMessage::User {
                content: "run the tool".into(),
                attachments: Vec::new(),
            },
            LlmMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "bash".into(),
                content: "ok".into(),
            },
            LlmMessage::User {
                content: "now do another thing".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then the two user messages are kept separate.
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[0]["content"], "run the tool");
        assert_eq!(req.messages[1]["role"], "tool");
        assert_eq!(req.messages[2]["role"], "user");
        assert_eq!(req.messages[2]["content"], "now do another thing");
    }

    #[rstest::rstest]
    fn user_messages_separated_by_assistant_with_tool_calls_are_not_coalesced() {
        // Given user → assistant(with tool_calls) → user.
        let messages = vec![
            LlmMessage::User {
                content: "check the weather".into(),
                attachments: Vec::new(),
            },
            LlmMessage::Assistant {
                content: "looking".into(),
                tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: "{}".into(),
                }]),
            },
            LlmMessage::User {
                content: "what about tomorrow?".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then the two user messages are kept separate.
        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[1]["role"], "assistant");
        assert_eq!(req.messages[2]["role"], "user");
    }

    #[rstest::rstest]
    fn explicit_system_message_stays_leading_and_users_coalesce_after() {
        // Given two consecutive user messages and an explicit system prompt.
        let messages = vec![
            LlmMessage::User {
                content: "hello".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "world".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request with the system prompt parameter.
        let req = build_request(
            "gpt-4",
            Some("Be concise."),
            &messages,
            &[],
            &serde_json::Map::new(),
        );

        // Then the system message leads and user messages are coalesced after it.
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0]["role"], "system");
        assert_eq!(req.messages[0]["content"], "Be concise.");
        assert_eq!(req.messages[1]["role"], "user");
        assert_eq!(
            req.messages[1]["content"].as_str().unwrap(),
            "hello\n\nworld"
        );
    }

    #[rstest::rstest]
    fn coalescing_preserves_content_order_in_multi_turn_tool_flow() {
        // Given a realistic multi-turn conversation with tool use.
        // User → Assistant → ToolCall → ToolResult → User → Assistant
        // The two assistant messages (first has tool_calls, second doesn't)
        // should stay separate.
        let messages = vec![
            LlmMessage::User {
                content: "check the weather".into(),
                attachments: Vec::new(),
            },
            LlmMessage::Assistant {
                content: "Let me check.".into(),
                tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: r#"{"city":"SF"}"#.into(),
                }]),
            },
            LlmMessage::Tool {
                tool_call_id: "call_1".into(),
                name: "get_weather".into(),
                content: "72°F sunny".into(),
            },
            LlmMessage::Assistant {
                content: "It's 72°F and sunny in SF.".into(),
                tool_calls: None,
            },
            LlmMessage::User {
                content: "thanks".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then all messages are preserved in order with no coalescing.
        assert_eq!(req.messages.len(), 5);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[1]["role"], "assistant");
        assert_eq!(req.messages[2]["role"], "tool");
        assert_eq!(req.messages[3]["role"], "assistant");
        assert_eq!(req.messages[4]["role"], "user");
    }

    #[rstest::rstest]
    fn single_message_is_not_modified_by_coalescing() {
        // Given a single user message.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then it is unchanged.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[0]["content"], "hello");
    }

    #[rstest::rstest]
    fn empty_messages_list_produces_empty_request_messages() {
        // Given no messages.
        let messages: Vec<LlmMessage> = vec![];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then request messages is empty.
        assert!(req.messages.is_empty());
    }

    #[rstest::rstest]
    fn coalescing_user_with_empty_content_joins_with_separator() {
        // Given two user messages where the first is empty.
        let messages = vec![
            LlmMessage::User {
                content: String::new(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "actual content".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then they are coalesced with a separator.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(
            req.messages[0]["content"].as_str().unwrap(),
            "\n\nactual content"
        );
    }

    #[rstest::rstest]
    fn realistic_consecutive_user_entries_produces_single_user_message() {
        // Given a pattern matching the ZAI bug: user → actor → error.
        // entries_to_messages maps all of these to User role.
        let messages = vec![
            LlmMessage::User {
                content: "what files are here?".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "[Actor: bash] ls\nfile1.txt\nfile2.txt".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "[Error] connection timed out".into(),
                attachments: Vec::new(),
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then they are merged into one user message.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
        let content = req.messages[0]["content"].as_str().unwrap();
        assert!(content.contains("what files are here?"));
        assert!(content.contains("[Actor: bash]"));
        assert!(content.contains("[Error]"));
    }

    #[rstest::rstest]
    fn tool_definition_to_json_produces_valid_schema() {
        // Given a tool definition.
        let def = ToolDefinition {
            name: "get_weather".into(),
            description: "Get weather".into(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters: serde_json::json!({"type": "object"}),
            server_tool_type: None,
        };

        // When converting to JSON.
        let json = tool_definition_to_json(&def);

        // Then it has the required OpenAI structure.
        assert_eq!(json["type"], "function");
        let func = &json["function"];
        assert_eq!(func["name"], "get_weather");
        assert_eq!(func["description"], "Get weather");
        assert_eq!(func["parameters"]["type"], "object");
    }

    #[rstest::rstest]
    fn server_tool_definition_serializes_with_provider_type() {
        // Given a server tool definition.
        let def = ToolDefinition {
            name: "openrouter:web_search".to_owned(),
            description: "Search the web".to_owned(),
            parameters: serde_json::json!({"engine": "exa", "max_results": 5}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
        };

        // When converting to JSON.
        let json = tool_definition_to_json(&def);

        // Then it has the server tool shape (no function wrapper).
        assert_eq!(json["type"], "openrouter:web_search");
        assert_eq!(json["parameters"]["engine"], "exa");
        assert_eq!(json["parameters"]["max_results"], 5);
        // No "function" key.
        assert!(json.get("function").is_none());
    }

    #[rstest::rstest]
    fn mixed_tools_serialize_correctly() {
        // Given a mix of function and server tools.
        let func_tool = ToolDefinition {
            name: "bash".to_owned(),
            description: "Run command".to_owned(),
            parameters: serde_json::json!({"type": "object"}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: None,
        };
        let server_tool = ToolDefinition {
            name: "openrouter:web_search".to_owned(),
            description: "Search".to_owned(),
            parameters: serde_json::json!({}),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            server_tool_type: Some(ServerToolType::OpenrouterWebSearch),
        };

        // When converting both.
        let func_json = tool_definition_to_json(&func_tool);
        let server_json = tool_definition_to_json(&server_tool);

        // Then function tool has function wrapper.
        assert_eq!(func_json["type"], "function");
        assert!(func_json.get("function").is_some());

        // And server tool has provider type.
        assert_eq!(server_json["type"], "openrouter:web_search");
        assert!(server_json.get("function").is_none());
    }

    #[rstest::rstest]
    fn user_with_attachment_emits_image_url_block_and_text() {
        // Given a User message with one image attachment and text.
        let msg = LlmMessage::User {
            content: "describe this".into(),
            attachments: vec![Attachment::image("image/png", vec![1, 2, 3])],
        };

        // When converting to OpenAI JSON.
        let json = message_to_json(&msg);

        // Then content is an array with an image_url block followed by text.
        let content = json["content"].as_array().expect("array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image_url");
        assert!(
            content[0]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "describe this");
    }

    #[rstest::rstest]
    fn user_without_attachment_keeps_plain_string_content() {
        // Given a plain-text User message.
        let msg = LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        };

        // When converting to OpenAI JSON.
        let json = message_to_json(&msg);

        // Then content stays a plain string (fast path unchanged).
        assert_eq!(json["content"], "hello");
    }

    #[rstest::rstest]
    fn user_with_attachment_is_not_coalesced() {
        // Given two consecutive user messages where the second has an attachment.
        let messages = vec![
            LlmMessage::User {
                content: "first".into(),
                attachments: Vec::new(),
            },
            LlmMessage::User {
                content: "second".into(),
                attachments: vec![Attachment::image("image/png", vec![1])],
            },
        ];

        // When building request.
        let req = build_request("gpt-4", None, &messages, &[], &serde_json::Map::new());

        // Then the two user messages are NOT coalesced (array content can't merge).
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0]["role"], "user");
        assert_eq!(req.messages[1]["role"], "user");
        assert_eq!(req.messages[0]["content"], "first");
        assert!(req.messages[1]["content"].is_array());
    }
}
