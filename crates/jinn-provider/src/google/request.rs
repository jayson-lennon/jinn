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
    /// Tool configuration: mode `NONE` forbids calls without removing
    /// the declarations — Gemini's documented way to suppress tool use
    /// for one request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<serde_json::Value>,
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
    tool_choice: crate::ToolChoice,
) -> GeminiRequest {
    let system_instruction = system_prompt.map(|text| {
        serde_json::json!({
            "parts": [{"text": text}]
        })
    });

    // All messages → contents array. A message with attachments can expand to
    // more than one content, so each message flattens into the array.
    let contents: Vec<serde_json::Value> =
        messages.iter().flat_map(message_to_json_values).collect();

    let gemini_tools = if tools.is_empty() {
        None
    } else {
        Some(vec![serde_json::json!({
            "functionDeclarations": tools.iter().map(tool_definition_to_json).collect::<Vec<_>>()
        })])
    };

    let tool_config = if gemini_tools.is_some() {
        let mode = match tool_choice {
            crate::ToolChoice::Auto => "AUTO",
            crate::ToolChoice::None => "NONE",
        };
        Some(serde_json::json!({
            "functionCallingConfig": { "mode": mode }
        }))
    } else {
        None
    };

    GeminiRequest {
        contents,
        system_instruction,
        tools: gemini_tools,
        tool_config,
    }
}

/// Convert an [`LlmMessage`] to Gemini-format content JSON.
///
/// Most messages produce exactly one content, but a tool result carrying
/// attachments produces two: the function response, then a synthesized user
/// content carrying the images — a `functionResponse` part is text/JSON-only,
/// so the images cannot ride inside it.
fn message_to_json_values(msg: &LlmMessage) -> Vec<serde_json::Value> {
    match msg {
        LlmMessage::User {
            content,
            attachments,
        } if attachments.is_empty() => vec![serde_json::json!({
            "role": "user",
            "parts": [{"text": content}]
        })],
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
            vec![serde_json::json!({
                "role": "user",
                "parts": parts,
            })]
        }
        LlmMessage::Assistant {
            content,
            tool_calls: None,
        } => vec![serde_json::json!({
            "role": "model",
            "parts": [{"text": content}]
        })],
        LlmMessage::Assistant {
            content,
            tool_calls: Some(calls),
        } => vec![assistant_with_calls_content(content, calls)],
        LlmMessage::Tool {
            tool_call_id: _,
            name,
            content,
            attachments,
        } if attachments.is_empty() => vec![serde_json::json!({
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
        })],
        LlmMessage::Tool {
            tool_call_id,
            name,
            content,
            attachments,
        } => {
            // The function response stays as before; the images follow in a
            // synthesized ordinary user turn.
            let function_response = serde_json::json!({
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
            });
            let mut parts: Vec<serde_json::Value> =
                attachments.iter().map(attachment_to_gemini_part).collect();
            parts.push(serde_json::json!({
                "text": format!("[Image attached from tool call {tool_call_id} ({name})]"),
            }));
            vec![
                function_response,
                serde_json::json!({
                    "role": "user",
                    "parts": parts,
                }),
            ]
        }
    }
}

/// Builds the model content carrying text plus one `functionCall` part per
/// pending tool call.
fn assistant_with_calls_content(content: &str, calls: &[crate::ToolCall]) -> serde_json::Value {
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
        .map_or_else(Vec::new, |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::borrow::ToOwned::to_owned))
                .collect::<Vec<String>>()
        });

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
        let req = build_request(Some("Be helpful."), &messages, &[], crate::ToolChoice::Auto);

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
        let req = build_request(None, &messages, &[], crate::ToolChoice::Auto);

        // Then the systemInstruction field is absent.
        assert!(req.system_instruction.is_none());
        assert_eq!(req.contents.len(), 1);
    }

    #[rstest::rstest]
    fn build_request_none_choice_sets_function_calling_mode_none() {
        // Given messages and tool declarations, with a None (forbid) choice.
        let messages = vec![LlmMessage::User {
            content: "write the recap".into(),
            attachments: Vec::new(),
        }];
        let tools = vec![ToolDefinition {
            name: "echo".into(),
            description: "Echo".into(),
            prompt_snippet: None,
            prompt_guidelines: vec![],
            parameters: serde_json::json!({"type": "object"}),
            server_tool_type: None,
        }];

        // When building the request with the forbidding choice.
        let req = build_request(None, &messages, &tools, crate::ToolChoice::None);

        // Then the declarations stay (prefix continuity) and the config
        // carries Gemini's documented suppress mode.
        assert!(req.tools.is_some());
        let config = req.tool_config.as_ref().expect("tool config present");
        let json = serde_json::to_value(config).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"functionCallingConfig": {"mode": "NONE"}})
        );
    }

    #[rstest::rstest]
    fn user_message_uses_user_role() {
        // Given a user message.
        let message = LlmMessage::User {
            content: "hi".into(),
            attachments: Vec::new(),
        };

        // When converting it to the Gemini wire shape.
        let json = message_to_json_values(&message).remove(0);

        // Then its role is "user".
        assert_eq!(json["role"], "user");
    }

    #[rstest::rstest]
    fn assistant_message_uses_model_role() {
        // Given an assistant message with no tool calls.
        let message = LlmMessage::Assistant {
            content: "hey".into(),
            tool_calls: None,
        };

        // When converting it to the Gemini wire shape.
        let json = message_to_json_values(&message).remove(0);

        // Then its role is "model".
        assert_eq!(json["role"], "model");
    }

    #[rstest::rstest]
    fn tool_result_uses_function_role() {
        // Given a tool result message.
        let message = LlmMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "echo".into(),
            content: "result".into(),
            attachments: Vec::new(),
        };

        // When converting it to the Gemini wire shape.
        let json = message_to_json_values(&message).remove(0);

        // Then its role is "function" and its first part is a functionResponse.
        assert_eq!(json["role"], "function");
        let parts = json["parts"].as_array().unwrap();
        assert!(parts[0].get("functionResponse").is_some());
    }

    #[rstest::rstest]
    fn tool_definitions_use_function_declarations() {
        // Given a tool definition with an object parameter schema.
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

        // When converting it to the Gemini wire shape.
        let json = tool_definition_to_json(&def);

        // Then the tool name and its parameters are carried across.
        assert_eq!(json["name"], "echo");
        assert!(json.get("parameters").is_some());
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_includes_function_call_parts() {
        // Given an assistant message carrying one tool call.
        let message = LlmMessage::Assistant {
            content: String::new(),
            tool_calls: Some(vec![jinn_core_types::tool_types::ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: r#"{"x":1}"#.into(),
            }]),
        };

        // When converting it to the Gemini wire shape.
        let json = message_to_json_values(&message).remove(0);

        // Then its role is "model" and its first part is a functionCall.
        assert_eq!(json["role"], "model");
        let parts = json["parts"].as_array().unwrap();
        assert!(parts[0].get("functionCall").is_some());
    }

    #[rstest::rstest]
    fn user_with_attachment_emits_inline_data_and_text() {
        // Given a User message with one image attachment and text.
        let msg = LlmMessage::User {
            content: "describe this".into(),
            attachments: vec![Attachment::image("image/png".to_owned(), vec![1, 2, 3])],
        };

        // When converting to Gemini JSON.
        let json = message_to_json_values(&msg).remove(0);

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
        let json = message_to_json_values(&msg).remove(0);

        // Then parts is the single-text fast path (array of one text part).
        let parts = json["parts"].as_array().expect("array");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "hello");
    }

    #[rstest::rstest]
    fn tool_result_with_attachments_emits_function_response_then_synthesized_user() {
        // Given a tool result message carrying an image attachment.
        let message = LlmMessage::Tool {
            tool_call_id: "call_1".into(),
            name: "read_image".into(),
            content: "screenshot.png".into(),
            attachments: vec![Attachment::image("image/png".to_owned(), vec![1, 2, 3])],
        };

        // When building the request.
        let req = build_request(None, &[message], &[], crate::ToolChoice::Auto);

        // Then two contents are produced: the function response, then a
        // synthesized user content.
        assert_eq!(req.contents.len(), 2);
        assert_eq!(req.contents[0]["role"], "function");
        assert!(
            req.contents[0]["parts"][0]
                .get("functionResponse")
                .is_some()
        );
        // And the synthesized user content carries the inline image then attribution.
        assert_eq!(req.contents[1]["role"], "user");
        let parts = req.contents[1]["parts"].as_array().expect("array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[0]["inlineData"]["data"], "AQID");
        assert_eq!(
            parts[1]["text"],
            "[Image attached from tool call call_1 (read_image)]"
        );
    }
}
