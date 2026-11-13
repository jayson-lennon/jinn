//! Bot built-in tools — `conclude` and `notify_parent`.
//!
//! Two direction-named, single-purpose tools. `conclude` records what the
//! bot concluded, for the user; `notify_parent` starts a turn in the
//! parent. There is deliberately no tool that *reads* another bot's
//! reports: a panel of judges stays independent because none can see the
//! others' conclusions.
//!
//! The notification rides the inter-session variant — the same delivery a
//! `tell_session` plain send uses — so the parent's transcript renders it
//! as a message from another session, while every non-render consumer
//! (LLM context, search, tokens, compaction) treats it as a user message.

use crate::tool_types::ToolContext;
use crate::tool_types::tool_error;
use jinn_chat_input_msg::EnqueueUserMessage;
use jinn_core_types::chat_entry::ChatEntry;
use jinn_core_types::tool_types::{ToolCall, ToolDefinition, ToolResult};
use jinn_session_msg::SessionOrigin;
use jinn_session_store_msg::PersistSession;

use super::BoxedToolFuture;

/// The `conclude` tool definition: append to the calling bot's log.
///
/// Named for the act rather than the artifact. A run concludes once, which
/// is why the name is terminal: it rules out calling this repeatedly to
/// build a wall of text, without spending a line of description saying so.
pub fn conclude_definition() -> ToolDefinition {
    ToolDefinition {
        name: "conclude".to_owned(),
        description: "Record what you concluded during your execution. The first line must be a summary of 10 WORDS or fewer, for the human reading it. Remaining lines are context for you to use later. Information saved here will be made available to you later."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![
            "Use `conclude` to leave your concise verdict or findings. The first line should be up to 10 words for human consumption, and the remaining lines are context for you to use later."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "body": {
                    "type": "string",
                    "description": "Your conclusion. Keep it brief."
                }
            },
            "required": ["body"]
        }),
        server_tool_type: None,
    }
}

/// The `notify_parent` tool definition: start a turn in the parent session.
pub fn notify_parent_definition() -> ToolDefinition {
    ToolDefinition {
        name: "notify_parent".to_owned(),
        description: "Send a message to the parent session."
            .to_owned(),
        prompt_snippet: None,
        prompt_guidelines: vec![
            "Call `notify_parent` to send a message to the parent session. This should only be called if your attending rules indicate that you are supposed to interact with your parent."
                .to_owned(),
        ],
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message the parent session receives. Markdown format is supported"
                }
            },
            "required": ["message"]
        }),
        server_tool_type: None,
    }
}

/// Executes `report`: appends to the calling session's own report log.
///
/// Terminal by design — no message reaches the parent. The append happens
/// under the state write lock; the follow-up persist is published on the bus.
pub fn conclude_execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(body) = argument(&call, "body") else {
            return missing_argument(&call, "body");
        };
        let Some(state) = ctx.state.clone() else {
            return tool_error(&call, "conclude is unavailable without shared state");
        };
        let Some(session_id) = ctx.session_id.clone() else {
            return tool_error(&call, "conclude is unavailable without a session context");
        };

        {
            let mut guard = state.write();
            let Some(session) = guard.session.get_mut(&session_id) else {
                return tool_error(&call, "calling session is not live");
            };
            session.append_bot_report(body);
        }

        // Persist so the conclusion survives the process.
        if let Some(bus) = ctx.bus.clone() {
            bus.publish(PersistSession { session_id }).await;
        }

        succeeded(call, "Conclusion recorded.")
    })
}

/// Executes `notify_parent`: enqueues an inter-session message into the
/// parent session.
///
/// The entry is the same `Intersession` kind a `tell_session` plain send
/// builds — the bot as source, the parent as target, and the shared
/// attribution prefix composed by `tell_session`'s one composition site —
/// so the parent's transcript renders it in the inter-session color while
/// LLM context, search, tokens, and compaction all treat it as a user
/// message. Title minting stays keyed to genuine user entries, so an
/// untitled parent keeps its title until the user sends a real message.
///
/// Marks the parent interacted — a programmatic enqueue does not, and without
/// the mark `is_persistable()` gates every save, so the parent's turn would
/// run and silently never reach disk. Nothing else about the parent's turn is
/// altered: the notification is an ordinary turn that may complete
/// however many times the agent chooses to send another.
pub fn notify_parent_execute(call: ToolCall, ctx: ToolContext) -> BoxedToolFuture {
    Box::pin(async move {
        let Some(message) = argument(&call, "message") else {
            return missing_argument(&call, "message");
        };
        let Some(state) = ctx.state.clone() else {
            return tool_error(&call, "notify_parent is unavailable without shared state");
        };
        let Some(session_id) = ctx.session_id.clone() else {
            return tool_error(
                &call,
                "notify_parent is unavailable without a session context",
            );
        };
        let Some(bus) = ctx.bus.clone() else {
            return tool_error(&call, "notify_parent is unavailable without a bus");
        };

        let (parent_id, caller_title) = {
            let guard = state.read();
            let Some(caller) = guard.session.get(&session_id) else {
                return tool_error(&call, "calling session is not live");
            };
            // Only a bot may notify a parent, and there is no way to
            // name a target: the direction is fixed by the caller's own link.
            if caller.origin() != SessionOrigin::Bot {
                return tool_error(&call, "only a bot session can notify a parent");
            }
            let Some(parent_id) = caller.parent_session().clone() else {
                return tool_error(&call, "calling bot has no parent session");
            };
            // The caller's title feeds the attribution prefix; clone it out
            // before any mutable borrow of the session map.
            let caller_title = caller.title().map(str::to_owned);
            (parent_id, caller_title)
        };

        // Mark the parent interacted so its turn is persistable.
        {
            let mut guard = state.write();
            let Some(parent) = guard.session.get_mut(&parent_id) else {
                return tool_error(&call, "parent session is not live");
            };
            parent.mark_interacted();
        }

        // Wake the parent with an inter-session entry attributed to this
        // bot — the same variant and composition site a `tell_session`
        // plain delivery uses. Title minting stays keyed to genuine user
        // entries (the dispatch handlers skip non-User kinds), so the old
        // reason for sending a `User` entry no longer applies.
        let title = crate::tell_session::processed_title(caller_title.as_deref());
        let text = format!(
            "{}{message}",
            crate::tell_session::attribution_prefix(&session_id, title.as_deref())
        );
        bus.publish(EnqueueUserMessage {
            session_id: parent_id.clone(),
            entry: ChatEntry::intersession(session_id, title, parent_id, text),
        })
        .await;

        succeeded(call, "Parent session notified.")
    })
}

/// Reads one string argument from the call's JSON arguments.
fn argument(call: &ToolCall, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&call.arguments)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

/// Builds a failed result for a missing argument, naming the schema slot.
fn missing_argument(call: &ToolCall, key: &str) -> ToolResult {
    tool_error(call, &format!("missing required argument: {key}"))
}

/// Builds a successful result.
fn succeeded(call: ToolCall, content: &str) -> ToolResult {
    ToolResult {
        tool_call_id: call.id,
        name: call.name,
        content: content.to_owned(),
        success: true,
        full_content: None,
        truncation: None,
        pin_position: None,
        attachments: Vec::new(),
    }
}
