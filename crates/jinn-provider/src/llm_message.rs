//! Protocol-level LLM message types.
//!
//! [`LlmMessage`] is a serializable representation of conversation turns,
//! decoupled from any specific provider's message format.

use serde::{Deserialize, Serialize};

use jinn_core_types::tool_types::ToolCall;

/// A single message in an LLM conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum LlmMessage {
    /// A message from the user.
    User {
        /// The text content of the message.
        content: String,
        /// Non-text attachments (images, future media). Empty for plain-text messages.
        #[serde(default)]
        attachments: Vec<crate::Attachment>,
    },
    /// A message from the AI assistant.
    Assistant {
        /// The text content of the message.
        content: String,
        /// Tool calls the assistant wants to make, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    /// A tool result message.
    Tool {
        /// The ID of the tool call this result is for.
        tool_call_id: String,
        /// The name of the tool that was executed.
        name: String,
        /// The output content.
        content: String,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[rstest::rstest]
    fn backward_compat_user_deserialization() {
        // Given old-format JSON for a user message.
        let json = r#"{"role":"user","content":"hello"}"#;

        // When deserializing.
        let msg: LlmMessage = serde_json::from_str(json).expect("deserialize");

        // Then it produces the expected variant.
        assert_eq!(
            msg,
            LlmMessage::User {
                content: "hello".into(),
                attachments: Vec::new()
            }
        );
    }

    #[rstest::rstest]
    fn backward_compat_assistant_deserialization() {
        // Given old-format JSON for an assistant message.
        let json = r#"{"role":"assistant","content":"hi"}"#;

        // When deserializing.
        let msg: LlmMessage = serde_json::from_str(json).expect("deserialize");

        // Then it produces the expected variant with no tool calls.
        assert_eq!(
            msg,
            LlmMessage::Assistant {
                content: "hi".into(),
                tool_calls: None,
            }
        );
    }

    #[rstest::rstest]
    fn tool_message_roundtrips() {
        let msg = LlmMessage::Tool {
            tool_call_id: "call_1".to_owned(),
            name: "echo".to_owned(),
            content: "result text".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: LlmMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[rstest::rstest]
    fn assistant_with_tool_calls_roundtrips() {
        let msg = LlmMessage::Assistant {
            content: "Let me check.".to_owned(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_owned(),
                name: "echo".to_owned(),
                arguments: r#"{\"input\":\"hi\"}"#.to_owned(),
            }]),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: LlmMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, msg);

        #[rstest::rstest]
        fn user_with_attachments_roundtrips() {
            // Given a user message with an image attachment.
            let msg = LlmMessage::User {
                content: "describe this".to_owned(),
                attachments: vec![crate::Attachment::image("image/png", vec![1, 2, 3])],
            };

            // When serializing and deserializing.
            let json = serde_json::to_string(&msg).expect("serialize");
            let back: LlmMessage = serde_json::from_str(&json).expect("deserialize");

            // Then it roundtrips including the attachment.
            assert_eq!(back, msg);
        }
    }
}
