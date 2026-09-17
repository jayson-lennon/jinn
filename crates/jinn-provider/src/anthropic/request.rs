//! Request body builder for Anthropic Messages API.
//!
//! Converts [`LlmMessage`] and [`ToolDefinition`] into the JSON body
//! expected by Anthropic's `/v1/messages` endpoint.

use serde::Serialize;

use crate::Attachment;
use crate::LlmMessage;
use jinn_core_types::tool_types::ToolDefinition;

/// Top-level request body for Anthropic Messages API.
#[derive(Debug, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

/// Builds a [`MessagesRequest`] from protocol types.
///
/// The system prompt comes exclusively from the `system_prompt` parameter;
/// the message array is pure conversation and is never inspected for
/// system-level content.
pub fn build_request(
    model: &str,
    messages: &[LlmMessage],
    tools: &[ToolDefinition],
    system_prompt: Option<&str>,
) -> MessagesRequest {
    // Anthropic uses a top-level `system` field.
    let system_text = system_prompt.map(std::borrow::ToOwned::to_owned);

    // All messages go into the messages array.
    let anthropic_messages: Vec<serde_json::Value> = messages.iter().map(message_to_json).collect();

    let anthropic_tools = if tools.is_empty() {
        None
    } else {
        Some(tools.iter().map(tool_definition_to_json).collect())
    };

    let tool_choice = if anthropic_tools.is_some() {
        Some(serde_json::json!({"type": "auto"}))
    } else {
        None
    };

    MessagesRequest {
        model: model.to_owned(),
        messages: anthropic_messages,
        max_tokens: 8192,
        system: system_text,
        stream: true,
        tools: anthropic_tools,
        tool_choice,
    }
}

/// Convert an [`LlmMessage`] to an Anthropic-format message JSON.
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
            // Build a content array of image blocks followed by text.
            let mut blocks: Vec<serde_json::Value> = attachments
                .iter()
                .map(attachment_to_anthropic_block)
                .collect();
            if !content.is_empty() {
                blocks.push(serde_json::json!({
                    "type": "text",
                    "text": content,
                }));
            }
            serde_json::json!({
                "role": "user",
                "content": blocks,
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
            let mut content_blocks: Vec<serde_json::Value> = Vec::new();

            // Include text content if non-empty.
            if !content.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": content,
                }));
            }

            // Tool use blocks.
            for tc in calls {
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::default())),
                }));
            }

            serde_json::json!({
                "role": "assistant",
                "content": content_blocks,
            })
        }
        LlmMessage::Tool {
            tool_call_id,
            content,
            ..
        } => serde_json::json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": content,
            }],
        }),
    }
}

/// Convert a [`ToolDefinition`] to Anthropic-format tool JSON.
fn tool_definition_to_json(def: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "name": def.name,
        "description": def.description,
        "input_schema": def.parameters,
    })
}

/// Renders an [`Attachment`] as an Anthropic image content block:
/// `{ type: "image", source: { type: "base64", media_type, data } }`.
fn attachment_to_anthropic_block(attachment: &Attachment) -> serde_json::Value {
    use base64::Engine as _;
    let Attachment::Image { media_type, data } = attachment;
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": encoded,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn build_request_uses_explicit_system_prompt() {
        // Given conversation messages and an explicit system prompt.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request with the system prompt parameter.
        let req = build_request("claude-3", &messages, &[], Some("You are helpful."));

        // Then system is set from the parameter and messages are untouched.
        assert_eq!(req.system.as_deref(), Some("You are helpful."));
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0]["role"], "user");
    }

    #[rstest::rstest]
    fn build_request_without_system_prompt_leaves_system_absent() {
        // Given conversation messages and no system prompt.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request with None.
        let req = build_request("claude-3", &messages, &[], None);

        // Then the system field is absent.
        assert_eq!(req.system, None);
        assert_eq!(req.messages.len(), 1);
    }

    #[rstest::rstest]
    fn tool_result_message_uses_user_role() {
        let json = message_to_json(&LlmMessage::Tool {
            tool_call_id: "toolu_1".into(),
            name: "echo".into(),
            content: "result".into(),
        });
        assert_eq!(json["role"], "user");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_1");
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_uses_content_blocks() {
        let json = message_to_json(&LlmMessage::Assistant {
            content: String::new(),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "toolu_1".into(),
                name: "echo".into(),
                arguments: r#"{"x":1}"#.into(),
            }]),
        });
        assert_eq!(json["role"], "assistant");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["name"], "echo");
    }

    #[rstest::rstest]
    fn tool_definitions_use_input_schema() {
        let def = ToolDefinition {
            name: "echo".into(),
            description: "Echo".into(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters: serde_json::json!({"type": "object"}),
            server_tool_type: None,
        };
        let json = tool_definition_to_json(&def);
        assert!(json.get("input_schema").is_some());
        assert!(json.get("parameters").is_none());
    }

    #[rstest::rstest]
    fn user_with_attachment_emits_image_block_and_text() {
        // Given a User message with one image attachment and text.
        let msg = LlmMessage::User {
            content: "describe this".into(),
            attachments: vec![Attachment::image("image/png", vec![1, 2, 3])],
        };

        // When converting to Anthropic JSON.
        let json = message_to_json(&msg);

        // Then content is an array with an image block followed by a text block.
        let content = json["content"].as_array().expect("array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
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

        // When converting to Anthropic JSON.
        let json = message_to_json(&msg);

        // Then content stays a plain string (fast path unchanged).
        assert_eq!(json["content"], "hello");
    }
}
