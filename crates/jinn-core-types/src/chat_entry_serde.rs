//! Serde support for [`ChatEntryKind`].
//!
//! The kind enum is written out variant by variant rather than derived, because
//! its wire shape is a persisted format: each variant serializes to a tagged map
//! whose fields are the ones the session store has always written, and a derived
//! representation would silently reshape every stored session the first time a
//! field was renamed or reordered. The `Deserialize` half carries its own
//! visitor for the same reason — an unknown variant must be a named error rather
//! than a silent default.
//!
//! This lives beside the enum rather than inside it because the machinery and
//! the vocabulary are different jobs: the enum is what every other crate
//! pattern-matches on, and this is the one representation of it that has to
//! stay frozen against history. They must be edited together, which is why they
//! are adjacent modules rather than separate crates.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::chat_entry::{AttachmentOutcome, ChatEntryKind, PinPosition};
use super::tool_result_status::ToolResultStatus;
use crate::SessionId;

impl Serialize for ChatEntryKind {
    #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        match self {
            ChatEntryKind::User {
                display,
                expanded,
                attachments,
                outcome,
            } => {
                #[derive(Serialize)]
                struct UserData {
                    display: String,
                    expanded: String,
                    #[serde(default, skip_serializing_if = "Vec::is_empty")]
                    attachments: Vec<crate::attachment::Attachment>,
                    #[serde(default, skip_serializing_if = "AttachmentOutcome::is_empty")]
                    outcome: AttachmentOutcome,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "User",
                    &UserData {
                        display: display.clone(),
                        expanded: expanded.clone(),
                        attachments: attachments.clone(),
                        outcome: outcome.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::System(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("System", t)?;
                map.end()
            }
            ChatEntryKind::Error(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Error", t)?;
                map.end()
            }
            ChatEntryKind::Assistant(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Assistant", t)?;
                map.end()
            }
            ChatEntryKind::Actor { source, text } => {
                #[derive(Serialize)]
                struct ActorData {
                    source: String,
                    text: String,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "Actor",
                    &ActorData {
                        source: source.clone(),
                        text: text.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::ToolCall {
                id,
                name,
                arguments,
                child_session,
            } => {
                #[derive(Serialize)]
                struct ToolCallData {
                    id: String,
                    name: String,
                    arguments: String,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    child_session: Option<SessionId>,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "ToolCall",
                    &ToolCallData {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                        child_session: child_session.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::ToolResult {
                id,
                name,
                content,
                status,
                full_content,
                truncation,
                pin_position,
                is_alert,
            } => {
                #[derive(Serialize)]
                struct ToolResultData {
                    id: String,
                    name: String,
                    content: String,
                    status: ToolResultStatus,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    full_content: Option<String>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    truncation: Option<crate::tool_types::TruncationMeta>,
                    #[serde(default, skip_serializing_if = "Option::is_none")]
                    pin_position: Option<PinPosition>,
                    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
                    is_alert: bool,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "ToolResult",
                    &ToolResultData {
                        id: id.clone(),
                        name: name.clone(),
                        content: content.clone(),
                        status: *status,
                        full_content: full_content.clone(),
                        truncation: truncation.clone(),
                        pin_position: *pin_position,
                        is_alert: *is_alert,
                    },
                )?;
                map.end()
            }

            ChatEntryKind::Thinking(t) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Thinking", t)?;
                map.end()
            }
            ChatEntryKind::Transient(s) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Transient", s)?;
                map.end()
            }
            ChatEntryKind::Compaction {
                summary,
                tokens_before,
                tokens_after,
                entries_compacted,
                model_used,
            } => {
                #[derive(Serialize)]
                struct CompactionData {
                    summary: String,
                    tokens_before: usize,
                    tokens_after: usize,
                    entries_compacted: usize,
                    model_used: String,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "Compaction",
                    &CompactionData {
                        summary: summary.clone(),
                        tokens_before: *tokens_before,
                        tokens_after: *tokens_after,
                        entries_compacted: *entries_compacted,
                        model_used: model_used.clone(),
                    },
                )?;
                map.end()
            }
            ChatEntryKind::Annotation { citations } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("Annotation", citations)?;
                map.end()
            }
            ChatEntryKind::RuleInterrupt { rule, body } => {
                #[derive(Serialize)]
                struct RuleInterruptData {
                    rule: String,
                    body: String,
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "RuleInterrupt",
                    &RuleInterruptData {
                        rule: rule.clone(),
                        body: body.clone(),
                    },
                )?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ChatEntryKind {
    #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct ChatEntryKindVisitor;

        impl<'de> Visitor<'de> for ChatEntryKindVisitor {
            type Value = ChatEntryKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a ChatEntryKind map")
            }

            #[expect(clippy::too_many_lines, reason = "handler reads best as a single unit")]
            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::missing_field("variant"))?;
                match key.as_str() {
                    "User" => {
                        #[derive(Deserialize)]
                        struct UserData {
                            display: String,
                            expanded: String,
                            #[serde(default)]
                            attachments: Vec<crate::attachment::Attachment>,
                            #[serde(default)]
                            outcome: AttachmentOutcome,
                        }
                        let data: UserData = map.next_value()?;
                        Ok(ChatEntryKind::User {
                            display: data.display,
                            expanded: data.expanded,
                            attachments: data.attachments,
                            outcome: data.outcome,
                        })
                    }
                    "System" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::System(text))
                    }
                    "Error" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Error(text))
                    }
                    "Assistant" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Assistant(text))
                    }
                    "Actor" => {
                        #[derive(Deserialize)]
                        struct ActorData {
                            source: String,
                            text: String,
                        }
                        let data: ActorData = map.next_value()?;
                        Ok(ChatEntryKind::Actor {
                            source: data.source,
                            text: data.text,
                        })
                    }
                    "ToolCall" => {
                        #[derive(Deserialize)]
                        struct ToolCallData {
                            id: String,
                            name: String,
                            arguments: String,
                            #[serde(default)]
                            child_session: Option<SessionId>,
                        }
                        let data: ToolCallData = map.next_value()?;
                        Ok(ChatEntryKind::ToolCall {
                            id: data.id,
                            name: data.name,
                            arguments: data.arguments,
                            child_session: data.child_session,
                        })
                    }
                    "ToolResult" => {
                        // Supports both new format (status: ToolResultStatus + truncation)
                        // and old format (success: bool) for backward compat.
                        #[derive(Deserialize)]
                        struct ToolResultDataNew {
                            id: String,
                            name: String,
                            content: String,
                            status: ToolResultStatus,
                            #[serde(default)]
                            full_content: Option<String>,
                            #[serde(default)]
                            truncation: Option<crate::tool_types::TruncationMeta>,
                            #[serde(default)]
                            pin_position: Option<PinPosition>,
                        }
                        // Try new format first, fall back to old format.
                        #[derive(Deserialize)]
                        struct ToolResultDataOld {
                            id: String,
                            name: String,
                            content: String,
                            success: bool,
                        }
                        let value: serde_json::Value = map.next_value()?;
                        let result = serde_json::from_value::<ToolResultDataNew>(value.clone())
                            .map(|data| ChatEntryKind::ToolResult {
                                id: data.id,
                                name: data.name,
                                content: data.content,
                                status: data.status,
                                full_content: data.full_content,
                                truncation: data.truncation,
                                pin_position: data.pin_position,
                                is_alert: false,
                            })
                            .or_else(|_| {
                                serde_json::from_value::<ToolResultDataOld>(value).map(|data| {
                                    ChatEntryKind::ToolResult {
                                        id: data.id,
                                        name: data.name,
                                        content: data.content,
                                        is_alert: false,
                                        status: if data.success {
                                            ToolResultStatus::Success
                                        } else {
                                            ToolResultStatus::Failure
                                        },
                                        full_content: None,
                                        truncation: None,
                                        pin_position: None,
                                    }
                                })
                            })
                            .map_err(|e| {
                                de::Error::custom(format!("failed to deserialize ToolResult: {e}"))
                            })?;
                        Ok(result)
                    }

                    "Thinking" => {
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::Thinking(text))
                    }
                    "Info" | "Transient" => {
                        // Transient entries are not persisted. If we encounter one in
                        // deserialized data (e.g. from an older version), treat
                        // it as System so we don't lose the text.
                        let text: String = map.next_value()?;
                        Ok(ChatEntryKind::System(text))
                    }
                    "Compaction" => {
                        #[derive(Deserialize)]
                        struct CompactionData {
                            summary: String,
                            tokens_before: usize,
                            #[serde(default)]
                            tokens_after: usize,
                            entries_compacted: usize,
                            model_used: String,
                        }
                        let data: CompactionData = map.next_value()?;
                        Ok(ChatEntryKind::Compaction {
                            summary: data.summary,
                            tokens_before: data.tokens_before,
                            tokens_after: data.tokens_after,
                            entries_compacted: data.entries_compacted,
                            model_used: data.model_used,
                        })
                    }
                    "Annotation" => {
                        let citations: Vec<crate::url_citation::UrlCitation> = map.next_value()?;
                        Ok(ChatEntryKind::Annotation { citations })
                    }
                    "RuleInterrupt" => {
                        #[derive(Deserialize)]
                        struct RuleInterruptData {
                            rule: String,
                            body: String,
                        }
                        let data: RuleInterruptData = map.next_value()?;
                        Ok(ChatEntryKind::RuleInterrupt {
                            rule: data.rule,
                            body: data.body,
                        })
                    }
                    other => Err(de::Error::unknown_variant(
                        other,
                        &[
                            "User",
                            "System",
                            "Error",
                            "Assistant",
                            "Actor",
                            "ToolCall",
                            "ToolResult",
                            "Thinking",
                            "Transient",
                            "Compaction",
                            "Annotation",
                            "RuleInterrupt",
                        ],
                    )),
                }
            }
        }

        deserializer.deserialize_map(ChatEntryKindVisitor)
    }
}
