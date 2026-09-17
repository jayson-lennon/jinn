//! Request body builder for Google Gemini API.
//!
//! Converts [`LlmMessage`] and [`ToolDefinition`] into the JSON body
//! expected by Google's `streamGenerateContent` endpoint.

use serde::Serialize;

use crate::Attachment;
use crate::LlmMessage;
use jinn_core_types::tool_types::ToolDefinition;

/// Top-level request body for Google Gemini API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest {
    /// Conversation contents.
    pub contents: Vec<serde_json::Value>,
    /// System instruction (top-level field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<serde_json::Value>,
    /// Tool declarations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
}

/// Builds a [`GeminiRequest`] from protocol types.
///
/// The system prompt comes exclusively from the `system_prompt` parameter,
/// filling the top-level `systemInstruction` field; the contents array is
/// pure conversation and is never inspected for system-level content.
pub fn build_request(
    system_prompt: Option<&str>,
    messages: &[LlmMessage],
    tools: &[ToolDefinition],
) -> GeminiRequest {
    let system_instruction = system_prompt.map(|text| {
        serde_json::json!({
            "parts": [{"text": text}]
        })
    });

    // All messages → contents array.
    let contents: Vec<serde_json::Value> = messages.iter().map(message_to_json).collect();

    let gemini_tools = if tools.is_empty() {
        None
    } else {
        Some(vec![serde_json::json!({
            "functionDeclarations": tools.iter().map(tool_definition_to_json).collect::<Vec<_>>()
        })])
    };

    GeminiRequest {
        contents,
        system_instruction,
        tools: gemini_tools,
    }
}

/// Convert an [`LlmMessage`] to a Gemini-format content JSON.
fn message_to_json(msg: &LlmMessage) -> serde_json::Value {
    match msg {
        LlmMessage::User {
            content,
            attachments,
        } if attachments.is_empty() => serde_json::json!({
            "role": "user",
            "parts": [{"text": content}]
        }),
        LlmMessage::User {
            content,
            attachments,
        } => {
            // Build parts of inline_data image parts followed by text.
            let mut parts: Vec<serde_json::Value> =
                attachments.iter().map(attachment_to_gemini_part).collect();
            if !content.is_empty() {
                parts.push(serde_json::json!({"text": content}));
            }
            serde_json::json!({
                "role": "user",
                "parts": parts,
            })
        }
        LlmMessage::Assistant {
            content,
            tool_calls: None,
        } => serde_json::json!({
            "role": "model",
            "parts": [{"text": content}]
        }),
        LlmMessage::Assistant {
            content,
            tool_calls: Some(calls),
        } => {
            let mut parts: Vec<serde_json::Value> = Vec::new();

            if !content.is_empty() {
                parts.push(serde_json::json!({"text": content}));
            }

            for tc in calls {
                parts.push(serde_json::json!({
                    "functionCall": {
                        "name": tc.name,
                        "args": serde_json::from_str::<serde_json::Value>(&tc.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::default()))
                    }
                }));
            }

            serde_json::json!({
                "role": "model",
                "parts": parts,
            })
        }
        LlmMessage::Tool {
            tool_call_id: _,
            name,
            content,
        } => serde_json::json!({
            "role": "function",
            "parts": [{
                "functionResponse": {
                    "name": name,
                    "response": {
                        "name": name,
                        "content": serde_json::from_str::<serde_json::Value>(content)
                            .unwrap_or(serde_json::Value::String(content.clone()))
                    }
                }
            }],
        }),
    }
}

/// Convert a [`ToolDefinition`] to Gemini-format function declaration.
fn tool_definition_to_json(def: &ToolDefinition) -> serde_json::Value {
    let properties = def
        .parameters
        .get("properties")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let required = def
        .parameters
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    serde_json::json!({
        "name": def.name,
        "description": def.description,
        "parameters": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

/// Renders an [`Attachment`] as a Gemini inline_data part:
/// `{ inline_data: { mime_type, data } }` (data is base64).
fn attachment_to_gemini_part(attachment: &Attachment) -> serde_json::Value {
    use base64::Engine as _;
    let Attachment::Image { media_type, data } = attachment;
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    serde_json::json!({
        "inlineData": {
            "mimeType": media_type,
            "data": encoded,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn build_request_fills_system_instruction_from_param() {
        // Given conversation messages and an explicit system prompt.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request with the system prompt parameter.
        let req = build_request(Some("Be helpful."), &messages, &[]);

        // Then systemInstruction is filled and contents are untouched.
        assert!(req.system_instruction.is_some());
        let parts = req.system_instruction.as_ref().expect("checked")["parts"]
            .as_array()
            .expect("array");
        assert_eq!(parts[0]["text"].as_str().expect("text"), "Be helpful.");
        assert_eq!(req.contents.len(), 1);
        assert_eq!(req.contents[0]["role"], "user");
    }

    #[rstest::rstest]
    fn build_request_without_system_prompt_leaves_instruction_absent() {
        // Given conversation messages and no system prompt.
        let messages = vec![LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        }];

        // When building request with None.
        let req = build_request(None, &messages, &[]);

        // Then the systemInstruction field is absent.
        assert!(req.system_instruction.is_none());
        assert_eq!(req.contents.len(), 1);
    }

    #[rstest::rstest]
    fn user_message_uses_user_role() {
        let json = message_to_json(&LlmMessage::User {
            content: "hi".into(),
            attachments: Vec::new(),
        });
        assert_eq!(json["role"], "user");
    }

    #[rstest::rstest]
    fn assistant_message_uses_model_role() {
        let json = message_to_json(&LlmMessage::Assistant {
            content: "hey".into(),
            tool_calls: None,
        });
        assert_eq!(json["role"], "model");
    }

    #[rstest::rstest]
    fn tool_result_uses_function_role() {
        let json = message_to_json(&LlmMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "echo".into(),
            content: "result".into(),
        });
        assert_eq!(json["role"], "function");
        let parts = json["parts"].as_array().unwrap();
        assert!(parts[0].get("functionResponse").is_some());
    }

    #[rstest::rstest]
    fn tool_definitions_use_function_declarations() {
        let def = ToolDefinition {
            name: "echo".into(),
            description: "Echo".into(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"]
            }),
            server_tool_type: None,
        };
        let json = tool_definition_to_json(&def);
        assert_eq!(json["name"], "echo");
        assert!(json.get("parameters").is_some());
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_includes_function_call_parts() {
        let json = message_to_json(&LlmMessage::Assistant {
            content: String::new(),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: r#"{"x":1}"#.into(),
            }]),
        });
        assert_eq!(json["role"], "model");
        let parts = json["parts"].as_array().unwrap();
        assert!(parts[0].get("functionCall").is_some());
    }

    #[rstest::rstest]
    fn user_with_attachment_emits_inline_data_and_text() {
        // Given a User message with one image attachment and text.
        let msg = LlmMessage::User {
            content: "describe this".into(),
            attachments: vec![Attachment::image("image/png", vec![1, 2, 3])],
        };

        // When converting to Gemini JSON.
        let json = message_to_json(&msg);

        // Then parts contains an inline_data part followed by a text part.
        let parts = json["parts"].as_array().expect("array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[0]["inlineData"]["data"], "AQID");
        assert_eq!(parts[1]["text"], "describe this");
    }

    #[rstest::rstest]
    fn user_without_attachment_keeps_plain_text_part() {
        // Given a plain-text User message.
        let msg = LlmMessage::User {
            content: "hello".into(),
            attachments: Vec::new(),
        };

        // When converting to Gemini JSON.
        let json = message_to_json(&msg);

        // Then parts is the single-text fast path (array of one text part).
        let parts = json["parts"].as_array().expect("array");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "hello");
    }
}
