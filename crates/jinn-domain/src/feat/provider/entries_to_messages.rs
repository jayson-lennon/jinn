//! Conversion from chat entries to LLM messages.

use crate::feat::provider::llm_message::LlmMessage;
use jinn_core_types::tool_types::ToolCall;
use crate::protocol::{ChatEntry, ChatEntryKind, ContextOverride};

/// Convert chat history entries to LLM messages.
///
/// Produces messages for entries that are in context (as determined by
/// [`ChatEntry::is_in_context()`]). The `is_in_context()` guard at the
/// top of the loop ensures only eligible entries produce messages.
///
/// ## Message mapping
///
/// | Entry kind | LLM message | Notes |
/// |---|---|---|
/// | User | `LlmMessage::User` | Uses `expanded` content |
/// | Assistant | `LlmMessage::Assistant` | Tool calls attached from subsequent `ToolCall` entries |
/// | ToolCall | Attached to previous `Assistant` message | Or creates empty assistant |
/// | ToolResult | `LlmMessage::Tool` | |
/// | System | `LlmMessage::User` with `[System]` prefix | Only when in context (pinned or forced-include) |
/// | Actor | `LlmMessage::User` with `[Actor: source]` prefix | Only when in context |
/// | Error | `LlmMessage::User` with `[Error]` prefix or actionable framing | `[Error]` prefix when `Default` (incl. pinned); actionable framing (`The user has shared...`) when `ForcedInclude` |
/// | Thinking | `LlmMessage::User` with `[Thinking]` prefix | Only when in context |
/// | Transient | `LlmMessage::User` with `[Transient]` prefix | Only when in context |
/// | Compaction | `LlmMessage::User` with summary | Always in context by default |
///
/// Tool-loop atomicity is enforced upstream by the history editor (write
/// time): history mutations expand to whole tool loops, so this converter
/// never sees half a loop. The trailing loop during streaming may be
/// incomplete; the tripwire validator ([`enforce_valid_tool_sequences`])
/// strips any invalid sequence that still slips through (legacy persisted
/// state, future bugs).
///
/// Attaches a tool call to the most recent assistant message,
/// or creates an empty assistant message if none exists (orphan tool call).
fn push_tool_call_message(messages: &mut Vec<LlmMessage>, tool_call: ToolCall) {
    match messages.last_mut() {
        Some(LlmMessage::Assistant { tool_calls, .. }) => {
            tool_calls.get_or_insert_with(Vec::new).push(tool_call);
        }
        _ => {
            // Orphaned tool call - create an empty assistant message.
            messages.push(LlmMessage::Assistant {
                content: String::new(),
                tool_calls: Some(vec![tool_call]),
            });
        }
    }
}

/// Formats an error entry as a user message, using actionable framing for
/// `ForcedInclude` entries and the legacy `[Error]` prefix otherwise.
fn error_to_user_message(text: &str, context_override: ContextOverride) -> LlmMessage {
    let content = match context_override {
        ContextOverride::ForcedInclude => {
            format!("The user has shared the following output for you to address:\n\n{text}")
        }
        // Default and ForcedExclude (unreachable here; already
        // filtered by is_in_context). Pin alone does not trigger
        // the actionable framing.
        ContextOverride::Default | ContextOverride::ForcedExclude => {
            format!("[Error] {text}")
        }
    };
    LlmMessage::User {
        content,
        attachments: Vec::new(),
    }
}

/// Formats a compaction summary as a user message wrapping the summary in XML tags.
fn compaction_to_user_message(summary: &str) -> LlmMessage {
    LlmMessage::User {
        content: format!(
            "The conversation history before this point was compacted into the following summary:\n\n<summary>\n{summary}\n</summary>"
        ),
        attachments: Vec::new(),
    }
}

#[must_use]
pub fn entries_to_messages(entries: &[ChatEntry]) -> Vec<LlmMessage> {
    let mut messages = Vec::new();

    for entry in entries {
        // Defensive: skip entries not in context.
        // The history editor guarantees loop members share context state,
        // but this prevents bugs if entries_to_messages is called on
        // hand-constructed histories (tests, legacy persisted state).
        if !entry.is_in_context() {
            continue;
        }
        match &entry.kind {
            ChatEntryKind::User {
                expanded,
                attachments,
                ..
            } => {
                messages.push(LlmMessage::User {
                    content: expanded.clone(),
                    attachments: attachments.clone(),
                });
            }
            ChatEntryKind::Assistant(text) => {
                messages.push(LlmMessage::Assistant {
                    content: text.clone(),
                    tool_calls: None,
                });
            }
            ChatEntryKind::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                let tool_call = ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                };
                push_tool_call_message(&mut messages, tool_call);
            }
            ChatEntryKind::ToolResult {
                id, name, content, ..
            } => {
                messages.push(LlmMessage::Tool {
                    tool_call_id: id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                });
            }
            // System entries ride as a prefixed User message when in context
            // (pinned or forced-include); the system prompt itself is
            // assembled separately and never originates from history.
            ChatEntryKind::System(content) => {
                messages.push(LlmMessage::User {
                    content: format!("[System] {content}"),
                    attachments: Vec::new(),
                });
            }
            // Actor entries produce a User message when in context
            // (pinned or forced-include).
            ChatEntryKind::Actor { source, text } => {
                messages.push(LlmMessage::User {
                    content: format!("[Actor: {source}] {text}"),
                    attachments: Vec::new(),
                });
            }
            ChatEntryKind::Error(text) => {
                messages.push(error_to_user_message(text, entry.context_override()));
            }
            // Thinking entries produce a User message with [Thinking] prefix
            // when in context (pinned or forced-include).
            ChatEntryKind::Thinking(text) => {
                messages.push(LlmMessage::User {
                    content: format!("[Thinking] {text}"),
                    attachments: Vec::new(),
                });
            }
            // Transient entries produce a User message with [Transient] prefix
            // when in context (pinned or forced-include).
            ChatEntryKind::Transient(text) => {
                messages.push(LlmMessage::User {
                    content: format!("[Transient] {text}"),
                    attachments: Vec::new(),
                });
            }
            ChatEntryKind::Compaction { summary, .. } => {
                messages.push(compaction_to_user_message(summary));
            }
            // Annotations are display-only and never enter LLM context.
            ChatEntryKind::Annotation { .. } => {}
        }
    }

    enforce_valid_tool_sequences(&mut messages);
    messages
}

/// Strips invalid tool-call sequences from an emitted message list.
///
/// A single state-machine pass over the messages:
///
/// - An assistant declaring `tool_calls` opens a batch; the following
///   contiguous `tool` messages must resolve exactly the declared call ids
///   (order-insensitive, no duplicates, no extras).
/// - A `tool` message outside an open batch is dropped (orphan).
/// - A `tool` message that resolves nothing (unknown or duplicate id) is
///   dropped; the batch stays open for the remaining calls.
/// - Any other message while a batch still has unresolved calls closes the
///   batch: the declaring assistant keeps its text but loses `tool_calls`
///   (and is removed entirely when its text was empty). The interrupting
///   message itself is kept.
///
/// This is the safety net for states the write-time history editor cannot
/// produce: legacy persisted sessions and hypothetical future bugs. Normal
/// editor-produced histories never trip it.
pub(crate) fn enforce_valid_tool_sequences(messages: &mut Vec<LlmMessage>) {
    let mut out: Vec<LlmMessage> = Vec::with_capacity(messages.len());
    // Open batch: (declaring assistant's index in `out`, unresolved call ids,
    // resolved tool messages held until the batch validates).
    let mut open_batch: Option<(usize, std::collections::HashSet<String>, Vec<LlmMessage>)> = None;

    let source: Vec<LlmMessage> = std::mem::take(messages);
    for message in source {
        match message {
            LlmMessage::Tool {
                tool_call_id,
                name,
                content,
            } => match &mut open_batch {
                Some((_, remaining, resolved)) => {
                    if remaining.remove(&tool_call_id) {
                        resolved.push(LlmMessage::Tool {
                            tool_call_id,
                            name,
                            content,
                        });
                        if remaining.is_empty() {
                            // Batch fully resolved: commit the held results.
                            if let Some((_, _, mut resolved)) = open_batch.take() {
                                out.append(&mut resolved);
                            }
                        }
                    } else {
                        tracing::warn!(
                            tool_call_id = %tool_call_id,
                            "dropping tool message with unknown or duplicate call id"
                        );
                    }
                }
                None => {
                    tracing::warn!(
                        tool_call_id = %tool_call_id,
                        "dropping orphan tool message (no preceding tool_calls batch)"
                    );
                }
            },
            LlmMessage::Assistant {
                content,
                tool_calls: Some(calls),
            } => {
                close_batch(&mut out, &mut open_batch);
                let ids: std::collections::HashSet<String> =
                    calls.iter().map(|c| c.id.clone()).collect();
                if calls.is_empty() || ids.len() != calls.len() {
                    tracing::warn!(
                        declared = ?calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
                        "stripping assistant tool_calls with duplicate ids"
                    );
                    push_stripped_assistant(&mut out, content);
                } else {
                    let index = out.len();
                    out.push(LlmMessage::Assistant {
                        content,
                        tool_calls: Some(calls),
                    });
                    open_batch = Some((index, ids, Vec::new()));
                }
            }
            LlmMessage::Assistant {
                content,
                tool_calls: None,
            } => {
                close_batch(&mut out, &mut open_batch);
                out.push(LlmMessage::Assistant {
                    content,
                    tool_calls: None,
                });
            }
            other => {
                close_batch(&mut out, &mut open_batch);
                out.push(other);
            }
        }
    }
    close_batch(&mut out, &mut open_batch);
    *messages = out;
}

/// Closes an open batch. A fully resolved batch commits its held results;
/// an unresolved one is stripped: the declaring assistant loses
/// `tool_calls` (removed entirely when its text was empty) and the held
/// results are discarded.
fn close_batch(
    out: &mut Vec<LlmMessage>,
    open_batch: &mut Option<(usize, std::collections::HashSet<String>, Vec<LlmMessage>)>,
) {
    if let Some((index, remaining, resolved)) = open_batch.take() {
        if remaining.is_empty() {
            out.extend(resolved);
        } else {
            tracing::warn!(
                unresolved = ?remaining.iter().cloned().collect::<Vec<_>>(),
                "stripping assistant tool_calls without matching results"
            );
            strip_assistant_calls(out, index);
        }
    }
}

/// Strips `tool_calls` from the assistant at `index`; removes it entirely
/// when its text was empty.
fn strip_assistant_calls(out: &mut Vec<LlmMessage>, index: usize) {
    if let Some(LlmMessage::Assistant {
        content,
        tool_calls,
        ..
    }) = out.get_mut(index)
    {
        *tool_calls = None;
        if content.is_empty() {
            out.remove(index);
        }
    }
}

/// Pushes a text-only assistant, dropping it when the text is empty.
fn push_stripped_assistant(out: &mut Vec<LlmMessage>, content: String) {
    if !content.is_empty() {
        out.push(LlmMessage::Assistant {
            content,
            tool_calls: None,
        });
    }
}
